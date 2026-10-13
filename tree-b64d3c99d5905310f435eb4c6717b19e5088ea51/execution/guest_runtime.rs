//! Execute a [`GuestJobPlan`] with a Docker CLI. Used by the host Docker
//! backend and by the guest agent (guest-local daemon). Never mounts the
//! host docker.sock.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use velnor_model::{derive_execution_nonce, GuestJobPlan, JobConclusion, VsockMessage};

use super::artifacts::hex_sha256;
use super::backend::ExecutionEvent;
use super::VsockChannel;
use crate::executor::{CommandResult, CommandRunner};

/// Guest agent AF_VSOCK / Firecracker host-connect port.
pub const GUEST_AGENT_PORT: u32 = 5000;

/// Firecracker host-initiated vsock path: `{uds}_{cid}_{port}`.
#[must_use]
pub fn host_vsock_connect_path(uds_path: &Path, guest_cid: u32, port: u32) -> PathBuf {
    PathBuf::from(format!("{}_{guest_cid}_{port}", uds_path.display()))
}

/// Host vsock channel over Firecracker's Unix multiplex socket.
pub struct UnixVsockChannel {
    path: PathBuf,
    stream: Option<UnixStream>,
    timeout: Duration,
}

impl UnixVsockChannel {
    /// Connect on first send/recv to `{uds}_{cid}_{port}`.
    #[must_use]
    pub fn lazy(uds_path: PathBuf, guest_cid: u32, port: u32) -> Self {
        Self {
            path: host_vsock_connect_path(&uds_path, guest_cid, port),
            stream: None,
            timeout: Duration::from_secs(3600),
        }
    }

    fn connected(&mut self) -> Result<&mut UnixStream, String> {
        if self.stream.is_none() {
            let stream = UnixStream::connect(&self.path)
                .map_err(|error| format!("vsock connect {}: {error}", self.path.display()))?;
            stream
                .set_read_timeout(Some(self.timeout))
                .map_err(|error| format!("vsock read timeout: {error}"))?;
            stream
                .set_write_timeout(Some(self.timeout))
                .map_err(|error| format!("vsock write timeout: {error}"))?;
            self.stream = Some(stream);
        }
        self.stream
            .as_mut()
            .ok_or_else(|| "vsock stream missing after connect".into())
    }
}

impl VsockChannel for UnixVsockChannel {
    fn reset(&mut self) {
        self.stream = None;
    }

    fn set_idle_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
        if let Some(stream) = self.stream.as_mut() {
            // Best effort: a failure here surfaces on the next recv.
            let _ = stream.set_read_timeout(Some(timeout));
        }
    }

    fn send(&mut self, message: VsockMessage) -> Result<(), String> {
        let stream = self.connected()?;
        message
            .write_to(stream)
            .map_err(|error| format!("vsock write: {error}"))
    }

    fn recv(&mut self) -> Result<VsockMessage, String> {
        let stream = self.connected()?;
        VsockMessage::read_from(stream).map_err(|error| format!("vsock read: {error}"))
    }
}

/// In-process vsock double. `DeliverPlan` is acknowledged with `TeardownAck`.
#[derive(Debug, Default)]
pub struct LoopbackVsock {
    pub sent: Vec<VsockMessage>,
    pending: Vec<VsockMessage>,
    ready: Option<VsockMessage>,
    rebootstrap_ready: Option<VsockMessage>,
    guest_challenge: Option<String>,
    teardown_ack: Option<(String, String, u64)>,
    teardown_proof: Option<(String, String)>,
}

impl LoopbackVsock {
    /// Construct a test channel with a proved guest readiness frame.
    #[must_use]
    pub fn with_ready(isolation_id: impl Into<String>, generation: u64) -> Self {
        let isolation_id = isolation_id.into();
        let initial_nonce = "00000000-0000-4000-8000-000000000001".to_string();
        Self {
            ready: Some(VsockMessage::GuestReady {
                isolation_id: isolation_id.clone(),
                generation,
                docker_healthy: true,
                job_credentials_absent: true,
                session_challenge: initial_nonce.clone(),
            }),
            rebootstrap_ready: Some(VsockMessage::GuestReady {
                isolation_id,
                generation,
                docker_healthy: true,
                job_credentials_absent: true,
                session_challenge: "00000000-0000-4000-8000-000000000002".into(),
            }),
            guest_challenge: Some(initial_nonce),
            ..Self::default()
        }
    }

    /// Override the acknowledgement identity for negative host-side tests.
    #[must_use]
    pub fn with_teardown_ack(
        mut self,
        job_id: impl Into<String>,
        isolation_id: impl Into<String>,
        generation: u64,
    ) -> Self {
        self.teardown_ack = Some((job_id.into(), isolation_id.into(), generation));
        self
    }

    /// Override the acknowledgement nonce and plan digest for replay tests.
    #[must_use]
    pub fn with_teardown_proof(
        mut self,
        execution_nonce: impl Into<String>,
        plan_sha256: impl Into<String>,
    ) -> Self {
        self.teardown_proof = Some((execution_nonce.into(), plan_sha256.into()));
        self
    }
}

impl VsockChannel for LoopbackVsock {
    fn send(&mut self, message: VsockMessage) -> Result<(), String> {
        if matches!(&message, VsockMessage::PrepareSnapshot) {
            self.pending.push(VsockMessage::SnapshotReady);
            if let Some(ready) = self.rebootstrap_ready.take() {
                if let VsockMessage::GuestReady {
                    session_challenge, ..
                } = &ready
                {
                    self.guest_challenge = Some(session_challenge.clone());
                }
                self.pending.push(ready);
            }
        }
        if let VsockMessage::DeliverPlan {
            job_id,
            isolation_id,
            generation,
            execution_nonce,
            plan_sha256,
            plan_bytes,
        } = &message
        {
            if execution_nonce.is_empty() {
                return Err("loopback rejected empty execution nonce".into());
            }
            let actual_plan_sha256 = hex_sha256(plan_bytes);
            if *plan_sha256 != actual_plan_sha256 {
                return Err(format!(
                    "loopback rejected plan digest {plan_sha256}; expected {actual_plan_sha256}"
                ));
            }
            let session_challenge = self.guest_challenge.as_deref().ok_or_else(|| {
                "loopback rejected DeliverPlan without a guest readiness challenge".to_string()
            })?;
            let expected_nonce = derive_execution_nonce(
                session_challenge,
                job_id,
                isolation_id,
                *generation,
                plan_sha256,
            );
            if *execution_nonce != expected_nonce {
                return Err(format!(
                    "loopback rejected execution nonce {execution_nonce}; expected challenge-bound nonce {expected_nonce}"
                ));
            }
            let plan = decode_guest_plan(plan_bytes)?;
            if plan.job_id != *job_id
                || plan.isolation_id != *isolation_id
                || plan.generation != *generation
            {
                return Err("loopback rejected plan identity mismatch".into());
            }
            validate_guest_plan(&plan)?;
            for path in &plan.command_files {
                self.pending.push(VsockMessage::CommandFile {
                    path: path.clone(),
                    bytes: format!("{path}=bridged\n").into_bytes(),
                });
            }
            let export = result_export_payload(&plan, Some("https://example.test/env"));
            self.pending.push(VsockMessage::ResultExport {
                digest_sha256: hex_sha256(&export),
                bytes: export,
            });
            self.pending.push(VsockMessage::Stdio {
                stream: 1,
                bytes: b"[velnor-step result-bridge]\n".to_vec(),
            });
            let (conclusion, exit_code) = plan
                .planned_conclusion()
                .unwrap_or((JobConclusion::Success, 0));
            self.pending.push(VsockMessage::JobCompleted {
                conclusion,
                exit_code,
            });
            let (job_id, isolation_id, generation) = self
                .teardown_ack
                .take()
                .unwrap_or_else(|| (job_id.clone(), isolation_id.clone(), *generation));
            let (ack_nonce, ack_plan_sha256) = self
                .teardown_proof
                .take()
                .unwrap_or_else(|| (execution_nonce.clone(), plan_sha256.clone()));
            self.pending.push(VsockMessage::TeardownAck {
                job_id,
                isolation_id,
                generation,
                execution_nonce: ack_nonce,
                plan_sha256: ack_plan_sha256,
            });
        }
        self.sent.push(message);
        Ok(())
    }

    fn recv(&mut self) -> Result<VsockMessage, String> {
        if let Some(ready) = self.ready.take() {
            return Ok(ready);
        }
        if self.pending.is_empty() {
            return Err("loopback vsock empty".into());
        }
        Ok(self.pending.remove(0))
    }
}

/// Run the plan: network, services, job container, steps, cleanup.
///
/// # Errors
/// Unsupported guest-plan fields or Docker CLI failures. Does not fall back to
/// another backend.
pub fn execute_guest_plan(
    plan: &GuestJobPlan,
    runner: &mut dyn CommandRunner,
    events: &mut Vec<ExecutionEvent>,
    host_docker: bool,
) -> Result<i32, String> {
    if !host_docker {
        validate_guest_plan(plan)?;
    }
    if let Some((conclusion, exit_code)) = plan.planned_conclusion() {
        record_plan_files(plan, events);
        events.push(ExecutionEvent::JobCompleted {
            conclusion,
            exit_code,
        });
        return Ok(exit_code);
    }
    let label = plan.isolation_label();
    let network = format!("velnor-net-{}", plan.isolation_id);
    docker(
        runner,
        events,
        host_docker,
        &["network", "create", "--label", &label, &network],
    )?;
    for service in &plan.services {
        let mut args = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            service.name.clone(),
            "--network".into(),
            network.clone(),
            "--label".into(),
            label.clone(),
        ];
        if !service.network_alias.is_empty() {
            args.extend(["--network-alias".into(), service.network_alias.clone()]);
        }
        for port in &service.ports {
            args.extend(["-p".into(), port.clone()]);
        }
        for env in &service.env {
            args.extend(["-e".into(), format!("{}={}", env.name, env.value)]);
        }
        args.push(service.image.clone());
        docker_owned(runner, events, host_docker, args)?;
    }
    let job_name = format!("velnor-job-{}", plan.job_id);
    if !plan.image.is_empty() {
        let mut args = vec![
            "run".into(),
            "-d".into(),
            "--name".into(),
            job_name.clone(),
            "--network".into(),
            network.clone(),
            "--label".into(),
            label.clone(),
        ];
        for env in &plan.env {
            args.extend(["-e".into(), format!("{}={}", env.name, env.value)]);
        }
        args.extend([
            "-e".into(),
            "GITHUB_OUTPUT=/github/file_commands/GITHUB_OUTPUT".into(),
            "-e".into(),
            "GITHUB_ENV=/github/file_commands/GITHUB_ENV".into(),
            "-e".into(),
            "GITHUB_PATH=/github/file_commands/GITHUB_PATH".into(),
            "-e".into(),
            "GITHUB_STEP_SUMMARY=/github/file_commands/GITHUB_STEP_SUMMARY".into(),
        ]);
        if !plan.workspace.is_empty() {
            args.extend(["-w".into(), plan.workspace.clone()]);
        }
        args.extend([plan.image.clone(), "sleep".into(), "infinity".into()]);
        docker_owned(runner, events, host_docker, args)?;
    }
    if plan.buildx {
        docker(runner, events, host_docker, &["buildx", "version"])?;
    }
    if !plan.image.is_empty() {
        docker(
            runner,
            events,
            host_docker,
            &[
                "exec",
                &job_name,
                "sh",
                "-c",
                "mkdir -p /github/file_commands && : > /github/file_commands/GITHUB_OUTPUT && : > /github/file_commands/GITHUB_ENV && : > /github/file_commands/GITHUB_PATH && : > /github/file_commands/GITHUB_STEP_SUMMARY",
            ],
        )?;
    }
    let mut code = 0_i32;
    for step in &plan.steps {
        events.push(log_line(&format!("[velnor-step {}]", step.id)));
        let script = super::guest_actions::guest_step_script(step)?;
        let mut exec = vec!["exec".into()];
        for input in &step.inputs {
            let name = input
                .name
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        ch
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            exec.push("-e".into());
            exec.push(format!("VELNOR_INPUT_{name}={}", input.value));
        }
        exec.extend([job_name.clone(), "sh".into(), "-c".into(), script]);
        let result = docker_owned(runner, events, host_docker, exec)?;
        if !result.stdout.is_empty() {
            for line in result.stdout.lines() {
                events.push(log_line(line));
            }
        }
        if !result.stderr.is_empty() {
            for line in result.stderr.lines() {
                events.push(ExecutionEvent::Log {
                    stream: 2,
                    line: line.to_string(),
                });
            }
        }
        if result.code != 0 {
            code = result.code;
            break;
        }
    }
    if !plan.image.is_empty() {
        collect_guest_command_files(plan, &job_name, runner, events, host_docker);
    }
    let _ = docker(runner, events, host_docker, &["rm", "-f", &job_name]);
    for service in plan.services.iter().rev() {
        let _ = docker(runner, events, host_docker, &["rm", "-f", &service.name]);
    }
    let _ = docker(runner, events, host_docker, &["network", "rm", &network]);
    if plan.image.is_empty() {
        record_plan_files(plan, events);
    }
    events.push(ExecutionEvent::JobCompleted {
        conclusion: if code == 0 {
            JobConclusion::Success
        } else {
            JobConclusion::Failure
        },
        exit_code: code,
    });
    Ok(code)
}

pub(crate) fn validate_guest_plan(plan: &GuestJobPlan) -> Result<(), String> {
    if let Some(cache_digest) = plan.cache_digest.as_deref() {
        if !cache_digest.is_empty() {
            return Err(guest_capability_error(
                "guest.cache_digest",
                cache_digest,
                "absent or empty",
            ));
        }
    }
    // Command files are the result_bridge: guest writes GITHUB_* files and
    // the host collects their bytes over vsock. Non-empty is required.
    if !plan.cache.is_empty() {
        return Err(guest_capability_error(
            "guest.cache",
            &format!("{:?}", plan.cache),
            "empty until native guest cache transfer is implemented",
        ));
    }
    if !plan.artifacts.is_empty() {
        return Err(guest_capability_error(
            "guest.artifacts",
            &format!("{:?}", plan.artifacts),
            "empty until native guest artifact export is implemented",
        ));
    }
    if !plan.annotations.is_empty() {
        return Err(guest_capability_error(
            "guest.annotations",
            &format!("{:?}", plan.annotations),
            "empty until native guest annotation transfer is implemented",
        ));
    }
    if !plan.summary.is_empty() {
        return Err(guest_capability_error(
            "guest.summary",
            &plan.summary,
            "empty until native guest summary transfer is implemented",
        ));
    }
    if plan.buildx {
        return Err(guest_capability_error(
            "guest.buildx",
            "true",
            "false until native guest buildx execution is implemented",
        ));
    }
    if plan.testcontainers {
        return Err(guest_capability_error(
            "guest.testcontainers",
            "true",
            "false until native guest testcontainers execution is implemented",
        ));
    }
    if plan.image.trim().is_empty() {
        return Err(guest_capability_error(
            "guest.image",
            "<empty>",
            "non-empty guest image",
        ));
    }
    for (index, step) in plan.steps.iter().enumerate() {
        if let Err(error) = super::guest_actions::guest_step_script(step) {
            return Err(error.replace("guest.steps[].", &format!("guest.steps[{index}].")));
        }
    }
    Ok(())
}

pub(crate) fn decode_guest_plan(plan_bytes: &[u8]) -> Result<GuestJobPlan, String> {
    GuestJobPlan::decode(plan_bytes).map_err(|error| {
        if let Some(field) = unknown_json_field(&error) {
            let received = format!("unknown JSON key '{field}'");
            guest_capability_error(
                &format!("guest.plan.{field}"),
                &received,
                "declared GuestJobPlan JSON fields",
            )
        } else if let Some(field) = missing_json_field(&error) {
            guest_capability_error(
                &format!("guest.plan.{field}"),
                "<missing>",
                "the complete GuestJobPlan schema including this field",
            )
        } else {
            format!(
                "{} ({error})",
                guest_capability_error("guest.plan", "malformed", "valid serialized GuestJobPlan",)
            )
        }
    })
}

fn missing_json_field(error: &str) -> Option<&str> {
    let marker = "missing field `";
    let start = error.find(marker)? + marker.len();
    let end = error[start..].find('`')? + start;
    Some(&error[start..end])
}

fn unknown_json_field(error: &str) -> Option<&str> {
    let marker = "unknown field ";
    let start = error.find(marker)? + marker.len();
    let quote = error.as_bytes().get(start).copied()?;
    if quote != b'`' && quote != b'\'' {
        return None;
    }
    let end = error[start + 1..].find(char::from(quote))? + start + 1;
    Some(&error[start + 1..end])
}

pub(crate) fn guest_capability_error(field: &str, received: &str, accepted: &str) -> String {
    format!(
        "unsupported capability: field '{field}' received '{received}'; accepted '{accepted}'; manifest version {}",
        crate::manifest::MANIFEST_VERSION
    )
}

fn collect_guest_command_files(
    plan: &GuestJobPlan,
    job_name: &str,
    runner: &mut dyn CommandRunner,
    events: &mut Vec<ExecutionEvent>,
    host_docker: bool,
) {
    for path in &plan.command_files {
        let guest_path = format!("/github/file_commands/{path}");
        let bytes = match docker(
            runner,
            events,
            host_docker,
            &["exec", job_name, "cat", &guest_path],
        ) {
            Ok(result) => result.stdout.into_bytes(),
            Err(error) => {
                events.push(ExecutionEvent::Log {
                    stream: 2,
                    line: format!("result_bridge collect {path}: {error}"),
                });
                Vec::new()
            }
        };
        events.push(ExecutionEvent::CommandFile {
            path: path.clone(),
            bytes,
        });
    }
    record_result_export(plan, events);
}

pub(crate) fn record_planned_result_bridge(plan: &GuestJobPlan, events: &mut Vec<ExecutionEvent>) {
    record_plan_files(plan, events);
}

fn record_plan_files(plan: &GuestJobPlan, events: &mut Vec<ExecutionEvent>) {
    for path in &plan.command_files {
        events.push(ExecutionEvent::CommandFile {
            path: path.clone(),
            bytes: Vec::new(),
        });
    }
    record_result_export(plan, events);
}

fn record_result_export(plan: &GuestJobPlan, events: &mut Vec<ExecutionEvent>) {
    for output in &plan.outputs {
        events.push(ExecutionEvent::Output {
            name: output.name.clone(),
            value: output.value.clone(),
        });
    }
    let environment_url = plan
        .outputs
        .iter()
        .find(|output| output.name == "environment_url")
        .map(|output| output.value.as_str());
    let export = result_export_payload(plan, environment_url);
    events.push(ExecutionEvent::ResultExport {
        digest_sha256: super::artifacts::hex_sha256(&export),
        bytes: export,
    });
}

pub(crate) fn result_export_payload(plan: &GuestJobPlan, environment_url: Option<&str>) -> Vec<u8> {
    let outputs: Vec<serde_json::Value> = plan
        .outputs
        .iter()
        .map(|output| {
            serde_json::json!({
                "name": output.name,
                "value": output.value,
            })
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({
        "outputs": outputs,
        "environment_url": environment_url.unwrap_or(""),
    }))
    .unwrap_or_else(|_| b"{\"outputs\":[],\"environment_url\":\"\"}".to_vec())
}

fn docker_owned(
    runner: &mut dyn CommandRunner,
    events: &mut Vec<ExecutionEvent>,
    host_docker: bool,
    owned: Vec<String>,
) -> Result<CommandResult, String> {
    if owned.iter().any(|arg| arg.contains("docker.sock")) {
        return Err("guest plan refused a docker.sock mount".into());
    }
    if host_docker {
        events.push(ExecutionEvent::HostDockerInvoked(format!(
            "docker {}",
            owned.join(" ")
        )));
    } else {
        events.push(ExecutionEvent::GuestDocker(format!(
            "docker {}",
            owned.join(" ")
        )));
    }
    runner
        .run("docker", &owned)
        .map_err(|error| format!("docker {}: {error}", owned.join(" ")))
}

fn docker(
    runner: &mut dyn CommandRunner,
    events: &mut Vec<ExecutionEvent>,
    host_docker: bool,
    args: &[&str],
) -> Result<CommandResult, String> {
    let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    docker_owned(runner, events, host_docker, owned)
}

fn log_line(line: &str) -> ExecutionEvent {
    ExecutionEvent::Log {
        stream: 1,
        line: line.to_string(),
    }
}

/// Handle a vsock plan payload: decode, run, refuse host secrets.
///
/// # Errors
/// Decode or docker failures.
pub fn handle_delivered_plan(
    plan_bytes: &[u8],
    runner: &mut dyn CommandRunner,
    events: &mut Vec<ExecutionEvent>,
) -> Result<i32, String> {
    let plan = decode_guest_plan(plan_bytes)?;
    if plan_bytes.windows(11).any(|w| w == b"docker.sock") {
        return Err("delivered plan mentioned docker.sock".into());
    }
    execute_guest_plan(&plan, runner, events, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::RecordingCommands;
    use crate::executor::CommandResult;
    use velnor_model::{GuestService, GuestStep, JobConclusion};

    fn sample_plan() -> GuestJobPlan {
        GuestJobPlan {
            isolation_id: "job-1".into(),
            generation: 1,
            job_id: "job-1".into(),
            image: "velnor/job-ubuntu:26.04".into(),
            services: vec![GuestService {
                name: "pg".into(),
                image: "postgres:16".into(),
                network_alias: "postgres".into(),
                ports: vec!["5432".into()],
                env: Vec::new(),
            }],
            steps: vec![GuestStep {
                id: "run".into(),
                script: "echo hi".into(),
                action: None,
                inputs: Vec::new(),
            }],
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: Vec::new(),
            outputs: vec![velnor_model::GuestOutput {
                name: "result".into(),
                value: "ok".into(),
            }],
            env: vec![velnor_model::GuestEnvVar {
                name: "CI".into(),
                value: "true".into(),
            }],
            workspace: "/__w".into(),
            cache: Vec::new(),
            artifacts: Vec::new(),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: false,
            testcontainers: false,
        }
    }

    #[test]
    fn guest_plan_uses_guest_docker_and_refuses_host_socket() {
        let mut runner = RecordingCommands {
            next: CommandResult {
                code: 0,
                stdout: "ok".into(),
                stderr: String::new(),
            },
            ..RecordingCommands::default()
        };
        let mut events = Vec::new();
        let code = execute_guest_plan(&sample_plan(), &mut runner, &mut events, false).unwrap();
        assert_eq!(code, 0);
        assert!(events.iter().any(|event| matches!(
            event,
            ExecutionEvent::JobCompleted {
                conclusion: JobConclusion::Success,
                exit_code: 0
            }
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::GuestDocker(_))));
        assert!(events
            .iter()
            .all(|event| !matches!(event, ExecutionEvent::HostDockerInvoked(_))));
        assert!(runner
            .calls
            .iter()
            .all(|(_, args)| args.iter().all(|arg| !arg.contains("docker.sock"))));
        assert!(runner.calls.iter().any(|(_, args)| {
            args.windows(2)
                .any(|w| w == ["--network-alias", "postgres"])
        }));
        assert!(runner
            .calls
            .iter()
            .any(|(_, args)| args.windows(2).any(|w| w == ["-e", "CI=true"])));
        assert!(runner
            .calls
            .iter()
            .any(|(_, args)| args.windows(2).any(|w| w == ["-w", "/__w"])));
        assert!(events.iter().any(|event| matches!(
            event,
            ExecutionEvent::Output { name, value } if name == "result" && value == "ok"
        )));
    }

    #[test]
    fn empty_guest_script_fails_closed_before_docker_side_effects() {
        let mut plan = sample_plan();
        plan.steps[0].script.clear();
        let mut runner = RecordingCommands::default();
        let error = execute_guest_plan(&plan, &mut runner, &mut Vec::new(), false).unwrap_err();
        assert!(error.contains("guest.steps[0].script"), "{error}");
        assert!(error.contains("received '<empty>'"), "{error}");
        assert!(error.contains("manifest version"), "{error}");
        assert!(
            runner.calls.is_empty(),
            "guest plan ran Docker: {:?}",
            runner.calls
        );
    }

    #[test]
    fn guest_checkout_action_runs_git_inside_job_container() {
        let mut plan = sample_plan();
        plan.steps = vec![GuestStep {
            id: "checkout".into(),
            script: String::new(),
            action: Some("actions/checkout".into()),
            inputs: vec![
                velnor_model::GuestEnvVar {
                    name: "clone_url".into(),
                    value: "https://github.com/tailrocks/velnor.git".into(),
                },
                velnor_model::GuestEnvVar {
                    name: "version".into(),
                    value: "abc123".into(),
                },
                velnor_model::GuestEnvVar {
                    name: "destination".into(),
                    value: "/__w/velnor".into(),
                },
            ],
        }];
        let mut runner = RecordingCommands {
            next: CommandResult {
                code: 0,
                stdout: "ok".into(),
                stderr: String::new(),
            },
            ..RecordingCommands::default()
        };
        let mut events = Vec::new();
        let code = execute_guest_plan(&plan, &mut runner, &mut events, false).unwrap();
        assert_eq!(code, 0);
        assert!(runner.calls.iter().any(|(_, args)| {
            args.iter()
                .any(|arg| arg.contains("VELNOR_INPUT_clone_url="))
                && args.iter().any(|arg| arg.contains("git -C"))
        }));
        assert!(events.iter().any(|event| matches!(
            event,
            ExecutionEvent::JobCompleted {
                conclusion: JobConclusion::Success,
                ..
            }
        )));
    }

    #[test]
    fn guest_command_files_are_collected_as_result_bridge_bytes() {
        let mut plan = sample_plan();
        plan.command_files = vec!["GITHUB_ENV".into()];
        let mut runner = RecordingCommands {
            next: CommandResult {
                code: 0,
                stdout: "FOO=bar\n".into(),
                stderr: String::new(),
            },
            ..RecordingCommands::default()
        };
        let mut events = Vec::new();
        execute_guest_plan(&plan, &mut runner, &mut events, false).unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            ExecutionEvent::CommandFile { path, bytes }
                if path == "GITHUB_ENV" && bytes == b"FOO=bar\n"
        )));
        assert!(events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::ResultExport { .. })));
    }

    #[test]
    fn guest_action_fails_closed_before_docker_side_effects() {
        let mut plan = sample_plan();
        plan.steps[0].action = Some("actions/unknown@sha".into());
        let mut runner = RecordingCommands::default();
        let error = execute_guest_plan(&plan, &mut runner, &mut Vec::new(), false).unwrap_err();
        assert!(error.contains("guest.steps[0].action"), "{error}");
        assert!(error.contains("actions/unknown@sha"), "{error}");
        assert!(error.contains("manifest version"), "{error}");
        assert!(
            runner.calls.is_empty(),
            "guest plan ran Docker: {:?}",
            runner.calls
        );
    }

    fn assert_unsupported_guest_field(
        plan: GuestJobPlan,
        field: &str,
        received: &str,
        accepted: &str,
    ) {
        let mut runner = RecordingCommands::default();
        let error = execute_guest_plan(&plan, &mut runner, &mut Vec::new(), false).unwrap_err();
        assert!(error.contains(&format!("field '{field}'")), "{error}");
        assert!(error.contains(&format!("received '{received}'")), "{error}");
        assert!(error.contains(&format!("accepted '{accepted}'")), "{error}");
        assert!(
            error.contains(&format!(
                "manifest version {}",
                crate::manifest::MANIFEST_VERSION
            )),
            "{error}"
        );
        assert!(
            runner.calls.is_empty(),
            "guest plan ran Docker: {:?}",
            runner.calls
        );
    }

    #[test]
    fn unsupported_guest_fields_fail_closed_before_docker_side_effects() {
        let mut plan = sample_plan();
        plan.cache_digest = Some("sha256:cache".into());
        assert_unsupported_guest_field(
            plan,
            "guest.cache_digest",
            "sha256:cache",
            "absent or empty",
        );

        let mut plan = sample_plan();
        plan.cache = vec![velnor_model::GuestCacheOp {
            digest: "sha256:cache".into(),
        }];
        assert_unsupported_guest_field(
            plan,
            "guest.cache",
            "[GuestCacheOp { digest: \"sha256:cache\" }]",
            "empty until native guest cache transfer is implemented",
        );

        let mut plan = sample_plan();
        plan.artifacts = vec![velnor_model::GuestArtifactOp {
            name: "logs".into(),
            path: "/__w/logs".into(),
        }];
        assert_unsupported_guest_field(
            plan,
            "guest.artifacts",
            "[GuestArtifactOp { name: \"logs\", path: \"/__w/logs\" }]",
            "empty until native guest artifact export is implemented",
        );

        let mut plan = sample_plan();
        plan.annotations = vec!["notice".into()];
        assert_unsupported_guest_field(
            plan,
            "guest.annotations",
            "[\"notice\"]",
            "empty until native guest annotation transfer is implemented",
        );

        let mut plan = sample_plan();
        plan.summary = "ok".into();
        assert_unsupported_guest_field(
            plan,
            "guest.summary",
            "ok",
            "empty until native guest summary transfer is implemented",
        );

        let mut plan = sample_plan();
        plan.buildx = true;
        assert_unsupported_guest_field(
            plan,
            "guest.buildx",
            "true",
            "false until native guest buildx execution is implemented",
        );

        let mut plan = sample_plan();
        plan.testcontainers = true;
        assert_unsupported_guest_field(
            plan,
            "guest.testcontainers",
            "true",
            "false until native guest testcontainers execution is implemented",
        );
    }

    #[test]
    fn unknown_guest_json_key_fails_closed_before_docker_side_effects() {
        let plan = sample_plan();
        let mut json = serde_json::to_value(plan).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("unadmitted".into(), serde_json::json!("value"));
        let bytes = serde_json::to_vec(&json).unwrap();
        let mut runner = RecordingCommands::default();
        let error = handle_delivered_plan(&bytes, &mut runner, &mut Vec::new()).unwrap_err();
        assert!(error.contains("field 'guest.plan.unadmitted'"), "{error}");
        assert!(
            error.contains("received 'unknown JSON key 'unadmitted''"),
            "{error}"
        );
        assert!(
            error.contains("accepted 'declared GuestJobPlan JSON fields'"),
            "{error}"
        );
        assert!(
            error.contains(&format!(
                "manifest version {}",
                crate::manifest::MANIFEST_VERSION
            )),
            "{error}"
        );
        assert!(runner.calls.is_empty());
    }

    #[test]
    fn missing_guest_json_field_fails_closed_before_docker_side_effects() {
        let mut json = serde_json::to_value(sample_plan()).unwrap();
        json.as_object_mut().unwrap().remove("workspace");
        let mut runner = RecordingCommands::default();
        let error = handle_delivered_plan(
            &serde_json::to_vec(&json).unwrap(),
            &mut runner,
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(error.contains("field 'guest.plan.workspace'"), "{error}");
        assert!(error.contains("received '<missing>'"), "{error}");
        assert!(
            error.contains("accepted 'the complete GuestJobPlan schema including this field'"),
            "{error}"
        );
        assert!(error.contains("manifest version"), "{error}");
        assert!(runner.calls.is_empty());
    }

    #[test]
    fn delivered_plan_rejects_docker_sock_bytes() {
        let mut plan = sample_plan();
        plan.steps[0].script = "cat /var/run/docker.sock".into();
        let bytes = plan.encode().unwrap();
        let mut runner = RecordingCommands::default();
        let mut events = Vec::new();
        let error = handle_delivered_plan(&bytes, &mut runner, &mut events).unwrap_err();
        assert!(error.contains("docker.sock"), "{error}");
        assert!(runner.calls.is_empty());
    }

    #[test]
    fn loopback_vsock_acks_deliver_plan() {
        let mut vsock = LoopbackVsock::with_ready("job-1", 1);
        assert!(matches!(
            vsock.recv().unwrap(),
            VsockMessage::GuestReady { .. }
        ));
        let plan = sample_plan();
        let plan_bytes = plan.encode().unwrap();
        let plan_sha256 = hex_sha256(&plan_bytes);
        let execution_nonce = derive_execution_nonce(
            "00000000-0000-4000-8000-000000000001",
            &plan.job_id,
            &plan.isolation_id,
            plan.generation,
            &plan_sha256,
        );
        vsock
            .send(VsockMessage::DeliverPlan {
                job_id: plan.job_id.clone(),
                isolation_id: plan.isolation_id.clone(),
                generation: plan.generation,
                execution_nonce,
                plan_sha256: plan_sha256.clone(),
                plan_bytes,
            })
            .unwrap();
        loop {
            match vsock.recv().unwrap() {
                VsockMessage::CommandFile { .. }
                | VsockMessage::ResultExport { .. }
                | VsockMessage::Stdio { .. } => {}
                VsockMessage::JobCompleted {
                    conclusion: JobConclusion::Success,
                    exit_code: 0,
                } => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(matches!(
            vsock.recv().unwrap(),
            VsockMessage::TeardownAck {
                job_id,
                isolation_id,
                generation: 1,
                execution_nonce,
                plan_sha256,
            } if job_id == "job-1" && isolation_id == "job-1"
                && execution_nonce == derive_execution_nonce(
                    "00000000-0000-4000-8000-000000000001",
                    "job-1",
                    "job-1",
                    1,
                    &hex_sha256(&plan.encode().unwrap()),
                )
                && plan_sha256 == hex_sha256(&plan.encode().unwrap())
        ));
        assert!(matches!(vsock.sent[0], VsockMessage::DeliverPlan { .. }));
    }
}

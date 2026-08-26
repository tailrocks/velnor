//! Execute a [`GuestJobPlan`] with a Docker CLI. Used by the host Docker
//! backend and by the guest agent (guest-local daemon). Never mounts the
//! host docker.sock.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use velnor_model::{GuestJobPlan, JobConclusion, VsockMessage};

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
}

impl VsockChannel for LoopbackVsock {
    fn send(&mut self, message: VsockMessage) -> Result<(), String> {
        if let VsockMessage::DeliverPlan {
            isolation_id,
            generation,
            plan_bytes,
        } = &message
        {
            let (conclusion, exit_code, command_files) = match GuestJobPlan::decode(plan_bytes) {
                Ok(plan) => {
                    let (conclusion, exit_code) = plan
                        .planned_conclusion()
                        .unwrap_or((JobConclusion::Success, 0));
                    (conclusion, exit_code, plan.command_files)
                }
                Err(_) => (JobConclusion::Failure, 1, Vec::new()),
            };
            for path in command_files {
                self.pending.push(VsockMessage::CommandFile {
                    path,
                    bytes: Vec::new(),
                });
            }
            self.pending.push(VsockMessage::JobCompleted {
                conclusion,
                exit_code,
            });
            self.pending.push(VsockMessage::TeardownAck {
                isolation_id: isolation_id.clone(),
                generation: *generation,
            });
        }
        self.sent.push(message);
        Ok(())
    }

    fn recv(&mut self) -> Result<VsockMessage, String> {
        if self.pending.is_empty() {
            return Err("loopback vsock empty".into());
        }
        Ok(self.pending.remove(0))
    }
}

/// Run the plan: network, services, job container, steps, cleanup.
///
/// # Errors
/// Docker CLI failures. Does not fall back to another backend.
pub fn execute_guest_plan(
    plan: &GuestJobPlan,
    runner: &mut dyn CommandRunner,
    events: &mut Vec<ExecutionEvent>,
    host_docker: bool,
) -> Result<i32, String> {
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
        if !plan.workspace.is_empty() {
            args.extend(["-w".into(), plan.workspace.clone()]);
        }
        args.extend([plan.image.clone(), "sleep".into(), "infinity".into()]);
        docker_owned(runner, events, host_docker, args)?;
    }
    if plan.buildx {
        docker(runner, events, host_docker, &["buildx", "version"])?;
    }
    let mut code = 0_i32;
    for step in &plan.steps {
        events.push(log_line(&format!("[velnor-step {}]", step.id)));
        let script = if step.script.is_empty() {
            format!("echo {}", step.id)
        } else {
            step.script.clone()
        };
        let result = docker(
            runner,
            events,
            host_docker,
            &["exec", &job_name, "sh", "-c", &script],
        )?;
        if !result.stdout.is_empty() {
            for line in result.stdout.lines() {
                events.push(log_line(line));
            }
        }
        if result.code != 0 {
            code = result.code;
            break;
        }
    }
    let _ = docker(runner, events, host_docker, &["rm", "-f", &job_name]);
    for service in plan.services.iter().rev() {
        let _ = docker(runner, events, host_docker, &["rm", "-f", &service.name]);
    }
    let _ = docker(runner, events, host_docker, &["network", "rm", &network]);
    record_plan_files(plan, events);
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

fn record_plan_files(plan: &GuestJobPlan, events: &mut Vec<ExecutionEvent>) {
    for path in &plan.command_files {
        events.push(ExecutionEvent::CommandFile { path: path.clone() });
    }
    for output in &plan.outputs {
        events.push(ExecutionEvent::Output {
            name: output.name.clone(),
            value: output.value.clone(),
        });
    }
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
    let plan = GuestJobPlan::decode(plan_bytes)?;
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
            }],
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec!["GITHUB_OUTPUT".into()],
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
        let mut vsock = LoopbackVsock::default();
        let plan = sample_plan();
        vsock
            .send(VsockMessage::DeliverPlan {
                isolation_id: plan.isolation_id.clone(),
                generation: plan.generation,
                plan_bytes: plan.encode().unwrap(),
            })
            .unwrap();
        assert!(matches!(
            vsock.recv().unwrap(),
            VsockMessage::CommandFile { path, .. } if path == "GITHUB_OUTPUT"
        ));
        assert!(matches!(
            vsock.recv().unwrap(),
            VsockMessage::JobCompleted {
                conclusion: JobConclusion::Success,
                exit_code: 0
            }
        ));
        assert!(matches!(
            vsock.recv().unwrap(),
            VsockMessage::TeardownAck {
                isolation_id,
                generation: 1
            } if isolation_id == "job-1"
        ));
        assert!(matches!(vsock.sent[0], VsockMessage::DeliverPlan { .. }));
    }
}

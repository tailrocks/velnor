//! Host-Docker backend. Preserves current semantics through the contract.

use super::backend::{ExecutionError, ExecutionEvent, ValidatedPlan};
use super::isolation::IsolationIdentity;
use super::ExecutionWorld;
use std::time::Duration;
use velnor_model::JobConclusion;

const DOCKER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(20);

/// Host Docker execution. Uses `/var/run/docker.sock` (via the job lease).
#[derive(Debug, Default)]
pub struct DockerBackend {
    pub started: bool,
}

impl DockerBackend {
    /// # Errors
    /// Missing host Docker socket or `docker version` failure.
    pub fn preflight(world: &mut ExecutionWorld<'_>) -> Result<(), ExecutionError> {
        if !world.host_fs.exists(world.host_docker_socket) {
            return Err(ExecutionError::DockerPreflight(format!(
                "missing host Docker socket {}",
                world.host_docker_socket.display()
            )));
        }
        let result = world
            .runner
            .run("docker", &["version".into()])
            .map_err(|error| ExecutionError::DockerPreflight(error.to_string()))?;
        if result.code != 0 {
            return Err(ExecutionError::DockerPreflight(format!(
                "docker version exited {}: {}",
                result.code, result.stderr
            )));
        }
        Ok(())
    }

    pub(crate) fn prepare(
        &mut self,
        plan: &ValidatedPlan,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let _ = world;
        events.push(ExecutionEvent::HostDockerInvoked(format!(
            "prepare image {}",
            plan.job_container_image
        )));
        for (name, _) in &plan.env {
            events.push(ExecutionEvent::HostDockerInvoked(format!("env {name}")));
        }
        for service in &plan.services {
            events.push(ExecutionEvent::HostDockerInvoked(format!(
                "prepare service {} alias {}",
                service.image, service.network_alias
            )));
        }
        if plan.buildx {
            events.push(ExecutionEvent::HostDockerInvoked("buildx".into()));
        }
        if plan.testcontainers {
            events.push(ExecutionEvent::HostDockerInvoked(
                "testcontainers guest-local docker".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn start(
        &mut self,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let _ = world;
        self.started = true;
        events.push(ExecutionEvent::HostDockerInvoked(
            "start job container".into(),
        ));
        Ok(())
    }

    pub(crate) fn execute(
        &mut self,
        plan: &ValidatedPlan,
        isolation: &IsolationIdentity,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        if let Some(engine) = world.docker_engine.as_mut() {
            validate_production_identity(plan, isolation)?;
            engine.execute_github_job(events)?;
            return Ok(());
        }
        let guest = plan.to_guest(&isolation.id, isolation.generation);
        super::guest_runtime::execute_guest_plan(&guest, world.runner, events, true)
            .map_err(ExecutionError::DockerPreflight)?;
        Ok(())
    }

    pub(crate) fn cancel(
        &mut self,
        plan: &ValidatedPlan,
        isolation: &IsolationIdentity,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        // The container name is only the job-role identity. Docker ownership
        // is established by both labels before any cancellation cleanup.
        let job_id = format!("velnor-job-{}", isolation.id);
        crate::docker_lease::reclaim_job_owned(&job_id, &plan.daemon_id, |args| {
            // Cancellation must share terminal teardown's bounded command
            // path for both ownership snapshots and destructive operations.
            let result = world
                .runner
                .run_timeout("docker", args, DOCKER_CLEANUP_TIMEOUT)?;
            if result.code != 0 {
                anyhow::bail!(
                    "docker {} failed with code {}: {}",
                    args.join(" "),
                    result.code,
                    result.stderr
                );
            }
            Ok(result.stdout)
        })
        .map_err(|error| ExecutionError::DockerExecute(format!("cancel cleanup: {error:#}")))?;
        events.push(ExecutionEvent::HostDockerInvoked(format!(
            "docker label-scoped cancel {job_id}"
        )));
        events.push(ExecutionEvent::JobCompleted {
            conclusion: JobConclusion::Cancelled,
            exit_code: 1,
        });
        Ok(())
    }
}

fn validate_production_identity(
    plan: &ValidatedPlan,
    isolation: &IsolationIdentity,
) -> Result<(), ExecutionError> {
    if plan.job_id != isolation.id {
        return Err(ExecutionError::DockerPreflight(format!(
            "production Docker identity mismatch: plan.job_id {:?} != isolation.id {:?}",
            plan.job_id, isolation.id
        )));
    }

    let daemon_id = plan.daemon_id.as_str();
    if daemon_id.is_empty()
        || daemon_id.trim() != daemon_id
        || daemon_id.chars().any(char::is_control)
    {
        return Err(ExecutionError::DockerPreflight(format!(
            "production Docker identity invalid: daemon_id must be nonempty and free of surrounding whitespace/control characters for owned cleanup, got {:?}",
            plan.daemon_id
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        MemoryFs, ProductionDockerEngine, RecordingCommands, RecordingFirecracker,
    };
    use std::path::PathBuf;

    #[derive(Default)]
    struct RecordingEngine {
        calls: usize,
    }

    impl ProductionDockerEngine for RecordingEngine {
        fn execute_github_job(
            &mut self,
            _events: &mut Vec<ExecutionEvent>,
        ) -> Result<(), ExecutionError> {
            self.calls += 1;
            Ok(())
        }
    }

    fn execute_with_engine(
        plan: &ValidatedPlan,
        isolation: &IsolationIdentity,
        engine: &mut RecordingEngine,
        runner: &mut RecordingCommands,
    ) -> Result<(), ExecutionError> {
        let mut fs = MemoryFs::default();
        let mut firecracker = RecordingFirecracker::default();
        let kvm = PathBuf::from("/dev/kvm");
        let artifacts = PathBuf::from("/microvm");
        let docker = PathBuf::from("/var/run/docker.sock");
        let mut world = ExecutionWorld {
            kvm: &kvm,
            artifact_root: &artifacts,
            isolation_root: &artifacts,
            host_docker_socket: &docker,
            runner,
            firecracker: &mut firecracker,
            host_fs: &mut fs,
            vsock: None,
            docker_engine: Some(engine),
            allow_inline_guest_plan: true,
        };
        let mut backend = DockerBackend::default();
        let mut events = Vec::new();
        backend.execute(plan, isolation, &mut world, &mut events)
    }

    #[test]
    fn production_execute_rejects_plan_isolation_mismatch_without_side_effects() {
        let plan = ValidatedPlan::example_success("plan-job");
        let isolation = IsolationIdentity::new("other-job", 1);
        let mut engine = RecordingEngine::default();
        let mut runner = RecordingCommands::default();

        let error = execute_with_engine(&plan, &isolation, &mut engine, &mut runner)
            .expect_err("mismatched identities must fail closed");

        assert!(
            matches!(error, ExecutionError::DockerPreflight(detail) if detail.contains("plan.job_id") && detail.contains("isolation.id"))
        );
        assert_eq!(engine.calls, 0);
        assert!(runner.calls.is_empty());
    }

    #[test]
    fn production_execute_rejects_invalid_daemon_identity_without_side_effects() {
        let mut plan = ValidatedPlan::example_success("job");
        plan.daemon_id = "\n".into();
        let isolation = IsolationIdentity::new("job", 1);
        let mut engine = RecordingEngine::default();
        let mut runner = RecordingCommands::default();

        let error = execute_with_engine(&plan, &isolation, &mut engine, &mut runner)
            .expect_err("invalid daemon identity must fail closed");

        assert!(
            matches!(error, ExecutionError::DockerPreflight(detail) if detail.contains("daemon_id"))
        );
        assert_eq!(engine.calls, 0);
        assert!(runner.calls.is_empty());
    }

    #[test]
    fn production_execute_accepts_owned_identity() {
        let plan = ValidatedPlan::example_success("job");
        let isolation = IsolationIdentity::new("job", 1);
        let mut engine = RecordingEngine::default();
        let mut runner = RecordingCommands::default();

        execute_with_engine(&plan, &isolation, &mut engine, &mut runner)
            .expect("valid identity should reach production engine");

        assert_eq!(engine.calls, 1);
        assert!(runner.calls.is_empty());
    }
}

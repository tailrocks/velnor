//! Host-Docker backend. Preserves current semantics through the contract.

use super::backend::{ExecutionError, ExecutionEvent, ValidatedPlan};
use super::isolation::IsolationIdentity;
use super::ExecutionWorld;
use velnor_model::JobConclusion;

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
        isolation: &IsolationIdentity,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let job = format!("velnor-job-{}", isolation.id);
        let args = ["rm".into(), "--force".into(), job];
        events.push(ExecutionEvent::HostDockerInvoked(format!(
            "docker {}",
            args.join(" ")
        )));
        let _ = world.runner.run("docker", &args);
        events.push(ExecutionEvent::JobCompleted {
            conclusion: JobConclusion::Cancelled,
            exit_code: 1,
        });
        Ok(())
    }
}

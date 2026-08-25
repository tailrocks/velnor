//! Host-Docker backend. Preserves current semantics through the contract.

use super::backend::{ExecutionError, ExecutionEvent, ValidatedPlan};
use super::ExecutionWorld;

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
        for service in &plan.service_images {
            events.push(ExecutionEvent::HostDockerInvoked(format!(
                "prepare service {service}"
            )));
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
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let _ = world;
        if plan.cancel_requested {
            events.push(ExecutionEvent::Log {
                stream: 1,
                line: "cancel".into(),
            });
            return Ok(());
        }
        for step in &plan.steps {
            events.push(ExecutionEvent::Log {
                stream: 1,
                line: format!("*** {step} ***"),
            });
        }
        events.push(ExecutionEvent::HostDockerInvoked("docker run".into()));
        Ok(())
    }

    pub(crate) fn cancel(
        &mut self,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let _ = world;
        events.push(ExecutionEvent::HostDockerInvoked(
            "docker rm --force".into(),
        ));
        events.push(ExecutionEvent::Log {
            stream: 1,
            line: "cancel".into(),
        });
        Ok(())
    }
}

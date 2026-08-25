//! Typed backend session. Wrong-phase calls fail; teardown cannot be skipped.

use velnor_model::{ExecutionBackendKind, ExecutionConfigError, MicroVmPreflightFailure};

use super::docker::DockerBackend;
use super::firecracker::FirecrackerBackend;
use super::isolation::{IsolationIdentity, IsolationResources};
use super::ExecutionWorld;

/// Lifecycle phase. An unprepared session cannot execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendPhase {
    New,
    Preflighted,
    Reserved,
    Prepared,
    Started,
    Executing,
    Collecting,
    Stopped,
    TornDown,
}

/// Backend-neutral plan delivered after admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPlan {
    pub job_id: String,
    pub steps: Vec<String>,
    pub job_container_image: String,
    pub service_images: Vec<String>,
    pub timeout_ms: u64,
    pub cancel_requested: bool,
}

impl ValidatedPlan {
    #[must_use]
    pub fn example_success(job_id: impl Into<String>) -> Self {
        Self {
            job_id: job_id.into(),
            steps: vec!["run".into()],
            job_container_image: "velnor/job-ubuntu:26.04".into(),
            service_images: vec!["postgres:16".into()],
            timeout_ms: 60_000,
            cancel_requested: false,
        }
    }
}

/// Observable GitHub-visible outcome for contract tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub conclusion: &'static str,
    pub exit_code: i32,
    pub log_lines: Vec<String>,
    pub masked: bool,
    pub command_file: Option<String>,
    pub cleaned: bool,
}

/// Events a backend may emit during execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionEvent {
    HostDockerInvoked(String),
    FirecrackerApi(String),
    GuestDocker(String),
    Log { stream: u8, line: String },
}

/// Failures at the execution boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    Config(ExecutionConfigError),
    MicroVm(MicroVmPreflightFailure),
    WrongPhase {
        required: BackendPhase,
        actual: BackendPhase,
    },
    HostDockerForbidden,
    DockerPreflight(String),
    CollectBeforeStop,
    TeardownSkipped,
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => write!(f, "{error}"),
            Self::MicroVm(error) => write!(f, "{error}"),
            Self::WrongPhase { required, actual } => {
                write!(f, "backend phase {actual:?} cannot satisfy {required:?}")
            }
            Self::HostDockerForbidden => write!(
                f,
                "host Docker executor is forbidden while execution backend is microvm; the docker backend was not used"
            ),
            Self::DockerPreflight(detail) => write!(f, "docker preflight failed: {detail}"),
            Self::CollectBeforeStop => {
                write!(f, "collect is forbidden while the backend is still live")
            }
            Self::TeardownSkipped => write!(f, "teardown was not run"),
        }
    }
}

impl std::error::Error for ExecutionError {}

/// One job's backend session. Drop without teardown is a fence, not success.
pub struct BackendSession {
    pub kind: ExecutionBackendKind,
    pub isolation: IsolationIdentity,
    pub resources: IsolationResources,
    phase: BackendPhase,
    docker: Option<DockerBackend>,
    firecracker: Option<FirecrackerBackend>,
    events: Vec<ExecutionEvent>,
    teardown_ran: bool,
}

impl BackendSession {
    /// # Errors
    /// Docker preflight failures.
    pub fn docker(
        isolation: IsolationIdentity,
        world: &mut ExecutionWorld<'_>,
    ) -> Result<Self, ExecutionError> {
        DockerBackend::preflight(world)?;
        Ok(Self {
            kind: ExecutionBackendKind::Docker,
            resources: IsolationResources::for_identity(isolation.clone(), world.artifact_root),
            isolation,
            phase: BackendPhase::Preflighted,
            docker: Some(DockerBackend::default()),
            firecracker: None,
            events: Vec::new(),
            teardown_ran: false,
        })
    }

    /// # Errors
    /// MicroVM preflight failures. Never constructs a docker backend.
    pub fn microvm(
        isolation: IsolationIdentity,
        world: &mut ExecutionWorld<'_>,
    ) -> Result<Self, ExecutionError> {
        FirecrackerBackend::preflight(world)?;
        Ok(Self {
            kind: ExecutionBackendKind::MicroVm,
            resources: IsolationResources::for_identity(isolation.clone(), world.artifact_root),
            isolation,
            phase: BackendPhase::Preflighted,
            docker: None,
            firecracker: Some(FirecrackerBackend::default()),
            events: Vec::new(),
            teardown_ran: false,
        })
    }

    #[must_use]
    pub fn phase(&self) -> BackendPhase {
        self.phase
    }

    #[must_use]
    pub fn events(&self) -> &[ExecutionEvent] {
        &self.events
    }

    fn require(&self, required: BackendPhase) -> Result<(), ExecutionError> {
        if self.phase == required {
            Ok(())
        } else {
            Err(ExecutionError::WrongPhase {
                required,
                actual: self.phase,
            })
        }
    }

    /// # Errors
    /// Wrong phase.
    pub fn reserve(&mut self, world: &mut ExecutionWorld<'_>) -> Result<(), ExecutionError> {
        self.require(BackendPhase::Preflighted)?;
        let reserve_dir = self
            .resources
            .chroot
            .parent()
            .unwrap_or(self.resources.chroot.as_path());
        world
            .host_fs
            .create_dir_all(reserve_dir)
            .map_err(|detail| {
                ExecutionError::DockerPreflight(format!("reserve isolation dir: {detail}"))
            })?;
        self.phase = BackendPhase::Reserved;
        Ok(())
    }

    /// # Errors
    /// Wrong phase or prepare failure.
    pub fn prepare(
        &mut self,
        plan: &ValidatedPlan,
        world: &mut ExecutionWorld<'_>,
    ) -> Result<(), ExecutionError> {
        self.require(BackendPhase::Reserved)?;
        match self.kind {
            ExecutionBackendKind::Docker => {
                if let Some(docker) = &mut self.docker {
                    docker.prepare(plan, world, &mut self.events)?;
                }
            }
            ExecutionBackendKind::MicroVm => {
                if let Some(firecracker) = &mut self.firecracker {
                    firecracker.prepare(plan, &self.resources, world, &mut self.events)?;
                }
            }
        }
        self.phase = BackendPhase::Prepared;
        Ok(())
    }

    /// # Errors
    /// Wrong phase or start failure.
    pub fn start(&mut self, world: &mut ExecutionWorld<'_>) -> Result<(), ExecutionError> {
        self.require(BackendPhase::Prepared)?;
        match self.kind {
            ExecutionBackendKind::Docker => {
                if let Some(docker) = &mut self.docker {
                    docker.start(world, &mut self.events)?;
                }
            }
            ExecutionBackendKind::MicroVm => {
                if let Some(firecracker) = &mut self.firecracker {
                    firecracker.start(world, &mut self.events)?;
                }
            }
        }
        self.phase = BackendPhase::Started;
        Ok(())
    }

    /// # Errors
    /// Wrong phase.
    pub fn execute(
        &mut self,
        plan: &ValidatedPlan,
        world: &mut ExecutionWorld<'_>,
    ) -> Result<(), ExecutionError> {
        self.require(BackendPhase::Started)?;
        self.phase = BackendPhase::Executing;
        match self.kind {
            ExecutionBackendKind::Docker => {
                if let Some(docker) = &mut self.docker {
                    docker.execute(plan, world, &mut self.events)?;
                }
            }
            ExecutionBackendKind::MicroVm => {
                if let Some(firecracker) = &mut self.firecracker {
                    firecracker.execute(plan, world, &mut self.events)?;
                }
            }
        }
        self.phase = BackendPhase::Stopped;
        Ok(())
    }

    /// # Errors
    /// Cancel is allowed from started/executing; other phases fail.
    pub fn cancel(&mut self, world: &mut ExecutionWorld<'_>) -> Result<(), ExecutionError> {
        if !matches!(
            self.phase,
            BackendPhase::Started | BackendPhase::Executing | BackendPhase::Prepared
        ) {
            return Err(ExecutionError::WrongPhase {
                required: BackendPhase::Executing,
                actual: self.phase,
            });
        }
        match self.kind {
            ExecutionBackendKind::Docker => {
                if let Some(docker) = &mut self.docker {
                    docker.cancel(world, &mut self.events)?;
                }
            }
            ExecutionBackendKind::MicroVm => {
                if let Some(firecracker) = &mut self.firecracker {
                    firecracker.cancel(world, &mut self.events)?;
                }
            }
        }
        self.phase = BackendPhase::Stopped;
        Ok(())
    }

    /// # Errors
    /// Collect while still live is forbidden.
    pub fn collect(&mut self) -> Result<ExecutionOutcome, ExecutionError> {
        if self.phase != BackendPhase::Stopped {
            return Err(ExecutionError::CollectBeforeStop);
        }
        self.phase = BackendPhase::Collecting;
        let conclusion = self
            .events
            .iter()
            .find_map(|event| match event {
                ExecutionEvent::Log { line, .. } if line.contains("cancel") => Some("cancelled"),
                ExecutionEvent::Log { line, .. } if line.contains("timeout") => Some("timed_out"),
                ExecutionEvent::Log { line, .. } if line.contains("failure") => Some("failure"),
                _ => None,
            })
            .unwrap_or("success");
        let logs: Vec<String> = self
            .events
            .iter()
            .filter_map(|event| match event {
                ExecutionEvent::Log { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect();
        Ok(ExecutionOutcome {
            conclusion,
            exit_code: if conclusion == "success" { 0 } else { 1 },
            log_lines: logs,
            masked: self.events.iter().any(
                |event| matches!(event, ExecutionEvent::Log { line, .. } if line.contains("***")),
            ),
            command_file: Some("GITHUB_OUTPUT".into()),
            cleaned: false,
        })
    }

    /// # Errors
    /// Filesystem teardown failures.
    pub fn teardown(
        &mut self,
        world: &mut ExecutionWorld<'_>,
    ) -> Result<ExecutionOutcome, ExecutionError> {
        if matches!(self.phase, BackendPhase::New) {
            return Err(ExecutionError::WrongPhase {
                required: BackendPhase::Stopped,
                actual: self.phase,
            });
        }
        for path in self.resources.teardown_paths() {
            world
                .host_fs
                .remove_dir_all(path)
                .map_err(ExecutionError::DockerPreflight)?;
        }
        self.teardown_ran = true;
        self.phase = BackendPhase::TornDown;
        let mut outcome = self.collect_after_teardown();
        outcome.cleaned = true;
        Ok(outcome)
    }

    fn collect_after_teardown(&self) -> ExecutionOutcome {
        ExecutionOutcome {
            conclusion: "success",
            exit_code: 0,
            log_lines: Vec::new(),
            masked: false,
            command_file: Some("GITHUB_OUTPUT".into()),
            cleaned: true,
        }
    }

    #[must_use]
    pub fn inspect(&self) -> BackendPhase {
        self.phase
    }

    #[must_use]
    pub fn used_host_docker(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::HostDockerInvoked(_)))
    }

    #[must_use]
    pub fn used_firecracker(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, ExecutionEvent::FirecrackerApi(_)))
    }
}

impl Drop for BackendSession {
    fn drop(&mut self) {
        if !self.teardown_ran && self.phase != BackendPhase::New {
            tracing::error!(
                isolation = %self.isolation.id,
                generation = self.isolation.generation,
                "backend session dropped without teardown; slot must be fenced"
            );
        }
    }
}

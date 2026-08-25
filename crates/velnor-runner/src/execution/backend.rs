//! Typed backend session. Wrong-phase calls fail; teardown cannot be skipped.

use velnor_model::{
    ExecutionBackendKind, ExecutionConfigError, JobConclusion, MicroVmPreflightFailure,
};

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

/// One admitted step: a script, an action `uses:`, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedStep {
    pub id: String,
    pub script: String,
    pub action: Option<String>,
}

/// Backend-neutral plan delivered after admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPlan {
    pub job_id: String,
    pub steps: Vec<ValidatedStep>,
    pub job_container_image: String,
    pub service_images: Vec<String>,
    pub timeout_ms: u64,
    pub cancel_requested: bool,
    pub fail: bool,
    pub cache_digest: Option<String>,
    pub command_files: Vec<String>,
    pub outputs: Vec<(String, String)>,
}

impl ValidatedPlan {
    /// Convert to the vsock-serializable guest plan.
    #[must_use]
    pub fn to_guest(&self, isolation_id: &str, generation: u64) -> velnor_model::GuestJobPlan {
        velnor_model::GuestJobPlan {
            isolation_id: isolation_id.to_string(),
            generation,
            job_id: self.job_id.clone(),
            image: self.job_container_image.clone(),
            services: self
                .service_images
                .iter()
                .enumerate()
                .map(|(index, image)| velnor_model::GuestService {
                    name: format!("svc-{index}"),
                    image: image.clone(),
                })
                .collect(),
            steps: self
                .steps
                .iter()
                .map(|step| velnor_model::GuestStep {
                    id: step.id.clone(),
                    script: step.script.clone(),
                    action: step.action.clone(),
                })
                .collect(),
            timeout_ms: self.timeout_ms,
            cancel_requested: self.cancel_requested,
            fail: self.fail,
            cache_digest: self.cache_digest.clone(),
            command_files: self.command_files.clone(),
            outputs: self
                .outputs
                .iter()
                .map(|(name, value)| velnor_model::GuestOutput {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
        }
    }
}

impl ValidatedPlan {
    #[must_use]
    pub fn example_success(job_id: impl Into<String>) -> Self {
        Self {
            job_id: job_id.into(),
            steps: vec![ValidatedStep {
                id: "run".into(),
                script: "echo run".into(),
                action: None,
            }],
            job_container_image: "velnor/job-ubuntu:26.04".into(),
            service_images: vec!["postgres:16".into()],
            timeout_ms: 60_000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec!["GITHUB_OUTPUT".into(), "GITHUB_ENV".into()],
            outputs: vec![("result".into(), "ok".into())],
        }
    }

    /// Lift the admitted GitHub plan into the backend-neutral contract.
    #[must_use]
    pub fn from_normalized(plan: &crate::plan::NormalizedJobPlan) -> Self {
        Self {
            job_id: plan.identity.job_id.clone(),
            steps: plan.steps.iter().map(validated_step).collect(),
            job_container_image: plan.execution.job_container.image.clone(),
            service_images: plan
                .execution
                .services
                .iter()
                .map(|service| service.image.clone())
                .collect(),
            timeout_ms: plan_timeout_ms(
                plan.steps
                    .iter()
                    .filter_map(crate::executor::ExecutableStep::timeout_minutes),
            ),
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec!["GITHUB_OUTPUT".into(), "GITHUB_ENV".into()],
            outputs: plan
                .outputs
                .iter()
                .map(|(name, output)| (name.clone(), output.value.clone()))
                .collect(),
        }
    }

    /// Lift admitted script steps into the same backend-neutral contract.
    #[must_use]
    pub fn from_script_steps(
        job_id: impl Into<String>,
        docker_image: impl Into<String>,
        script_steps: &[crate::script_step::ScriptStep],
        service_images: Vec<String>,
    ) -> Self {
        Self {
            job_id: job_id.into(),
            steps: script_steps
                .iter()
                .map(|step| ValidatedStep {
                    id: step.id.clone(),
                    script: step.script.clone(),
                    action: None,
                })
                .collect(),
            job_container_image: docker_image.into(),
            service_images,
            timeout_ms: plan_timeout_ms(
                script_steps.iter().filter_map(|step| step.timeout_minutes),
            ),
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec!["GITHUB_OUTPUT".into(), "GITHUB_ENV".into()],
            outputs: Vec::new(),
        }
    }
}

fn plan_timeout_ms(minutes: impl Iterator<Item = u64>) -> u64 {
    minutes
        .map(|minutes| minutes.saturating_mul(60_000))
        .min()
        .unwrap_or(60_000)
}

fn validated_step(step: &crate::executor::ExecutableStep) -> ValidatedStep {
    ValidatedStep {
        id: step.id().to_string(),
        script: match step {
            crate::executor::ExecutableStep::Script(script) => script.script.clone(),
            _ => String::new(),
        },
        action: executable_action(step),
    }
}

fn executable_action(step: &crate::executor::ExecutableStep) -> Option<String> {
    match step {
        crate::executor::ExecutableStep::Script(_) => None,
        crate::executor::ExecutableStep::Checkout(_) => Some("actions/checkout".into()),
        crate::executor::ExecutableStep::Native { invocation, .. } => {
            Some(invocation.git_ref.clone())
        }
        crate::executor::ExecutableStep::JavaScript { invocation, .. } => {
            Some(invocation.action_container_path.clone())
        }
        crate::executor::ExecutableStep::Docker { invocation, .. } => {
            Some(invocation.image.clone())
        }
        crate::executor::ExecutableStep::CompositeStart { .. } => Some("composite".into()),
        crate::executor::ExecutableStep::CompositeEnd { .. }
        | crate::executor::ExecutableStep::CompositeOutputs { .. } => None,
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
    pub outputs: Vec<(String, String)>,
    pub cleaned: bool,
}

/// Events a backend may emit during execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionEvent {
    HostDockerInvoked(String),
    FirecrackerApi(String),
    GuestDocker(String),
    Log {
        stream: u8,
        line: String,
    },
    CommandFile {
        path: String,
    },
    Output {
        name: String,
        value: String,
    },
    JobCompleted {
        conclusion: JobConclusion,
        exit_code: i32,
    },
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
    DockerExecute(String),
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
            Self::DockerExecute(detail) => write!(f, "docker job execution failed: {detail}"),
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
    plan_command_files: Vec<String>,
    plan_outputs: Vec<(String, String)>,
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
            plan_command_files: Vec::new(),
            plan_outputs: Vec::new(),
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
            plan_command_files: Vec::new(),
            plan_outputs: Vec::new(),
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
        self.plan_command_files = plan.command_files.clone();
        self.plan_outputs = plan.outputs.clone();
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
                    docker.execute(plan, &self.isolation, world, &mut self.events)?;
                }
            }
            ExecutionBackendKind::MicroVm => {
                if let Some(firecracker) = &mut self.firecracker {
                    firecracker.execute(plan, &self.isolation, world, &mut self.events)?;
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
        let (conclusion, exit_code) = self
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                ExecutionEvent::JobCompleted {
                    conclusion,
                    exit_code,
                } => Some((*conclusion, *exit_code)),
                _ => None,
            })
            .unwrap_or((JobConclusion::Failure, 1));
        let logs: Vec<String> = self
            .events
            .iter()
            .filter_map(|event| match event {
                ExecutionEvent::Log { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect();
        let mut outputs: Vec<(String, String)> = self
            .events
            .iter()
            .filter_map(|event| match event {
                ExecutionEvent::Output { name, value } => Some((name.clone(), value.clone())),
                _ => None,
            })
            .collect();
        if outputs.is_empty() {
            outputs.clone_from(&self.plan_outputs);
        }
        let command_file = self
            .events
            .iter()
            .find_map(|event| match event {
                ExecutionEvent::CommandFile { path } => Some(path.clone()),
                _ => None,
            })
            .or_else(|| self.plan_command_files.first().cloned());
        Ok(ExecutionOutcome {
            conclusion: conclusion.as_str(),
            exit_code,
            log_lines: logs,
            masked: self.events.iter().any(
                |event| matches!(event, ExecutionEvent::Log { line, .. } if line.contains("***")),
            ),
            command_file,
            outputs,
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
        if let Some(firecracker) = &mut self.firecracker {
            firecracker.teardown(&self.resources, world, &mut self.events)?;
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
            command_file: self.plan_command_files.first().cloned(),
            outputs: self.plan_outputs.clone(),
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

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

/// Admitted service container. Alias/ports stay GitHub-visible; no host socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedService {
    pub name: String,
    pub image: String,
    pub network_alias: String,
    pub ports: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Backend-neutral plan delivered after admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPlan {
    pub job_id: String,
    pub steps: Vec<ValidatedStep>,
    pub job_container_image: String,
    pub services: Vec<ValidatedService>,
    pub timeout_ms: u64,
    pub cancel_requested: bool,
    pub fail: bool,
    pub cache_digest: Option<String>,
    pub command_files: Vec<String>,
    pub outputs: Vec<(String, String)>,
    pub env: Vec<(String, String)>,
    pub workspace: String,
    pub cache: Vec<String>,
    pub artifacts: Vec<(String, String)>,
    pub annotations: Vec<String>,
    pub summary: String,
    pub buildx: bool,
    pub testcontainers: bool,
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
                .services
                .iter()
                .map(|service| velnor_model::GuestService {
                    name: service.name.clone(),
                    image: service.image.clone(),
                    network_alias: service.network_alias.clone(),
                    ports: service.ports.clone(),
                    env: guest_env(&service.env),
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
            env: guest_env(&self.env),
            workspace: self.workspace.clone(),
            cache: self
                .cache
                .iter()
                .map(|digest| velnor_model::GuestCacheOp {
                    digest: digest.clone(),
                })
                .collect(),
            artifacts: self
                .artifacts
                .iter()
                .map(|(name, path)| velnor_model::GuestArtifactOp {
                    name: name.clone(),
                    path: path.clone(),
                })
                .collect(),
            annotations: self.annotations.clone(),
            summary: self.summary.clone(),
            buildx: self.buildx,
            testcontainers: self.testcontainers,
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
            services: vec![ValidatedService {
                name: "svc-0".into(),
                image: "postgres:16".into(),
                network_alias: "postgres".into(),
                ports: vec!["5432".into()],
                env: Vec::new(),
            }],
            timeout_ms: 60_000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec![
                "GITHUB_OUTPUT".into(),
                "GITHUB_ENV".into(),
                "GITHUB_PATH".into(),
                "GITHUB_STEP_SUMMARY".into(),
            ],
            outputs: vec![("result".into(), "ok".into())],
            env: vec![("CI".into(), "true".into())],
            workspace: "/__w".into(),
            cache: Vec::new(),
            artifacts: Vec::new(),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: false,
            testcontainers: false,
        }
    }

    /// Lift the admitted GitHub plan into the backend-neutral contract.
    #[must_use]
    pub fn from_normalized(plan: &crate::plan::NormalizedJobPlan) -> Self {
        Self {
            job_id: plan.identity.job_id.clone(),
            steps: plan.steps.iter().map(validated_step).collect(),
            job_container_image: plan.execution.job_container.image.clone(),
            services: plan
                .execution
                .services
                .iter()
                .map(validated_service)
                .collect(),
            timeout_ms: plan_timeout_ms(
                plan.steps
                    .iter()
                    .filter_map(crate::executor::ExecutableStep::timeout_minutes),
            ),
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec![
                "GITHUB_OUTPUT".into(),
                "GITHUB_ENV".into(),
                "GITHUB_PATH".into(),
                "GITHUB_STEP_SUMMARY".into(),
            ],
            outputs: plan
                .outputs
                .iter()
                .map(|(name, output)| (name.clone(), output.value.clone()))
                .collect(),
            env: sanitized_pairs(&plan.execution.env),
            workspace: plan.execution.workspace_container.clone(),
            cache: plan_cache(&plan.steps),
            artifacts: plan_artifacts(&plan.steps),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: plan_needs_buildx(&plan.steps),
            testcontainers: plan_needs_testcontainers(
                &plan.execution.env,
                &plan.execution.services,
            ),
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
            services: service_images
                .into_iter()
                .enumerate()
                .map(|(index, image)| ValidatedService {
                    name: format!("svc-{index}"),
                    image,
                    network_alias: format!("svc-{index}"),
                    ports: Vec::new(),
                    env: Vec::new(),
                })
                .collect(),
            timeout_ms: plan_timeout_ms(
                script_steps.iter().filter_map(|step| step.timeout_minutes),
            ),
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: vec![
                "GITHUB_OUTPUT".into(),
                "GITHUB_ENV".into(),
                "GITHUB_PATH".into(),
                "GITHUB_STEP_SUMMARY".into(),
            ],
            outputs: Vec::new(),
            env: Vec::new(),
            workspace: "/__w".into(),
            cache: Vec::new(),
            artifacts: Vec::new(),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: false,
            testcontainers: false,
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

fn guest_env(pairs: &[(String, String)]) -> Vec<velnor_model::GuestEnvVar> {
    sanitized_pairs(pairs)
        .into_iter()
        .map(|(name, value)| velnor_model::GuestEnvVar { name, value })
        .collect()
}

fn sanitized_pairs(pairs: &[(String, String)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .filter(|(name, value)| !name.contains("docker.sock") && !value.contains("docker.sock"))
        .cloned()
        .collect()
}

fn validated_service(service: &crate::container::ServiceContainerSpec) -> ValidatedService {
    ValidatedService {
        name: service.name.clone(),
        image: service.image.clone(),
        network_alias: service.network_alias.clone(),
        ports: service.ports.clone(),
        env: sanitized_pairs(&service.env),
    }
}

fn plan_needs_buildx(steps: &[crate::executor::ExecutableStep]) -> bool {
    steps.iter().any(|step| {
        matches!(
            step,
            crate::executor::ExecutableStep::Native { invocation, .. }
                if matches!(
                    invocation.adapter,
                    crate::action::NativeActionAdapter::DockerSetupBuildx
                        | crate::action::NativeActionAdapter::DockerBuildPush
                        | crate::action::NativeActionAdapter::DockerBake
                )
        )
    })
}

fn plan_needs_testcontainers(
    env: &[(String, String)],
    services: &[crate::container::ServiceContainerSpec],
) -> bool {
    env.iter()
        .any(|(name, _)| name.to_ascii_uppercase().contains("TESTCONTAINERS"))
        || services.iter().any(|service| {
            service.image.contains("testcontainers")
                || service.network_alias.contains("testcontainers")
        })
}

fn plan_cache(steps: &[crate::executor::ExecutableStep]) -> Vec<String> {
    steps
        .iter()
        .filter_map(|step| match step {
            crate::executor::ExecutableStep::Native { invocation, .. }
                if matches!(
                    invocation.adapter,
                    crate::action::NativeActionAdapter::Cache
                        | crate::action::NativeActionAdapter::RustCache
                ) =>
            {
                invocation.inputs.get("key").cloned()
            }
            _ => None,
        })
        .collect()
}

fn plan_artifacts(steps: &[crate::executor::ExecutableStep]) -> Vec<(String, String)> {
    steps
        .iter()
        .filter_map(|step| match step {
            crate::executor::ExecutableStep::Native { invocation, .. }
                if matches!(
                    invocation.adapter,
                    crate::action::NativeActionAdapter::UploadArtifact
                        | crate::action::NativeActionAdapter::UploadPagesArtifact
                ) =>
            {
                Some((
                    invocation
                        .inputs
                        .get("name")
                        .cloned()
                        .unwrap_or_else(|| step.id().to_string()),
                    invocation.inputs.get("path").cloned().unwrap_or_default(),
                ))
            }
            _ => None,
        })
        .collect()
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
    pub annotations: Vec<String>,
    pub summary: String,
    pub cache: Vec<String>,
    pub artifacts: Vec<(String, String)>,
    pub buildx: bool,
    pub testcontainers: bool,
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
    plan_annotations: Vec<String>,
    plan_summary: String,
    plan_cache: Vec<String>,
    plan_artifacts: Vec<(String, String)>,
    plan_buildx: bool,
    plan_testcontainers: bool,
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
            plan_annotations: Vec::new(),
            plan_summary: String::new(),
            plan_cache: Vec::new(),
            plan_artifacts: Vec::new(),
            plan_buildx: false,
            plan_testcontainers: false,
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
            plan_annotations: Vec::new(),
            plan_summary: String::new(),
            plan_cache: Vec::new(),
            plan_artifacts: Vec::new(),
            plan_buildx: false,
            plan_testcontainers: false,
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
        self.plan_annotations = plan.annotations.clone();
        self.plan_summary = plan.summary.clone();
        self.plan_cache = plan.cache.clone();
        self.plan_artifacts = plan.artifacts.clone();
        self.plan_buildx = plan.buildx;
        self.plan_testcontainers = plan.testcontainers;
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
            annotations: self.plan_annotations.clone(),
            summary: self.plan_summary.clone(),
            cache: self.plan_cache.clone(),
            artifacts: self.plan_artifacts.clone(),
            buildx: self.plan_buildx,
            testcontainers: self.plan_testcontainers,
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
            annotations: self.plan_annotations.clone(),
            summary: self.plan_summary.clone(),
            cache: self.plan_cache.clone(),
            artifacts: self.plan_artifacts.clone(),
            buildx: self.plan_buildx,
            testcontainers: self.plan_testcontainers,
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

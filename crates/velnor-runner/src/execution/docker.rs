//! Host-Docker backend. Preserves current semantics through the contract.

use super::backend::{ExecutionError, ExecutionEvent, ValidatedPlan};
use super::isolation::IsolationIdentity;
use super::ExecutionWorld;
use crate::docker::facts::{self, Fact, FactLifetime};
use crate::executor::CommandRunner;
use std::sync::atomic::{AtomicU64, Ordering};
use velnor_model::JobConclusion;

/// The Engine's cgroup driver and cgroup version.
///
/// A daemon-generation fact: it is fixed by the daemon's configuration at
/// startup and can only change when `dockerd` restarts. It used to be fetched
/// once per job through `docker info`, the Engine's heaviest read endpoint,
/// and thrown away by any non-zero `docker` exit — including an ordinary
/// failing user step, which cannot change a cgroup driver.
static CGROUP_DRIVER: Fact<String> = Fact::new("docker-info-cgroup", FactLifetime::Daemon);

/// Proof that the Docker VM preserves Velnor's per-container placement
/// (`--cgroup-parent`) while leaving containers unbounded.
///
/// A daemon-generation fact: the projection is decided by the Engine and the
/// VM kernel it runs on, both of which are part of the daemon key. It used to
/// create, inspect and remove a disposable probe container at every preflight
/// because the daemon generation could not be observed on macOS.
static VM_RESOURCE_CONTROLS: Fact<()> =
    Fact::new("docker-vm-resource-controls", FactLifetime::Daemon);

pub const DOCKER_JOB_CGROUP_PARENT: &str = crate::docker_lease::JOB_CGROUP_PARENT;
pub const DOCKER_RESOURCE_BOUNDARY_CHECK: &str = "docker-resource-boundary";
pub const MACOS_DOCKER_CAPABILITY_PROBE_IMAGE: &str = "alpine:3.20";
static CAPABILITY_PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostPlatform {
    Linux,
    MacOs,
    Other,
}

impl HostPlatform {
    fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::MacOs => "macOS",
            Self::Other => "this host platform",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockerIsolationMode {
    LinuxSystemdV2,
    DockerVmCgroupV2,
}

/// Select the host isolation proof without assuming that the host kernel is
/// the kernel running Docker. This selects the cgroup driver and version
/// only — a mode, not a ceiling. Placement under the job cgroup is proven
/// separately, and no CPU/RAM ceiling is ever required or applied.
pub fn validate_docker_isolation(
    platform: HostPlatform,
    driver: &str,
    version: &str,
) -> std::result::Result<DockerIsolationMode, String> {
    if version != "2" {
        return Err(format!(
            "Docker job isolation requires cgroup v2 on {}; got driver {driver:?}, version {version:?}",
            platform.label()
        ));
    }
    match platform {
        HostPlatform::Linux if driver.eq_ignore_ascii_case("systemd") => {
            Ok(DockerIsolationMode::LinuxSystemdV2)
        }
        HostPlatform::MacOs
            if driver.eq_ignore_ascii_case("cgroupfs")
                || driver.eq_ignore_ascii_case("systemd") =>
        {
            Ok(DockerIsolationMode::DockerVmCgroupV2)
        }
        _ => Err(format!(
            "Docker job isolation requires systemd cgroup driver on Linux or a macOS Docker VM with cgroup v2; got driver {driver:?}, version {version:?} on {}",
            platform.label()
        )),
    }
}

/// Validate the Docker-visible projection of the per-container boundary used
/// by the macOS Docker VM path: the probe container lands under the job
/// cgroup parent with no CPU or memory ceiling (`NanoCpus=0`, `Memory=0`).
pub fn validate_unbounded_resource_projection(
    cgroup_parent: &str,
    nano_cpus: &str,
    memory: &str,
) -> std::result::Result<(), String> {
    if cgroup_parent == DOCKER_JOB_CGROUP_PARENT && nano_cpus == "0" && memory == "0" {
        Ok(())
    } else {
        Err(format!(
            "{DOCKER_RESOURCE_BOUNDARY_CHECK} expected unbounded CgroupParent={DOCKER_JOB_CGROUP_PARENT}, NanoCpus=0, Memory=0; got CgroupParent={cgroup_parent:?}, NanoCpus={nano_cpus:?}, Memory={memory:?}"
        ))
    }
}

/// Host Docker execution. Uses the resolved local endpoint (via the job lease).
#[derive(Debug, Default)]
pub struct DockerBackend {
    pub started: bool,
}

impl DockerBackend {
    /// # Errors
    /// Missing host Docker socket or Docker/cgroup-boundary probe failure.
    pub fn preflight(world: &mut ExecutionWorld<'_>) -> Result<(), ExecutionError> {
        let endpoint = crate::docker::engine::resolve_docker_endpoint().map_err(|error| {
            ExecutionError::DockerPreflight(format!("Docker endpoint resolution failed: {error:#}"))
        })?;
        let socket =
            if world.host_fs.exists(&endpoint.socket) || world.runner.is_host_process_runner() {
                endpoint.socket
            } else {
                // Test doubles model the socket through `ExecutionWorld` instead
                // of the real host filesystem. Production host runners never use
                // this fallback, so an unresolved portable endpoint cannot be
                // hidden by the legacy fixture path.
                world.host_docker_socket.to_path_buf()
            };
        if !world.host_fs.exists(&socket) {
            return Err(ExecutionError::DockerPreflight(format!(
                "missing host Docker socket {}",
                socket.display()
            )));
        }
        verify_docker_job_cgroup_boundary(world.runner)?;
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
        let _power_assertion =
            crate::platform::PowerAssertionGuard::acquire(&format!("velnor-job-{}", isolation.id));
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
        let job = crate::github_adapter::job_container_name_for_id(&isolation.id);
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

pub(crate) fn verify_docker_job_cgroup_boundary(
    runner: &mut dyn CommandRunner,
) -> Result<(), ExecutionError> {
    verify_docker_job_cgroup_boundary_with_image(runner, None)
}

pub(crate) fn verify_docker_job_cgroup_boundary_with_image(
    runner: &mut dyn CommandRunner,
    image: Option<&str>,
) -> Result<(), ExecutionError> {
    // A fact learned through a runner that does not spawn host processes is not
    // a fact about this host, so it is never cached as one.
    let host_runner = runner.is_host_process_runner();
    // One generation read guards every daemon-lifetime fact below.
    let generation = host_runner.then(facts::daemon).flatten();
    let driver = CGROUP_DRIVER.get_or_try_init(
        generation.clone(),
        || -> Result<String, ExecutionError> {
            let probed = crate::docker::Docker::job(&mut *runner)
                .daemon_cgroup()
                .map_err(|error| {
                    ExecutionError::DockerPreflight(format!("docker cgroup probe: {error:#}"))
                })?;
            Ok(format!("{} {}", probed.driver, probed.version))
        },
    )?;

    let words = driver.split_whitespace().collect::<Vec<_>>();
    if words.len() != 2 || words.iter().any(|word| word.is_empty()) {
        return Err(ExecutionError::DockerPreflight(format!(
            "Docker cgroup probe returned malformed driver/version {:?}; expected '<driver> <version>'",
            driver.trim()
        )));
    }
    let driver_name = words[0];
    let version = words[1];
    let mode = validate_docker_isolation(HostPlatform::current(), driver_name, version)
        .map_err(ExecutionError::DockerPreflight)?;

    if mode == DockerIsolationMode::DockerVmCgroupV2 {
        if host_runner {
            VM_RESOURCE_CONTROLS.get_or_try_init(generation, || {
                verify_docker_vm_resource_controls(
                    runner,
                    image.unwrap_or(MACOS_DOCKER_CAPABILITY_PROBE_IMAGE),
                )
            })?;
        }
        return Ok(());
    }

    // The job slice is identity and cleanup ancestry, not a ceiling: it must
    // be loaded, and its effective CPU/RAM properties must all read
    // `infinity`. Any finite value is a surviving quota — a stale package
    // drop-in or an operator override — and fails closed.
    let slice = crate::docker_lease::JOB_CGROUP_PARENT;
    let state = systemd_slice_load_and_ceilings(runner, slice)?;
    if state.load_state != "loaded" {
        return Err(ExecutionError::DockerPreflight(format!(
            "Docker job cgroup boundary requires loaded {slice}; got {:?}",
            state.load_state
        )));
    }
    for (property, value) in [
        ("CPUQuotaPerSecUSec", state.cpu_quota.as_str()),
        ("MemoryMax", state.memory_max.as_str()),
        ("MemoryHigh", state.memory_high.as_str()),
    ] {
        if !value.eq_ignore_ascii_case("infinity") {
            return Err(ExecutionError::DockerPreflight(format!(
                "Docker job cgroup boundary requires no CPU/RAM ceiling on {slice}; {property} is {value:?}"
            )));
        }
    }

    Ok(())
}

fn verify_docker_vm_resource_controls(
    runner: &mut dyn CommandRunner,
    image: &str,
) -> Result<(), ExecutionError> {
    let sequence = CAPABILITY_PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!("velnor-capability-probe-{}-{sequence}", std::process::id());
    // Placement only: the probe carries no --cpus/--memory, and the
    // projection must show the container unbounded under the job parent.
    let create_args = vec![
        "create".to_owned(),
        "--name".to_owned(),
        name.clone(),
        "--cgroup-parent".to_owned(),
        crate::docker_lease::JOB_CGROUP_PARENT.to_owned(),
        image.to_owned(),
    ];
    let created = runner.run("docker", &create_args).map_err(|error| {
        ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe could not create a non-running container from image {image:?}: {error}"
        ))
    })?;
    if created.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe failed to create a container with --cgroup-parent={}: exited {}: {}",
            crate::docker_lease::JOB_CGROUP_PARENT,
            created.code,
            created.stderr.trim()
        )));
    }

    let inspect_args = vec![
        "inspect".to_owned(),
        "--format".to_owned(),
        "{{.HostConfig.CgroupParent}}\t{{.HostConfig.NanoCpus}}\t{{.HostConfig.Memory}}".to_owned(),
        "--".to_owned(),
        name.clone(),
    ];
    let inspected = runner.run("docker", &inspect_args);
    let removed = runner.run(
        "docker",
        &[
            "rm".to_owned(),
            "--force".to_owned(),
            "--".to_owned(),
            name.clone(),
        ],
    );
    let inspected = inspected.map_err(|error| {
        ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe could not inspect its container: {error}"
        ))
    })?;
    let removed = removed.map_err(|error| {
        ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe cleanup failed for {name}: {error}"
        ))
    })?;
    if removed.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe cleanup failed for {name}: exited {}: {}",
            removed.code,
            removed.stderr.trim()
        )));
    }
    if inspected.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe inspect failed: exited {}: {}",
            inspected.code,
            inspected.stderr.trim()
        )));
    }
    let mut fields = inspected.stdout.trim().split('\t');
    let (Some(parent), Some(cpus), Some(memory), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe returned malformed inspect output {:?}",
            inspected.stdout.trim()
        )));
    };
    if let Err(detail) = validate_unbounded_resource_projection(parent, cpus, memory) {
        return Err(ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe found a ceiling on the probe container: {detail}"
        )));
    }
    Ok(())
}

/// Effective load state and CPU/RAM ceiling properties of the job slice.
struct SystemdSliceState {
    load_state: String,
    cpu_quota: String,
    memory_max: String,
    memory_high: String,
}

fn systemd_slice_load_and_ceilings(
    runner: &mut dyn CommandRunner,
    slice: &str,
) -> Result<SystemdSliceState, ExecutionError> {
    let result = runner
        .run(
            "systemctl",
            &[
                "show".into(),
                "--property=LoadState".into(),
                "--property=CPUQuotaPerSecUSec".into(),
                "--property=MemoryMax".into(),
                "--property=MemoryHigh".into(),
                slice.into(),
            ],
        )
        .map_err(|error| {
            ExecutionError::DockerPreflight(format!(
                "systemd slice state probe for {slice}: {error}"
            ))
        })?;
    if result.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "systemd slice state probe for {slice} exited {}: {}",
            result.code, result.stderr
        )));
    }

    // `systemctl --value` does not preserve the requested property order on
    // every systemd version. Parse named fields so the probe is order-safe.
    let mut load_state = None;
    let mut cpu_quota = None;
    let mut memory_max = None;
    let mut memory_high = None;
    for line in result.stdout.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "LoadState" => load_state = Some(value.trim().to_string()),
            "CPUQuotaPerSecUSec" => cpu_quota = Some(value.trim().to_string()),
            "MemoryMax" => memory_max = Some(value.trim().to_string()),
            "MemoryHigh" => memory_high = Some(value.trim().to_string()),
            _ => {}
        }
    }
    let (Some(load_state), Some(cpu_quota), Some(memory_max), Some(memory_high)) =
        (load_state, cpu_quota, memory_max, memory_high)
    else {
        return Err(ExecutionError::DockerPreflight(format!(
            "systemd slice state probe for {slice} returned malformed output {:?}",
            result.stdout.trim()
        )));
    };
    Ok(SystemdSliceState {
        load_state,
        cpu_quota,
        memory_max,
        memory_high,
    })
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests may panic"
)]
mod tests {
    use super::*;
    use crate::execution::{HostFs, MemoryFs, RecordingCommands, RecordingFirecracker};
    use std::path::PathBuf;

    #[test]
    fn linux_systemd_v2_keeps_the_host_slice_proof() {
        assert_eq!(
            validate_docker_isolation(HostPlatform::Linux, "systemd", "2",),
            Ok(DockerIsolationMode::LinuxSystemdV2)
        );
    }

    #[test]
    fn linux_cgroupfs_v2_does_not_bypass_the_systemd_boundary() {
        let Err(error) = validate_docker_isolation(HostPlatform::Linux, "cgroupfs", "2") else {
            panic!("cgroupfs on Linux must not skip the systemd boundary");
        };
        assert!(error.contains("systemd cgroup driver"), "{error}");
    }

    #[test]
    fn macos_vm_cgroupfs_v2_selects_the_vm_mode() {
        assert_eq!(
            validate_docker_isolation(HostPlatform::MacOs, "cgroupfs", "2",),
            Ok(DockerIsolationMode::DockerVmCgroupV2)
        );
    }

    #[test]
    fn macos_vm_resource_projection_requires_unbounded_placement() {
        assert!(
            validate_unbounded_resource_projection(DOCKER_JOB_CGROUP_PARENT, "0", "0",).is_ok()
        );
        // A ceiling on the probe container fails, as does a wrong parent.
        for (parent, cpus, memory) in [
            (DOCKER_JOB_CGROUP_PARENT, "500000000", "0"),
            (DOCKER_JOB_CGROUP_PARENT, "0", "67108864"),
            ("other.slice", "0", "0"),
        ] {
            let error = validate_unbounded_resource_projection(parent, cpus, memory).unwrap_err();
            assert!(error.contains(DOCKER_RESOURCE_BOUNDARY_CHECK), "{error}");
        }
    }

    #[test]
    fn every_platform_rejects_cgroup_v1() {
        for platform in [
            HostPlatform::Linux,
            HostPlatform::MacOs,
            HostPlatform::Other,
        ] {
            let Err(error) = validate_docker_isolation(platform, "cgroupfs", "1") else {
                panic!("{platform:?} must reject cgroup v1");
            };
            assert!(error.contains("cgroup v2"), "{error}");
        }
    }

    #[test]
    fn docker_cancel_uses_canonical_container_name_for_unsafe_job_id() {
        let mut fs = MemoryFs::default();
        let docker_socket = PathBuf::from("/var/run/docker.sock");
        fs.write(&docker_socket, b"socket").unwrap();
        let mut runner = RecordingCommands::default();
        let mut firecracker = RecordingFirecracker::default();
        let kvm = PathBuf::from("/dev/kvm");
        let artifacts = PathBuf::from("/microvm");
        let mut world = ExecutionWorld {
            kvm: &kvm,
            artifact_root: &artifacts,
            isolation_root: &artifacts,
            host_docker_socket: &docker_socket,
            runner: &mut runner,
            firecracker: &mut firecracker,
            host_fs: &mut fs,
            vsock: None,
            docker_engine: None,
            allow_inline_guest_plan: true,
        };
        let mut backend = DockerBackend::default();
        let isolation = IsolationIdentity::new("run/42:unsafe", 1);
        let mut events = Vec::new();

        backend.cancel(&isolation, &mut world, &mut events).unwrap();

        assert_eq!(
            runner
                .calls
                .iter()
                .find(|(program, _)| program == "docker")
                .map(|(_, args)| args),
            Some(&vec![
                "rm".to_owned(),
                "--force".to_owned(),
                "velnor-job-run_42_unsafe".to_owned(),
            ])
        );
    }
}

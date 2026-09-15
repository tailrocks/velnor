//! Host-Docker backend. Preserves current semantics through the contract.

use super::backend::{ExecutionError, ExecutionEvent, ValidatedPlan};
use super::isolation::IsolationIdentity;
use super::ExecutionWorld;
use crate::docker::facts::{self, Fact, FactLifetime};
use crate::executor::CommandRunner;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use velnor_model::JobConclusion;

/// The drop-in that sets the job slice's CPU quota. Its modification time is
/// the generation of every host fact derived from the slice's configuration.
const JOB_CGROUP_DROPIN: &str = "/etc/systemd/system/velnor-jobs.slice.d/10-host-cpu.conf";

/// The Engine's cgroup driver and cgroup version.
///
/// A daemon-generation fact: it is fixed by the daemon's configuration at
/// startup and can only change when `dockerd` restarts. It used to be fetched
/// once per job through `docker info`, the Engine's heaviest read endpoint,
/// and thrown away by any non-zero `docker` exit — including an ordinary
/// failing user step, which cannot change a cgroup driver.
static CGROUP_DRIVER: Fact<String> = Fact::new("docker-info-cgroup", FactLifetime::Daemon);

/// The `CPUQuota=` the job slice's unit configuration declares.
///
/// A host fact: it changes only when the drop-in on disk changes, which is the
/// generation it is keyed on.
static SLICE_UNIT_QUOTA: Fact<String> = Fact::new("job-slice-unit-quota", FactLifetime::Host);

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
pub struct DockerResourceCapabilities {
    pub cpu_limit: bool,
    pub memory_limit: bool,
    pub cgroup_parent: bool,
}

impl DockerResourceCapabilities {
    pub fn all() -> Self {
        Self {
            cpu_limit: true,
            memory_limit: true,
            cgroup_parent: true,
        }
    }

    fn missing(self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.cpu_limit {
            missing.push("--cpus");
        }
        if !self.memory_limit {
            missing.push("--memory");
        }
        if !self.cgroup_parent {
            missing.push("--cgroup-parent");
        }
        missing
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockerIsolationMode {
    LinuxSystemdV2,
    DockerVmCgroupV2,
}

/// Select the host isolation proof without assuming that the host kernel is
/// the kernel running Docker. macOS Docker engines are Linux VMs: cgroupfs v2
/// is acceptable only after Docker itself proves the controls Velnor emits.
pub fn validate_docker_isolation(
    platform: HostPlatform,
    driver: &str,
    version: &str,
    capabilities: DockerResourceCapabilities,
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
            let missing = capabilities.missing();
            if missing.is_empty() {
                Ok(DockerIsolationMode::DockerVmCgroupV2)
            } else {
                Err(format!(
                    "macOS Docker VM does not expose equivalent resource isolation; Docker {driver} cgroup v2 is missing {}. Velnor requires Docker support for --cpus, --memory, and --cgroup-parent",
                    missing.join(", ")
                ))
            }
        }
        _ => Err(format!(
            "Docker job isolation requires systemd cgroup driver on Linux or a macOS Docker VM with cgroupfs v2 resource controls; got driver {driver:?}, version {version:?} on {}",
            platform.label()
        )),
    }
}

/// Validate the Docker-visible projection of the per-container resource
/// boundary used by the macOS Docker VM path.
pub fn validate_docker_resource_projection(
    cgroup_parent: &str,
    nano_cpus: &str,
    memory: &str,
) -> std::result::Result<(), String> {
    if cgroup_parent == DOCKER_JOB_CGROUP_PARENT && nano_cpus == "500000000" && memory == "67108864"
    {
        Ok(())
    } else {
        Err(format!(
            "{DOCKER_RESOURCE_BOUNDARY_CHECK} expected CgroupParent={DOCKER_JOB_CGROUP_PARENT}, NanoCpus=500000000, Memory=67108864; got CgroupParent={cgroup_parent:?}, NanoCpus={nano_cpus:?}, Memory={memory:?}"
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
    let driver = CGROUP_DRIVER.get_or_try_init(
        host_runner.then(facts::daemon).flatten(),
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
    let mode = validate_docker_isolation(
        HostPlatform::current(),
        driver_name,
        version,
        if host_runner {
            DockerResourceCapabilities::all()
        } else {
            // Test and guest runners cannot prove host Docker capabilities;
            // their scripted cgroup projection is covered by the pure policy
            // tests below and must never populate host facts.
            DockerResourceCapabilities::all()
        },
    )
    .map_err(ExecutionError::DockerPreflight)?;

    if mode == DockerIsolationMode::DockerVmCgroupV2 {
        if host_runner {
            verify_docker_vm_resource_controls(
                runner,
                image.unwrap_or(MACOS_DOCKER_CAPABILITY_PROBE_IMAGE),
            )?;
        }
        return Ok(());
    }

    let slice = crate::docker_lease::JOB_CGROUP_PARENT;
    let cpu_count = runner
        .run("getconf", &["_NPROCESSORS_ONLN".into()])
        .map_err(|error| {
            ExecutionError::DockerPreflight(format!("online CPU count probe for {slice}: {error}"))
        })?;
    if cpu_count.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "online CPU count probe for {slice} exited {}: {}",
            cpu_count.code, cpu_count.stderr
        )));
    }

    let cpu_count = cpu_count.stdout.trim().parse::<u64>().map_err(|_| {
        ExecutionError::DockerPreflight(format!(
            "online CPU count probe for {slice} returned invalid value {:?}",
            cpu_count.stdout.trim()
        ))
    })?;
    let expected_quota = cpu_count.checked_mul(95).ok_or_else(|| {
        ExecutionError::DockerPreflight(format!(
            "online CPU count is too large to calculate CPUQuota for {slice}"
        ))
    })?;
    if expected_quota == 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "online CPU count is zero for {slice}"
        )));
    }

    let (load_state, quota) = systemd_slice_state(runner, slice)?;
    if load_state != "loaded" {
        return Err(ExecutionError::DockerPreflight(format!(
            "Docker job cgroup boundary requires loaded {slice}; got {load_state:?}"
        )));
    }
    let expected_quota_usec = u128::from(expected_quota) * 10_000;
    let effective_quota_usec = parse_systemd_duration_usec(&quota).ok_or_else(|| {
        ExecutionError::DockerPreflight(format!(
            "Docker job cgroup boundary requires finite CPUQuotaPerSecUSec on {slice}; got {quota:?}"
        ))
    })?;
    if effective_quota_usec != expected_quota_usec {
        return Err(ExecutionError::DockerPreflight(format!(
            "Docker job cgroup boundary requires CPUQuotaPerSecUSec={expected_quota_usec}us on {slice}; got {quota:?}"
        )));
    }

    // The unit's declared quota is a host fact keyed on the drop-in that sets
    // it. The comparison below runs every time even on a cache hit, so a
    // changed expectation is still rejected.
    let declared_quota = SLICE_UNIT_QUOTA.get_or_try_init(
        host_runner
            .then(|| facts::host(Path::new(JOB_CGROUP_DROPIN)))
            .flatten(),
        || -> Result<String, ExecutionError> {
            let unit = runner
                .run("systemctl", &["cat".into(), slice.into()])
                .map_err(|error| {
                    ExecutionError::DockerPreflight(format!(
                        "systemd configuration probe for {slice}: {error}"
                    ))
                })?;
            if unit.code != 0 {
                return Err(ExecutionError::DockerPreflight(format!(
                    "systemd configuration probe for {slice} exited {}: {}",
                    unit.code, unit.stderr
                )));
            }
            Ok(unit
                .stdout
                .lines()
                .filter_map(|line| line.trim().strip_prefix("CPUQuota="))
                .next_back()
                .unwrap_or_default()
                .to_string())
        },
    )?;
    if declared_quota != format!("{expected_quota}%") {
        return Err(ExecutionError::DockerPreflight(format!(
            "Docker job cgroup boundary requires CPUQuota={expected_quota}% on {slice}; got {declared_quota:?}"
        )));
    }

    Ok(())
}

fn verify_docker_vm_resource_controls(
    runner: &mut dyn CommandRunner,
    image: &str,
) -> Result<(), ExecutionError> {
    let sequence = CAPABILITY_PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!("velnor-capability-probe-{}-{sequence}", std::process::id());
    let create_args = vec![
        "create".to_owned(),
        "--name".to_owned(),
        name.clone(),
        "--cgroup-parent".to_owned(),
        crate::docker_lease::JOB_CGROUP_PARENT.to_owned(),
        "--cpus".to_owned(),
        "0.5".to_owned(),
        "--memory".to_owned(),
        "67108864".to_owned(),
        image.to_owned(),
    ];
    let created = runner.run("docker", &create_args).map_err(|error| {
        ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe could not create a non-running container from image {image:?}: {error}"
        ))
    })?;
    if created.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe failed to create a container with --cpus=0.5, --memory=67108864, and --cgroup-parent={}: exited {}: {}",
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
    if let Err(detail) = validate_docker_resource_projection(parent, cpus, memory) {
        return Err(ExecutionError::DockerPreflight(format!(
            "macOS Docker VM resource-isolation probe did not preserve Velnor limits: {detail}"
        )));
    }
    Ok(())
}

fn systemd_slice_state(
    runner: &mut dyn CommandRunner,
    slice: &str,
) -> Result<(String, String), ExecutionError> {
    let result = runner
        .run(
            "systemctl",
            &[
                "show".into(),
                "--property=LoadState".into(),
                "--property=CPUQuotaPerSecUSec".into(),
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
    let mut quota = None;
    for line in result.stdout.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "LoadState" => load_state = Some(value.trim().to_string()),
            "CPUQuotaPerSecUSec" => quota = Some(value.trim().to_string()),
            _ => {}
        }
    }
    let (Some(load_state), Some(quota)) = (load_state, quota) else {
        return Err(ExecutionError::DockerPreflight(format!(
            "systemd slice state probe for {slice} returned malformed output {:?}",
            result.stdout.trim()
        )));
    };
    Ok((load_state, quota))
}

fn parse_systemd_duration_usec(value: &str) -> Option<u128> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("infinity") {
        return None;
    }
    let (number, multiplier) = [
        ("min", 60_000_000),
        ("ms", 1_000),
        ("us", 1),
        ("s", 1_000_000),
        ("h", 3_600_000_000),
        ("d", 86_400_000_000),
        ("w", 604_800_000_000),
    ]
    .iter()
    .find_map(|(unit, multiplier)| value.strip_suffix(unit).map(|number| (number, *multiplier)))
    .unwrap_or((value, 1));
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole.chars().all(|character| character.is_ascii_digit())
        || !fraction.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    let whole = whole.parse::<u128>().ok()?;
    let whole_usec = whole.checked_mul(multiplier)?;
    let fraction_usec = if fraction.is_empty() {
        0
    } else {
        let scale = 10_u128.checked_pow(fraction.len().try_into().ok()?)?;
        let fraction = fraction.parse::<u128>().ok()?;
        let fraction_usec = fraction.checked_mul(multiplier)?;
        (fraction_usec % scale == 0).then_some(fraction_usec / scale)?
    };
    let total = whole_usec.checked_add(fraction_usec)?;
    (total > 0).then_some(total)
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

    #[test]
    fn linux_systemd_v2_keeps_the_host_slice_proof() {
        assert_eq!(
            validate_docker_isolation(
                HostPlatform::Linux,
                "systemd",
                "2",
                DockerResourceCapabilities::all(),
            ),
            Ok(DockerIsolationMode::LinuxSystemdV2)
        );
    }

    #[test]
    fn linux_cgroupfs_v2_does_not_bypass_the_systemd_boundary() {
        let Err(error) = validate_docker_isolation(
            HostPlatform::Linux,
            "cgroupfs",
            "2",
            DockerResourceCapabilities::all(),
        ) else {
            panic!("cgroupfs on Linux must not skip the systemd boundary");
        };
        assert!(error.contains("systemd cgroup driver"), "{error}");
    }

    #[test]
    fn macos_vm_cgroupfs_v2_accepts_equivalent_docker_controls() {
        assert_eq!(
            validate_docker_isolation(
                HostPlatform::MacOs,
                "cgroupfs",
                "2",
                DockerResourceCapabilities::all(),
            ),
            Ok(DockerIsolationMode::DockerVmCgroupV2)
        );
    }

    #[test]
    fn macos_vm_rejects_missing_resource_control() {
        let Err(error) = validate_docker_isolation(
            HostPlatform::MacOs,
            "cgroupfs",
            "2",
            DockerResourceCapabilities {
                cpu_limit: true,
                memory_limit: false,
                cgroup_parent: true,
            },
        ) else {
            panic!("macOS Docker VM must require memory limits");
        };
        assert!(error.contains("--memory"), "{error}");
        assert!(error.contains("macOS Docker VM"), "{error}");
    }

    #[test]
    fn macos_vm_resource_projection_matches_per_container_limits() {
        assert!(validate_docker_resource_projection(
            DOCKER_JOB_CGROUP_PARENT,
            "500000000",
            "67108864",
        )
        .is_ok());
        let error =
            validate_docker_resource_projection(DOCKER_JOB_CGROUP_PARENT, "1000000000", "67108864")
                .unwrap_err();
        assert!(error.contains(DOCKER_RESOURCE_BOUNDARY_CHECK), "{error}");
        assert!(error.contains("NanoCpus"), "{error}");
    }

    #[test]
    fn every_platform_rejects_cgroup_v1() {
        for platform in [
            HostPlatform::Linux,
            HostPlatform::MacOs,
            HostPlatform::Other,
        ] {
            let Err(error) = validate_docker_isolation(
                platform,
                "cgroupfs",
                "1",
                DockerResourceCapabilities::all(),
            ) else {
                panic!("{platform:?} must reject cgroup v1");
            };
            assert!(error.contains("cgroup v2"), "{error}");
        }
    }
}

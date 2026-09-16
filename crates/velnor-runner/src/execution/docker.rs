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

/// Proof that the Docker VM preserves Velnor's per-container resource controls
/// (`--cpus`, `--memory`, `--cgroup-parent`).
///
/// A daemon-generation fact: the projection is decided by the Engine and the
/// VM kernel it runs on, both of which are part of the daemon key. It used to
/// create, inspect and remove a disposable probe container at every preflight
/// because the daemon generation could not be observed on macOS.
static VM_RESOURCE_CONTROLS: Fact<()> =
    Fact::new("docker-vm-resource-controls", FactLifetime::Daemon);

/// Whether this daemon can mount a D18 read-through store layer: an overlay
/// volume whose lower is the shared Velnor store filesystem and whose upper
/// and work directories are job-labelled named volumes on the daemon's own
/// storage.
///
/// A daemon-generation fact: it is decided by the daemon kernel's overlayfs
/// and the filesystem the daemon sees the store through (native ext4/xfs on
/// Linux; virtiofs or gRPC-FUSE under Docker Desktop/OrbStack, which
/// overlayfs must accept as a lower). It is probed by mounting one and
/// writing through it — a mount that succeeds and then refuses writes is
/// exactly the failure mode the probe exists to catch.
static STORE_OVERLAY: Fact<StoreOverlaySupport> =
    Fact::new("docker-store-overlay", FactLifetime::Daemon);

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

/// Outcome of the read-through store overlay capability probe
/// ([`store_overlay_support`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreOverlaySupport {
    /// The daemon mounted an overlay over the store filesystem and a write
    /// through it landed in the job-scoped upper, not in the store.
    Supported,
    /// The daemon cannot mount a writable overlay over the store filesystem.
    /// The reason is the probe's own observation, suitable for preflight
    /// output and the job log.
    Unsupported { reason: String },
}

impl StoreOverlaySupport {
    #[must_use]
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported)
    }

    /// One line for preflight and diagnostics output.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Supported => "supported".to_owned(),
            Self::Unsupported { reason } => format!("unsupported: {reason}"),
        }
    }
}

/// Label carried by every disposable resource the store overlay probe
/// creates, so a crashed probe can be recognised and reclaimed.
pub const STORE_OVERLAY_PROBE_LABEL: &str = "velnor.preflight=store-overlay";

/// Whether the daemon behind `runner` can mount D18 read-through store
/// layers, probed once per daemon generation.
///
/// `work_dir` is the runner's host-visible work root and
/// `docker_host_work_dir` the daemon's view of it (see
/// [`crate::container::JobContainerSpec::docker_host_work_dir`]); the probe
/// builds its layers beneath `work_dir` so they sit on the same filesystem
/// the real store does.
///
/// # Errors
/// The probe could not run to a verdict: the daemon refused to create or
/// remove the disposable volume/container, or the paths could not be mapped
/// into the daemon's view. An overlay that mounts but does not behave is a
/// verdict ([`StoreOverlaySupport::Unsupported`]), not an error.
pub fn store_overlay_support(
    runner: &mut dyn CommandRunner,
    image: Option<&str>,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
) -> Result<StoreOverlaySupport, ExecutionError> {
    let host_runner = runner.is_host_process_runner();
    STORE_OVERLAY.get_or_try_init(host_runner.then(facts::daemon).flatten(), || {
        probe_store_overlay(
            runner,
            image.unwrap_or(MACOS_DOCKER_CAPABILITY_PROBE_IMAGE),
            work_dir,
            docker_host_work_dir,
        )
    })
}

const STORE_OVERLAY_PROBE_LOWER_MARKER: &str = "velnor-lower-marker";
const STORE_OVERLAY_PROBE_WRITTEN: &str = "velnor-written-through-overlay";

fn probe_store_overlay(
    runner: &mut dyn CommandRunner,
    image: &str,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
) -> Result<StoreOverlaySupport, ExecutionError> {
    let sequence = CAPABILITY_PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "velnor-store-overlay-probe-{}-{sequence}",
        std::process::id()
    );
    let probe_root = work_dir.join("preflight").join(&name);
    let overlay = crate::storage::StoreOverlay {
        volume: name.clone(),
        lower: probe_root.join("lower"),
        upper_volume: format!("{name}-upper"),
        work_volume: format!("{name}-work"),
        target: "/__store".to_owned(),
    };
    let prepared = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&overlay.lower)?;
        std::fs::write(
            overlay.lower.join(STORE_OVERLAY_PROBE_LOWER_MARKER),
            "velnor\n",
        )
    })();
    if let Err(error) = prepared {
        let _ = std::fs::remove_dir_all(&probe_root);
        return Err(ExecutionError::DockerPreflight(format!(
            "store overlay probe could not prepare its lower under {}: {error}",
            probe_root.display()
        )));
    }
    let verdict = run_store_overlay_probe(
        runner,
        image,
        &name,
        &overlay,
        work_dir,
        docker_host_work_dir,
    );
    // Cleanup runs on every path; a failed cleanup is an error even when the
    // verdict was reached, since a leaked overlay volume would pin the probe
    // lower and leaked scratch volumes would hold daemon storage.
    let mut remove_args = vec![
        "volume".to_owned(),
        "rm".to_owned(),
        "--force".to_owned(),
        "--".to_owned(),
        name.clone(),
    ];
    remove_args.extend(overlay.scratch_volumes().map(str::to_owned));
    let removed = runner.run("docker", &remove_args);
    let _ = std::fs::remove_dir_all(&probe_root);
    let removed = removed.map_err(|error| {
        ExecutionError::DockerPreflight(format!(
            "store overlay probe cleanup failed for volume {name}: {error}"
        ))
    })?;
    if removed.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "store overlay probe cleanup failed for volume {name}: exited {}: {}",
            removed.code,
            removed.stderr.trim()
        )));
    }
    verdict
}

fn run_store_overlay_probe(
    runner: &mut dyn CommandRunner,
    image: &str,
    name: &str,
    overlay: &crate::storage::StoreOverlay,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
) -> Result<StoreOverlaySupport, ExecutionError> {
    let (label_key, label_value) = STORE_OVERLAY_PROBE_LABEL
        .split_once('=')
        .unwrap_or((STORE_OVERLAY_PROBE_LABEL, ""));
    let labels = [(label_key, label_value)];
    let scratch_volumes = overlay.scratch_volumes().map(str::to_owned);
    for volume in &scratch_volumes {
        let created = runner
            .run(
                "docker",
                &crate::storage::StoreOverlay::create_scratch_volume_args(volume, &labels),
            )
            .map_err(|error| {
                ExecutionError::DockerPreflight(format!(
                    "store overlay probe could not create scratch volume {volume}: {error}"
                ))
            })?;
        if created.code != 0 {
            return Err(ExecutionError::DockerPreflight(format!(
                "store overlay probe could not create scratch volume {volume}: exited {}: {}",
                created.code,
                created.stderr.trim()
            )));
        }
    }
    let inspected = runner
        .run(
            "docker",
            &crate::storage::StoreOverlay::inspect_mountpoints_args(&scratch_volumes),
        )
        .map_err(|error| {
            ExecutionError::DockerPreflight(format!(
                "store overlay probe could not inspect its scratch volumes: {error}"
            ))
        })?;
    if inspected.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "store overlay probe could not inspect its scratch volumes: exited {}: {}",
            inspected.code,
            inspected.stderr.trim()
        )));
    }
    let scratch = crate::storage::ScratchMountpoints::parse(&scratch_volumes, &inspected.stdout)
        .map_err(|error| {
            ExecutionError::DockerPreflight(format!(
                "store overlay probe could not locate its scratch volumes: {error}"
            ))
        })?;
    let create_args = overlay
        .create_volume_args(&labels, &scratch, |path, label| {
            probe_daemon_path(path, label, work_dir, docker_host_work_dir)
        })
        .map_err(|error| {
            ExecutionError::DockerPreflight(format!(
                "store overlay probe could not map its layers into the daemon's view: {error}"
            ))
        })?;
    let created = runner.run("docker", &create_args).map_err(|error| {
        ExecutionError::DockerPreflight(format!(
            "store overlay probe could not create volume {name}: {error}"
        ))
    })?;
    if created.code != 0 {
        return Err(ExecutionError::DockerPreflight(format!(
            "store overlay probe could not create overlay volume {name}: exited {}: {}",
            created.code,
            created.stderr.trim()
        )));
    }
    // The upper is daemon-side, so the container is the only witness that
    // the write landed in the merged tree; the host sees the lower, so a
    // write that reached the store is observed there.
    let run_args = vec![
        "run".to_owned(),
        "--rm".to_owned(),
        "--name".to_owned(),
        name.to_owned(),
        "--label".to_owned(),
        STORE_OVERLAY_PROBE_LABEL.to_owned(),
        "-v".to_owned(),
        overlay.mount_operand(),
        image.to_owned(),
        "sh".to_owned(),
        "-c".to_owned(),
        format!(
            "test -f {target}/{lower} && echo velnor > {target}/{written} && test -f {target}/{written}",
            target = overlay.target,
            lower = STORE_OVERLAY_PROBE_LOWER_MARKER,
            written = STORE_OVERLAY_PROBE_WRITTEN,
        ),
    ];
    let ran = runner.run("docker", &run_args).map_err(|error| {
        ExecutionError::DockerPreflight(format!(
            "store overlay probe could not run its container: {error}"
        ))
    })?;
    if ran.code != 0 {
        return Ok(StoreOverlaySupport::Unsupported {
            reason: format!(
                "the daemon could not mount a writable overlay over the store filesystem (probe container exited {}: {})",
                ran.code,
                ran.stderr.trim()
            ),
        });
    }
    Ok(store_overlay_verdict(
        overlay.lower.join(STORE_OVERLAY_PROBE_WRITTEN).exists(),
    ))
}

/// Decide the probe verdict once the container wrote through the overlay
/// and read its write back: the merged tree took the write, so the only
/// remaining question is whether it stayed in the job-scoped upper or leaked
/// into the store the host sees.
fn store_overlay_verdict(written_in_lower: bool) -> StoreOverlaySupport {
    if written_in_lower {
        StoreOverlaySupport::Unsupported {
            reason: "a write through the overlay reached the lower (trusted) layer".to_owned(),
        }
    } else {
        StoreOverlaySupport::Supported
    }
}

/// Map a probe path into the daemon's view of the work root.
fn probe_daemon_path(
    path: &Path,
    label: &str,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
) -> std::io::Result<std::path::PathBuf> {
    let Some(daemon_work_dir) = docker_host_work_dir else {
        return Ok(path.to_path_buf());
    };
    let relative = path.strip_prefix(work_dir).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{label} '{}' is outside the runner work root '{}'",
                path.display(),
                work_dir.display()
            ),
        )
    })?;
    Ok(daemon_work_dir.join(relative))
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
            VM_RESOURCE_CONTROLS.get_or_try_init(generation, || {
                verify_docker_vm_resource_controls(
                    runner,
                    image.unwrap_or(MACOS_DOCKER_CAPABILITY_PROBE_IMAGE),
                )
            })?;
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
    use crate::execution::{HostFs, MemoryFs, RecordingCommands, RecordingFirecracker};
    use std::path::PathBuf;

    /// A daemon double for the store overlay probe: records every call,
    /// keeps a "volume store" of its own for the scratch volumes, and answers
    /// `docker run` by acting like an overlay of the given behaviour.
    struct OverlayDaemon {
        calls: Vec<Vec<String>>,
        behaviour: OverlayBehaviour,
        /// The daemon's volume root: scratch volumes get a data directory
        /// here, which `volume inspect` reports as their mountpoint.
        volume_root: PathBuf,
        volume_options: Option<String>,
    }

    #[derive(Clone, Copy)]
    enum OverlayBehaviour {
        /// A daemon whose overlayfs takes the layers: the write lands in the
        /// scratch upper.
        WritesToUpper,
        /// An overlayfs that mounts the layers read-only, so the write
        /// inside the container fails.
        ReadOnlyMount,
        /// A broken overlay that writes straight through to the lower layer.
        WritesToLower,
        /// The daemon refuses to create the overlay volume at all.
        RefusesVolume,
    }

    impl CommandRunner for OverlayDaemon {
        fn run(
            &mut self,
            program: &str,
            args: &[String],
        ) -> anyhow::Result<crate::executor::CommandResult> {
            assert_eq!(program, "docker");
            self.calls.push(args.to_vec());
            let ok = crate::executor::CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            };
            if args.starts_with(&["volume".to_owned(), "create".to_owned()]) {
                let options = args.windows(2).find_map(|pair| {
                    (pair[0] == "--opt")
                        .then(|| pair[1].strip_prefix("o="))
                        .flatten()
                });
                let Some(options) = options else {
                    // A plain scratch volume: no host bind, a data directory
                    // in the daemon's own storage.
                    let name = args.last().unwrap();
                    assert!(!args.iter().any(|arg| arg == "--opt"), "{args:?}");
                    std::fs::create_dir_all(self.volume_root.join(name).join("_data")).unwrap();
                    return Ok(ok);
                };
                if matches!(self.behaviour, OverlayBehaviour::RefusesVolume) {
                    return Ok(crate::executor::CommandResult {
                        code: 1,
                        stdout: String::new(),
                        stderr: "Error response from daemon: invalid option".to_owned(),
                    });
                }
                self.volume_options = Some(options.to_owned());
                return Ok(ok);
            }
            if args.starts_with(&["volume".to_owned(), "inspect".to_owned()]) {
                let names = &args[args.iter().position(|arg| arg == "--").unwrap() + 1..];
                let stdout = names
                    .iter()
                    .map(|name| {
                        let data = self.volume_root.join(name).join("_data");
                        assert!(data.is_dir(), "inspected volume {name} was never created");
                        format!("{}\n", data.display())
                    })
                    .collect();
                return Ok(crate::executor::CommandResult { stdout, ..ok });
            }
            if args.first().is_some_and(|arg| arg == "run") {
                let options = self.volume_options.clone().expect("volume created first");
                let layer = |key: &str| -> PathBuf {
                    options
                        .split(',')
                        .find_map(|part| part.strip_prefix(key))
                        .map(PathBuf::from)
                        .expect(key)
                };
                assert!(layer("lowerdir=")
                    .join(STORE_OVERLAY_PROBE_LOWER_MARKER)
                    .is_file());
                // The upper and work operands are the daemon's own scratch
                // data directories, never anything under the host store.
                for key in ["upperdir=", "workdir="] {
                    let dir = layer(key);
                    assert!(
                        dir.starts_with(&self.volume_root) && dir.is_dir(),
                        "{key} must name a scratch volume data directory: {}",
                        dir.display()
                    );
                }
                return Ok(match self.behaviour {
                    OverlayBehaviour::WritesToUpper => {
                        std::fs::write(
                            layer("upperdir=").join(STORE_OVERLAY_PROBE_WRITTEN),
                            "velnor\n",
                        )
                        .unwrap();
                        ok
                    }
                    OverlayBehaviour::WritesToLower => {
                        std::fs::write(
                            layer("lowerdir=").join(STORE_OVERLAY_PROBE_WRITTEN),
                            "velnor\n",
                        )
                        .unwrap();
                        ok
                    }
                    OverlayBehaviour::ReadOnlyMount => crate::executor::CommandResult {
                        code: 1,
                        stdout: String::new(),
                        stderr: "sh: can't create /__store/velnor-written-through-overlay: Read-only file system".to_owned(),
                    },
                    OverlayBehaviour::RefusesVolume => {
                        panic!("a daemon that refused the volume never runs the probe")
                    }
                });
            }
            if args.starts_with(&["volume".to_owned(), "rm".to_owned()]) {
                for name in &args[args.iter().position(|arg| arg == "--").unwrap() + 1..] {
                    let _ = std::fs::remove_dir_all(self.volume_root.join(name));
                }
                return Ok(ok);
            }
            Ok(ok)
        }
    }

    impl OverlayDaemon {
        fn new(behaviour: OverlayBehaviour) -> Self {
            static VOLUME_ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let volume_root = std::env::temp_dir().join(format!(
                "velnor-store-overlay-daemon-volumes-{}-{}",
                std::process::id(),
                VOLUME_ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&volume_root).unwrap();
            Self {
                calls: Vec::new(),
                behaviour,
                volume_root,
                volume_options: None,
            }
        }

        /// Volumes still present in the daemon's storage: a leak if any.
        fn volumes(&self) -> Vec<String> {
            std::fs::read_dir(&self.volume_root)
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok())
                        .map(|entry| entry.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        }

        fn probe(
            &mut self,
            docker_host_work_dir: Option<&Path>,
        ) -> Result<StoreOverlaySupport, ExecutionError> {
            static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let work_dir = std::env::temp_dir().join(format!(
                "velnor-store-overlay-probe-test-{}-{}",
                std::process::id(),
                TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&work_dir).unwrap();
            let verdict = probe_store_overlay(self, "alpine:3.20", &work_dir, docker_host_work_dir);
            assert!(
                std::fs::read_dir(work_dir.join("preflight"))
                    .map(|entries| entries.count() == 0)
                    .unwrap_or(true),
                "the probe must remove its lower"
            );
            assert_eq!(
                self.volumes(),
                Vec::<String>::new(),
                "the probe must remove its scratch volumes"
            );
            let _ = std::fs::remove_dir_all(&work_dir);
            let _ = std::fs::remove_dir_all(&self.volume_root);
            verdict
        }
    }

    #[test]
    fn store_overlay_probe_reports_a_native_daemon_as_supported() {
        let mut daemon = OverlayDaemon::new(OverlayBehaviour::WritesToUpper);
        assert_eq!(daemon.probe(None).unwrap(), StoreOverlaySupport::Supported);
        // Two scratch volumes, their mountpoints, the overlay volume, the
        // run, one removal of all three — and nothing mounted by the host.
        let verbs: Vec<String> = daemon
            .calls
            .iter()
            .map(|call| call[..2.min(call.len())].join(" "))
            .collect();
        assert_eq!(
            verbs,
            [
                "volume create",
                "volume create",
                "volume inspect",
                "volume create",
                "run --rm",
                "volume rm"
            ]
        );
        let run = &daemon.calls[4];
        assert!(run.contains(&STORE_OVERLAY_PROBE_LABEL.to_owned()));
        assert!(run.iter().any(
            |arg| arg.ends_with(":/__store") && arg.starts_with("velnor-store-overlay-probe-")
        ));
        let removed = daemon.calls.last().unwrap();
        let names = &removed[removed.iter().position(|arg| arg == "--").unwrap() + 1..];
        assert_eq!(names.len(), 3, "{removed:?}");
        assert!(names[1].ends_with("-upper") && names[2].ends_with("-work"));
        // Every disposable volume carries the probe label.
        for call in daemon.calls.iter().filter(|call| call[1] == "create") {
            assert!(
                call.windows(2)
                    .any(|pair| pair[0] == "--label" && pair[1] == STORE_OVERLAY_PROBE_LABEL),
                "{call:?}"
            );
        }
    }

    #[test]
    fn store_overlay_probe_reports_a_read_only_mount_as_unsupported() {
        // An overlayfs that takes the layers but mounts read-only: the mount
        // "succeeds" and the write fails. That is a verdict with the
        // daemon's own words, not an error, and every volume is still
        // removed.
        let mut daemon = OverlayDaemon::new(OverlayBehaviour::ReadOnlyMount);
        let StoreOverlaySupport::Unsupported { reason } = daemon.probe(None).unwrap() else {
            panic!("a read-only overlay must be unsupported");
        };
        assert!(reason.contains("Read-only file system"), "{reason}");
        assert!(daemon
            .calls
            .last()
            .is_some_and(|call| call.starts_with(&["volume".to_owned(), "rm".to_owned()])));
    }

    #[test]
    fn store_overlay_probe_keeps_the_upper_off_the_host_store() {
        // The virtiofs defect: an upper on the host bind is what OrbStack's
        // overlayfs refused. The probe's overlay names the daemon's scratch
        // data directories as upper and work; only the lower is a host
        // path mapped into the daemon's view.
        let mut daemon = OverlayDaemon::new(OverlayBehaviour::WritesToUpper);
        let work_dir = std::env::temp_dir().join(format!(
            "velnor-store-overlay-scratch-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&work_dir).unwrap();
        let verdict = probe_store_overlay(&mut daemon, "alpine:3.20", &work_dir, Some(&work_dir));
        let _ = std::fs::remove_dir_all(&work_dir);
        assert_eq!(verdict.unwrap(), StoreOverlaySupport::Supported);
        let overlay_create = daemon
            .calls
            .iter()
            .find(|call| call.iter().any(|arg| arg == "type=overlay"))
            .expect("overlay volume create");
        let options = overlay_create
            .iter()
            .find_map(|arg| arg.strip_prefix("o="))
            .unwrap();
        let layer = |key: &str| -> PathBuf {
            options
                .split(',')
                .find_map(|part| part.strip_prefix(key))
                .map(PathBuf::from)
                .expect(key)
        };
        assert!(layer("lowerdir=").starts_with(&work_dir));
        assert!(layer("upperdir=").starts_with(&daemon.volume_root));
        assert!(layer("workdir=").starts_with(&daemon.volume_root));
        let _ = std::fs::remove_dir_all(&daemon.volume_root);
    }

    #[test]
    fn store_overlay_probe_rejects_writes_that_reach_the_lower_layer() {
        let mut daemon = OverlayDaemon::new(OverlayBehaviour::WritesToLower);
        let StoreOverlaySupport::Unsupported { reason } = daemon.probe(None).unwrap() else {
            panic!("a write into the trusted layer must be unsupported");
        };
        assert!(reason.contains("lower"), "{reason}");
    }

    #[test]
    fn store_overlay_probe_maps_its_layers_into_the_daemon_view() {
        let mut daemon = OverlayDaemon::new(OverlayBehaviour::WritesToUpper);
        // Mapping the layers under a daemon root the double cannot see makes
        // the marker lookup fail loudly inside the double; use the identity
        // mapping through an explicit daemon root equal to the work dir.
        let work_dir = std::env::temp_dir().join(format!(
            "velnor-store-overlay-map-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&work_dir).unwrap();
        let verdict = probe_store_overlay(&mut daemon, "alpine:3.20", &work_dir, Some(&work_dir));
        let _ = std::fs::remove_dir_all(&work_dir);
        assert_eq!(verdict.unwrap(), StoreOverlaySupport::Supported);
        let mapped = probe_daemon_path(
            Path::new("/host/work/preflight/p/upper"),
            "upper",
            Path::new("/host/work"),
            Some(Path::new("/daemon/work")),
        )
        .unwrap();
        assert_eq!(mapped, PathBuf::from("/daemon/work/preflight/p/upper"));
        assert!(probe_daemon_path(
            Path::new("/elsewhere/upper"),
            "upper",
            Path::new("/host/work"),
            Some(Path::new("/daemon/work")),
        )
        .is_err());
    }

    #[test]
    fn store_overlay_probe_without_a_verdict_is_an_error() {
        let mut daemon = OverlayDaemon::new(OverlayBehaviour::RefusesVolume);
        let error = daemon.probe(None).unwrap_err();
        assert!(error.to_string().contains("invalid option"), "{error}");
    }

    #[test]
    fn store_overlay_verdict_needs_the_write_to_stay_out_of_the_lower() {
        assert_eq!(store_overlay_verdict(false), StoreOverlaySupport::Supported);
        assert!(!store_overlay_verdict(true).is_supported());
    }

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

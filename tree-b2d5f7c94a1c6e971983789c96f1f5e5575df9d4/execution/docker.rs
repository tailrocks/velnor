//! Host-Docker backend. Preserves current semantics through the contract.

use super::backend::{ExecutionError, ExecutionEvent, ValidatedPlan};
use super::isolation::IsolationIdentity;
use super::ExecutionWorld;
use crate::executor::CommandRunner;
#[cfg(not(test))]
use std::{
    sync::{Mutex, OnceLock},
    time::SystemTime,
};
use velnor_model::JobConclusion;

#[cfg(not(test))]
const JOB_CGROUP_DROPIN: &str = "/etc/systemd/system/velnor-jobs.slice.d/10-host-cpu.conf";

#[cfg(not(test))]
static DOCKER_BOUNDARY_PROOF: OnceLock<Mutex<Option<SystemTime>>> = OnceLock::new();

/// Host Docker execution. Uses `/var/run/docker.sock` (via the job lease).
#[derive(Debug, Default)]
pub struct DockerBackend {
    pub started: bool,
}

impl DockerBackend {
    /// # Errors
    /// Missing host Docker socket or Docker/cgroup-boundary probe failure.
    pub fn preflight(world: &mut ExecutionWorld<'_>) -> Result<(), ExecutionError> {
        if !world.host_fs.exists(world.host_docker_socket) {
            return Err(ExecutionError::DockerPreflight(format!(
                "missing host Docker socket {}",
                world.host_docker_socket.display()
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
    let driver = runner
        .run(
            "docker",
            &[
                "info".into(),
                "--format".into(),
                "{{.CgroupDriver}} {{.CgroupVersion}}".into(),
            ],
        )
        .map_err(|error| {
            invalidate_docker_job_cgroup_boundary();
            ExecutionError::DockerPreflight(format!("docker cgroup probe: {error}"))
        })?;
    if driver.code != 0 {
        invalidate_docker_job_cgroup_boundary();
        return Err(ExecutionError::DockerPreflight(format!(
            "docker info cgroup probe exited {}: {}",
            driver.code, driver.stderr
        )));
    }

    if driver.stdout.split_whitespace().collect::<Vec<_>>() != ["systemd", "2"] {
        return Err(ExecutionError::DockerPreflight(format!(
            "Docker job cgroup boundary requires systemd cgroup driver on cgroup v2; got {:?}",
            driver.stdout.trim()
        )));
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

    #[cfg(not(test))]
    if let Some(marker_mtime) = job_cgroup_dropin_mtime()
        && DOCKER_BOUNDARY_PROOF
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|proof_mtime| proof_mtime == marker_mtime)
    {
        return Ok(());
    }

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
    let effective_quota = unit
        .stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix("CPUQuota="))
        .next_back()
        .unwrap_or_default();
    if effective_quota != format!("{expected_quota}%") {
        return Err(ExecutionError::DockerPreflight(format!(
            "Docker job cgroup boundary requires CPUQuota={expected_quota}% on {slice}; got {effective_quota:?}"
        )));
    }

    #[cfg(not(test))]
    if let Some(marker_mtime) = job_cgroup_dropin_mtime() {
        *DOCKER_BOUNDARY_PROOF
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(marker_mtime);
    }
    Ok(())
}

#[cfg(not(test))]
fn job_cgroup_dropin_mtime() -> Option<SystemTime> {
    std::fs::metadata(JOB_CGROUP_DROPIN)
        .and_then(|metadata| metadata.modified())
        .ok()
}

pub(crate) fn invalidate_docker_job_cgroup_boundary() {
    #[cfg(not(test))]
    if let Some(cache) = DOCKER_BOUNDARY_PROOF.get() {
        *cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
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
                "--value".into(),
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
    let mut values = result.stdout.lines();
    let load_state = values.next().unwrap_or_default().trim().to_string();
    let quota = values.next().unwrap_or_default().trim().to_string();
    if load_state.is_empty() || quota.is_empty() || values.next().is_some() {
        return Err(ExecutionError::DockerPreflight(format!(
            "systemd slice state probe for {slice} returned malformed output {:?}",
            result.stdout.trim()
        )));
    }
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

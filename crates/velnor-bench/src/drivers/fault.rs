//! Real fault injection into real container lifecycles.
//!
//! Each workload here triggers one catalogue fault
//! ([`crate::fault::FAULT_CATALOGUE`]) against a real Docker daemon and
//! asserts the real containment: the failure surfaces where it must, and
//! teardown removes every owned object. The [`FaultOutcome`](crate::fault::FaultOutcome)
//! on each observation records whether the fault actually triggered
//! (`injected`) and whether the containment held (`contained`).
//!
//! These scenarios run under the `docker-direct` fallback, so the containment
//! under test is the harness's own cleanup contract, stated on the record.
//! Injecting into a real Velnor job needs the `velnor-job` driver; until it
//! exists these are the only runnable fault proofs, and the other 23 classes
//! stay declared-but-unrun.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{bail, Context as _, Result};

use crate::{
    census::DockerCensus,
    drivers::{docker::parse_docker_id, Context, Workload},
    fault::FaultOutcome,
    gittrace::GitEvidence,
    record::{Observation, Resources},
    scenario::Scenario,
    stage::Stage,
    sys::tree_bytes,
};

/// Shape of the injected fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// SIGKILL the container while the user step runs.
    KilledMidStep,
    /// The user command exits non-zero.
    StepFails,
    /// Inspect and remove an object that does not exist.
    ObjectMissing,
    /// Create the same network twice.
    NetworkConflict,
}

impl Kind {
    fn class(self) -> &'static str {
        match self {
            Self::KilledMidStep => "docker-kill-mid-step",
            Self::StepFails => "docker-exec-nonzero",
            Self::ObjectMissing => "docker-object-missing",
            Self::NetworkConflict => "docker-network-conflict",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::KilledMidStep => "kill",
            Self::StepFails => "exit3",
            Self::ObjectMissing => "missing",
            Self::NetworkConflict => "netconf",
        }
    }
}

pub(super) fn build(scenario: &Scenario) -> Result<Box<dyn Workload>> {
    let kind = match scenario.id {
        "fault/container-killed-mid-step" => Kind::KilledMidStep,
        "fault/step-command-fails" => Kind::StepFails,
        "fault/object-missing" => Kind::ObjectMissing,
        "fault/network-conflict" => Kind::NetworkConflict,
        other => bail!(
            "no fault workload is implemented for {other}; \
             it is declared in the matrix and reported as unrun"
        ),
    };
    Ok(Box::new(FaultWorkload {
        kind,
        run_id: format!("{}-{}", std::process::id(), unique_nonce()),
        scratch: None,
        image_id: None,
        iteration: 0,
        uncontained: 0,
        owned_containers: Vec::new(),
        owned_networks: Vec::new(),
    }))
}

fn unique_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

struct FaultWorkload {
    kind: Kind,
    run_id: String,
    scratch: Option<PathBuf>,
    image_id: Option<String>,
    iteration: u64,
    uncontained: u64,
    owned_containers: Vec<String>,
    owned_networks: Vec<String>,
}

impl FaultWorkload {
    fn container_name(&self) -> String {
        format!(
            "vb-fault-{}-{}-{}",
            self.run_id,
            self.iteration,
            self.kind.tag()
        )
    }

    fn network_name(&self) -> String {
        format!(
            "vb-fault-net-{}-{}-{}",
            self.run_id,
            self.iteration,
            self.kind.tag()
        )
    }

    fn missing_name(&self) -> String {
        // An object that can never exist: the workload never creates it, and
        // the run-scoped name cannot collide with another run's objects.
        format!("vb-fault-absent-{}-{}", self.run_id, self.iteration)
    }

    fn owner_label(&self) -> String {
        format!("com.velnor.bench.owner=fault-{}", self.run_id)
    }

    fn role_label(&self) -> String {
        format!("com.velnor.bench.role=fault-{}", self.kind.tag())
    }
}

/// Image identity the workload measured. Fails closed: without the image
/// there is no container lifecycle to inject into.
fn ensure_image(context: &mut Context, image: &str) -> Result<String> {
    let inspected = context
        .runner
        .run(
            "docker",
            &["image", "inspect", "--format", "{{.Id}}", image],
        )
        .context("docker image inspect")?
        .stdout
        .trim()
        .to_owned();
    if !inspected.is_empty() {
        return verify_image_id(&inspected);
    }
    let pulled = context
        .runner
        .run("docker", &["pull", image])
        .context("docker pull")?
        .clone();
    if !pulled.ok() {
        bail!(
            "docker pull {} failed with exit code {}: {}",
            image,
            pulled.code,
            pulled.stderr.trim()
        );
    }
    let inspected = context
        .runner
        .run(
            "docker",
            &["image", "inspect", "--format", "{{.Id}}", image],
        )
        .context("docker image inspect after pull")?
        .stdout
        .trim()
        .to_owned();
    verify_image_id(&inspected)
}

fn verify_image_id(inspected: &str) -> Result<String> {
    let id = inspected.strip_prefix("sha256:").unwrap_or(inspected);
    Ok(format!("sha256:{}", parse_docker_id(id)?))
}

/// Strictly parse `docker wait` output: one integer, nothing else.
fn parse_wait_code(stdout: &str) -> Result<i32> {
    let trimmed = stdout.trim();
    if trimmed.lines().count() != 1 {
        bail!("docker wait returned malformed output: {trimmed:?}");
    }
    trimmed
        .parse::<i32>()
        .with_context(|| format!("docker wait returned a non-integer exit code: {trimmed:?}"))
}

/// True when the daemon answers that the named object does not exist.
fn is_not_found(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("no such")
}

impl Workload for FaultWorkload {
    fn prepare(&mut self, context: &mut Context) -> Result<()> {
        let scenario = crate::scenario::find(match self.kind {
            Kind::KilledMidStep => "fault/container-killed-mid-step",
            Kind::StepFails => "fault/step-command-fails",
            Kind::ObjectMissing => "fault/object-missing",
            Kind::NetworkConflict => "fault/network-conflict",
        })
        .expect("fault scenario is declared");
        let scratch = context.scenario_dir(scenario)?;
        self.scratch = Some(scratch);
        let image = context.job_image.clone();
        let image_id = ensure_image(context, &image)?;
        self.image_id = Some(image_id);
        Ok(())
    }

    fn iterate(&mut self, context: &mut Context) -> Result<Observation> {
        self.iteration += 1;
        context.runner.reset();
        let scratch = self
            .scratch
            .clone()
            .context("fault workload was not prepared")?;
        let disk_before = tree_bytes(&scratch);
        let started = Instant::now();
        let mut stages = BTreeMap::new();

        let outcome = match self.kind {
            Kind::KilledMidStep => self.run_killed_mid_step(context, &mut stages)?,
            Kind::StepFails => self.run_step_fails(context, &mut stages)?,
            Kind::ObjectMissing => self.run_object_missing(context, &mut stages)?,
            Kind::NetworkConflict => self.run_network_conflict(context, &mut stages)?,
        };
        if !outcome.contained {
            self.uncontained += 1;
        }

        let total_ms = elapsed_ms(started);
        let usage = context.runner.rusage();
        let disk_after = tree_bytes(&scratch);
        let resources = Resources {
            cpu_user_us: usage.user_us,
            cpu_system_us: usage.system_us,
            max_rss_bytes: usage.max_rss_bytes,
            block_input_ops: usage.block_input_ops,
            block_output_ops: usage.block_output_ops,
            disk_bytes_delta: i64::try_from(disk_after).unwrap_or(i64::MAX)
                - i64::try_from(disk_before).unwrap_or(0),
            process_count: context.runner.process_count() as u64,
            docker_invocations: context.runner.count_of("docker") as u64,
            ..Resources::default()
        };
        let docker_census = DockerCensus::from_invocations(context.runner.invocations());
        Ok(Observation {
            total_ms,
            stages_ms: stages,
            checkout_phases_ms: BTreeMap::new(),
            resources,
            git: GitEvidence::NotMeasured,
            docker_census,
            fault: Some(outcome),
        })
    }

    fn teardown(&mut self, context: &mut Context) -> Result<()> {
        let mut failures = Vec::new();
        for name in std::mem::take(&mut self.owned_containers) {
            if let Err(error) = remove_container(context, &name) {
                failures.push(format!("container {name}: {error:#}"));
            }
        }
        for name in std::mem::take(&mut self.owned_networks) {
            if let Err(error) = remove_network(context, &name) {
                failures.push(format!("network {name}: {error:#}"));
            }
        }
        if let Some(scratch) = self.scratch.take()
            && let Err(error) = std::fs::remove_dir_all(&scratch)
        {
            failures.push(format!("scratch {}: {error}", scratch.display()));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("fault workload teardown failed: {}", failures.join("; "))
        }
    }

    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if let Some(image_id) = &self.image_id {
            notes.push(format!("fault workload measured image {image_id}"));
        }
        notes.push(format!(
            "fault class {}: {} iteration(s), {} uncontained",
            self.kind.class(),
            self.iteration,
            self.uncontained
        ));
        notes
    }
}

/// Remove an owned container. Absent is success: the per-iteration teardown
/// already removed it.
fn remove_container(context: &mut Context, name: &str) -> Result<()> {
    let invocation = context
        .runner
        .run("docker", &["rm", "--force", name])
        .with_context(|| format!("docker rm {name}"))?
        .clone();
    if invocation.ok() || is_not_found(&invocation.stderr) {
        return Ok(());
    }
    bail!(
        "docker rm {name} failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    )
}

fn remove_network(context: &mut Context, name: &str) -> Result<()> {
    let invocation = context
        .runner
        .run("docker", &["network", "rm", name])
        .with_context(|| format!("docker network rm {name}"))?
        .clone();
    if invocation.ok() || is_not_found(&invocation.stderr) {
        return Ok(());
    }
    bail!(
        "docker network rm {name} failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    )
}

/// Names of owned objects that still exist. Empty is containment.
fn container_residue(context: &mut Context, name: &str) -> Vec<String> {
    match context
        .runner
        .run("docker", &["inspect", "--format", "{{.Id}}", name])
    {
        Ok(invocation) if invocation.ok() => vec![format!("container {name}")],
        _ => Vec::new(),
    }
}

fn network_residue(context: &mut Context, name: &str) -> Vec<String> {
    match context.runner.run(
        "docker",
        &["network", "inspect", "--format", "{{.Id}}", name],
    ) {
        Ok(invocation) if invocation.ok() => vec![format!("network {name}")],
        _ => Vec::new(),
    }
}

impl FaultWorkload {
    /// Prove the container is running before killing it, so a kill against a
    /// dead container cannot pass as an injection.
    fn await_running(&self, context: &mut Context, name: &str) -> Result<bool> {
        for _ in 0..10 {
            let invocation = context
                .runner
                .run(
                    "docker",
                    &["inspect", "--format", "{{.State.Running}}", name],
                )
                .context("docker inspect running state")?
                .clone();
            if !invocation.ok() {
                bail!(
                    "docker inspect {name} failed with exit code {}: {}",
                    invocation.code,
                    invocation.stderr.trim()
                );
            }
            match invocation.stdout.trim() {
                "true" => return Ok(true),
                "false" => std::thread::sleep(Duration::from_millis(500)),
                other => bail!("docker inspect {name} returned unexpected state {other:?}"),
            }
        }
        Ok(false)
    }

    fn run_killed_mid_step(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<FaultOutcome> {
        let name = self.container_name();
        let image = context.job_image.clone();
        let owner = self.owner_label();
        let role = self.role_label();

        let started = Instant::now();
        let created = context
            .runner
            .run(
                "docker",
                &[
                    "create",
                    "--name",
                    name.as_str(),
                    "--label",
                    owner.as_str(),
                    "--label",
                    role.as_str(),
                    image.as_str(),
                    "sleep",
                    "300",
                ],
            )
            .context("docker create")?
            .clone();
        if !created.ok() {
            bail!(
                "docker create failed with exit code {}: {}",
                created.code,
                created.stderr.trim()
            );
        }
        parse_docker_id(created.stdout.trim()).context("docker create returned no ID")?;
        stages.insert(Stage::ContainerCreate, elapsed_ms(started));
        self.owned_containers.push(name.clone());

        let started = Instant::now();
        let started_ok = context
            .runner
            .run("docker", &["start", name.as_str()])
            .context("docker start")?
            .clone();
        if !started_ok.ok() {
            bail!(
                "docker start failed with exit code {}: {}",
                started_ok.code,
                started_ok.stderr.trim()
            );
        }
        stages.insert(Stage::ContainerStart, elapsed_ms(started));

        // The injection: SIGKILL a provably running container.
        let started = Instant::now();
        let running = self.await_running(context, &name)?;
        let killed = context
            .runner
            .run("docker", &["kill", name.as_str()])
            .context("docker kill")?
            .clone();
        let waited = context
            .runner
            .run_with_timeout("docker", &["wait", name.as_str()], Duration::from_secs(60))
            .context("docker wait")?
            .clone();
        stages.insert(Stage::FirstUserCommand, elapsed_ms(started));
        let injected = running && killed.ok() && waited.ok();
        let code = if waited.ok() {
            parse_wait_code(&waited.stdout).unwrap_or(-1)
        } else {
            -1
        };
        // SIGKILL surfaces as 137. Anything else means the kill did not land
        // as injected — or the step exited on its own first.
        let injected = injected && code == 137;

        let started = Instant::now();
        remove_container(context, &name)?;
        self.owned_containers.retain(|owned| owned != &name);
        let residue = container_residue(context, &name);
        stages.insert(Stage::Teardown, elapsed_ms(started));

        let contained = injected && residue.is_empty();
        Ok(FaultOutcome {
            class: self.kind.class().to_owned(),
            injected,
            contained,
            residue,
            detail: format!(
                "running={running} kill_exit={} wait_exit={code}",
                killed.code,
            ),
        })
    }

    fn run_step_fails(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<FaultOutcome> {
        let name = self.container_name();
        let image = context.job_image.clone();
        let owner = self.owner_label();
        let role = self.role_label();

        let started = Instant::now();
        let created = context
            .runner
            .run(
                "docker",
                &[
                    "create",
                    "--name",
                    name.as_str(),
                    "--label",
                    owner.as_str(),
                    "--label",
                    role.as_str(),
                    image.as_str(),
                    "sh",
                    "-c",
                    "exit 3",
                ],
            )
            .context("docker create")?
            .clone();
        if !created.ok() {
            bail!(
                "docker create failed with exit code {}: {}",
                created.code,
                created.stderr.trim()
            );
        }
        parse_docker_id(created.stdout.trim()).context("docker create returned no ID")?;
        stages.insert(Stage::ContainerCreate, elapsed_ms(started));
        self.owned_containers.push(name.clone());

        let started = Instant::now();
        let started_ok = context
            .runner
            .run("docker", &["start", name.as_str()])
            .context("docker start")?
            .clone();
        if !started_ok.ok() {
            bail!(
                "docker start failed with exit code {}: {}",
                started_ok.code,
                started_ok.stderr.trim()
            );
        }
        stages.insert(Stage::ContainerStart, elapsed_ms(started));

        // The injection is the failing user command itself: `exit 3`.
        let started = Instant::now();
        let waited = context
            .runner
            .run_with_timeout("docker", &["wait", name.as_str()], Duration::from_secs(120))
            .context("docker wait")?
            .clone();
        stages.insert(Stage::FirstUserCommand, elapsed_ms(started));
        let code = if waited.ok() {
            parse_wait_code(&waited.stdout).unwrap_or(-1)
        } else {
            -1
        };
        let injected = waited.ok() && code == 3;

        let started = Instant::now();
        remove_container(context, &name)?;
        self.owned_containers.retain(|owned| owned != &name);
        let residue = container_residue(context, &name);
        stages.insert(Stage::Teardown, elapsed_ms(started));

        let contained = injected && residue.is_empty();
        Ok(FaultOutcome {
            class: self.kind.class().to_owned(),
            injected,
            contained,
            residue,
            detail: format!("wait_exit={code}"),
        })
    }

    fn run_object_missing(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<FaultOutcome> {
        let name = self.missing_name();

        // The injection: operate on an object that does not exist.
        let started = Instant::now();
        let inspected = context
            .runner
            .run("docker", &["inspect", "--format", "{{.Id}}", name.as_str()])
            .context("docker inspect missing object")?
            .clone();
        stages.insert(Stage::DockerSetup, elapsed_ms(started));
        let inspect_clean = !inspected.ok() && is_not_found(&inspected.stderr);

        let started = Instant::now();
        let removed = context
            .runner
            .run("docker", &["rm", "--force", name.as_str()])
            .context("docker rm missing object")?
            .clone();
        let residue = container_residue(context, &name);
        stages.insert(Stage::Teardown, elapsed_ms(started));
        // Key on the daemon's answer, not the exit code: `docker rm --force`
        // against an absent object exits 0 on Engine 29 while still reporting
        // "No such container" on stderr. Either way the stderr proves absence;
        // a removal of a real object would print its name instead.
        let remove_clean = is_not_found(&removed.stderr);

        let injected = inspect_clean && remove_clean;
        let contained = injected && residue.is_empty();
        Ok(FaultOutcome {
            class: self.kind.class().to_owned(),
            injected,
            contained,
            residue,
            detail: format!(
                "inspect_exit={} remove_exit={}",
                inspected.code, removed.code
            ),
        })
    }

    fn run_network_conflict(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<FaultOutcome> {
        let name = self.network_name();
        let owner = self.owner_label();
        let role = self.role_label();

        let started = Instant::now();
        let created = context
            .runner
            .run(
                "docker",
                &[
                    "network",
                    "create",
                    "--label",
                    owner.as_str(),
                    "--label",
                    role.as_str(),
                    name.as_str(),
                ],
            )
            .context("docker network create")?
            .clone();
        if !created.ok() {
            bail!(
                "docker network create failed with exit code {}: {}",
                created.code,
                created.stderr.trim()
            );
        }
        parse_docker_id(created.stdout.trim()).context("docker network create returned no ID")?;
        stages.insert(Stage::DockerSetup, elapsed_ms(started));
        self.owned_networks.push(name.clone());

        // The injection: create the same network again.
        let started = Instant::now();
        let conflicted = context
            .runner
            .run("docker", &["network", "create", name.as_str()])
            .context("docker network create conflict")?
            .clone();
        stages.insert(Stage::ContainerCreate, elapsed_ms(started));
        let injected = !conflicted.ok()
            && conflicted
                .stderr
                .to_ascii_lowercase()
                .contains("already exists");

        let started = Instant::now();
        remove_network(context, &name)?;
        self.owned_networks.retain(|owned| owned != &name);
        let residue = network_residue(context, &name);
        stages.insert(Stage::Teardown, elapsed_ms(started));

        let contained = injected && residue.is_empty();
        Ok(FaultOutcome {
            class: self.kind.class().to_owned(),
            injected,
            contained,
            residue,
            detail: format!("conflict_exit={}", conflicted.code),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fault_scenario_has_a_workload_and_a_catalogue_class() {
        for id in [
            "fault/container-killed-mid-step",
            "fault/step-command-fails",
            "fault/object-missing",
            "fault/network-conflict",
        ] {
            let scenario = crate::scenario::find(id).expect("declared scenario");
            let workload = crate::drivers::build(scenario, crate::scenario::Driver::DockerDirect)
                .expect("fault workload builds");
            let notes = workload.notes();
            assert!(
                notes.iter().any(|note| note.contains("fault class")),
                "{id} workload reports no fault class"
            );
        }
        for class in [
            "docker-kill-mid-step",
            "docker-exec-nonzero",
            "docker-object-missing",
            "docker-network-conflict",
        ] {
            assert!(crate::fault::find(class).is_some(), "{class} catalogued");
        }
    }

    #[test]
    fn wait_output_must_be_one_integer() {
        assert_eq!(parse_wait_code("137\n").expect("code"), 137);
        assert_eq!(parse_wait_code("0").expect("code"), 0);
        assert!(parse_wait_code("").is_err());
        assert!(parse_wait_code("137\n0\n").is_err());
        assert!(parse_wait_code("killed").is_err());
    }

    #[test]
    fn not_found_matching_accepts_only_absent_object_errors() {
        assert!(is_not_found("Error: No such container: abc"));
        assert!(is_not_found("Error: no such network: xyz"));
        assert!(!is_not_found(""));
        assert!(!is_not_found(
            "Error response from daemon: Cannot connect to the Docker daemon"
        ));
    }
}

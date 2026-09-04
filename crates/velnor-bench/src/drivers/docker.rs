//! Real container workloads driven through the Docker CLI.
//!
//! This driver observes the container lifecycle and nothing else. It never
//! reports broker, acquisition, admission or capacity latency, because it never
//! talks to a broker: [`crate::record::BenchRecord::validate`] enforces that
//! separation on the record it produces.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use anyhow::{bail, Context as _, Result};

use crate::{
    drivers::{Context, Workload},
    gittrace::GitCounters,
    record::{Observation, Resources},
    scenario::Scenario,
    stage::Stage,
    sys::{tree_bytes, Invocation},
};

/// Shape of the container workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Image already on the host; measures pure container lifecycle.
    ExistingImage,
    /// Job-shaped container: workspace mount plus a user command.
    JobContainer,
    /// Removes the image first, so the pull is inside the measurement.
    ImagePull,
    /// Job container plus a linked service container and a readiness wait.
    ServiceContainer,
    /// `docker build` with every layer cached.
    BuildCached,
    /// `docker build` with the cache defeated at the first instruction.
    BuildUncached,
    /// `docker buildx build` through the BuildKit driver.
    Buildx,
}

/// The command a job-shaped container runs as its first user step.
const USER_COMMAND: &str = "printf velnor-bench-first-user-command";

/// Service image used for the service-container scenario. Chosen because it is
/// tiny and has a deterministic readiness signal.
const SERVICE_IMAGE: &str = "docker.io/library/redis:7-alpine";

static NEXT_BUILD_TAG: AtomicU64 = AtomicU64::new(0);

fn is_build_kind(kind: Kind) -> bool {
    matches!(kind, Kind::BuildCached | Kind::BuildUncached | Kind::Buildx)
}

fn unique_build_tag() -> String {
    let serial = NEXT_BUILD_TAG.fetch_add(1, Ordering::Relaxed);
    format!("velnor-bench-cached-{}-{serial}:latest", std::process::id())
}

fn with_cleanup(primary: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup_error) => {
            primary.context(format!("workload cleanup also failed: {cleanup_error:#}"))
        }
    }
}

fn remove_image_if_missing(context: &mut Context, image: &str) -> Result<()> {
    let invocation = context
        .runner
        .run("docker", &["image", "rm", "-f", image])
        .context("docker image removal")?;
    if invocation.ok()
        || invocation
            .stderr
            .to_ascii_lowercase()
            .contains("no such image")
    {
        return Ok(());
    }
    bail!(
        "docker image removal failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    )
}

fn force_remove(context: &mut Context, name: &str) -> Result<()> {
    let invocation = context
        .runner
        .run("docker", &["rm", "-f", name])
        .with_context(|| format!("removing Docker container {name}"))?;
    if invocation.ok()
        || invocation
            .stderr
            .to_ascii_lowercase()
            .contains("no such container")
    {
        return Ok(());
    }
    bail!(
        "removing Docker container {name} failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    )
}

fn remove_network(context: &mut Context, name: &str) -> Result<()> {
    let invocation = context
        .runner
        .run("docker", &["network", "rm", name])
        .with_context(|| format!("removing Docker network {name}"))?;
    if invocation.ok()
        || invocation
            .stderr
            .to_ascii_lowercase()
            .contains("no such network")
    {
        return Ok(());
    }
    bail!(
        "removing Docker network {name} failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    )
}

fn cleanup_resources(context: &mut Context, containers: &[&str], networks: &[&str]) -> Result<()> {
    let mut failures = Vec::new();
    for &name in containers {
        if let Err(error) = force_remove(context, name) {
            failures.push(error.to_string());
        }
    }
    for &name in networks {
        if let Err(error) = remove_network(context, name) {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("Docker resource cleanup failed: {}", failures.join("; "))
    }
}

pub(super) fn build(scenario: &Scenario) -> Result<Box<dyn Workload>> {
    let kind = match scenario.id {
        "docker/existing-image" => Kind::ExistingImage,
        "docker/simple-job-container" => Kind::JobContainer,
        "docker/image-pull" => Kind::ImagePull,
        "docker/service-container" => Kind::ServiceContainer,
        "docker/build-cached" => Kind::BuildCached,
        "docker/build-uncached" => Kind::BuildUncached,
        "docker/buildx" => Kind::Buildx,
        // Rust scenarios fall back to running the same build inside the real
        // job image; that workload lives in the cargo driver, which knows how
        // to mutate a workspace. Routing them here would measure a container,
        // not a build.
        other => bail!(
            "no docker-direct workload is implemented for {other}; \
             it is declared in the matrix and reported as unrun"
        ),
    };
    Ok(Box::new(DockerWorkload {
        kind,
        build_tag: is_build_kind(kind).then(unique_build_tag),
        build_tag_needs_cleanup: false,
        iteration: 0,
        notes: Vec::new(),
    }))
}

struct DockerWorkload {
    kind: Kind,
    build_tag: Option<String>,
    build_tag_needs_cleanup: bool,
    iteration: u64,
    notes: Vec<String>,
}

/// Wall time of one measured block, in milliseconds.
fn timed<T>(body: impl FnOnce() -> T) -> (T, u64) {
    let started = Instant::now();
    let value = body();
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    (value, elapsed)
}

fn require_success(invocation: &Invocation, operation: &str) -> Result<()> {
    if invocation.ok() {
        return Ok(());
    }
    bail!(
        "{operation} failed with exit code {}: {}",
        invocation.code,
        invocation.stderr.trim()
    );
}

fn inspect_container_state(context: &mut Context, name: &str) -> Result<()> {
    let invocation = context.runner.run(
        "docker",
        &["inspect", "--format", "{{.State.Status}}", name],
    )?;
    require_success(invocation, "docker inspect completion")?;
    if invocation.stdout.trim().is_empty() {
        bail!("docker inspect completion returned no container state");
    }
    Ok(())
}

impl DockerWorkload {
    fn container_name(&self, prefix: &str) -> String {
        format!(
            "velnor-bench-{prefix}-{}-{}",
            std::process::id(),
            self.iteration
        )
    }
}

impl Workload for DockerWorkload {
    fn prepare(&mut self, context: &mut Context) -> Result<()> {
        match self.kind {
            Kind::ImagePull => {
                // Nothing to warm: the pull is the measurement.
                self.notes.push(
                    "bytes_downloaded is the uncompressed image size the daemon reports after \
                     the pull, not the compressed bytes moved over the wire; the Docker CLI \
                     does not expose the latter"
                        .to_owned(),
                );
            }
            Kind::ServiceContainer => {
                let image = SERVICE_IMAGE.to_owned();
                let pulled = context
                    .runner
                    .run("docker", &["pull", &image])
                    .context("pulling the service image")?;
                require_success(pulled, "docker pull service image")?;
                self.notes
                    .push(format!("service container image is {SERVICE_IMAGE}"));
            }
            _ => {
                let image = context.job_image.clone();
                let present = context
                    .runner
                    .run("docker", &["image", "inspect", &image])
                    .map(|invocation| invocation.ok())
                    .unwrap_or(false);
                if !present {
                    let pulled = context
                        .runner
                        .run("docker", &["pull", &image])
                        .context("pulling the job image")?;
                    require_success(pulled, "docker pull job image")?;
                }
            }
        }
        if matches!(
            self.kind,
            Kind::BuildCached | Kind::BuildUncached | Kind::Buildx
        ) {
            let dir = context.work_root.join("docker-build-context");
            std::fs::create_dir_all(&dir)?;
            write_build_context(&dir, &context.job_image)?;
            if self.kind == Kind::BuildCached {
                // Warm the layer cache once, outside the measurement.
                let tag = self
                    .build_tag
                    .as_deref()
                    .expect("build workloads have an owned image tag");
                self.build_tag_needs_cleanup = true;
                let warmed = context
                    .runner
                    .run("docker", &["build", "-t", tag, &dir.display().to_string()])?;
                require_success(warmed, "docker cached-build warmup")?;
            }
        }
        Ok(())
    }

    fn iterate(&mut self, context: &mut Context) -> Result<Observation> {
        self.iteration += 1;
        context.runner.reset();
        let work_root = context.work_root.clone();
        let disk_before = tree_bytes(&work_root);
        let started = Instant::now();

        let mut stages = BTreeMap::new();
        let mut resources = Resources::default();

        match self.kind {
            Kind::ExistingImage | Kind::JobContainer => {
                self.run_container_lifecycle(context, &mut stages)?;
            }
            Kind::ImagePull => {
                self.run_image_pull(context, &mut stages, &mut resources)?;
            }
            Kind::ServiceContainer => {
                self.run_service_container(context, &mut stages)?;
            }
            Kind::BuildCached | Kind::BuildUncached | Kind::Buildx => {
                self.run_build(context, &mut stages, &mut resources)?;
            }
        }

        let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let usage = context.runner.rusage();
        let disk_after = tree_bytes(&work_root);

        resources.cpu_user_us = usage.user_us;
        resources.cpu_system_us = usage.system_us;
        resources.max_rss_bytes = usage.max_rss_bytes;
        resources.block_input_ops = usage.block_input_ops;
        resources.block_output_ops = usage.block_output_ops;
        resources.disk_bytes_delta =
            i64::try_from(disk_after).unwrap_or(i64::MAX) - i64::try_from(disk_before).unwrap_or(0);
        resources.process_count = context.runner.process_count() as u64;
        resources.docker_invocations = context.runner.count_of("docker") as u64;

        Ok(Observation {
            total_ms,
            stages_ms: stages,
            checkout_phases_ms: BTreeMap::new(),
            resources,
            git: GitCounters::default(),
        })
    }

    fn teardown(&mut self, context: &mut Context) -> Result<()> {
        if self.build_tag_needs_cleanup {
            let tag = self
                .build_tag
                .as_deref()
                .expect("owned build image has a tag");
            remove_image_if_missing(context, tag).context("tearing down benchmark image")?;
            self.build_tag_needs_cleanup = false;
        }
        Ok(())
    }

    fn notes(&self) -> Vec<String> {
        self.notes.clone()
    }
}

impl DockerWorkload {
    fn run_container_lifecycle(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<()> {
        let name = self.container_name("job");
        let image = context.job_image.clone();
        let mount = if self.kind == Kind::JobContainer {
            let workspace = context.work_root.join("workspace");
            std::fs::create_dir_all(&workspace)?;
            Some(format!("{}:/velnor/workspace", workspace.display()))
        } else {
            None
        };

        let (setup, setup_ms) = timed(|| {
            context
                .runner
                .run(
                    "docker",
                    &["image", "inspect", "--format", "{{.Id}}", &image],
                )
                .map(|invocation| invocation.ok())
        });
        if !setup.unwrap_or(false) {
            bail!("job image {image} disappeared between preparation and measurement");
        }
        stages.insert(Stage::DockerSetup, setup_ms);

        let mut create_args = vec![
            "create".to_owned(),
            "--name".to_owned(),
            name.clone(),
            "--entrypoint".to_owned(),
            "/bin/sh".to_owned(),
        ];
        if let Some(mount) = &mount {
            create_args.push("-v".to_owned());
            create_args.push(mount.clone());
        }
        create_args.push(image.clone());
        create_args.push("-c".to_owned());
        // Keep the container alive so start and the first user command are
        // separately observable, exactly as the runner keeps a job container.
        create_args.push("sleep 30".to_owned());

        let (created, create_ms) = timed(|| context.runner.run("docker", &create_args).cloned());
        let created = created?;
        if !created.ok() {
            let stderr = created.stderr.clone();
            let primary = anyhow::anyhow!("docker create failed: {}", stderr.trim());
            return Err(with_cleanup(primary, force_remove(context, &name)));
        }
        stages.insert(Stage::ContainerCreate, create_ms);

        let (started, start_ms) =
            timed(|| context.runner.run("docker", &["start", &name]).cloned());
        let started = started?;
        if !started.ok() {
            let stderr = started.stderr.clone();
            let primary = anyhow::anyhow!("docker start failed: {}", stderr.trim());
            return Err(with_cleanup(primary, force_remove(context, &name)));
        }
        stages.insert(Stage::ContainerStart, start_ms);

        let (executed, exec_ms) = timed(|| {
            context
                .runner
                .run("docker", &["exec", &name, "/bin/sh", "-c", USER_COMMAND])
                .cloned()
        });
        let executed = executed?;
        if !executed.ok() {
            let stderr = executed.stderr.clone();
            let primary = anyhow::anyhow!("first user command failed: {}", stderr.trim());
            return Err(with_cleanup(primary, force_remove(context, &name)));
        }
        stages.insert(Stage::FirstUserCommand, exec_ms);

        // What a runner does after the last step and before teardown: read the
        // exit state back out of the daemon.
        let (completion, completion_ms) = timed(|| inspect_container_state(context, &name));
        if let Err(error) = completion {
            return Err(with_cleanup(error, force_remove(context, &name)));
        }
        stages.insert(Stage::CompletionOverhead, completion_ms);

        let (removed, teardown_ms) =
            timed(|| context.runner.run("docker", &["rm", "-f", &name]).cloned());
        let removed = removed?;
        if !removed.ok() {
            let stderr = removed.stderr.clone();
            let primary = anyhow::anyhow!("docker container teardown failed: {}", stderr.trim());
            return Err(with_cleanup(primary, force_remove(context, &name)));
        }
        stages.insert(Stage::Teardown, teardown_ms);
        Ok(())
    }

    fn run_image_pull(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
        resources: &mut Resources,
    ) -> Result<()> {
        let image = context.job_image.clone();
        remove_image_if_missing(context, &image).context("removing image for cold pull")?;

        let (pulled, pull_ms) = timed(|| context.runner.run("docker", &["pull", &image]).cloned());
        let pulled = pulled?;
        if !pulled.ok() {
            bail!("docker pull failed: {}", pulled.stderr.trim());
        }
        stages.insert(Stage::DockerSetup, pull_ms);

        if let Ok(size) = context.runner.capture(
            "docker",
            &["image", "inspect", &image, "--format", "{{.Size}}"],
        ) && let Ok(bytes) = size.parse::<u64>()
        {
            resources.bytes_downloaded = bytes;
        }
        self.run_container_lifecycle_after_pull(context, stages)
    }

    fn run_container_lifecycle_after_pull(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<()> {
        let name = self.container_name("pull");
        let image = context.job_image.clone();
        let (created, create_ms) = timed(|| {
            context
                .runner
                .run(
                    "docker",
                    &[
                        "create",
                        "--name",
                        &name,
                        "--entrypoint",
                        "/bin/sh",
                        &image,
                        "-c",
                        "sleep 30",
                    ],
                )
                .cloned()
        });
        let created = created?;
        if !created.ok() {
            let primary =
                anyhow::anyhow!("docker create after pull failed: {}", created.stderr.trim());
            return Err(with_cleanup(primary, force_remove(context, &name)));
        }
        stages.insert(Stage::ContainerCreate, create_ms);
        let (started, start_ms) =
            timed(|| context.runner.run("docker", &["start", &name]).cloned());
        let started = started?;
        if !started.ok() {
            let primary =
                anyhow::anyhow!("docker start after pull failed: {}", started.stderr.trim());
            return Err(with_cleanup(primary, force_remove(context, &name)));
        }
        stages.insert(Stage::ContainerStart, start_ms);
        let (executed, exec_ms) = timed(|| {
            context
                .runner
                .run("docker", &["exec", &name, "/bin/sh", "-c", USER_COMMAND])
                .cloned()
        });
        let executed = executed?;
        if !executed.ok() {
            let primary = anyhow::anyhow!(
                "first user command after pull failed: {}",
                executed.stderr.trim()
            );
            return Err(with_cleanup(primary, force_remove(context, &name)));
        }
        stages.insert(Stage::FirstUserCommand, exec_ms);
        let (completion, completion_ms) = timed(|| inspect_container_state(context, &name));
        if let Err(error) = completion {
            return Err(with_cleanup(error, force_remove(context, &name)));
        }
        stages.insert(Stage::CompletionOverhead, completion_ms);
        let (removed, teardown_ms) =
            timed(|| context.runner.run("docker", &["rm", "-f", &name]).cloned());
        let removed = removed?;
        if !removed.ok() {
            let primary = anyhow::anyhow!(
                "docker teardown after pull failed: {}",
                removed.stderr.trim()
            );
            return Err(with_cleanup(primary, force_remove(context, &name)));
        }
        stages.insert(Stage::Teardown, teardown_ms);
        Ok(())
    }

    fn run_service_container(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<()> {
        let network = self.container_name("net");
        let service = self.container_name("svc");
        let job = self.container_name("job");
        let image = context.job_image.clone();

        let (network_created, network_ms) = timed(|| {
            context
                .runner
                .run("docker", &["network", "create", &network])
                .cloned()
        });
        let network_created = network_created?;
        if !network_created.ok() {
            bail!(
                "docker network create failed: {}",
                network_created.stderr.trim()
            );
        }
        let (service_started, service_ms) = timed(|| {
            context
                .runner
                .run(
                    "docker",
                    &[
                        "run",
                        "-d",
                        "--name",
                        &service,
                        "--network",
                        &network,
                        SERVICE_IMAGE,
                    ],
                )
                .cloned()
        });
        let service_started = service_started?;
        if !service_started.ok() {
            let primary = anyhow::anyhow!(
                "docker service start failed: {}",
                service_started.stderr.trim()
            );
            return Err(with_cleanup(
                primary,
                cleanup_resources(context, &[&service], &[&network]),
            ));
        }
        let setup_ms = network_ms.saturating_add(service_ms);
        stages.insert(Stage::DockerSetup, setup_ms);

        // Readiness wait is part of container start from a job's perspective.
        let (ready, start_ms) = timed(|| wait_for_health(context, &service));
        stages.insert(Stage::ContainerStart, start_ms);
        if !ready {
            let primary = anyhow::anyhow!("service container never became reachable");
            return Err(with_cleanup(
                primary,
                cleanup_resources(context, &[&service], &[&network]),
            ));
        }

        let (created, create_ms) = timed(|| {
            context
                .runner
                .run(
                    "docker",
                    &[
                        "create",
                        "--name",
                        &job,
                        "--network",
                        &network,
                        "--entrypoint",
                        "/bin/sh",
                        &image,
                        "-c",
                        "sleep 30",
                    ],
                )
                .cloned()
        });
        let created = created?;
        if !created.ok() {
            let primary = anyhow::anyhow!(
                "docker service job create failed: {}",
                created.stderr.trim()
            );
            return Err(with_cleanup(
                primary,
                cleanup_resources(context, &[&job, &service], &[&network]),
            ));
        }
        stages.insert(Stage::ContainerCreate, create_ms);

        let (executed, exec_ms) = timed(|| -> Result<()> {
            let started = context.runner.run("docker", &["start", &job])?;
            require_success(started, "docker service job start")?;
            let executed = context
                .runner
                .run("docker", &["exec", &job, "/bin/sh", "-c", USER_COMMAND])?;
            require_success(executed, "docker service first user command")?;
            Ok(())
        });
        if let Err(error) = executed {
            return Err(with_cleanup(
                error,
                cleanup_resources(context, &[&job, &service], &[&network]),
            ));
        }
        stages.insert(Stage::FirstUserCommand, exec_ms);
        let (completion, completion_ms) = timed(|| inspect_container_state(context, &job));
        if let Err(error) = completion {
            return Err(with_cleanup(
                error,
                cleanup_resources(context, &[&job, &service], &[&network]),
            ));
        }
        stages.insert(Stage::CompletionOverhead, completion_ms);

        let (removed, teardown_ms) =
            timed(|| cleanup_resources(context, &[&job, &service], &[&network]));
        removed?;
        stages.insert(Stage::Teardown, teardown_ms);
        Ok(())
    }

    fn run_build(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
        resources: &mut Resources,
    ) -> Result<()> {
        let dir = context.work_root.join("docker-build-context");
        let (prepared, setup_ms) = timed(|| -> Result<()> {
            if self.kind == Kind::BuildUncached {
                // Defeat the cache at the first instruction, which is what an
                // uncached build actually means.
                std::fs::write(
                    dir.join("cache-buster"),
                    format!("{}-{}", std::process::id(), self.iteration),
                )?;
            }
            Ok(())
        });
        prepared?;
        stages.insert(Stage::DockerSetup, setup_ms);

        let path = dir.display().to_string();
        let tag = self
            .build_tag
            .clone()
            .expect("build workloads have an owned image tag");
        let args: Vec<String> = if self.kind == Kind::Buildx {
            vec![
                "buildx".to_owned(),
                "build".to_owned(),
                "--load".to_owned(),
                "-t".to_owned(),
                tag.clone(),
                path,
            ]
        } else {
            vec!["build".to_owned(), "-t".to_owned(), tag.clone(), path]
        };
        // A failed build may have created or retagged an image before it
        // reported the error. The tag is unique to this workload, so it is
        // safe for teardown to remove it even on that partial path.
        self.build_tag_needs_cleanup = true;
        let (built, build_ms) = timed(|| context.runner.run("docker", &args));
        let built = built?;
        if !built.ok() {
            bail!("docker build failed: {}", built.stderr.trim());
        }
        let (hits, misses) = count_layer_cache(&format!("{}\n{}", built.stdout, built.stderr));
        resources.cache_hits = hits;
        resources.cache_misses = misses;
        stages.insert(Stage::FirstUserCommand, build_ms);
        stages.insert(Stage::CompletionOverhead, 0);
        stages.insert(Stage::Teardown, 0);
        Ok(())
    }
}

/// Poll until the daemon reports the container running and its port answers.
fn wait_for_health(context: &mut Context, name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Ok(status) = context.runner.capture(
            "docker",
            &["inspect", "--format", "{{.State.Running}}", name],
        ) && status == "true"
            && let Ok(probe) = context
                .runner
                .run("docker", &["exec", name, "redis-cli", "ping"])
            && probe.ok()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// BuildKit prints `CACHED` for a reused step; every other step line is a miss.
fn finish_buildkit_step(
    steps: &mut BTreeMap<String, ()>,
    id: &str,
    cached: bool,
    hits: &mut u64,
    misses: &mut u64,
) {
    if steps.remove(id).is_some() {
        if cached {
            *hits += 1;
        } else {
            *misses += 1;
        }
    }
}

fn count_layer_cache(output: &str) -> (u64, u64) {
    let mut hits = 0_u64;
    let mut misses = 0_u64;
    let mut buildkit_steps = BTreeMap::<String, ()>::new();
    let mut classic_step_cached = None;

    for line in output.lines() {
        let trimmed = line.trim_start();
        // BuildKit progress lines identify real Dockerfile steps with a
        // numeric fraction (`[2/4]`). Internal/export steps also use brackets
        // but have no fraction and must not enter layer accounting.
        if let Some((id, detail)) = trimmed.split_once(' ')
            && id.starts_with('#')
            && detail
                .split_once(']')
                .is_some_and(|(header, _)| header.contains('[') && header.contains('/'))
        {
            buildkit_steps.insert(id.to_owned(), ());
        }
        if trimmed.starts_with('#') {
            let id = trimmed.split_whitespace().next().unwrap_or_default();
            if trimmed.contains("CACHED") {
                finish_buildkit_step(&mut buildkit_steps, id, true, &mut hits, &mut misses);
            } else if trimmed.contains("DONE") {
                finish_buildkit_step(&mut buildkit_steps, id, false, &mut hits, &mut misses);
            }
        }

        // Classic-builder output announces a step before printing either
        // `Using cache` or the completed layer. Finalize the previous step
        // only when the next step begins, then finalize the last one at EOF.
        if trimmed.starts_with("Step ") {
            if let Some(cached) = classic_step_cached.replace(false) {
                if cached {
                    hits += 1;
                } else {
                    misses += 1;
                }
            }
        } else if trimmed.contains("Using cache") && classic_step_cached.is_some() {
            classic_step_cached = Some(true);
        }
    }

    if let Some(cached) = classic_step_cached {
        if cached {
            hits += 1;
        } else {
            misses += 1;
        }
    }
    misses += buildkit_steps.len() as u64;
    (hits, misses)
}

/// Small, deterministic build context. It exists so the build scenarios measure
/// the builder rather than the size of some unrelated image.
fn write_build_context(dir: &Path, base_image: &str) -> Result<()> {
    std::fs::write(dir.join("cache-buster"), b"stable")?;
    std::fs::write(
        dir.join("Dockerfile"),
        format!(
            "FROM {base_image}\n\
             COPY cache-buster /velnor-bench/cache-buster\n\
             RUN printf layer-one > /velnor-bench/one\n\
             RUN printf layer-two > /velnor-bench/two\n"
        ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_docker_scenario_with_a_fallback_has_a_workload() {
        for scenario in crate::scenario::MATRIX {
            if scenario.family == crate::scenario::Family::Docker
                && scenario.fallback == Some(crate::scenario::Driver::DockerDirect)
            {
                assert!(build(scenario).is_ok(), "{} has no workload", scenario.id);
            }
        }
    }

    #[test]
    fn a_scenario_without_a_docker_workload_is_refused_not_faked() {
        let scenario = crate::scenario::find("rust/cold").expect("scenario");
        let error = build(scenario).map(|_| ()).expect_err("must refuse");
        assert!(error.to_string().contains("reported as unrun"), "{error}");
    }

    #[test]
    fn cache_lines_are_counted_from_real_builder_output() {
        let buildkit = "#4 [1/3] FROM docker.io/library/alpine\n\
                        #5 [2/3] COPY cache-buster /velnor-bench/cache-buster\n\
                        #5 CACHED\n\
                        #6 [3/3] RUN printf layer-one > /velnor-bench/one\n";
        assert_eq!(count_layer_cache(buildkit), (1, 2));
        let classic = "Step 1/3 : FROM alpine\nStep 2/3 : COPY x /x\n ---> Using cache\n";
        assert_eq!(count_layer_cache(classic), (1, 1));
        let terminal = "#1 [1/2] FROM alpine\n#1 DONE 0.1s\n#2 [2/2] RUN printf ok\n#2 CACHED\n";
        assert_eq!(count_layer_cache(terminal), (1, 1));
        assert_eq!(count_layer_cache(""), (0, 0));
    }

    #[test]
    fn the_build_context_is_written_deterministically() {
        let dir = std::env::temp_dir().join(format!("velnor-bench-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create");
        write_build_context(&dir, "alpine:3").expect("write");
        let dockerfile = std::fs::read_to_string(dir.join("Dockerfile")).expect("read");
        assert!(dockerfile.starts_with("FROM alpine:3\n"));
        assert!(dockerfile.contains("COPY cache-buster"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timed_reports_a_duration() {
        let (value, elapsed) = timed(|| 7_u32);
        assert_eq!(value, 7);
        assert!(elapsed < 10_000);
    }

    #[test]
    fn failed_docker_invocations_are_not_accepted_as_measurements() {
        let invocation = Invocation {
            program: "docker".into(),
            args: vec!["exec".into()],
            code: 17,
            stdout: String::new(),
            stderr: "container failed".into(),
            wall: Duration::ZERO,
        };
        let error = require_success(&invocation, "docker exec").expect_err("must fail closed");
        assert!(error.to_string().contains("exit code 17"));
        assert!(error.to_string().contains("container failed"));
    }

    #[test]
    fn build_tags_are_unique_to_the_workload_owner() {
        let first = unique_build_tag();
        let second = unique_build_tag();
        assert_ne!(first, second);
        assert!(first.starts_with("velnor-bench-cached-"));
        assert!(first.ends_with(":latest"));
    }

    #[test]
    fn cleanup_error_keeps_the_primary_failure() {
        let error = with_cleanup(
            anyhow::anyhow!("primary workload failure"),
            Err(anyhow::anyhow!("container removal failed")),
        );
        let rendered = format!("{error:#}");
        assert!(rendered.contains("primary workload failure"));
        assert!(rendered.contains("container removal failed"));
    }
}

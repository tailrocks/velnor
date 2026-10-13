//! Real container workloads driven through the Docker CLI.
//!
//! This driver observes the container lifecycle and nothing else. It never
//! reports broker, acquisition, admission or capacity latency, because it never
//! talks to a broker: [`crate::record::BenchRecord::validate`] enforces that
//! separation on the record it produces.

use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{bail, Context as _, Result};

use crate::{
    drivers::{Context, Workload},
    gittrace::GitCounters,
    record::{Observation, Resources},
    scenario::Scenario,
    stage::Stage,
    sys::{tree_bytes, Rusage},
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
        iteration: 0,
        notes: Vec::new(),
    }))
}

struct DockerWorkload {
    kind: Kind,
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

impl DockerWorkload {
    fn container_name(&self, prefix: &str) -> String {
        format!(
            "velnor-bench-{prefix}-{}-{}",
            std::process::id(),
            self.iteration
        )
    }

    /// Remove a container, ignoring "no such container".
    fn force_remove(context: &mut Context, name: &str) {
        let _ = context.runner.run("docker", &["rm", "-f", name]);
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
                context
                    .runner
                    .run("docker", &["pull", &image])
                    .context("pulling the service image")?;
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
                    context
                        .runner
                        .run("docker", &["pull", &image])
                        .context("pulling the job image")?;
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
                let _ = context.runner.run(
                    "docker",
                    &[
                        "build",
                        "-t",
                        "velnor-bench-cached:latest",
                        &dir.display().to_string(),
                    ],
                );
            }
        }
        Ok(())
    }

    fn iterate(&mut self, context: &mut Context) -> Result<Observation> {
        self.iteration += 1;
        context.runner.reset();
        let before_usage = Rusage::children();
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
        let usage = Rusage::children().since(before_usage);
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
        if matches!(
            self.kind,
            Kind::BuildCached | Kind::BuildUncached | Kind::Buildx
        ) {
            let _ = context.runner.run(
                "docker",
                &["image", "rm", "-f", "velnor-bench-cached:latest"],
            );
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

        let (created, create_ms) = timed(|| context.runner.run("docker", &create_args));
        let created = created?;
        if !created.ok() {
            let stderr = created.stderr.clone();
            Self::force_remove(context, &name);
            bail!("docker create failed: {}", stderr.trim());
        }
        stages.insert(Stage::ContainerCreate, create_ms);

        let (started, start_ms) = timed(|| context.runner.run("docker", &["start", &name]));
        let started = started?;
        if !started.ok() {
            let stderr = started.stderr.clone();
            Self::force_remove(context, &name);
            bail!("docker start failed: {}", stderr.trim());
        }
        stages.insert(Stage::ContainerStart, start_ms);

        let (executed, exec_ms) = timed(|| {
            context
                .runner
                .run("docker", &["exec", &name, "/bin/sh", "-c", USER_COMMAND])
        });
        let executed = executed?;
        if !executed.ok() {
            let stderr = executed.stderr.clone();
            Self::force_remove(context, &name);
            bail!("first user command failed: {}", stderr.trim());
        }
        stages.insert(Stage::FirstUserCommand, exec_ms);

        // What a runner does after the last step and before teardown: read the
        // exit state back out of the daemon.
        let (_, completion_ms) = timed(|| {
            context.runner.run(
                "docker",
                &["inspect", "--format", "{{.State.Status}}", &name],
            )
        });
        stages.insert(Stage::CompletionOverhead, completion_ms);

        let (_, teardown_ms) = timed(|| context.runner.run("docker", &["rm", "-f", &name]));
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
        let _ = context.runner.run("docker", &["image", "rm", "-f", &image]);

        let (pulled, pull_ms) = timed(|| context.runner.run("docker", &["pull", &image]));
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
            context.runner.run(
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
        });
        created?;
        stages.insert(Stage::ContainerCreate, create_ms);
        let (_, start_ms) = timed(|| context.runner.run("docker", &["start", &name]));
        stages.insert(Stage::ContainerStart, start_ms);
        let (_, exec_ms) = timed(|| {
            context
                .runner
                .run("docker", &["exec", &name, "/bin/sh", "-c", USER_COMMAND])
        });
        stages.insert(Stage::FirstUserCommand, exec_ms);
        stages.insert(Stage::CompletionOverhead, 0);
        let (_, teardown_ms) = timed(|| context.runner.run("docker", &["rm", "-f", &name]));
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

        let (_, setup_ms) = timed(|| {
            let _ = context
                .runner
                .run("docker", &["network", "create", &network]);
            context.runner.run(
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
        });
        stages.insert(Stage::DockerSetup, setup_ms);

        // Readiness wait is part of container start from a job's perspective.
        let (ready, start_ms) = timed(|| wait_for_health(context, &service));
        stages.insert(Stage::ContainerStart, start_ms);
        if !ready {
            let _ = context.runner.run("docker", &["rm", "-f", &service]);
            let _ = context.runner.run("docker", &["network", "rm", &network]);
            bail!("service container never became reachable");
        }

        let (created, create_ms) = timed(|| {
            context.runner.run(
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
        });
        created?;
        stages.insert(Stage::ContainerCreate, create_ms);

        let (_, exec_ms) = timed(|| {
            let _ = context.runner.run("docker", &["start", &job]);
            context
                .runner
                .run("docker", &["exec", &job, "/bin/sh", "-c", USER_COMMAND])
        });
        stages.insert(Stage::FirstUserCommand, exec_ms);
        stages.insert(Stage::CompletionOverhead, 0);

        let (_, teardown_ms) = timed(|| {
            let _ = context.runner.run("docker", &["rm", "-f", &job]);
            let _ = context.runner.run("docker", &["rm", "-f", &service]);
            context.runner.run("docker", &["network", "rm", &network])
        });
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
        let tag = "velnor-bench-cached:latest".to_owned();
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
fn count_layer_cache(output: &str) -> (u64, u64) {
    let mut hits = 0;
    let mut misses = 0;
    for line in output.lines() {
        let trimmed = line.trim_start();
        // Step lines look like `#5 [2/4] RUN ...` or, in the classic builder,
        // `Step 2/4 : RUN ...`.
        let is_step = trimmed.starts_with("Step ")
            || (trimmed.starts_with('#') && trimmed.contains(']') && trimmed.contains('['));
        if trimmed.contains("CACHED") {
            hits += 1;
        } else if is_step {
            misses += 1;
        }
    }
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
        assert_eq!(count_layer_cache(buildkit), (1, 3));
        let classic = "Step 1/3 : FROM alpine\nStep 2/3 : COPY x /x\n ---> Using cache\n";
        assert_eq!(count_layer_cache(classic), (0, 2));
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
}

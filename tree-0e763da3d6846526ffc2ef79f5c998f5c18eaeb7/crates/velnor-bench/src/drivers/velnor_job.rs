//! Local Velnor job dispatch: a real job lifecycle with no broker.
//!
//! This is the `velnor-job` driver for rows whose lifecycle is measurable
//! without remote dispatch. Each iteration dispatches a real job through
//! the runner's own admission and capacity primitives and a real container
//! lifecycle on the host Engine, and times every stage it really performs:
//!
//! * [`Stage::Ready`] — the iteration workspace is created and verified;
//! * [`Stage::Admission`] — the runner's real admission decision,
//!   [`HostCapacity::probe`] folded into the [`DiskPressure`] state machine,
//!   exactly the question the daemon asks before acquiring a job. A full
//!   disk refuses the iteration; it is never rounded up to admitted.
//! * [`Stage::Capacity`] — the real capacity reservation: the daemon must
//!   answer `docker info`, and the [`HostCapacity::probe_with_docker`]
//!   promisable bytes must cover the admission floor after Docker's growth
//!   allowance. A down daemon or a full disk fails the iteration.
//! * [`Stage::CheckoutStart`] — the iteration workspace is materialized;
//! * [`Stage::DockerSetup`] through [`Stage::Teardown`] — a real job-shaped
//!   container lifecycle: image inspect, per-job network, create, start,
//!   first user command, completion inspect, removal.
//!
//! Three stages stay unobserved by construction, and the record notes name
//! them: [`Stage::BrokerDelivery`] and [`Stage::AcquiredPayload`] (local
//! dispatch has no broker) and [`Stage::Checkout`] (no git remote is
//! touched). They are absent from `stages_ms`, never zero-filled: a zero
//! would claim the broker answered instantly, which no local run observes.
//! Rows that need those stages keep requiring remote dispatch and stay
//! unrun until it exists.
//!
//! [`HostCapacity::probe`]: velnor_runner::host_capacity::HostCapacity::probe
//! [`HostCapacity::probe_with_docker`]: velnor_runner::host_capacity::HostCapacity::probe_with_docker
//! [`DiskPressure`]: velnor_runner::host_capacity::DiskPressure

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context as _, Result};
use velnor_runner::host_capacity::{
    DiskAction, DiskPolicy, DiskPressure, HostCapacity, DEFAULT_MIN_FREE_BYTES,
};

use crate::{
    drivers::{Context, Workload},
    gittrace::GitEvidence,
    record::{Observation, Resources},
    scenario::Scenario,
    stage::Stage,
    sys::{tree_bytes, Invocation},
};

/// Shape of the local job workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// One job lifecycle with the full local stage breakdown.
    StageBreakdown,
    /// `context.concurrency` job lifecycles whose containers overlap, while
    /// the benchmark's Docker control-plane phases remain serialized.
    ConcurrentSlots,
    /// The same workload as a trusted and an untrusted job, each in its
    /// own trust-scoped partition mounting only that class's state; stages
    /// carry the per-stage slower of the pair, not a claim about remote
    /// trust admission or cache isolation.
    TrustPartition,
    /// The Nth job on a retaining host: `warmups` unmeasured jobs run in
    /// `prepare`, each retaining its workspace and one stopped container,
    /// then the measured one.
    PersistentHost { warmups: usize },
    /// Warmups retain state, a scoped owned-object GC removes exactly that
    /// retained state in `prepare`, then the measured first job after it.
    AfterGc,
}

/// The command a job-shaped container runs as its first user step.
const USER_COMMAND: &str = "printf velnor-job-first-user-command";

/// Docker growth the capacity reservation holds back for one trivial bench
/// job: its container layer plus churn headroom. A property of this
/// workload, stated here and on the record — not a claim about Velnor jobs.
const DOCKER_GROWTH_ALLOWANCE_BYTES: u64 = 512 * 1024 * 1024;

static NEXT_OWNER_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn build(scenario: &Scenario) -> Result<Box<dyn Workload>> {
    let kind = match scenario.id {
        "lifecycle/stage-breakdown" => Kind::StageBreakdown,
        "lifecycle/concurrent-slots" => Kind::ConcurrentSlots,
        "lifecycle/trust-partition" => Kind::TrustPartition,
        "persistent-host/job-1" => Kind::PersistentHost { warmups: 0 },
        "persistent-host/job-2" => Kind::PersistentHost { warmups: 1 },
        "persistent-host/job-10" => Kind::PersistentHost { warmups: 9 },
        "persistent-host/job-100" => Kind::PersistentHost { warmups: 99 },
        "persistent-host/after-gc" => Kind::AfterGc,
        // Rust, Docker and Fault rows need remote dispatch: a broker to
        // deliver the job and, for Rust, the runner's acceleration stores.
        // Local containers cannot establish those, so these stay unrun.
        other => bail!(
            "{other}: the local velnor-job driver measures the job lifecycle, not remote dispatch; \
             this row needs a registered runner and dispatch credentials, and this harness will not simulate one"
        ),
    };
    Ok(Box::new(VelnorJobWorkload {
        kind,
        scenario: scenario.id,
        scratch: None,
        owner: NEXT_OWNER_ID.fetch_add(1, Ordering::Relaxed),
        nonce: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos()),
        job_image: None,
        iteration: 0,
        admission: DiskPressure::new(DiskPolicy::default()),
        owned_containers: Vec::new(),
        owned_networks: Vec::new(),
        owned_images: Vec::new(),
        notes: vec![
            "velnor-job local driver: real admission, capacity and container lifecycle on this host; \
             broker-delivery, acquired-payload and checkout are unobserved (no broker, no git remote)"
                .to_owned(),
        ],
    }))
}

/// Wall time of one measured block, in milliseconds.
fn timed<T>(body: impl FnOnce() -> T) -> (T, u64) {
    let started = Instant::now();
    let value = body();
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    (value, elapsed)
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
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

/// A Docker ID is a single lowercase hex token. Anything else is a daemon
/// error smuggled through stdout, and must never authorize deletion.
fn parse_docker_id(stdout: &str) -> Result<String> {
    let id = stdout.trim();
    if id.len() < 12
        || id.len() > 64
        || id.split_whitespace().count() != 1
        || !id.chars().all(|character| character.is_ascii_hexdigit())
    {
        bail!("Docker returned an invalid ID: {id:?}");
    }
    Ok(id.to_owned())
}

fn parse_owned_image_inspect(output: &str, expected_owner: &str) -> Result<String> {
    let mut fields = output.split_whitespace();
    let raw_id = fields
        .next()
        .context("Docker image inspect returned no image ID")?;
    let owner = fields
        .next()
        .context("Docker image inspect returned no owner label")?;
    if owner != expected_owner {
        bail!("Docker image owner label mismatch: expected {expected_owner:?}, got {owner:?}");
    }
    if fields.next().is_some() {
        bail!("Docker image inspect returned extra fields");
    }
    let id = raw_id.strip_prefix("sha256:").unwrap_or(raw_id);
    Ok(format!("sha256:{}", parse_docker_id(id)?))
}

fn remove_owned_image(
    context: &mut Context,
    tag: &str,
    expected_id: Option<&str>,
    expected_owner: &str,
) -> Result<()> {
    let inspected = context
        .runner
        .run(
            "docker",
            &[
                "image",
                "inspect",
                "--format",
                "{{.Id}} {{index .Config.Labels \"com.velnor.bench.owner\"}}",
                tag,
            ],
        )?
        .clone();
    if !inspected.ok() {
        if inspected
            .stderr
            .to_ascii_lowercase()
            .contains("no such image")
        {
            return Ok(());
        }
        bail!(
            "docker owned image inspection failed with exit code {}: {}",
            inspected.code,
            inspected.stderr.trim()
        );
    }
    let id = parse_owned_image_inspect(&inspected.stdout, expected_owner)?;
    if let Some(expected_id) = expected_id
        && id != expected_id
    {
        bail!("Docker image identity mismatch: expected {expected_id:?}, got {id:?}");
    }
    // Remove by the verified immutable ID. A tag can be retargeted after the
    // inspection; deleting the ID cannot remove the retargeted image.
    let removed = context.runner.run("docker", &["image", "rm", &id])?.clone();
    if removed.ok()
        || removed
            .stderr
            .to_ascii_lowercase()
            .contains("no such image")
    {
        return Ok(());
    }
    bail!(
        "docker owned image removal failed with exit code {}: {}",
        removed.code,
        removed.stderr.trim()
    )
}

fn with_cleanup(primary: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup_error) => {
            primary.context(format!("workload cleanup also failed: {cleanup_error:#}"))
        }
    }
}

#[derive(Debug, Clone)]
struct OwnedObject {
    name: String,
    id: Option<String>,
}

struct VelnorJobWorkload {
    kind: Kind,
    scenario: &'static str,
    scratch: Option<PathBuf>,
    owner: u64,
    nonce: u128,
    job_image: Option<String>,
    iteration: u64,
    admission: DiskPressure,
    owned_containers: Vec<OwnedObject>,
    owned_networks: Vec<OwnedObject>,
    owned_images: Vec<OwnedObject>,
    notes: Vec<String>,
}

impl VelnorJobWorkload {
    fn scratch(&self) -> Result<&PathBuf> {
        self.scratch
            .as_ref()
            .context("velnor-job workload was not prepared")
    }

    fn owner_token(&self) -> String {
        format!("{:032x}-{}", self.nonce, self.owner)
    }

    fn object_name(&self, role: &str) -> String {
        format!(
            "velnor-job-{role}-{}-{:032x}-{}-{}",
            std::process::id(),
            self.nonce,
            self.owner,
            self.iteration,
        )
    }

    fn gc_image_tag(&self) -> String {
        format!(
            "velnor-job-gc-{}-{:032x}-{}:latest",
            std::process::id(),
            self.nonce,
            self.owner
        )
    }

    fn own_container(&mut self, name: String) {
        if !self.owned_containers.iter().any(|owned| owned.name == name) {
            self.owned_containers.push(OwnedObject { name, id: None });
        }
    }

    fn own_network(&mut self, name: String) {
        if !self.owned_networks.iter().any(|owned| owned.name == name) {
            self.owned_networks.push(OwnedObject { name, id: None });
        }
    }

    fn own_image(&mut self, name: String) {
        if !self.owned_images.iter().any(|owned| owned.name == name) {
            self.owned_images.push(OwnedObject { name, id: None });
        }
    }

    fn record_id(objects: &mut [OwnedObject], name: &str, id: String) -> Result<()> {
        let owned = objects
            .iter_mut()
            .find(|owned| owned.name == name)
            .with_context(|| format!("{name} was not registered before creation"))?;
        owned.id = Some(id);
        Ok(())
    }

    fn capture_owned_image(&mut self, context: &mut Context, tag: &str) -> Result<()> {
        let inspected = context
            .runner
            .run(
                "docker",
                &[
                    "image",
                    "inspect",
                    "--format",
                    "{{.Id}} {{index .Config.Labels \"com.velnor.bench.owner\"}}",
                    tag,
                ],
            )?
            .clone();
        require_success(&inspected, "docker image inspect owned image")?;
        let id = parse_owned_image_inspect(&inspected.stdout, &self.owner_token())?;
        Self::record_id(&mut self.owned_images, tag, id)
    }

    fn remove_owned_image_by_tag(&mut self, context: &mut Context, tag: &str) -> Result<()> {
        let expected_id = self
            .owned_images
            .iter()
            .find(|owned| owned.name == tag)
            .and_then(|owned| owned.id.as_deref());
        let owner = self.owner_token();
        remove_owned_image(context, tag, expected_id, &owner)?;
        self.owned_images.retain(|owned| owned.name != tag);
        Ok(())
    }

    /// Remove everything still owned. Objects are removed by verified ID;
    /// an object whose ID was never captured is removed by its unique name,
    /// which carries pid plus nonce and cannot collide with a recycler.
    /// Survivors are kept, so teardown retries them instead of leaking.
    fn cleanup_owned(&mut self, context: &mut Context) -> Result<()> {
        let mut failures = Vec::new();
        for owned in std::mem::take(&mut self.owned_containers) {
            let target = owned.id.as_deref().unwrap_or(&owned.name);
            match context.runner.run("docker", &["rm", "-f", target]) {
                Ok(invocation)
                    if invocation.ok()
                        || invocation
                            .stderr
                            .to_ascii_lowercase()
                            .contains("no such container") => {}
                Ok(invocation) => {
                    failures.push(format!(
                        "container {} (exit {}): {}",
                        owned.name,
                        invocation.code,
                        invocation.stderr.trim()
                    ));
                    self.owned_containers.push(owned);
                }
                Err(error) => {
                    failures.push(format!("container {}: {error:#}", owned.name));
                    self.owned_containers.push(owned);
                }
            }
        }
        for owned in std::mem::take(&mut self.owned_networks) {
            let target = owned.id.as_deref().unwrap_or(&owned.name);
            match context.runner.run("docker", &["network", "rm", target]) {
                Ok(invocation)
                    if invocation.ok()
                        || invocation
                            .stderr
                            .to_ascii_lowercase()
                            .contains("no such network") => {}
                Ok(invocation) => {
                    failures.push(format!(
                        "network {} (exit {}): {}",
                        owned.name,
                        invocation.code,
                        invocation.stderr.trim()
                    ));
                    self.owned_networks.push(owned);
                }
                Err(error) => {
                    failures.push(format!("network {}: {error:#}", owned.name));
                    self.owned_networks.push(owned);
                }
            }
        }
        for owned in std::mem::take(&mut self.owned_images) {
            let expected_id = owned.id.as_deref();
            if let Err(error) =
                remove_owned_image(context, &owned.name, expected_id, &self.owner_token())
            {
                failures.push(format!("image {}: {error:#}", owned.name));
                self.owned_images.push(owned);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("owned object cleanup failed: {}", failures.join("; "))
        }
    }

    /// Ready: the slot announces it can take work by creating and verifying
    /// the iteration workspace. Admission: the runner's real disk decision.
    /// Capacity: the daemon must answer, and the promisable bytes must cover
    /// the admission floor after Docker's growth allowance. CheckoutStart:
    /// the workspace is materialized for the container below.
    fn preamble(
        &mut self,
        context: &mut Context,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<PathBuf> {
        let scratch = self.scratch()?.clone();
        let workspace = scratch.join(format!("iteration-{}", self.iteration));

        let (ready, ready_ms) = timed(|| -> Result<()> {
            std::fs::create_dir_all(&workspace)?;
            let marker = workspace.join("ready");
            std::fs::write(&marker, self.scenario)?;
            let back = std::fs::read_to_string(&marker)?;
            if back != self.scenario {
                bail!("iteration workspace did not round-trip its ready marker");
            }
            Ok(())
        });
        ready?;
        stages.insert(Stage::Ready, ready_ms);

        let (admission, admission_ms) = timed(|| -> Result<()> {
            let capacity = HostCapacity::probe(&scratch)?;
            match self
                .admission
                .observe(capacity.available_bytes, unix_secs())
            {
                DiskAction::Admit => Ok(()),
                action => bail!(
                    "local admission refused ({action:?}): {} bytes available",
                    capacity.available_bytes
                ),
            }
        });
        admission?;
        stages.insert(Stage::Admission, admission_ms);

        let (capacity, capacity_ms) = timed(|| -> Result<()> {
            let info = context
                .runner
                .run("docker", &["info", "--format", "{{.ServerVersion}}"])?
                .clone();
            require_success(&info, "docker info")?;
            if info.stdout.trim().is_empty() {
                bail!("docker info returned no server version");
            }
            let df = context
                .runner
                .run("docker", &["system", "df", "--format", "{{json .}}"])?
                .clone();
            require_success(&df, "docker system df")?;
            let docker_bytes = velnor_runner::host_capacity::docker_usage_bytes_from_df(&df.stdout);
            let reserved = HostCapacity::probe_with_docker(&scratch, docker_bytes)?
                .promisable_bytes(DOCKER_GROWTH_ALLOWANCE_BYTES);
            if reserved < DEFAULT_MIN_FREE_BYTES {
                bail!("local capacity refused: {reserved} promisable bytes cannot cover the floor");
            }
            Ok(())
        });
        capacity?;
        stages.insert(Stage::Capacity, capacity_ms);

        let (checkout, checkout_ms) = timed(|| -> Result<()> {
            // The local equivalent of checking the tree out: the workload
            // files the container will mount. No git remote is touched, so
            // `Checkout` itself stays unobserved.
            for name in ["job.sh", "payload.txt"] {
                std::fs::write(
                    workspace.join(name),
                    format!("velnor-job {name} for {}\n", self.scenario),
                )?;
            }
            Ok(())
        });
        checkout?;
        stages.insert(Stage::CheckoutStart, checkout_ms);

        Ok(workspace)
    }

    /// One job-shaped container lifecycle: image inspect and per-job network
    /// ([`Stage::DockerSetup`]), create, start, first user command,
    /// completion inspect, and removal of the container and its network
    /// ([`Stage::Teardown`]). `trust` labels the job's trust class.
    fn job_lifecycle(
        &mut self,
        context: &mut Context,
        workspace: &Path,
        trust: &str,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<()> {
        let image = self
            .job_image
            .clone()
            .context("job image was not resolved during preparation")?;
        let owner = format!("com.velnor.bench.owner={}", self.owner_token());
        let role = "com.velnor.bench.role=job".to_owned();
        let trust_label = format!("com.velnor.bench.trust={trust}");

        let network = format!("{}-{}", self.object_name("net"), trust);
        self.own_network(network.clone());
        let (setup, setup_ms) = timed(|| -> Result<String> {
            let inspect = context
                .runner
                .run(
                    "docker",
                    &["image", "inspect", "--format", "{{.Id}}", &image],
                )?
                .clone();
            require_success(&inspect, "docker image inspect")?;
            let created = context
                .runner
                .run(
                    "docker",
                    &[
                        "network",
                        "create",
                        "--label",
                        &owner,
                        "--label",
                        &role,
                        "--label",
                        &trust_label,
                        &network,
                    ],
                )?
                .clone();
            require_success(&created, "docker network create")?;
            parse_docker_id(&created.stdout)
        });
        let network_id = match setup {
            Ok(id) => id,
            Err(error) => return Err(with_cleanup(error, self.cleanup_owned(context))),
        };
        if let Err(error) = Self::record_id(&mut self.owned_networks, &network, network_id.clone())
        {
            return Err(with_cleanup(error, self.cleanup_owned(context)));
        }
        stages.insert(Stage::DockerSetup, setup_ms);

        let name = format!("{}-{trust}", self.object_name("job"));
        self.own_container(name.clone());
        let mount = format!("{}:/velnor/workspace", workspace.display());
        let (created, create_ms) = timed(|| {
            context
                .runner
                .run(
                    "docker",
                    &[
                        "create",
                        "--name",
                        &name,
                        "--network",
                        &network_id,
                        "--entrypoint",
                        "/bin/sh",
                        "--label",
                        &owner,
                        "--label",
                        &role,
                        "--label",
                        &trust_label,
                        "-v",
                        &mount,
                        &image,
                        "-c",
                        "sleep 30",
                    ],
                )
                .cloned()
        });
        let created = match created {
            Ok(created) => created,
            Err(error) => {
                return Err(with_cleanup(error.into(), self.cleanup_owned(context)));
            }
        };
        if !created.ok() {
            let primary = anyhow::anyhow!("docker create failed: {}", created.stderr.trim());
            return Err(with_cleanup(primary, self.cleanup_owned(context)));
        }
        let id = match parse_docker_id(&created.stdout)
            .context("docker create returned no verified ID")
            .and_then(|id| {
                Self::record_id(&mut self.owned_containers, &name, id.clone())?;
                Ok(id)
            }) {
            Ok(id) => id,
            Err(error) => return Err(with_cleanup(error, self.cleanup_owned(context))),
        };
        stages.insert(Stage::ContainerCreate, create_ms);

        let run = |context: &mut Context, args: &[&str]| -> Result<Invocation> {
            Ok(context.runner.run("docker", args)?.clone())
        };
        let (started, start_ms) = timed(|| run(context, &["start", &id]));
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                return Err(with_cleanup(error, self.cleanup_owned(context)));
            }
        };
        if !started.ok() {
            let primary = anyhow::anyhow!("docker start failed: {}", started.stderr.trim());
            return Err(with_cleanup(primary, self.cleanup_owned(context)));
        }
        stages.insert(Stage::ContainerStart, start_ms);

        let (executed, exec_ms) =
            timed(|| run(context, &["exec", &id, "/bin/sh", "-c", USER_COMMAND]));
        let executed = match executed {
            Ok(executed) => executed,
            Err(error) => {
                return Err(with_cleanup(error, self.cleanup_owned(context)));
            }
        };
        if !executed.ok() {
            let primary = anyhow::anyhow!("first user command failed: {}", executed.stderr.trim());
            return Err(with_cleanup(primary, self.cleanup_owned(context)));
        }
        stages.insert(Stage::FirstUserCommand, exec_ms);

        let (completion, completion_ms) = timed(|| -> Result<()> {
            let inspect = run(context, &["inspect", "--format", "{{.State.Status}}", &id])?;
            require_success(&inspect, "docker inspect completion")?;
            if inspect.stdout.trim().is_empty() {
                bail!("docker inspect completion returned no container state");
            }
            Ok(())
        });
        if let Err(error) = completion {
            return Err(with_cleanup(error, self.cleanup_owned(context)));
        }
        stages.insert(Stage::CompletionOverhead, completion_ms);

        let (removed, teardown_ms) = timed(|| -> Result<()> {
            let removed = run(context, &["rm", "-f", &id])?;
            require_success(&removed, "docker rm")?;
            self.owned_containers.retain(|owned| owned.name != name);
            let network_removed = run(context, &["network", "rm", &network_id])?;
            require_success(&network_removed, "docker network rm")?;
            self.owned_networks.retain(|owned| owned.name != network);
            Ok(())
        });
        if let Err(error) = removed {
            return Err(with_cleanup(error, self.cleanup_owned(context)));
        }
        stages.insert(Stage::Teardown, teardown_ms);
        Ok(())
    }

    /// `concurrency` job lifecycles with overlapping container lifetimes:
    /// create all, start all, run all, inspect all, remove all. The harness
    /// drives Docker from one thread, so control-plane syscalls serialize;
    /// this measures a shared daemon with live sibling containers, not true
    /// concurrent dispatch. Stages are phase-window walls: the batch's
    /// critical path.
    fn concurrent_batch(
        &mut self,
        context: &mut Context,
        workspace: &Path,
        jobs: usize,
        stages: &mut BTreeMap<Stage, u64>,
    ) -> Result<()> {
        let image = self
            .job_image
            .clone()
            .context("job image was not resolved during preparation")?;
        let owner = format!("com.velnor.bench.owner={}", self.owner_token());
        let role = "com.velnor.bench.role=job".to_owned();
        let mount = format!("{}:/velnor/workspace", workspace.display());

        let (setup, setup_ms) = timed(|| -> Result<Vec<(String, String)>> {
            let inspect = context
                .runner
                .run(
                    "docker",
                    &["image", "inspect", "--format", "{{.Id}}", &image],
                )?
                .clone();
            require_success(&inspect, "docker image inspect")?;
            let mut networks = Vec::with_capacity(jobs);
            for index in 0..jobs {
                let network = format!("{}-slot-{index}", self.object_name("net"));
                self.own_network(network.clone());
                let created = context
                    .runner
                    .run(
                        "docker",
                        &[
                            "network", "create", "--label", &owner, "--label", &role, &network,
                        ],
                    )?
                    .clone();
                require_success(&created, "docker network create")?;
                let id = parse_docker_id(&created.stdout)?;
                Self::record_id(&mut self.owned_networks, &network, id.clone())?;
                networks.push((network, id));
            }
            Ok(networks)
        });
        let networks = match setup {
            Ok(networks) => networks,
            Err(error) => return Err(with_cleanup(error, self.cleanup_owned(context))),
        };
        stages.insert(Stage::DockerSetup, setup_ms);

        let (created, create_ms) = timed(|| -> Result<Vec<(String, String)>> {
            let mut containers = Vec::with_capacity(jobs);
            for (index, (_, network_id)) in networks.iter().enumerate() {
                let name = format!("{}-slot-{index}", self.object_name("job"));
                self.own_container(name.clone());
                let created = context
                    .runner
                    .run(
                        "docker",
                        &[
                            "create",
                            "--name",
                            &name,
                            "--network",
                            network_id,
                            "--entrypoint",
                            "/bin/sh",
                            "--label",
                            &owner,
                            "--label",
                            &role,
                            "-v",
                            &mount,
                            &image,
                            "-c",
                            "sleep 30",
                        ],
                    )?
                    .clone();
                require_success(&created, "docker create")?;
                let id = parse_docker_id(&created.stdout)?;
                Self::record_id(&mut self.owned_containers, &name, id.clone())?;
                containers.push((name, id));
            }
            Ok(containers)
        });
        let containers = match created {
            Ok(containers) => containers,
            Err(error) => return Err(with_cleanup(error, self.cleanup_owned(context))),
        };
        stages.insert(Stage::ContainerCreate, create_ms);

        // One serialized phase at a time: every container starts, then every
        // container runs its first command, then every completion is read.
        // Container lifetimes overlap across the whole batch; API calls do not.
        let phase = |context: &mut Context,
                     containers: &[(String, String)],
                     operation: &str,
                     args: &dyn Fn(&str) -> Vec<String>|
         -> Result<()> {
            for (_, id) in containers {
                let invocation = context.runner.run("docker", &args(id))?.clone();
                require_success(&invocation, operation)?;
            }
            Ok(())
        };
        let (started, start_ms) = timed(|| {
            phase(context, &containers, "docker start", &|id| {
                vec!["start".to_owned(), id.to_owned()]
            })
        });
        if let Err(error) = started {
            return Err(with_cleanup(error, self.cleanup_owned(context)));
        }
        stages.insert(Stage::ContainerStart, start_ms);

        let (executed, exec_ms) = timed(|| {
            phase(context, &containers, "first user command", &|id| {
                vec![
                    "exec".to_owned(),
                    id.to_owned(),
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    USER_COMMAND.to_owned(),
                ]
            })
        });
        if let Err(error) = executed {
            return Err(with_cleanup(error, self.cleanup_owned(context)));
        }
        stages.insert(Stage::FirstUserCommand, exec_ms);

        let (completion, completion_ms) = timed(|| {
            phase(context, &containers, "docker inspect completion", &|id| {
                vec![
                    "inspect".to_owned(),
                    "--format".to_owned(),
                    "{{.State.Status}}".to_owned(),
                    id.to_owned(),
                ]
            })
        });
        if let Err(error) = completion {
            return Err(with_cleanup(error, self.cleanup_owned(context)));
        }
        stages.insert(Stage::CompletionOverhead, completion_ms);

        let (removed, teardown_ms) = timed(|| -> Result<()> {
            for (name, id) in &containers {
                let removed = context.runner.run("docker", &["rm", "-f", id])?.clone();
                require_success(&removed, "docker rm")?;
                self.owned_containers.retain(|owned| &owned.name != name);
            }
            for (network, id) in &networks {
                let removed = context
                    .runner
                    .run("docker", &["network", "rm", id])?
                    .clone();
                require_success(&removed, "docker network rm")?;
                self.owned_networks.retain(|owned| &owned.name != network);
            }
            Ok(())
        });
        if let Err(error) = removed {
            return Err(with_cleanup(error, self.cleanup_owned(context)));
        }
        stages.insert(Stage::Teardown, teardown_ms);
        Ok(())
    }

    /// Resolve the job image to an immutable digest, pulling only when it is
    /// absent. Runs in `prepare`, never measured.
    fn resolve_job_image(&mut self, context: &mut Context) -> Result<()> {
        let image = context.job_image.clone();
        let present = context
            .runner
            .run("docker", &["image", "inspect", &image])
            .map(|invocation| invocation.ok())
            .unwrap_or(false);
        if !present {
            let pulled = context.runner.run("docker", &["pull", &image])?.clone();
            require_success(&pulled, "docker pull job image")?;
        }
        let digest = context
            .runner
            .run(
                "docker",
                &["image", "inspect", "--format", "{{.Id}}", &image],
            )?
            .clone();
        require_success(&digest, "docker image inspect job image")?;
        self.job_image = Some(digest.stdout.trim().to_owned());
        Ok(())
    }

    /// Trust-scoped partition of the iteration workspace: the class's own
    /// state dir, carrying the same workload files plus a round-tripped
    /// class marker, so each twin mounts only its own class's state
    /// instead of differing by label alone.
    fn trust_partition(workspace: &Path, trust: &str) -> Result<PathBuf> {
        let partition = workspace.join("trust").join(trust);
        std::fs::create_dir_all(&partition)?;
        for name in ["job.sh", "payload.txt"] {
            let source = workspace.join(name);
            if source.is_file() {
                std::fs::copy(&source, partition.join(name))?;
            }
        }
        let marker = partition.join("trust-marker");
        std::fs::write(&marker, trust)?;
        let back = std::fs::read_to_string(&marker)?;
        if back != trust {
            bail!("trust partition {trust} did not round-trip its marker");
        }
        Ok(partition)
    }

    /// One full lifecycle with its stages discarded: warmups establish
    /// retained state, they are never measurements. Each warmup retains
    /// its workspace on disk and one stopped container on the daemon —
    /// real accumulation the measured job then observes — all owned by
    /// this workload and removed by teardown (or by the after-gc GC).
    fn warmup(&mut self, context: &mut Context, trust: &str) -> Result<()> {
        let workspace = self.scratch()?.join(format!("warmup-{}", self.iteration));
        self.iteration += 1;
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(
            workspace.join("warmup-marker"),
            format!("warmup {} for {}\n", self.iteration, self.scenario),
        )?;
        let mut stages = BTreeMap::new();
        self.job_lifecycle(context, &workspace, trust, &mut stages)?;
        let image = self
            .job_image
            .clone()
            .context("job image was not resolved during preparation")?;
        let owner = format!("com.velnor.bench.owner={}", self.owner_token());
        let role = "com.velnor.bench.role=job".to_owned();
        let name = format!("{}-retained", self.object_name("job"));
        self.own_container(name.clone());
        let created = context
            .runner
            .run(
                "docker",
                &[
                    "create",
                    "--name",
                    &name,
                    "--label",
                    &owner,
                    "--label",
                    &role,
                    "--entrypoint",
                    "/bin/sh",
                    &image,
                    "-c",
                    "sleep 30",
                ],
            )?
            .clone();
        require_success(&created, "docker create retained warmup container")?;
        let id = parse_docker_id(&created.stdout)?;
        Self::record_id(&mut self.owned_containers, &name, id)?;
        Ok(())
    }
}

impl Workload for VelnorJobWorkload {
    fn prepare(&mut self, context: &mut Context) -> Result<()> {
        if self.scratch.is_some() {
            bail!("velnor-job workload still owns scratch; teardown is required before prepare");
        }
        let scratch = context.work_root.join(format!(
            "velnor-job-{}-{:x}-{}",
            std::process::id(),
            self.nonce,
            self.owner
        ));
        std::fs::create_dir_all(&scratch)?;
        self.scratch = Some(scratch);
        self.resolve_job_image(context)?;

        match self.kind {
            Kind::StageBreakdown | Kind::ConcurrentSlots | Kind::TrustPartition => {}
            Kind::PersistentHost { warmups } => {
                for _ in 0..warmups {
                    self.warmup(context, "trusted")?;
                }
                if warmups > 0 {
                    self.notes.push(format!(
                        "persistent-host precondition: {warmups} unmeasured warmup job(s) ran before the measured one, \
                         each retaining its workspace and one stopped container"
                    ));
                }
            }
            Kind::AfterGc => {
                for _ in 0..2 {
                    self.warmup(context, "trusted")?;
                }
                // Scoped GC: remove exactly the retained warmup state above
                // — the stopped containers on the daemon plus the warmup
                // workspaces on disk — and measure what it freed; one
                // tracked image build exercises the owned-image removal
                // path too. No host-wide prune ever runs here: the harness
                // must not delete state it does not own.
                let retained_containers = self.owned_containers.len();
                let scratch = self.scratch()?.clone();
                let mut retained_bytes = 0u64;
                let mut retained_workspaces = 0usize;
                for entry in std::fs::read_dir(&scratch)? {
                    let path = entry?.path();
                    if path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("warmup-"))
                    {
                        retained_bytes += tree_bytes(&path);
                        retained_workspaces += 1;
                        std::fs::remove_dir_all(&path)?;
                    }
                }
                // One tracked image build exercises the owned-image removal
                // path alongside the warmup-state removal above.
                let dir = scratch.join("gc-context");
                std::fs::create_dir_all(&dir)?;
                // The tag, not the resolved ID: a bare ID is not a valid
                // `FROM` reference (the daemon reads it as a repository).
                // The tag was verified present during image resolution.
                let image = context.job_image.clone();
                std::fs::write(
                    dir.join("Dockerfile"),
                    format!("FROM {image}\nRUN echo velnor-job-gc-churn > /gc.txt\n"),
                )?;
                let tag = self.gc_image_tag();
                self.own_image(tag.clone());
                let built = context
                    .runner
                    .run(
                        "docker",
                        &[
                            "build",
                            "--label",
                            &format!("com.velnor.bench.owner={}", self.owner_token()),
                            "-t",
                            &tag,
                            &dir.display().to_string(),
                        ],
                    )?
                    .clone();
                require_success(&built, "docker build gc churn")?;
                let size = context
                    .runner
                    .run(
                        "docker",
                        &["image", "inspect", "--format", "{{.Size}}", &tag],
                    )?
                    .clone();
                require_success(&size, "docker image inspect gc churn")?;
                self.capture_owned_image(context, &tag)?;
                self.remove_owned_image_by_tag(context, &tag)?;
                let _ = std::fs::remove_dir_all(&dir);
                self.cleanup_owned(context)
                    .context("after-gc scoped GC of retained warmup state")?;
                self.notes.push(format!(
                    "after-gc precondition: 2 warmup job(s) retained state, then scoped GC removed \
                     {retained_containers} owned container(s) and {retained_workspaces} warmup workspace(s) \
                     ({retained_bytes} bytes) plus one owned image ({} bytes); no host-wide prune",
                    size.stdout.trim()
                ));
            }
        }
        // Warmups consumed iteration numbers for unique names; measured
        // iterations start over at zero.
        self.iteration = 0;
        Ok(())
    }

    fn iterate(&mut self, context: &mut Context) -> Result<Observation> {
        self.iteration += 1;
        context.runner.reset();
        let scratch_root = self.scratch()?.clone();
        let disk_before = tree_bytes(&scratch_root);
        let started = Instant::now();
        let mut stages = BTreeMap::new();

        let workspace = self.preamble(context, &mut stages)?;
        match self.kind {
            Kind::StageBreakdown | Kind::PersistentHost { .. } | Kind::AfterGc => {
                self.job_lifecycle(context, &workspace, "trusted", &mut stages)?;
            }
            Kind::ConcurrentSlots => {
                let jobs = context.concurrency.max(1);
                self.concurrent_batch(context, &workspace, jobs, &mut stages)?;
            }
            Kind::TrustPartition => {
                // Both classes really run, each in its own trust-scoped
                // partition mounting only that class's state; the record
                // carries the per-stage slower, so no class hides behind
                // the other's speed.
                let trusted_workspace = Self::trust_partition(&workspace, "trusted")?;
                let mut trusted = BTreeMap::new();
                self.job_lifecycle(context, &trusted_workspace, "trusted", &mut trusted)?;
                let untrusted_workspace = Self::trust_partition(&workspace, "untrusted")?;
                let mut untrusted = BTreeMap::new();
                self.job_lifecycle(context, &untrusted_workspace, "untrusted", &mut untrusted)?;
                for (stage, value) in untrusted {
                    let entry = trusted.entry(stage).or_insert(0);
                    *entry = (*entry).max(value);
                }
                stages.extend(trusted);
            }
        }

        let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let usage = context.runner.rusage();
        let disk_after = tree_bytes(&scratch_root);
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
        let docker_census =
            crate::census::DockerCensus::from_invocations(context.runner.invocations());

        Ok(Observation {
            total_ms,
            stages_ms: stages,
            checkout_phases_ms: BTreeMap::new(),
            resources,
            git: GitEvidence::NotMeasured,
            docker_census,
            fault: None,
        })
    }

    fn teardown(&mut self, context: &mut Context) -> Result<()> {
        let mut failures = Vec::new();
        if let Err(error) = self.cleanup_owned(context) {
            failures.push(format!("owned object cleanup failed: {error:#}"));
        }
        if self.owned_containers.is_empty()
            && self.owned_networks.is_empty()
            && failures.is_empty()
            && let Some(root) = self.scratch.clone()
        {
            match std::fs::remove_dir_all(&root) {
                Ok(()) => self.scratch = None,
                Err(error) => failures.push(format!(
                    "remove velnor-job workload scratch {} failed: {error:#}",
                    root.display()
                )),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!(
                "velnor-job workload teardown failed: {}",
                failures.join("; ")
            )
        }
    }

    fn notes(&self) -> Vec<String> {
        let mut notes = self.notes.clone();
        notes.push(format!(
            "capacity reservation holds back a {}MiB docker growth allowance for one trivial bench job",
            DOCKER_GROWTH_ALLOWANCE_BYTES / 1024 / 1024
        ));
        match self.kind {
            Kind::ConcurrentSlots => notes.push(
                "concurrent-slots: container lifetimes overlap, but Docker control-plane phases are serialized; stages are phase-window walls, not true concurrent dispatch"
                    .to_owned(),
            ),
            Kind::TrustPartition => notes.push(
                "trust-partition: trusted and untrusted jobs both ran in separate trust-scoped partitions, \
                 each mounting only its own class state; stages carry the per-stage slower and do not measure remote trust admission or cache isolation"
                    .to_owned(),
            ),
            _ => {}
        }
        notes
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;
    use crate::{
        census::{ClassObservation, DockerCensus},
        env::{EnvironmentIdentity, ProbeInputs},
        record::{BenchRecord, Summaries, RESULT_SCHEMA},
        scenario::{Driver, Family, Runnability},
        sys::Runner,
    };

    fn test_workload(nonce: u128) -> VelnorJobWorkload {
        VelnorJobWorkload {
            kind: Kind::AfterGc,
            scenario: "persistent-host/after-gc",
            scratch: None,
            owner: 7,
            nonce,
            job_image: None,
            iteration: 3,
            admission: DiskPressure::new(DiskPolicy::default()),
            owned_containers: Vec::new(),
            owned_networks: Vec::new(),
            owned_images: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn owned_image_inspect_requires_matching_owner_and_valid_id() {
        let id = "a".repeat(64);
        assert_eq!(
            parse_owned_image_inspect(&format!("sha256:{id} owner-token"), "owner-token")
                .expect("valid owned image"),
            format!("sha256:{id}")
        );
        assert!(
            parse_owned_image_inspect(&format!("sha256:{id} other-owner"), "owner-token").is_err()
        );
        assert!(parse_owned_image_inspect("sha256:not-an-id owner-token", "owner-token").is_err());
        assert!(parse_owned_image_inspect(
            &format!("sha256:{id} owner-token trailing"),
            "owner-token"
        )
        .is_err());
    }

    #[test]
    fn fallback_object_names_and_gc_tags_include_nonce() {
        let workload = test_workload(0xabc);
        assert_eq!(
            workload.object_name("job"),
            format!(
                "velnor-job-job-{}-{:032x}-7-3",
                std::process::id(),
                0xabc_u128
            )
        );
        assert_eq!(
            workload.gc_image_tag(),
            format!(
                "velnor-job-gc-{}-{:032x}-7:latest",
                std::process::id(),
                0xabc_u128
            )
        );
    }

    #[test]
    fn local_rows_have_a_driver_and_remote_only_rows_do_not() {
        for id in [
            "lifecycle/stage-breakdown",
            "lifecycle/concurrent-slots",
            "lifecycle/trust-partition",
            "persistent-host/job-1",
            "persistent-host/job-2",
            "persistent-host/job-10",
            "persistent-host/job-100",
            "persistent-host/after-gc",
        ] {
            let scenario = crate::scenario::find(id).expect("scenario");
            assert!(build(scenario).is_ok(), "{id} must build a local workload");
            assert!(
                crate::drivers::build(scenario, Driver::VelnorJob).is_ok(),
                "{id} must route through drivers::build"
            );
        }
        // Rust, Docker and Fault rows need remote dispatch and stay unrun.
        for id in [
            "rust/cold",
            "docker/existing-image",
            "fault/step-command-fails",
        ] {
            let scenario = crate::scenario::find(id).expect("scenario");
            let error = match crate::drivers::build(scenario, Driver::VelnorJob) {
                Ok(_) => panic!("{id}: remote-only row must not build"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("dispatch credentials"),
                "{id}: {error:#}"
            );
        }
    }

    #[test]
    fn local_stages_validate_under_the_velnor_job_driver() {
        // The local driver omits the broker/remote stages; validation must
        // accept the honest subset — no zero-filled broker claims required —
        // and reject a record that carries them.
        let local: Vec<Stage> = Stage::ALL
            .into_iter()
            .filter(|stage| {
                !matches!(
                    stage,
                    Stage::BrokerDelivery | Stage::AcquiredPayload | Stage::Checkout
                )
            })
            .collect();
        assert_eq!(local.len(), 10);
        let observation = |index: u64| Observation {
            total_ms: index * 100,
            stages_ms: local.iter().map(|stage| (*stage, index * 10)).collect(),
            checkout_phases_ms: BTreeMap::new(),
            resources: Resources {
                process_count: 12,
                docker_invocations: 12,
                ..Resources::default()
            },
            git: GitEvidence::NotMeasured,
            docker_census: DockerCensus {
                by_class: BTreeMap::from([(
                    "query".to_owned(),
                    ClassObservation {
                        count: 12,
                        latency_ms: 40,
                    },
                )]),
            },
            fault: None,
        };
        let observations: Vec<Observation> = (1..=4).map(observation).collect();
        let mut runner = Runner::new();
        let environment = EnvironmentIdentity::probe(
            &ProbeInputs {
                velnor_repo: std::env::current_dir().expect("cwd"),
                fixture_repo: None,
                work_root: std::env::temp_dir(),
                job_image: None,
                runner_config_dir: None,
            },
            &mut runner,
        );
        let record = |observations: Vec<Observation>| BenchRecord {
            schema: RESULT_SCHEMA.to_owned(),
            run_id: "test".to_owned(),
            recorded_at_unix_ms: 1,
            scenario: "lifecycle/stage-breakdown".to_owned(),
            family: Family::Lifecycle,
            driver: Driver::VelnorJob,
            runnability: Runnability::Preferred {
                driver: Driver::VelnorJob,
            },
            environment: environment.clone(),
            observations: observations.clone(),
            summaries: Summaries::new(&observations).expect("summaries"),
            notes: Vec::new(),
            context: BTreeMap::new(),
        };
        record(observations.clone())
            .validate()
            .expect("local stages must validate");

        // A zero-filled broker stage claims the broker answered instantly,
        // which no local run observes: subset-only validation accepted it,
        // the narrowed observable set rejects it.
        let mut dishonest = observations;
        for observation in &mut dishonest {
            observation.stages_ms.insert(Stage::BrokerDelivery, 0);
        }
        assert!(matches!(
            record(dishonest).validate(),
            Err(crate::record::RecordError::StageOutsideDriverCoverage {
                stage: Stage::BrokerDelivery,
                ..
            })
        ));
    }
}

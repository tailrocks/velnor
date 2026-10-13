//! The declarative measurement matrix.
//!
//! Scenarios are data, not code paths: adding a measurement means adding a row
//! here and, if its driver does not already exist, one driver. A scenario the
//! current host cannot run is still declared, and the harness reports exactly
//! which requirement is missing instead of quietly narrowing the matrix.

use serde::{Deserialize, Serialize};

use crate::{env::EnvironmentIdentity, stage::Stage};

/// Grouping used for reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    /// Acquisition and startup stage breakdown.
    Lifecycle,
    /// Rust build-cache behaviour.
    Rust,
    /// Container and image behaviour.
    Docker,
    /// Behaviour of a host that keeps state across jobs.
    PersistentHost,
}

impl Family {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lifecycle => "lifecycle",
            Self::Rust => "rust",
            Self::Docker => "docker",
            Self::PersistentHost => "persistent-host",
        }
    }
}

/// What actually executes the workload, and therefore which stages the result
/// can legitimately contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Driver {
    /// A real job dispatched to a registered Velnor runner. Only this driver
    /// observes the acquisition and admission stages.
    VelnorJob,
    /// A real container driven directly through the Docker CLI. Observes the
    /// container lifecycle but no broker, acquisition or admission latency.
    DockerDirect,
    /// A real build executed on the host with no container at all. Observes
    /// only the user command; this is what the replaced bash script measured,
    /// and it is never a claim about Velnor.
    CargoDirect,
}

impl Driver {
    /// Stages this driver is able to observe. A record may not carry a stage
    /// outside this set.
    #[must_use]
    pub const fn observable_stages(self) -> &'static [Stage] {
        match self {
            Self::VelnorJob => &Stage::ALL,
            Self::DockerDirect => &[
                Stage::DockerSetup,
                Stage::ContainerCreate,
                Stage::ContainerStart,
                Stage::FirstUserCommand,
                Stage::CompletionOverhead,
                Stage::Teardown,
            ],
            Self::CargoDirect => &[Stage::FirstUserCommand],
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VelnorJob => "velnor-job",
            Self::DockerDirect => "docker-direct",
            Self::CargoDirect => "cargo-direct",
        }
    }
}

/// A host capability a scenario needs before it can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Requirement {
    DockerDaemon,
    Buildx,
    /// The benchmark contains an executable Velnor job driver.
    ///
    /// This remains absent until the runner-owned dispatch harness is wired;
    /// host credentials alone must not make an unimplemented driver appear
    /// runnable.
    VelnorJobDriver,
    /// A Velnor runner registered against a GitHub repository.
    RegisteredRunner,
    /// Credentials able to dispatch a workflow and read its timeline.
    GithubCredentials,
    /// A checkout of `tailrocks/velnor-actions-fixture`.
    ActionsFixture,
    /// A Rust toolchain on the host or in the job image.
    RustToolchain,
    /// Mr. Boxington, the Rust artifact store.
    Mbx,
    /// Outbound network to a registry or git remote.
    NetworkEgress,
    /// The scenario exercises Linux-only runner surface (cgroups, systemd).
    LinuxHost,
}

impl Requirement {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DockerDaemon => "docker-daemon",
            Self::Buildx => "buildx",
            Self::VelnorJobDriver => "velnor-job-driver",
            Self::RegisteredRunner => "registered-runner",
            Self::GithubCredentials => "github-credentials",
            Self::ActionsFixture => "actions-fixture",
            Self::RustToolchain => "rust-toolchain",
            Self::Mbx => "mbx",
            Self::NetworkEgress => "network-egress",
            Self::LinuxHost => "linux-host",
        }
    }
}

/// One row of the measurement matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scenario {
    pub id: &'static str,
    pub family: Family,
    pub description: &'static str,
    /// The driver that yields the complete, authoritative measurement.
    pub preferred: Driver,
    /// A weaker driver that still measures something real, used when the
    /// preferred driver's requirements are unmet. The record always states
    /// which driver produced the numbers.
    pub fallback: Option<Driver>,
    pub requires: &'static [Requirement],
    /// Extra requirements the fallback driver needs beyond the common ones.
    pub fallback_requires: &'static [Requirement],
}

/// Why a scenario cannot run here, or which driver it will use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runnability {
    /// The preferred driver's requirements are met.
    Preferred { driver: Driver },
    /// The preferred driver is unavailable; the fallback measures a subset.
    Degraded {
        driver: Driver,
        missing_for_preferred: Vec<Requirement>,
    },
    /// Nothing can run; every listed requirement is absent.
    Unrunnable { missing: Vec<Requirement> },
}

impl Runnability {
    /// The driver that will execute, if any.
    #[must_use]
    pub const fn driver(&self) -> Option<Driver> {
        match self {
            Self::Preferred { driver } | Self::Degraded { driver, .. } => Some(*driver),
            Self::Unrunnable { .. } => None,
        }
    }
}

/// Host capabilities, derived from a probed environment plus explicit operator
/// input for the things no probe can infer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub docker_daemon: bool,
    pub buildx: bool,
    /// Whether this build includes the real Velnor job benchmark driver.
    /// This is deliberately false until that driver is implemented.
    pub velnor_job_driver: bool,
    pub registered_runner: bool,
    pub github_credentials: bool,
    pub actions_fixture: bool,
    pub rust_toolchain: bool,
    pub mbx: bool,
    pub network_egress: bool,
    pub linux_host: bool,
}

impl Capabilities {
    /// Derive from a probed environment. `github_credentials` and
    /// `network_egress` are not inferred from a successful probe: a cached
    /// image or a warm token would make them look present when they are not,
    /// so they are supplied by the caller.
    #[must_use]
    pub fn from_environment(
        environment: &EnvironmentIdentity,
        github_credentials: bool,
        network_egress: bool,
    ) -> Self {
        Self {
            docker_daemon: environment.has_docker(),
            buildx: environment.buildkit_version.is_known(),
            velnor_job_driver: false,
            registered_runner: environment.has_runner(),
            github_credentials,
            actions_fixture: environment.fixture_commit.is_known(),
            rust_toolchain: environment.cargo_version.is_known(),
            mbx: environment.mbx_version.is_known(),
            network_egress,
            linux_host: cfg!(target_os = "linux"),
        }
    }

    #[must_use]
    pub const fn has(self, requirement: Requirement) -> bool {
        match requirement {
            Requirement::DockerDaemon => self.docker_daemon,
            Requirement::Buildx => self.buildx,
            Requirement::VelnorJobDriver => self.velnor_job_driver,
            Requirement::RegisteredRunner => self.registered_runner,
            Requirement::GithubCredentials => self.github_credentials,
            Requirement::ActionsFixture => self.actions_fixture,
            Requirement::RustToolchain => self.rust_toolchain,
            Requirement::Mbx => self.mbx,
            Requirement::NetworkEgress => self.network_egress,
            Requirement::LinuxHost => self.linux_host,
        }
    }
}

impl Scenario {
    /// Requirements the host does not satisfy.
    #[must_use]
    pub fn missing(&self, capabilities: Capabilities) -> Vec<Requirement> {
        self.requires
            .iter()
            .copied()
            .filter(|requirement| !capabilities.has(*requirement))
            .collect()
    }

    /// Decide how, or whether, this scenario runs on the given host.
    #[must_use]
    pub fn runnability(&self, capabilities: Capabilities) -> Runnability {
        let missing = self.missing(capabilities);
        if missing.is_empty() {
            return Runnability::Preferred {
                driver: self.preferred,
            };
        }
        // Rust benchmark rows are only authoritative when they reach the
        // real Velnor job driver. A host Cargo fallback would produce a
        // plausible number while measuring none of Velnor's lifecycle.
        if self.family == Family::Rust && self.preferred == Driver::VelnorJob {
            return Runnability::Unrunnable { missing };
        }
        let Some(fallback) = self.fallback else {
            return Runnability::Unrunnable { missing };
        };
        // The fallback drops the preferred driver's requirements and adds its
        // own; anything still absent makes the scenario unrunnable.
        let fallback_missing: Vec<Requirement> = self
            .fallback_requires
            .iter()
            .copied()
            .filter(|requirement| !capabilities.has(*requirement))
            .collect();
        if fallback_missing.is_empty() {
            Runnability::Degraded {
                driver: fallback,
                missing_for_preferred: missing,
            }
        } else {
            Runnability::Unrunnable {
                missing: fallback_missing,
            }
        }
    }
}

const VELNOR_JOB: &[Requirement] = &[
    Requirement::VelnorJobDriver,
    Requirement::RegisteredRunner,
    Requirement::GithubCredentials,
    Requirement::ActionsFixture,
    Requirement::DockerDaemon,
    Requirement::NetworkEgress,
];
const CONTAINER: &[Requirement] = &[Requirement::DockerDaemon];
/// The cargo fallback runs on the host with no container and no runner: it
/// measures the build alone, which is precisely the scope of the bash script
/// this crate replaces, and is never a claim about Velnor.
const HOST_RUST: &[Requirement] = &[Requirement::RustToolchain];
const HOST_RUST_ONLINE: &[Requirement] = &[Requirement::RustToolchain, Requirement::NetworkEgress];
const CONTAINER_PULL: &[Requirement] = &[Requirement::DockerDaemon, Requirement::NetworkEgress];
const CONTAINER_BUILDX: &[Requirement] = &[Requirement::DockerDaemon, Requirement::Buildx];

macro_rules! scenarios {
    ($( $id:literal, $family:ident, $preferred:ident, $fallback:expr, $requires:expr, $fallback_requires:expr, $description:literal );* $(;)?) => {
        pub const MATRIX: &[Scenario] = &[
            $( Scenario {
                id: $id,
                family: Family::$family,
                description: $description,
                preferred: Driver::$preferred,
                fallback: $fallback,
                requires: $requires,
                fallback_requires: $fallback_requires,
            } ),*
        ];
    };
}

scenarios! {
    // Lifecycle: only a real job observes acquisition and admission, so these
    // have no fallback by construction.
    "lifecycle/stage-breakdown", Lifecycle, VelnorJob, None, VELNOR_JOB, &[],
        "Full ready -> broker -> acquire -> admission -> capacity -> checkout -> docker -> first command -> completion -> teardown breakdown";
    "lifecycle/concurrent-slots", Lifecycle, VelnorJob, None, VELNOR_JOB, &[],
        "Stage breakdown while every configured slot is busy";

    // Rust cache behaviour. These rows require a real Velnor job: host Cargo
    // can measure compilation, but cannot establish Velnor acceleration.
    "rust/cold", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "Fully cold: no target dir, no shared cache, no mbx store";
    "rust/warm", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "Warm caches from an immediately preceding identical job";
    "rust/noop", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "Re-run with no input change; measures pure fingerprint overhead";
    "rust/fresh-worktree-same-commit", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "New worktree at the same commit; isolates path and mtime sensitivity";
    "rust/small-source-edit", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "One-line edit in a leaf crate";
    "rust/lockfile-update", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST_ONLINE,
        "Cargo.lock changes; measures dependency rebuild blast radius";
    "rust/manifest-touch", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "Manifest mtime changes with no content change; isolates manifest-driven invalidation";
    "rust/feature-set-change", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "Different feature set for the same dependency graph";
    "rust/build-script-change", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "build.rs edit; measures rerun-if invalidation";
    "rust/native-sys", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "Native -sys crate with a C toolchain dependency";
    "rust/proc-macro", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "Proc-macro crate rebuild and its downstream invalidation";
    "rust/cargo-check", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "cargo check over the workspace";
    "rust/cargo-build", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "cargo build over the workspace";
    "rust/nextest", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "cargo nextest run over the workspace";
    "rust/clippy", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "cargo clippy --all-targets over the workspace";
    "rust/doc", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "cargo doc over the workspace";
    "rust/concurrent-jobs", Rust, VelnorJob, Some(Driver::CargoDirect), VELNOR_JOB, HOST_RUST,
        "Concurrent Rust jobs contending for one shared cache and mbx store";

    // Docker behaviour.
    "docker/existing-image", Docker, VelnorJob, Some(Driver::DockerDirect), VELNOR_JOB, CONTAINER,
        "Container lifecycle for an image already present on the host";
    "docker/image-pull", Docker, VelnorJob, Some(Driver::DockerDirect), VELNOR_JOB, CONTAINER_PULL,
        "Cold pull of the job image from the registry";
    "docker/simple-job-container", Docker, VelnorJob, Some(Driver::DockerDirect), VELNOR_JOB, CONTAINER,
        "Job container running a trivial user command";
    "docker/service-container", Docker, VelnorJob, Some(Driver::DockerDirect), VELNOR_JOB, CONTAINER,
        "Job container plus a linked service container and health wait";
    "docker/docker-action", Docker, VelnorJob, None, VELNOR_JOB, &[],
        "Container action step resolved, built and executed by the runner";
    "docker/buildx", Docker, VelnorJob, Some(Driver::DockerDirect), VELNOR_JOB, CONTAINER_BUILDX,
        "docker buildx build through the BuildKit driver";
    "docker/build-cached", Docker, VelnorJob, Some(Driver::DockerDirect), VELNOR_JOB, CONTAINER,
        "docker build with every layer already cached";
    "docker/build-uncached", Docker, VelnorJob, Some(Driver::DockerDirect), VELNOR_JOB, CONTAINER,
        "docker build with the cache defeated at the first layer";
    "docker/testcontainers", Docker, VelnorJob, None, VELNOR_JOB, &[],
        "Job that starts containers of its own through a mounted Docker socket";

    // Persistent host behaviour: these are properties of runner state across
    // jobs and cannot be approximated without the runner.
    "persistent-host/job-1", PersistentHost, VelnorJob, None, VELNOR_JOB, &[],
        "First job on a freshly provisioned host";
    "persistent-host/job-2", PersistentHost, VelnorJob, None, VELNOR_JOB, &[],
        "Second job; first to see any retained state";
    "persistent-host/job-10", PersistentHost, VelnorJob, None, VELNOR_JOB, &[],
        "Tenth job; steady-state cache behaviour";
    "persistent-host/job-100", PersistentHost, VelnorJob, None, VELNOR_JOB, &[],
        "Hundredth job; accumulation, leak and fragmentation behaviour";
    "persistent-host/after-gc", PersistentHost, VelnorJob, None, VELNOR_JOB, &[],
        "First job after a full disk and image garbage collection";
}

/// Look one scenario up by id.
#[must_use]
pub fn find(id: &str) -> Option<&'static Scenario> {
    MATRIX.iter().find(|scenario| scenario.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scenario_id_is_unique_and_namespaced() {
        let mut ids: Vec<&str> = MATRIX.iter().map(|scenario| scenario.id).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate scenario id");
        for scenario in MATRIX {
            let (family, rest) = scenario.id.split_once('/').expect("namespaced id");
            assert_eq!(family, scenario.family.as_str(), "{}", scenario.id);
            assert!(!rest.is_empty());
            assert!(!scenario.description.is_empty(), "{}", scenario.id);
        }
    }

    #[test]
    fn the_matrix_covers_every_required_measurement() {
        for id in [
            "lifecycle/stage-breakdown",
            "rust/cold",
            "rust/warm",
            "rust/noop",
            "rust/fresh-worktree-same-commit",
            "rust/small-source-edit",
            "rust/lockfile-update",
            "rust/manifest-touch",
            "rust/feature-set-change",
            "rust/build-script-change",
            "rust/native-sys",
            "rust/proc-macro",
            "rust/cargo-check",
            "rust/cargo-build",
            "rust/nextest",
            "rust/clippy",
            "rust/doc",
            "rust/concurrent-jobs",
            "docker/existing-image",
            "docker/image-pull",
            "docker/simple-job-container",
            "docker/service-container",
            "docker/docker-action",
            "docker/buildx",
            "docker/build-cached",
            "docker/build-uncached",
            "docker/testcontainers",
            "persistent-host/job-1",
            "persistent-host/job-2",
            "persistent-host/job-10",
            "persistent-host/job-100",
            "persistent-host/after-gc",
        ] {
            assert!(find(id).is_some(), "matrix is missing {id}");
        }
    }

    #[test]
    fn a_fallback_never_claims_a_stage_its_driver_cannot_see() {
        for scenario in MATRIX {
            if let Some(fallback) = scenario.fallback {
                assert!(
                    fallback.observable_stages().len()
                        < Driver::VelnorJob.observable_stages().len(),
                    "{} declares a fallback that claims full coverage",
                    scenario.id
                );
                assert!(
                    !fallback
                        .observable_stages()
                        .contains(&Stage::BrokerDelivery),
                    "{} would attribute broker latency to a driver that never talks to the broker",
                    scenario.id
                );
            }
        }
    }

    #[test]
    fn a_bare_host_can_run_nothing() {
        let bare = Capabilities::default();
        for scenario in MATRIX {
            assert!(
                matches!(scenario.runnability(bare), Runnability::Unrunnable { .. }),
                "{} claims to run on a host with no capabilities",
                scenario.id
            );
        }
    }

    #[test]
    fn a_docker_only_host_degrades_rather_than_pretending() {
        let capabilities = Capabilities {
            docker_daemon: true,
            rust_toolchain: true,
            ..Capabilities::default()
        };
        let scenario = find("docker/existing-image").expect("scenario");
        let runnability = scenario.runnability(capabilities);
        let Runnability::Degraded {
            driver,
            missing_for_preferred,
        } = runnability
        else {
            panic!("expected degradation, got {runnability:?}");
        };
        assert_eq!(driver, Driver::DockerDirect);
        assert!(missing_for_preferred.contains(&Requirement::RegisteredRunner));

        // Persistent-host scenarios have no honest fallback and must stay unrun.
        let host = find("persistent-host/job-100").expect("scenario");
        assert!(matches!(
            host.runnability(capabilities),
            Runnability::Unrunnable { .. }
        ));
    }

    #[test]
    fn rust_rows_never_degrade_to_host_cargo() {
        let capabilities = Capabilities {
            rust_toolchain: true,
            ..Capabilities::default()
        };
        for scenario in MATRIX
            .iter()
            .filter(|scenario| scenario.family == Family::Rust)
        {
            assert!(
                matches!(
                    scenario.runnability(capabilities),
                    Runnability::Unrunnable { .. }
                ),
                "{} must not emit a host-Cargo measurement",
                scenario.id
            );
        }
    }

    #[test]
    fn a_fully_capable_host_reports_unimplemented_job_driver() {
        let capabilities = Capabilities {
            docker_daemon: true,
            buildx: true,
            velnor_job_driver: false,
            registered_runner: true,
            github_credentials: true,
            actions_fixture: true,
            rust_toolchain: true,
            mbx: true,
            network_egress: true,
            linux_host: true,
        };
        for scenario in MATRIX {
            match scenario.runnability(capabilities) {
                Runnability::Degraded {
                    driver,
                    missing_for_preferred,
                } => {
                    assert_eq!(driver, Driver::DockerDirect, "{}", scenario.id);
                    assert_eq!(
                        missing_for_preferred,
                        vec![Requirement::VelnorJobDriver],
                        "{}",
                        scenario.id
                    );
                }
                Runnability::Unrunnable { missing } => {
                    assert_eq!(
                        missing,
                        vec![Requirement::VelnorJobDriver],
                        "{}",
                        scenario.id
                    );
                }
                Runnability::Preferred { driver } => {
                    panic!(
                        "{} must not claim unimplemented driver {driver:?} is runnable",
                        scenario.id
                    );
                }
            }
        }
    }
}

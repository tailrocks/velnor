//! Private per-worker DinD daemon provisioning.
//!
//! Each worker owns one DinD daemon container that serves ONLY its paired
//! runner container. The daemon listens on a private Unix socket on a
//! per-worker bind mount — never TCP, never the host's
//! `/var/run/docker.sock`:
//!
//! * dockerd starts with a single `-H unix://...` listener, so no TCP
//!   port exists to publish even by accident;
//! * the create argv carries no `-p`/`--publish` flag (proven by test);
//! * the state dir bind carries no Docker socket from the host (proven by
//!   test: the only socket under it is the one this daemon creates);
//! * the runner reaches the daemon through the identical absolute socket
//!   path on the shared bind (see [`super::runner`]).
//!
//! Provisioning is idempotent on the recorded [`WorkerIdentity`](super::ownership::WorkerIdentity):
//! an existing container with matching ownership labels is adopted, an
//! existing container with foreign labels fails closed (a name collision
//! outside our ownership must never be commandeered).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

use super::ownership::{WorkerIdentity, ROLE_DIND};
use super::runner::PinnedImage;
use super::WorkerRunner;

/// Guest-absolute mount point of the per-worker state dir bind.
///
/// The state dir bind (`<state-dir>:/velnor/scaleset`) is mounted at this
/// identical absolute path in BOTH containers of a pair, so the socket path
/// below names the same file on both sides without any translation.
pub const STATE_MOUNT: &str = "/velnor/scaleset";
/// Guest-absolute private daemon socket (shared bind, same path both sides).
pub const DIND_SOCKET: &str = "/velnor/scaleset/dind.sock";
/// Guest-absolute BuildKit cache dir on the shared bind (identical path
/// both sides; inner builds address `--cache-to/--cache-from
/// type=local` here).
pub const BUILDKIT_CACHE_DIR: &str = "/velnor/scaleset/buildkit-cache";
/// dockerd's data root inside the daemon container (named volume).
pub const DIND_DATA_ROOT: &str = "/var/lib/docker";

/// Fully-derived DinD provision spec: image, identity, host state dir.
#[derive(Debug, Clone)]
pub struct DindSpec {
    identity: WorkerIdentity,
    image: PinnedImage,
    state_dir: PathBuf,
}

impl DindSpec {
    /// Derive the spec. `state_dir` is the host directory for this
    /// worker (`<daemon-state>/scale-set/<slug>`); provisioning creates it.
    #[must_use]
    pub fn new(identity: WorkerIdentity, image: PinnedImage, state_dir: &Path) -> Self {
        Self {
            identity,
            image,
            state_dir: state_dir.to_path_buf(),
        }
    }

    #[must_use]
    pub fn identity(&self) -> &WorkerIdentity {
        &self.identity
    }

    #[must_use]
    pub fn image(&self) -> &PinnedImage {
        &self.image
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Host path of the private socket once dockerd creates it.
    #[must_use]
    pub fn host_socket(&self) -> PathBuf {
        self.state_dir.join("dind.sock")
    }

    /// Host path of the BuildKit cache dir on the shared bind.
    #[must_use]
    pub fn host_cache_dir(&self) -> PathBuf {
        self.state_dir.join("buildkit-cache")
    }

    /// `docker create` argv for the daemon container.
    ///
    /// Invariants (all proven by unit tests on this vector):
    /// * `--privileged` (DinD cannot run unprivileged; documented, not hidden);
    /// * no `-p`/`--publish`/`--expose`: no TCP surface at all;
    /// * no host socket bind: the only socket is the daemon's own;
    /// * dockerd command line carries exactly one `-H unix://` listener.
    #[must_use]
    pub fn create_args(&self) -> Vec<String> {
        let mut args = vec![
            "create".to_string(),
            "--privileged".to_string(),
            "--name".to_string(),
            self.identity.dind_container(),
            "--network".to_string(),
            self.identity.network(),
            "--env".to_string(),
            // No TLS material: the only listener is a filesystem socket
            // only this worker pair can reach.
            "DOCKER_TLS_CERTDIR=".to_string(),
            "--volume".to_string(),
            format!("{}:{DIND_DATA_ROOT}", self.identity.dind_data_volume()),
            "--volume".to_string(),
            format!("{}:{STATE_MOUNT}", self.state_dir.display()),
        ];
        args.extend(self.identity.label_args(ROLE_DIND));
        args.push("--".to_string());
        args.push(self.image.reference().to_string());
        // dockerd with ONE listener: the private Unix socket. No
        // `tcp://` listener exists, so nothing can be published.
        args.push(format!("-H unix://{DIND_SOCKET}"));
        args
    }

    /// Readiness probe argv: `docker version` against the private socket
    /// from INSIDE the daemon container (proves dockerd serves the
    /// socket; the dind image ships the CLI).
    #[must_use]
    pub fn probe_args(&self) -> Vec<String> {
        vec![
            "exec".to_string(),
            self.identity.dind_container(),
            "docker".to_string(),
            "-H".to_string(),
            format!("unix://{DIND_SOCKET}"),
            "version".to_string(),
            "--format".to_string(),
            "{{.Server.Version}}".to_string(),
        ]
    }
}

/// What [`ensure_dind`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DindProvision {
    /// A live container with matching ownership labels was adopted.
    Adopted,
    /// A fresh daemon container was created and started.
    Created,
}

/// Parse `docker inspect --format {{.Id}}` output for adoption.
#[must_use]
pub fn parse_container_id(output: &str) -> Option<String> {
    let id = output.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Parse one `key=value` label line of `docker inspect --format` output.
#[must_use]
pub fn parse_label_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

/// Parse the readiness probe output: any non-empty server version is ready.
#[must_use]
pub fn parse_probe_output(output: &str) -> bool {
    !output.trim().is_empty()
}

/// Ensure the private daemon exists and is started, adopting on retry.
///
/// * Looks up the recorded container name; a missing container is created
///   from [`DindSpec::create_args`] and started.
/// * An existing container's ownership labels must match this worker;
///   foreign labels fail closed instead of adopting a stranger.
/// * Never pulls here: images are pulled + verified by the tool-content
///   hook ([`super::runner`]) before provisioning starts.
pub(crate) fn ensure_dind(runner: &mut dyn WorkerRunner, spec: &DindSpec) -> Result<DindProvision> {
    std::fs::create_dir_all(spec.state_dir()).with_context(|| {
        format!(
            "create scale-set worker state dir {}",
            spec.state_dir().display()
        )
    })?;
    std::fs::create_dir_all(spec.host_cache_dir()).with_context(|| {
        format!(
            "create scale-set BuildKit cache dir {}",
            spec.host_cache_dir().display()
        )
    })?;

    let name = spec.identity().dind_container();
    let inspect = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--format".to_string(),
                "{{.Id}}".to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("inspect DinD container {name}"))?;
    if parse_container_id(&inspect.stdout).is_none() {
        let created = runner
            .run("docker", &spec.create_args())
            .with_context(|| format!("create DinD container {name}"))?;
        if created.code != 0 {
            anyhow::bail!(
                "create DinD container {name} exited {}: {}",
                created.code,
                created.stderr.trim()
            );
        }
        let started = runner
            .run(
                "docker",
                &["start".to_string(), "--".to_string(), name.clone()],
            )
            .with_context(|| format!("start DinD container {name}"))?;
        if started.code != 0 {
            anyhow::bail!(
                "start DinD container {name} exited {}: {}",
                started.code,
                started.stderr.trim()
            );
        }
        return Ok(DindProvision::Created);
    }

    verify_ownership_labels(runner, spec)?;
    // Idempotent start: starting a running container succeeds, starting a
    // stopped owned container resumes it.
    let started = runner
        .run(
            "docker",
            &["start".to_string(), "--".to_string(), name.clone()],
        )
        .with_context(|| format!("start DinD container {name}"))?;
    if started.code != 0 {
        anyhow::bail!(
            "start DinD container {name} exited {}: {}",
            started.code,
            started.stderr.trim()
        );
    }
    Ok(DindProvision::Adopted)
}

/// Fail closed when the existing container is not ours.
fn verify_ownership_labels(runner: &mut dyn WorkerRunner, spec: &DindSpec) -> Result<()> {
    verify_container_ownership(
        runner,
        &spec.identity().dind_container(),
        &spec.identity().ownership().as_str(),
    )
}

/// Adoption gate shared by both containers of a pair: the existing
/// container's ownership label must equal the recorded ownership.
/// Foreign or missing labels fail closed — a stranger is never adopted.
pub(crate) fn verify_container_ownership(
    runner: &mut dyn WorkerRunner,
    container: &str,
    expected_ownership: &str,
) -> Result<()> {
    let labels = runner
        .run(
            "docker",
            &[
                "inspect".to_string(),
                "--format".to_string(),
                r#"{{range $k, $v := .Config.Labels}}{{$k}}={{$v}}{{"\n"}}{{end}}"#.to_string(),
                "--".to_string(),
                container.to_string(),
            ],
        )
        .with_context(|| format!("inspect container labels {container}"))?;
    if labels.code != 0 {
        anyhow::bail!(
            "inspect container labels {container} exited {}: {}",
            labels.code,
            labels.stderr.trim()
        );
    }
    let mut ownership = None;
    for line in labels.stdout.lines() {
        if let Some((key, value)) = parse_label_line(line)
            && key == super::ownership::OWNERSHIP_LABEL
        {
            ownership = Some(value);
        }
    }
    match ownership {
        Some(found) if found == expected_ownership => Ok(()),
        Some(found) => anyhow::bail!(
            "container {container} carries foreign ownership {found:?}, expected {expected_ownership:?}: refusing to adopt"
        ),
        None => anyhow::bail!(
            "container {container} carries no ownership label: refusing to adopt"
        ),
    }
}

/// Single readiness probe: true when dockerd answers on the private socket.
///
/// A failing probe is NOT an error — the caller retries with its own
/// backoff, then fails the worker when the deadline passes. Only a
/// transport failure (the `docker exec` itself could not run) errors.
pub(crate) fn dind_ready(runner: &mut dyn WorkerRunner, spec: &DindSpec) -> Result<bool> {
    let probe = runner
        .run("docker", &spec.probe_args())
        .context("probe DinD readiness")?;
    if probe.code != 0 {
        return Ok(false);
    }
    Ok(parse_probe_output(&probe.stdout))
}

/// How long the supervisor waits for first readiness before failing.
pub const DIND_READY_TIMEOUT: Duration = Duration::from_secs(120);
/// Interval between readiness probes while provisioning.
pub const DIND_READY_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// What [`ensure_network`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkProvision {
    /// A network with matching ownership labels was adopted.
    Adopted,
    /// A fresh per-worker network was created.
    Created,
}

/// `docker network create` argv for the per-worker bridge.
///
/// The network carries the ownership labels (no role label: it belongs
/// to the pair, not one container) and publishes nothing — the runner
/// joins the DinD netns, so the bridge exists only to isolate the pair.
#[must_use]
pub fn network_create_args(identity: &WorkerIdentity) -> Vec<String> {
    let mut args = vec![
        "network".to_string(),
        "create".to_string(),
        "--driver".to_string(),
        "bridge".to_string(),
    ];
    for (key, value) in identity.labels() {
        args.push("--label".to_string());
        args.push(format!("{key}={value}"));
    }
    args.push("--".to_string());
    args.push(identity.network());
    args
}

/// Ensure the per-worker network exists, adopting on retry.
///
/// Missing → create; present → ownership labels must match this worker.
pub(crate) fn ensure_network(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
) -> Result<NetworkProvision> {
    let name = identity.network();
    let inspect = runner
        .run(
            "docker",
            &[
                "network".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                "{{.Id}}".to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("inspect worker network {name}"))?;
    if inspect.stdout.trim().is_empty() {
        let created = runner
            .run("docker", &network_create_args(identity))
            .with_context(|| format!("create worker network {name}"))?;
        if created.code != 0 {
            anyhow::bail!(
                "create worker network {name} exited {}: {}",
                created.code,
                created.stderr.trim()
            );
        }
        return Ok(NetworkProvision::Created);
    }
    verify_network_ownership(runner, identity)?;
    Ok(NetworkProvision::Adopted)
}

fn verify_network_ownership(
    runner: &mut dyn WorkerRunner,
    identity: &WorkerIdentity,
) -> Result<()> {
    let name = identity.network();
    let labels = runner
        .run(
            "docker",
            &[
                "network".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                r#"{{range $k, $v := .Labels}}{{$k}}={{$v}}{{"\n"}}{{end}}"#.to_string(),
                "--".to_string(),
                name.clone(),
            ],
        )
        .with_context(|| format!("inspect worker network labels {name}"))?;
    if labels.code != 0 {
        anyhow::bail!(
            "inspect worker network labels {name} exited {}: {}",
            labels.code,
            labels.stderr.trim()
        );
    }
    let mut ownership = None;
    for line in labels.stdout.lines() {
        if let Some((key, value)) = parse_label_line(line)
            && key == super::ownership::OWNERSHIP_LABEL
        {
            ownership = Some(value);
        }
    }
    let expected = identity.ownership().as_str();
    match ownership {
        Some(found) if found == expected => Ok(()),
        Some(found) => anyhow::bail!(
            "network {name} carries foreign ownership {found:?}, expected {expected:?}: refusing to adopt"
        ),
        None => anyhow::bail!("network {name} carries no ownership label: refusing to adopt"),
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
    use super::super::WorkerOutput;
    use super::*;
    use std::collections::VecDeque;

    const DIND_REF: &str =
        "docker@sha256:2a232a42256f70d78e3cc5d2b5d6b3276710a0de0596c145f627ecfae90282ac";

    fn spec() -> DindSpec {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        DindSpec::new(
            identity,
            PinnedImage::parse(DIND_REF).unwrap(),
            Path::new("/tmp/velnor-test-dind-state"),
        )
    }

    struct ScriptRunner {
        results: VecDeque<WorkerOutput>,
        seen: Vec<Vec<String>>,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<WorkerOutput>) -> Self {
            Self {
                results: results.into(),
                seen: Vec::new(),
            }
        }

        fn ok(stdout: &str) -> WorkerOutput {
            WorkerOutput {
                code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            }
        }

        fn fail(code: i32, stderr: &str) -> WorkerOutput {
            WorkerOutput {
                code,
                stdout: String::new(),
                stderr: stderr.to_string(),
            }
        }
    }

    impl WorkerRunner for ScriptRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<WorkerOutput> {
            assert_eq!(program, "docker");
            self.seen.push(args.to_vec());
            self.results
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("script exhausted at docker {}", args.join(" ")))
        }
    }

    #[test]
    fn create_argv_has_no_tcp_surface() {
        let args = spec().create_args();
        for forbidden in ["-p", "--publish", "--expose", "-P", "--publish-all"] {
            assert!(
                !args.iter().any(|arg| arg == forbidden),
                "DinD argv must not carry {forbidden}: {args:?}"
            );
        }
        // dockerd gets exactly one listener, and it is the Unix socket.
        let listeners: Vec<_> = args
            .iter()
            .filter(|arg| arg.starts_with("-H"))
            .cloned()
            .collect();
        assert_eq!(listeners, [format!("-H unix://{DIND_SOCKET}")]);
        assert!(!args.iter().any(|arg| arg.contains("tcp://")), "{args:?}");
    }

    #[test]
    fn create_argv_binds_no_host_socket() {
        let args = spec().create_args();
        assert!(
            !args.iter().any(|arg| arg.contains("/var/run/docker.sock")),
            "{args:?}"
        );
        // The state bind is the worker's own dir at the identical path.
        assert!(args.contains(&format!("/tmp/velnor-test-dind-state:{STATE_MOUNT}")));
        // Privileged is explicit (DinD requirement), pinned image, ownership labels.
        assert!(args.contains(&"--privileged".to_string()));
        assert!(args.contains(&DIND_REF.to_string()));
        assert!(args
            .iter()
            .any(|arg| arg.contains("velnor.scaleset.ownership=")));
    }

    #[test]
    fn probe_targets_the_private_socket() {
        let args = spec().probe_args();
        assert_eq!(args[0], "exec");
        assert!(args.contains(&format!("unix://{DIND_SOCKET}")));
        assert!(!args.iter().any(|arg| arg.contains("tcp://")));
    }

    #[test]
    fn probe_output_parses() {
        assert!(parse_probe_output("28.5.2\n"));
        assert!(!parse_probe_output(""));
        assert!(!parse_probe_output("  \n"));
    }

    #[test]
    fn missing_container_is_created_and_started() {
        let dir = std::env::temp_dir().join(format!("velnor-dind-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok(""),           // inspect: missing
            ScriptRunner::ok("deadbeef\n"), // create
            ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007-2ad92676\n"), // start
        ]);
        let provision = ensure_dind(&mut runner, &spec).unwrap();
        assert_eq!(provision, DindProvision::Created);
        assert_eq!(runner.seen.len(), 3);
        assert_eq!(runner.seen[1][0], "create");
        assert!(dir.join("buildkit-cache").is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn owned_container_is_adopted_and_started() {
        let dir = std::env::temp_dir().join(format!("velnor-dind-adopt-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("deadbeef\n"), // inspect: present
            ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"), // labels
            ScriptRunner::ok("velnor-scaleset-dind-s7-velnor-set-0007-2ad92676\n"), // start
        ]);
        let provision = ensure_dind(&mut runner, &spec).unwrap();
        assert_eq!(provision, DindProvision::Adopted);
        assert!(!runner.seen.iter().any(|argv| argv[0] == "create"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn foreign_container_fails_closed() {
        let dir = std::env::temp_dir().join(format!("velnor-dind-foreign-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("deadbeef\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/someone-else\n"),
        ]);
        let error = ensure_dind(&mut runner, &spec).unwrap_err();
        assert!(error.to_string().contains("foreign ownership"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unlabeled_container_fails_closed() {
        let dir =
            std::env::temp_dir().join(format!("velnor-dind-unlabeled-{}", std::process::id()));
        let mut spec = spec();
        spec.state_dir = dir.clone();
        let mut runner =
            ScriptRunner::scripted(vec![ScriptRunner::ok("deadbeef\n"), ScriptRunner::ok("")]);
        let error = ensure_dind(&mut runner, &spec).unwrap_err();
        assert!(error.to_string().contains("no ownership label"), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn network_argv_is_a_labeled_bridge_without_publish() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        let args = network_create_args(&identity);
        assert_eq!(args[0], "network");
        assert_eq!(args[1], "create");
        assert!(args.contains(&"velnor-scaleset-net-s7-velnor-set-0007-2ad92676".to_string()));
        assert!(args
            .iter()
            .any(|arg| arg.contains("velnor.scaleset.ownership=7/velnor-set-0007")));
        for forbidden in ["-p", "--publish", "--expose"] {
            assert!(!args.iter().any(|arg| arg == forbidden), "{args:?}");
        }
    }

    #[test]
    fn network_is_created_then_adopted() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        let mut runner =
            ScriptRunner::scripted(vec![ScriptRunner::ok(""), ScriptRunner::ok("netid\n")]);
        assert_eq!(
            ensure_network(&mut runner, &identity).unwrap(),
            NetworkProvision::Created
        );
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("netid\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/velnor-set-0007\n"),
        ]);
        assert_eq!(
            ensure_network(&mut runner, &identity).unwrap(),
            NetworkProvision::Adopted
        );
    }

    #[test]
    fn foreign_network_fails_closed() {
        let identity = WorkerIdentity::new(super::super::ownership::OwnershipId::bind(
            7,
            "velnor-set-0007",
        ));
        let mut runner = ScriptRunner::scripted(vec![
            ScriptRunner::ok("netid\n"),
            ScriptRunner::ok("velnor.scaleset.ownership=7/stranger\n"),
        ]);
        let error = ensure_network(&mut runner, &identity).unwrap_err();
        assert!(error.to_string().contains("foreign ownership"), "{error}");
    }

    #[test]
    fn readiness_probe_failure_is_not_ready_not_error() {
        let spec = spec();
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::fail(1, "Cannot connect")]);
        assert!(!dind_ready(&mut runner, &spec).unwrap());
        let mut runner = ScriptRunner::scripted(vec![ScriptRunner::ok("28.5.2\n")]);
        assert!(dind_ready(&mut runner, &spec).unwrap());
    }
}

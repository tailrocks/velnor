//! Environment identity captured with every result.
//!
//! A benchmark number without the machine it came from is not a measurement.
//! Every field below is mandatory in the wire schema: a record missing any of
//! them fails to deserialise. Fields the host genuinely cannot answer are
//! recorded as [`Fact::Unavailable`] with the reason, which is a different
//! thing from a field that was never captured.
//!
//! Note on CPU: the replaced script recorded Python's `platform.processor()`,
//! which on both Linux and macOS reports the *architecture* (`arm64`,
//! `x86_64`) and not the CPU model. `cpu_model` here is the brand string
//! (`machdep.cpu.brand_string` / `/proc/cpuinfo: model name`) and `cpu_arch` is
//! kept as a separate field so the two can never be confused again.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{fact::Fact, sys::Runner};

/// Stable discriminator for the environment block.
pub const ENVIRONMENT_SCHEMA: &str = "velnor.bench.environment.v1";

/// Runner configuration under which the measurement was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerConfiguration {
    /// Concurrent job slots configured on the host.
    pub slots: Fact<u32>,
    /// Execution backend (`docker`, `microvm`, ...).
    pub backend: Fact<String>,
    /// Runner labels the measured jobs targeted.
    pub labels: Fact<Vec<String>>,
    /// Absolute path of the runner config base, when one exists.
    pub config_dir: Fact<String>,
}

/// Complete identity of the machine and software that produced a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdentity {
    pub schema: String,
    pub cpu_model: Fact<String>,
    pub cpu_cores: Fact<u32>,
    pub ram_bytes: Fact<u64>,
    pub cpu_arch: Fact<String>,
    pub os: Fact<String>,
    pub kernel: Fact<String>,
    /// Filesystem type backing the working root the scenarios write to.
    pub filesystem: Fact<String>,
    pub docker_server_version: Fact<String>,
    /// The API version the client actually negotiated with the daemon.
    pub docker_api_version: Fact<String>,
    pub docker_storage_driver: Fact<String>,
    pub buildkit_version: Fact<String>,
    pub velnor_commit: Fact<String>,
    pub fixture_commit: Fact<String>,
    pub rustc_version: Fact<String>,
    pub cargo_version: Fact<String>,
    pub mbx_version: Fact<String>,
    /// Content digest of the job image, never its mutable tag.
    pub job_image_digest: Fact<String>,
    pub runner: RunnerConfiguration,
}

/// Where the harness should look for the identities it cannot derive itself.
#[derive(Debug, Clone)]
pub struct ProbeInputs {
    /// Velnor checkout whose commit identifies the code under measurement.
    pub velnor_repo: std::path::PathBuf,
    /// Actions fixture checkout the scenarios drive.
    pub fixture_repo: Option<std::path::PathBuf>,
    /// Working root the scenarios write into; its filesystem is recorded.
    pub work_root: std::path::PathBuf,
    /// Job image reference whose digest is recorded.
    pub job_image: Option<String>,
    /// Runner config base, when a registered runner is present.
    pub runner_config_dir: Option<std::path::PathBuf>,
}

impl EnvironmentIdentity {
    /// Probe the host. Never fails: an unanswerable question becomes a recorded
    /// reason, so the resulting record is always schema-complete.
    #[must_use]
    pub fn probe(inputs: &ProbeInputs, runner: &mut Runner) -> Self {
        Self {
            schema: ENVIRONMENT_SCHEMA.to_owned(),
            cpu_model: cpu_model(runner).into(),
            cpu_cores: cpu_cores(runner).into(),
            ram_bytes: ram_bytes(runner).into(),
            cpu_arch: Fact::Known(std::env::consts::ARCH.to_owned()),
            os: Fact::Known(std::env::consts::OS.to_owned()),
            kernel: runner.capture("uname", &["-sr"]).into(),
            filesystem: filesystem_type(&inputs.work_root, runner).into(),
            docker_server_version: runner
                .capture("docker", &["version", "--format", "{{.Server.Version}}"])
                .into(),
            docker_api_version: runner
                .capture("docker", &["version", "--format", "{{.Client.APIVersion}}"])
                .into(),
            docker_storage_driver: runner
                .capture("docker", &["info", "--format", "{{.Driver}}"])
                .into(),
            buildkit_version: buildkit_version(runner).into(),
            velnor_commit: git_commit(&inputs.velnor_repo, runner).into(),
            fixture_commit: inputs.fixture_repo.as_ref().map_or_else(
                || Fact::unavailable("no actions fixture checkout was supplied"),
                |repo| git_commit(repo, runner).into(),
            ),
            rustc_version: runner.capture("rustc", &["--version"]).into(),
            cargo_version: runner.capture("cargo", &["--version"]).into(),
            mbx_version: runner.capture("mbx", &["--version"]).into(),
            job_image_digest: inputs.job_image.as_ref().map_or_else(
                || Fact::unavailable("no job image was supplied"),
                |image| image_digest(image, runner).into(),
            ),
            runner: runner_configuration(inputs, runner),
        }
    }

    /// Fields the host could not answer, as `field: reason` pairs.
    #[must_use]
    pub fn gaps(&self) -> Vec<(&'static str, String)> {
        let mut gaps = Vec::new();
        let mut push = |name: &'static str, reason: Option<&str>| {
            if let Some(reason) = reason {
                gaps.push((name, reason.to_owned()));
            }
        };
        push("cpu_model", self.cpu_model.reason());
        push("cpu_cores", self.cpu_cores.reason());
        push("ram_bytes", self.ram_bytes.reason());
        push("cpu_arch", self.cpu_arch.reason());
        push("os", self.os.reason());
        push("kernel", self.kernel.reason());
        push("filesystem", self.filesystem.reason());
        push("docker_server_version", self.docker_server_version.reason());
        push("docker_api_version", self.docker_api_version.reason());
        push("docker_storage_driver", self.docker_storage_driver.reason());
        push("buildkit_version", self.buildkit_version.reason());
        push("velnor_commit", self.velnor_commit.reason());
        push("fixture_commit", self.fixture_commit.reason());
        push("rustc_version", self.rustc_version.reason());
        push("cargo_version", self.cargo_version.reason());
        push("mbx_version", self.mbx_version.reason());
        push("job_image_digest", self.job_image_digest.reason());
        push("runner.slots", self.runner.slots.reason());
        push("runner.backend", self.runner.backend.reason());
        push("runner.labels", self.runner.labels.reason());
        push("runner.config_dir", self.runner.config_dir.reason());
        gaps
    }

    /// True when a Docker daemon answered the version probe.
    #[must_use]
    pub const fn has_docker(&self) -> bool {
        self.docker_server_version.is_known()
    }

    /// True when a runner configuration was found.
    #[must_use]
    pub const fn has_runner(&self) -> bool {
        self.runner.slots.is_known()
    }
}

fn cpu_model(runner: &mut Runner) -> Result<String, String> {
    if cfg!(target_os = "macos") {
        return runner.capture("sysctl", &["-n", "machdep.cpu.brand_string"]);
    }
    let text = std::fs::read_to_string("/proc/cpuinfo")
        .map_err(|error| format!("/proc/cpuinfo unreadable: {error}"))?;
    parse_cpuinfo_model(&text).ok_or_else(|| "/proc/cpuinfo has no model name".to_owned())
}

fn parse_cpuinfo_model(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            matches!(key.trim(), "model name" | "Model" | "cpu model")
                .then(|| value.trim().to_owned())
        })
        .filter(|model| !model.is_empty())
}

fn cpu_cores(runner: &mut Runner) -> Result<u32, String> {
    let key = if cfg!(target_os = "macos") {
        "hw.ncpu"
    } else {
        return std::thread::available_parallelism()
            .map(|count| count.get() as u32)
            .map_err(|error| format!("available_parallelism failed: {error}"));
    };
    runner
        .capture("sysctl", &["-n", key])?
        .parse()
        .map_err(|error| format!("sysctl {key} was not an integer: {error}"))
}

fn ram_bytes(runner: &mut Runner) -> Result<u64, String> {
    if cfg!(target_os = "macos") {
        return runner
            .capture("sysctl", &["-n", "hw.memsize"])?
            .parse()
            .map_err(|error| format!("sysctl hw.memsize was not an integer: {error}"));
    }
    let text = std::fs::read_to_string("/proc/meminfo")
        .map_err(|error| format!("/proc/meminfo unreadable: {error}"))?;
    parse_meminfo_total(&text).ok_or_else(|| "/proc/meminfo has no MemTotal".to_owned())
}

fn parse_meminfo_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes.saturating_mul(1024))
}

fn filesystem_type(path: &Path, runner: &mut Runner) -> Result<String, String> {
    let path = path.display().to_string();
    if cfg!(target_os = "macos") {
        // `df -P` names the device; `mount` names its filesystem type.
        let df = runner.capture("df", &["-P", &path])?;
        let device = df
            .lines()
            .nth(1)
            .and_then(|line| line.split_whitespace().next())
            .ok_or_else(|| format!("df -P {path} produced no device row"))?
            .to_owned();
        let mounts = runner.capture("mount", &[] as &[&str])?;
        return parse_darwin_mount_type(&mounts, &device)
            .ok_or_else(|| format!("no mount entry for {device}"));
    }
    runner.capture("stat", &["-f", "-c", "%T", &path])
}

fn parse_darwin_mount_type(mounts: &str, device: &str) -> Option<String> {
    let line = mounts
        .lines()
        .find(|line| line.split_whitespace().next() == Some(device))?;
    let open = line.rfind('(')?;
    let close = line.rfind(')')?;
    let inside = line.get(open + 1..close)?;
    let kind = inside.split(',').next()?.trim();
    (!kind.is_empty()).then(|| kind.to_owned())
}

fn buildkit_version(runner: &mut Runner) -> Result<String, String> {
    runner.capture("docker", &["buildx", "version"])
}

fn git_commit(repo: &Path, runner: &mut Runner) -> Result<String, String> {
    runner.capture(
        "git",
        &["-C", &repo.display().to_string(), "rev-parse", "HEAD"],
    )
}

fn image_digest(image: &str, runner: &mut Runner) -> Result<String, String> {
    // The identity of the image under measurement is not optional, and the
    // image may legitimately not be on the host yet. Pull it here rather than
    // recording a gap that a later scenario would silently fill.
    let present = runner
        .run("docker", &["image", "inspect", image])
        .map(|invocation| invocation.ok())
        .unwrap_or(false);
    if !present {
        runner.capture("docker", &["pull", image])?;
    }
    // RepoDigests is the content identity; fall back to the local image ID,
    // which is still a digest, when the image was never pushed or pulled.
    if let Ok(digests) = runner.capture(
        "docker",
        &[
            "image",
            "inspect",
            image,
            "--format",
            "{{join .RepoDigests \",\"}}",
        ],
    ) && let Some(first) = digests
        .split(',')
        .next()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        return Ok(first.to_owned());
    }
    runner.capture(
        "docker",
        &["image", "inspect", image, "--format", "{{.Id}}"],
    )
}

fn runner_configuration(inputs: &ProbeInputs, runner: &mut Runner) -> RunnerConfiguration {
    let Some(dir) = inputs.runner_config_dir.as_ref() else {
        let reason = "no runner config directory was supplied; \
                      pass --runner-config-dir for a registered host";
        return RunnerConfiguration {
            slots: Fact::unavailable(reason),
            backend: Fact::unavailable(reason),
            labels: Fact::unavailable(reason),
            config_dir: Fact::unavailable(reason),
        };
    };
    let _ = runner;
    if !dir.is_dir() {
        let reason = format!("{} is not a directory", dir.display());
        return RunnerConfiguration {
            slots: Fact::unavailable(reason.clone()),
            backend: Fact::unavailable(reason.clone()),
            labels: Fact::unavailable(reason.clone()),
            config_dir: Fact::unavailable(reason),
        };
    }
    RunnerConfiguration {
        slots: count_slots(dir).into(),
        backend: read_backend(dir).into(),
        labels: read_labels(dir).into(),
        config_dir: Fact::Known(dir.display().to_string()),
    }
}

/// Slot count is the number of `slot-*` config bases the daemon manages.
fn count_slots(dir: &Path) -> Result<u32, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|error| format!("{} unreadable: {error}", dir.display()))?;
    let slots = entries
        .flatten()
        .filter(|entry| {
            entry.file_name().to_string_lossy().starts_with("slot-") && entry.path().is_dir()
        })
        .count();
    u32::try_from(slots).map_err(|_| "slot count does not fit in u32".to_owned())
}

fn read_backend(dir: &Path) -> Result<String, String> {
    let path = dir.join("execution.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("{} unreadable: {error}", path.display()))?;
    parse_toml_string(&text, "backend")
        .ok_or_else(|| format!("{} declares no backend", path.display()))
}

fn read_labels(dir: &Path) -> Result<Vec<String>, String> {
    let path = dir.join("labels");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("{} unreadable: {error}", path.display()))?;
    let labels: Vec<String> = text
        .split([',', '\n'])
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .collect();
    if labels.is_empty() {
        return Err(format!("{} lists no labels", path.display()));
    }
    Ok(labels)
}

/// Minimal `key = "value"` reader for the one field we need out of the runner's
/// execution config. Anything richer belongs to the runner's own parser.
fn parse_toml_string(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
        })
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> ProbeInputs {
        ProbeInputs {
            velnor_repo: std::env::current_dir().expect("cwd"),
            fixture_repo: None,
            work_root: std::env::temp_dir(),
            job_image: None,
            runner_config_dir: None,
        }
    }

    #[test]
    fn probe_is_schema_complete_and_round_trips() {
        let mut runner = Runner::new();
        let identity = EnvironmentIdentity::probe(&inputs(), &mut runner);
        assert_eq!(identity.schema, ENVIRONMENT_SCHEMA);
        let json = serde_json::to_string(&identity).expect("serialise");
        let parsed: EnvironmentIdentity = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, identity);
        // Architecture and CPU model are distinct fields; the replaced script
        // conflated them.
        assert!(identity.cpu_arch.is_known());
        assert_ne!(
            identity.cpu_model.known().map(String::as_str),
            Some(std::env::consts::ARCH)
        );
    }

    #[test]
    fn a_record_missing_a_field_is_rejected_by_the_schema() {
        let mut runner = Runner::new();
        let identity = EnvironmentIdentity::probe(&inputs(), &mut runner);
        let mut value = serde_json::to_value(&identity).expect("serialise");
        value
            .as_object_mut()
            .expect("object")
            .remove("cpu_model")
            .expect("cpu_model present");
        let error = serde_json::from_value::<EnvironmentIdentity>(value)
            .expect_err("missing field must be rejected");
        assert!(error.to_string().contains("cpu_model"), "{error}");
    }

    #[test]
    fn an_absent_runner_is_recorded_as_a_reason_not_a_hole() {
        let mut runner = Runner::new();
        let identity = EnvironmentIdentity::probe(&inputs(), &mut runner);
        assert!(!identity.has_runner());
        let gaps = identity.gaps();
        assert!(
            gaps.iter().any(|(field, _)| *field == "runner.slots"),
            "{gaps:?}"
        );
        assert!(identity.runner.slots.reason().is_some());
    }

    #[test]
    fn cpuinfo_model_is_the_brand_string() {
        let text = "processor\t: 0\nmodel name\t: AMD EPYC 7763 64-Core Processor\n";
        assert_eq!(
            parse_cpuinfo_model(text).as_deref(),
            Some("AMD EPYC 7763 64-Core Processor")
        );
        assert_eq!(parse_cpuinfo_model("processor\t: 0\n"), None);
    }

    #[test]
    fn meminfo_total_is_converted_to_bytes() {
        assert_eq!(
            parse_meminfo_total("MemTotal:       16302636 kB\nMemFree: 1 kB\n"),
            Some(16_693_899_264)
        );
        assert_eq!(parse_meminfo_total("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn darwin_mount_type_is_read_from_the_parenthesised_list() {
        let mounts = "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
                      map auto_home on /System/Volumes/Data/home (autofs, automounted)\n";
        assert_eq!(
            parse_darwin_mount_type(mounts, "/dev/disk3s1s1").as_deref(),
            Some("apfs")
        );
        assert_eq!(
            parse_darwin_mount_type(mounts, "/dev/disk9").as_deref(),
            None
        );
    }

    #[test]
    fn backend_is_read_from_execution_config() {
        let text = "# comment = \"ignored\"\nbackend = \"docker\"\nslots = 4\n";
        assert_eq!(
            parse_toml_string(text, "backend").as_deref(),
            Some("docker")
        );
        assert_eq!(parse_toml_string(text, "comment"), None);
        assert_eq!(parse_toml_string(text, "missing"), None);
    }

    #[test]
    fn slot_count_and_labels_come_from_a_real_config_base() {
        let dir = std::env::temp_dir().join(format!("velnor-bench-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("slot-0")).expect("create");
        std::fs::create_dir_all(dir.join("slot-1")).expect("create");
        std::fs::write(dir.join("execution.toml"), "backend = \"docker\"\n").expect("write");
        std::fs::write(dir.join("labels"), "self-hosted, velnor\n").expect("write");

        let probe = ProbeInputs {
            runner_config_dir: Some(dir.clone()),
            ..inputs()
        };
        let mut runner = Runner::new();
        let configuration = runner_configuration(&probe, &mut runner);
        assert_eq!(configuration.slots, Fact::Known(2));
        assert_eq!(configuration.backend, Fact::Known("docker".to_owned()));
        assert_eq!(
            configuration.labels,
            Fact::Known(vec!["self-hosted".to_owned(), "velnor".to_owned()])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

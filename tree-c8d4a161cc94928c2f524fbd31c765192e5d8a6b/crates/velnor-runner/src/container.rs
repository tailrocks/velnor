#![allow(dead_code)]

use std::{
    fs, io,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use crate::container::host_budget::{HostBudget, SlotBudget};
use crate::docker_argv::{DockerArgv, DockerCommand, FlagSink, ImageReference};

/// One derived resource budget for the machine, and the share of it that
/// belongs to a single runner slot. Every job container is sized from it.
pub(crate) mod host_budget;

pub use crate::docker_argv::PreparedDockerArgs;

const NODE_ACTION_BASE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const JOB_NOFILE_LIMIT: &str = "65536:65536";
const JOB_DOCKER_HOST: &str = "unix:///var/run/docker.sock";

fn is_docker_control_env(name: &str) -> bool {
    name.eq_ignore_ascii_case("DOCKER_HOST")
        || name.eq_ignore_ascii_case("DOCKER_CONTEXT")
        || name.eq_ignore_ascii_case("DOCKER_CONFIG")
}

fn append_flags_without_limits(
    command: &mut impl FlagSink,
    options: &[String],
    strip_cpu: bool,
    strip_memory: bool,
) {
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        if strip_cpu && option == "--cpus" {
            index += 1;
            if options
                .get(index)
                .is_some_and(|value| !value.starts_with('-'))
            {
                index += 1;
            }
            continue;
        }
        if strip_cpu && option.starts_with("--cpus=") {
            index += 1;
            continue;
        }
        if strip_memory && option == "--memory" {
            index += 1;
            if options
                .get(index)
                .is_some_and(|value| !value.starts_with('-'))
            {
                index += 1;
            }
            continue;
        }
        if strip_memory && option.starts_with("--memory=") {
            index += 1;
            continue;
        }
        command.flag(option.clone());
        index += 1;
    }
}

fn parse_memory_options(options: &[String]) -> io::Result<Vec<u64>> {
    let mut limits = Vec::new();
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        let value = if option == "--memory" {
            let value = options.get(index + 1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "docker --memory option is missing its value",
                )
            })?;
            if value.starts_with('-') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "docker --memory option has an invalid value",
                ));
            }
            index += 2;
            value.as_str()
        } else if let Some(value) = option.strip_prefix("--memory=") {
            index += 1;
            value
        } else {
            index += 1;
            continue;
        };
        let Some(bytes) = parse_docker_memory_bytes(value) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "docker --memory option has an invalid value",
            ));
        };
        limits.push(bytes);
    }
    Ok(limits)
}

/// Parse the RAM units accepted by Docker's `--memory` flag. Normalize to
/// bytes before comparing limits so a larger textual unit cannot bypass the
/// derived per-slot cap.
fn parse_docker_memory_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    let split = value
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split);
    if number.is_empty() || number == "." || number.matches('.').count() > 1 {
        return None;
    }
    let number = number.parse::<f64>().ok()?;
    if !number.is_finite() || number <= 0.0 {
        return None;
    }
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1_u64,
        "k" | "kb" | "ki" | "kib" => 1_u64 << 10,
        "m" | "mb" | "mi" | "mib" => 1_u64 << 20,
        "g" | "gb" | "gi" | "gib" => 1_u64 << 30,
        "t" | "tb" | "ti" | "tib" => 1_u64 << 40,
        "p" | "pb" | "pi" | "pib" => 1_u64 << 50,
        _ => return None,
    };
    let bytes = number * multiplier as f64;
    if !bytes.is_finite() || bytes < 1.0 || bytes > u64::MAX as f64 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(bytes.floor() as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreTrustClass {
    Untrusted,
    Trusted,
    Release,
}

#[derive(Debug, Clone)]
pub struct JobContainerSpec {
    pub name: String,
    pub image: String,
    pub network: String,
    pub workspace_host: PathBuf,
    pub temp_host: PathBuf,
    pub home_host: PathBuf,
    pub actions_host: PathBuf,
    pub tools_host: PathBuf,
    pub mount_docker_socket: bool,
    /// Validated daemon slot count carried into this job. Resource budgeting
    /// must not infer topology from this job's filesystem layout.
    pub slot_count: NonZeroU32,
    pub env: Vec<(String, String)>,
    /// Daemon-enforced Docker resource limits. CPU and memory limits are
    /// normalized with workflow createOptions into runner-owned values before
    /// emission.
    pub resource_options: Vec<String>,
    pub options: Vec<String>,
    pub services: Vec<ServiceContainerSpec>,
    pub node_action_image: String,
    pub docker_cli_host_path: Option<PathBuf>,
    pub docker_cli_plugin_host_dir: Option<PathBuf>,
    pub docker_host_work_dir: Option<PathBuf>,
    pub verify_bind_mounts: bool,
    pub daemon_id: String,
    pub repository: Option<String>,
    /// Host-persistent incremental-build generation. The runner reflink/copies
    /// it into the job-local workspace target after checkout and publishes the
    /// completed job tree back atomically. It is never a nested bind mount:
    /// one would make rename(2) across `target` return EXDEV even though the
    /// same workflow succeeds on GitHub-hosted runners.
    pub cargo_target_host: Option<PathBuf>,
    /// Trust namespace admitted for executable and build stores.
    pub store_trust_class: StoreTrustClass,
    /// Docker-only Mr Boxington store. `None` for MicroVM jobs.
    pub mbx_store_host: Option<PathBuf>,
    /// Docker-only explicit sccache action store.
    pub sccache_store_host: Option<PathBuf>,
}

/// Job environment that carries the daemon's resource budget. A workflow that
/// sets any of these is asking for a share of a machine it cannot see, so the
/// daemon's derived value wins and the override is named in the notice.
const BUDGET_ENV: [&str; 4] = [
    "CARGO_BUILD_JOBS",
    "MAKEFLAGS",
    "MBX_SCHEDULER_CPUS",
    "MBX_SCHEDULER_MEMORY",
];

const DEFAULT_CONTAINER_EXEC_PATH: &str =
    "/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const MBX_CONTAINER_EXEC_PATH: &str =
    "/opt/mbx/bin:/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

impl JobContainerSpec {
    /// The tightest valid `--cpus` limit declared by the operator
    /// (`--job-cpus`/`VELNOR_JOB_CPUS`) or workflow createOptions.
    fn declared_container_cpus(&self) -> Option<f64> {
        [&self.resource_options, &self.options]
            .into_iter()
            .flat_map(|options| {
                options
                    .windows(2)
                    .filter(|pair| pair[0] == "--cpus")
                    .map(|pair| pair[1].as_str())
                    .chain(
                        options
                            .iter()
                            .filter_map(|option| option.strip_prefix("--cpus=")),
                    )
                    .collect::<Vec<_>>()
            })
            .filter_map(|value| value.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .min_by(f64::total_cmp)
    }

    /// The tightest valid Docker memory limit declared by the operator or
    /// workflow createOptions. Invalid or incomplete limits are rejected by
    /// `start_args` instead of being silently dropped while enforcing policy.
    fn declared_container_memory(&self) -> io::Result<Option<u64>> {
        let mut limits = Vec::new();
        for options in [&self.options, &self.resource_options] {
            limits.extend(parse_memory_options(options)?);
        }
        Ok(limits.into_iter().min())
    }

    /// The share of the machine this job may use.
    ///
    /// The packaged `velnor-jobs.slice` quota is an *aggregate* ceiling for
    /// every job on the host, not a per-slot allowance. Nothing used to divide
    /// it, so four slots each ran a Cargo build sized to the whole machine.
    fn slot_budget(&self) -> SlotBudget {
        HostBudget::observe_host()
            .per_slot(self.slot_count)
            .capped_by_container_cpus(self.declared_container_cpus())
    }

    /// One line naming the budget, how it was derived, and any workflow value
    /// it overrode. Resource policy that nobody can see is indistinguishable
    /// from the mysterious idleness it is meant to remove.
    pub fn resource_budget_notice(&self) -> String {
        let budget = self.slot_budget();
        budget.notice(&self.workflow_budget_overrides())
    }

    fn workflow_budget_overrides(&self) -> Vec<String> {
        BUDGET_ENV
            .iter()
            .filter(|name| {
                self.env
                    .iter()
                    .any(|(key, _)| key.eq_ignore_ascii_case(name))
            })
            .map(|name| (*name).to_owned())
            .collect()
    }

    /// Append workflow and daemon flags while replacing every declared CPU
    /// limit with the runner-derived hard cap. Docker's last duplicate flag
    /// wins, so leaving an operator value in argv would let it silently defeat
    /// the smaller per-slot limit.
    fn append_container_options(
        &self,
        command: &mut DockerCommand,
        budget: &SlotBudget,
    ) -> io::Result<()> {
        let cpu_option = budget.docker_cpu_option();
        let memory_cap = budget
            .memory_bytes
            .value()
            .copied()
            .filter(|bytes| *bytes > 0);
        let declared_memory = self.declared_container_memory()?;

        if cpu_option.is_some() || memory_cap.is_some() {
            append_flags_without_limits(
                command,
                &self.options,
                cpu_option.is_some(),
                memory_cap.is_some(),
            );
            append_flags_without_limits(
                command,
                &self.resource_options,
                cpu_option.is_some(),
                memory_cap.is_some(),
            );
        } else {
            // No derived hard cap exists. Retain explicit policy instead of
            // widening the container by deleting the only known limit.
            command.flags(self.options.iter().cloned());
            command.flags(self.resource_options.iter().cloned());
        }

        if let Some(cpu_option) = cpu_option {
            command.flags(cpu_option);
        }
        if let Some(memory_cap) = memory_cap {
            // An explicit operator/workflow value may narrow the derived
            // share, never widen it. The final Docker flag is the one source
            // of truth after all user options have been expanded.
            let memory = declared_memory.map_or(memory_cap, |declared| declared.min(memory_cap));
            command.flags(["--memory".to_owned(), memory.to_string()]);
        }
        Ok(())
    }

    /// Daemon-derived CPU and memory budget for this job.
    ///
    /// Appended after workflow environment and workflow createOptions, so a
    /// workflow cannot widen its own share — the same precedence the
    /// acceleration stores and `resource_options` already use. Cargo and
    /// `make` are capped through `CARGO_BUILD_JOBS` and `MAKEFLAGS`, which is
    /// also mbx's documented way to cap one build's permits without shrinking
    /// the machine-wide pool its siblings share, and mbx's own scheduler is
    /// sized through its documented `MBX_SCHEDULER_*` contract instead of its
    /// "logical CPUs / 85% of physical memory" defaults, neither of which can
    /// see the slice quota. An unobservable budget produces nothing at all.
    fn append_resource_budget(&self, command: &mut DockerCommand, budget: &SlotBudget) {
        for (name, value) in budget.job_env() {
            command.env(name, value);
        }
        command.env(
            "VELNOR_JOB_BUDGET",
            budget.notice(&self.workflow_budget_overrides()),
        );
    }

    fn append_rust_acceleration(&self, command: &mut DockerCommand) {
        if let Some(host) = &self.mbx_store_host {
            command.pair("-v", self.mount_arg(host, "/var/cache/mbx"));
            command.envs([
                ("MBX_CACHE_DIR", "/var/cache/mbx"),
                ("MBX_TARGET_ROOT", "/var/cache/mbx/targets"),
                ("MBX_GC_AUTO", "true"),
                ("MBX_GC_MAX_SIZE", "20GiB"),
                ("MBX_TARGET_MAX_SIZE", "30GiB"),
                ("MBX_GC_MAX_TOTAL_SIZE", "50GiB"),
            ]);
        } else if let Some(host) = &self.sccache_store_host {
            command.pair("-v", self.mount_arg(host, "/var/cache/sccache"));
            command.envs([
                ("MBX_DISABLE", "1"),
                ("RUSTC_WRAPPER", "sccache"),
                ("SCCACHE_DIR", "/var/cache/sccache"),
                ("SCCACHE_CACHE_SIZE", "20G"),
                ("SCCACHE_GHA_ENABLED", "false"),
            ]);
        }
    }

    /// Directory that holds the mode-0600 env files backing this job's Docker
    /// commands. Job-scoped, so it disappears with the job temp tree.
    pub(crate) fn env_dir(&self) -> PathBuf {
        self.temp_host.join("_velnor").join("exec-env")
    }

    /// Validated image for this job container.
    ///
    /// # Errors
    /// The configured image is not an OCI reference.
    fn image_reference(&self) -> io::Result<ImageReference> {
        Ok(ImageReference::parse(&self.image)?)
    }

    pub fn create_network_args(&self) -> Vec<String> {
        let mut command = DockerArgv::new(["network", "create"]);
        command.flags([
            "--label".to_owned(),
            format!("velnor.daemon-id={}", self.daemon_id),
            "--label".to_owned(),
            format!("velnor.job-id={}", self.name),
        ]);
        command.operands().operand(self.network.clone()).into_argv()
    }

    /// `docker run` for the job container.
    ///
    /// # Errors
    /// The job image is not a valid OCI reference, or an env file backing the
    /// job environment could not be created.
    pub fn start_args(&self) -> io::Result<PreparedDockerArgs> {
        let image = self.image_reference()?;
        let mut command = DockerCommand::new(self.env_dir(), ["run"]);
        let args = &mut command;
        args.flags([
            "--detach".into(),
            "--add-host".into(),
            // Standard alias for host services (GitHub-hosted runners expose
            // the same name). The gha cache service and future daemon-side
            // endpoints are reached as http://host.docker.internal:<port>.
            "host.docker.internal:host-gateway".into(),
            "--name".into(),
            self.name.clone(),
            "--workdir".into(),
            "/__w".into(),
            "-v".into(),
            self.mount_arg(&self.workspace_host, "/__w"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/__t"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/tmp"),
            "-v".into(),
            self.mount_arg(
                &self.temp_host,
                &self.docker_host_path(&self.temp_host).display().to_string(),
            ),
            "-v".into(),
            self.mount_arg(
                &self.workspace_host,
                &self
                    .docker_host_path(&self.workspace_host)
                    .display()
                    .to_string(),
            ),
            "-v".into(),
            self.mount_arg(&self.home_host, "/github/home"),
            // Playwright's browser payload is a versioned download cache, not
            // workspace output. Persist it per trust/repository so unchanged
            // jobs do not download Chromium and FFmpeg on every fresh container.
            "-v".into(),
            self.mount_arg(
                &self.playwright_browser_store_host(),
                "/github/home/.cache/ms-playwright",
            ),
            // Share immutable Cargo downloads and indexes across the daemon,
            // but keep extracted registry sources and git checkouts in the
            // job home. Separate containers can otherwise race while creating
            // `.cargo-ok` in the same extracted crate (Cargo's package-cache
            // lock does not serialize that mutation across container jobs).
            "-v".into(),
            self.mount_arg(
                &cargo_store_host(&self.temp_host).join("registry/cache"),
                "/github/home/.cargo/registry/cache",
            ),
            "-v".into(),
            self.mount_arg(
                &cargo_store_host(&self.temp_host).join("registry/index"),
                "/github/home/.cargo/registry/index",
            ),
            "-v".into(),
            self.mount_arg(
                &cargo_store_host(&self.temp_host).join("git/db"),
                "/github/home/.cargo/git/db",
            ),
            // $CARGO_HOME/bin holds executable proxies on PATH, so it is
            // shared only inside one trust/repository scope. Registry/git data
            // above stays daemon-shared for warmth because cargo does not
            // execute files directly from those caches.
            "-v".into(),
            self.mount_arg(
                &self.cargo_executable_store_host(),
                "/github/home/.cargo/bin",
            ),
            // Host-persistent mise tool store: installed tools are executable
            // and mutable (managed runtimes can receive global packages after
            // setup), so `installs` is scoped by slot + trust/repository. One
            // job owns a slot at a time, preventing cross-job npm/pip/cargo
            // mutation races while keeping later jobs on that slot warm. The
            // download-only cache remains daemon-shared.
            "-v".into(),
            self.mount_arg(&self.mise_executable_store_host(), "/opt/mise/installs"),
            // Persistent per-version mise BINARY store (Plan 008 Step 2). Scoped
            // by trust/repository like `installs`; the setup script publishes a
            // verified `<os-arch>/<exact-version>/mise` here so a fresh job
            // reuses it. `/opt/mise/bin` stays the read-only baked bootstrap.
            "-v".into(),
            self.mount_arg(&self.mise_binary_store_host(), "/opt/velnor/mise-binaries"),
            "-v".into(),
            self.mount_arg(
                &mise_store_host(&self.temp_host).join("cache"),
                "/opt/mise/cache",
            ),
            "-v".into(),
            self.mount_arg(&workflow_host(&self.temp_host), "/github/workflow"),
            "-v".into(),
            format!("{}:ro", self.mount_arg(&self.actions_host, "/__a")),
            "-v".into(),
            self.mount_arg(&self.tools_host, "/__tool"),
        ]);
        args.env("HOME", "/github/home");
        args.env("DOCKER_HOST", JOB_DOCKER_HOST);
        args.env("RUSTUP_HOME", "/root/.rustup");
        args.env("CARGO_HOME", "/github/home/.cargo");
        args.env("RUNNER_TEMP", "/__t");
        args.env("RUNNER_TOOL_CACHE", "/__tool");
        args.env("AGENT_TOOLSDIRECTORY", "/__tool");
        args.env(
            "VELNOR_DOCKER_HOST_TEMP",
            self.docker_host_path(&self.temp_host).display().to_string(),
        );
        args.env(
            "VELNOR_DOCKER_HOST_WORKSPACE",
            self.docker_host_path(&self.workspace_host)
                .display()
                .to_string(),
        );
        for (name, value) in &self.env {
            if is_docker_control_env(name) {
                continue;
            }
            args.env(name.clone(), value.clone());
        }
        self.append_ownership_labels(args);
        let budget = self.slot_budget();
        self.append_container_options(args, &budget)?;
        // Docker creates the actual workload in dockerd's cgroup, not in the
        // Velnor worker process. Keep the outer job below the package-owned
        // aggregate cap; the job lease proxy applies the same policy to
        // containers created from inside the job.
        self.append_job_cgroup_parent(args);

        // Docker Engine 29 inherits systemd's 1024-file descriptor default
        // when no container limit is explicit. Large Rust/Zig links open one
        // descriptor per object and fail with ProcessFdQuotaExceeded. Make the
        // job contract deterministic and large enough for GitHub-scale builds.
        args.flags(["--ulimit".to_owned(), format!("nofile={JOB_NOFILE_LIMIT}")]);

        // GitHub-hosted Ubuntu jobs expose localhost over IPv4. Docker also
        // assigns localhost to ::1, which can split same-process servers and
        // clients across address families (for example Vite binds ::1 while
        // Bun fetches 127.0.0.1). Keep loopback behavior lane-identical.
        args.flags(["--sysctl", "net.ipv6.conf.all.disable_ipv6=1"]);

        self.append_docker_socket_mount(args);
        self.append_docker_cli_mounts(args);

        // The per-job network is runner policy. Keep it after expanded job
        // and daemon resource options so the job cannot be displaced from the
        // network shared with its workflow services.
        args.pair("--network", self.network.clone());

        // Daemon-owned acceleration mounts and variables must follow both
        // workflow environment and expanded container options. A trusted job
        // may add options, but cannot redirect a persistent store or stack
        // sccache with the image's mbx shim.
        self.append_rust_acceleration(args);

        // The machine budget divided by the number of provisioned slots. Last,
        // so neither workflow environment nor workflow createOptions can widen
        // this job's share of a host it cannot see.
        self.append_resource_budget(args, &budget);

        // PID 1 tails a live console file instead of /dev/null, so
        // `docker logs <job-container>` mirrors the GitHub UI step output.
        // Velnor appends each masked step's lines to this file (mounted at
        // /__t). `tail -F` waits for the file if it does not exist yet.
        command
            .image(&image)
            .operands([
                "sh",
                "-c",
                "mkdir -p /__t/_velnor && touch /__t/_velnor/console.log && exec tail -n +1 -F /__t/_velnor/console.log",
            ])
            .finish()
    }

    /// `docker run` args that copy the job image's baked /opt/mise installs +
    /// cache into the shared host store without clobbering newer entries.
    /// Mounting an (initially empty) shared store over /opt/mise/installs
    /// shadows the image-baked tools while the baked shims keep pointing at
    /// them — observed live as `mise ERROR gh is not a valid shim` on a fresh
    /// store. Seeding once per image digest removes that class.
    ///
    /// # Errors
    /// The job image is not a valid OCI reference.
    pub fn seed_mise_store_args(&self) -> io::Result<Vec<String>> {
        let image = self.image_reference()?;
        let store = mise_store_host(&self.temp_host);
        let mut args = DockerArgv::new(["run"]);
        args.flags([
            "--rm".to_owned(),
            "--entrypoint".to_owned(),
            "sh".to_owned(),
            "--name".to_owned(),
            format!("velnor-mise-seed-{}", self.name),
        ]);
        // This is a transient job-owned container, not an anonymous helper.
        // Labels let crash recovery distinguish it from co-located Docker
        // workloads if the CLI dies before Docker processes `--rm`.
        self.append_ownership_labels(&mut args);
        self.append_job_cgroup_parent(&mut args);
        args.flags([
            "-v".to_owned(),
            self.mount_arg(
                &self.mise_executable_store_host(),
                "/__velnor_seed/installs",
            ),
            "-v".to_owned(),
            self.mount_arg(&store.join("cache"), "/__velnor_seed/cache"),
        ]);
        Ok(args
            .image(&image)
            .operands([
                "-c",
                "cp -an /opt/mise/installs/. /__velnor_seed/installs/ 2>/dev/null || true; \
                 cp -an /opt/mise/cache/. /__velnor_seed/cache/ 2>/dev/null || true",
            ])
            .into_argv())
    }

    pub fn prepare_exec_script_args(
        &self,
        script_path_in_container: &str,
        shell: Shell,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
    ) -> io::Result<PreparedDockerArgs> {
        self.prepare_exec_process_args(
            working_directory,
            env,
            secret_masks,
            &shell.command_args(script_path_in_container),
        )
    }

    /// # Errors
    /// Creating an env file for the step environment failed, or a masked
    /// secret would have reached argv.
    pub fn prepare_exec_process_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        command: &[String],
    ) -> io::Result<PreparedDockerArgs> {
        self.exec_command(working_directory, env, secret_masks, command, false)
    }

    /// Like prepare_exec_process_args, but with stdin kept open (`docker exec
    /// -i`) so the caller can stream data (for example a registry password).
    ///
    /// # Errors
    /// Creating an env file for the step environment failed, or a masked
    /// secret would have reached argv.
    pub fn prepare_exec_process_stdin_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        command: &[String],
    ) -> io::Result<PreparedDockerArgs> {
        self.exec_command(working_directory, env, secret_masks, command, true)
    }

    fn exec_command(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        command: &[String],
        stdin: bool,
    ) -> io::Result<PreparedDockerArgs> {
        let mut builder = DockerCommand::new(self.env_dir(), ["exec"]);
        if stdin {
            builder.flag("-i");
        }
        builder.pair("--workdir", working_directory);
        self.append_base_exec_env(&mut builder);
        self.append_step_env(&mut builder, env);
        let prepared = builder
            .operands()
            .operand(self.name.clone())
            .operands(command.iter().cloned())
            .finish()?;
        audit_argv_for_secrets(prepared.args(), secret_masks)?;
        Ok(prepared)
    }

    /// Step environment. Every entry is recorded as an environment variable,
    /// never as an argv token: the builder decides between a mode-0600
    /// `--env-file` and a bare `-e NAME` process-environment forward.
    fn append_step_env(&self, command: &mut DockerCommand, env: &[(String, String)]) {
        for (name, value) in env {
            if is_docker_control_env(name) {
                continue;
            }
            command.env(name.clone(), value.clone());
        }
    }

    /// Truthful base env for every exec'd process: the job home is the
    /// bind-mounted /github/home (so `~` caches and docker client state
    /// persist on the host), the rustup toolchain store stays at the
    /// image-baked /root/.rustup, and cargo's registry/git live under the
    /// job home (backed by the host-persistent cargo store mounts).
    /// PATH resolves the image-baked rustup proxy before mise shims. Otherwise
    /// a shimmed tool such as `gh` can make mise probe shimmed `rustup`,
    /// recursively forking until the job exhausts its cgroup.
    /// Re-asserted per exec because OrbStack (macOS dev hosts) injects the
    /// host user's HOME into exec'd processes; explicit -e wins. Docker
    /// endpoint/context/config overrides from workflow and step env are
    /// dropped, so every in-container Docker client stays on the lease.
    fn append_base_exec_env(&self, command: &mut DockerCommand) {
        command.env("HOME", "/github/home");
        command.env("DOCKER_HOST", JOB_DOCKER_HOST);
        command.env("RUSTUP_HOME", "/root/.rustup");
        command.env("CARGO_HOME", "/github/home/.cargo");
        command.env("PATH", self.default_exec_path());
        command.env(
            "VELNOR_DOCKER_HOST_TEMP",
            self.docker_host_path(&self.temp_host).display().to_string(),
        );
        command.env(
            "VELNOR_DOCKER_HOST_WORKSPACE",
            self.docker_host_path(&self.workspace_host)
                .display()
                .to_string(),
        );
    }

    /// Runtime PATH for commands executed inside the job container. The image
    /// puts the MBX cargo shim first, but every `docker exec` receives an
    /// explicit PATH to defeat host-runtime injection; omitting it silently
    /// turns the default acceleration path back into ordinary Cargo.
    pub(crate) fn default_exec_path(&self) -> &'static str {
        if self.mbx_store_host.is_some() {
            MBX_CONTAINER_EXEC_PATH
        } else {
            DEFAULT_CONTAINER_EXEC_PATH
        }
    }

    /// `docker run` for a Node action sidecar.
    ///
    /// # Errors
    /// The node image is not a valid OCI reference, an env file could not be
    /// created, or a masked secret would have reached argv.
    pub fn prepare_run_node_action_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        path_prepend: &[String],
        node_image: &str,
        entrypoint_container_path: &str,
    ) -> io::Result<PreparedDockerArgs> {
        let image = ImageReference::parse(node_image)?;
        let mut command = DockerCommand::new(self.env_dir(), ["run"]);
        let args = &mut command;
        args.flags([
            "--rm".to_owned(),
            "--name".to_owned(),
            self.sidecar_container_name("node-action"),
            "--network".to_owned(),
            self.network.clone(),
            "--workdir".to_owned(),
            working_directory.to_owned(),
            "-v".to_owned(),
            self.mount_arg(&self.workspace_host, "/__w"),
            "-v".to_owned(),
            self.mount_arg(&self.workspace_host, "/github/workspace"),
            "-v".to_owned(),
            self.mount_arg(&self.temp_host, "/__t"),
            "-v".to_owned(),
            self.mount_arg(&self.temp_host, "/tmp"),
            "-v".to_owned(),
            self.mount_arg(&self.temp_host, "/github/runner_temp"),
            "-v".to_owned(),
            self.mount_arg(&self.temp_host, "/github/file_commands"),
            "-v".to_owned(),
            self.mount_arg(&self.home_host, "/github/home"),
            "-v".to_owned(),
            self.mount_arg(&workflow_host(&self.temp_host), "/github/workflow"),
            "-v".to_owned(),
            format!("{}:ro", self.mount_arg(&self.actions_host, "/__a")),
            "-v".to_owned(),
            self.mount_arg(&self.tools_host, "/__tool"),
        ]);
        args.env("HOME", "/github/home");
        args.env("RUNNER_TOOL_CACHE", "/__tool");
        args.env("AGENT_TOOLSDIRECTORY", "/__tool");
        // The Node image entrypoint/shell drops env names with '-', but
        // @actions/core reads inputs like INPUT_PUSH-TO-REGISTRY.
        args.pair("--entrypoint", "node");
        self.append_ownership_labels(args);
        self.append_docker_socket_mount(args);
        self.append_docker_cli_mounts(args);
        self.append_rust_acceleration(args);
        self.append_job_cgroup_parent(args);
        if !path_prepend.is_empty() {
            let path = path_prepend
                .iter()
                .cloned()
                .chain(std::iter::once(NODE_ACTION_BASE_PATH.to_owned()))
                .collect::<Vec<_>>()
                .join(":");
            args.env("PATH", path);
        }
        self.append_step_env(args, env);
        let prepared = command
            .image(&image)
            .operand(entrypoint_container_path.to_owned())
            .finish()?;
        audit_argv_for_secrets(prepared.args(), secret_masks)?;
        Ok(prepared)
    }

    /// `docker build` for a Dockerfile action.
    ///
    /// # Errors
    /// The generated tag is not a valid OCI reference.
    pub fn build_docker_action_args(
        &self,
        image: &str,
        dockerfile_host: &Path,
        context_host: &Path,
    ) -> io::Result<Vec<String>> {
        let image = ImageReference::parse(image)?;
        let mut args = DockerArgv::new(["build"]);
        args.flags([
            "--cgroup-parent".to_owned(),
            crate::docker_lease::JOB_CGROUP_PARENT.to_owned(),
            "--tag".to_owned(),
            image.as_str().to_owned(),
            "--file".to_owned(),
            self.docker_host_path(dockerfile_host).display().to_string(),
        ]);
        Ok(args
            .operands()
            .operand(self.docker_host_path(context_host).display().to_string())
            .into_argv())
    }

    /// `docker run` for a Dockerfile/`docker://` action.
    ///
    /// The action's `runs.args` are workflow-controlled and are emitted only
    /// after the end-of-flags separator, so they can never become Docker
    /// flags.
    ///
    /// # Errors
    /// The action image is not a valid OCI reference, an env file could not
    /// be created, or a masked secret would have reached argv.
    pub fn prepare_run_docker_action_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        image: &str,
        entrypoint: Option<&str>,
        command_args: &[String],
    ) -> io::Result<PreparedDockerArgs> {
        let image = ImageReference::parse(image)?;
        let mut command = DockerCommand::new(self.env_dir(), ["run"]);
        let args = &mut command;
        args.flags([
            "--rm".to_owned(),
            "--name".to_owned(),
            self.sidecar_container_name("docker-action"),
            "--network".to_owned(),
            self.network.clone(),
            "--workdir".to_owned(),
            working_directory.to_owned(),
            "-v".to_owned(),
            self.mount_arg(&self.workspace_host, "/__w"),
            "-v".to_owned(),
            self.mount_arg(&self.workspace_host, "/github/workspace"),
            "-v".to_owned(),
            self.mount_arg(&self.temp_host, "/__t"),
            "-v".to_owned(),
            self.mount_arg(&self.temp_host, "/tmp"),
            "-v".to_owned(),
            self.mount_arg(&self.temp_host, "/github/runner_temp"),
            "-v".to_owned(),
            self.mount_arg(&self.temp_host, "/github/file_commands"),
            "-v".to_owned(),
            self.mount_arg(&self.home_host, "/github/home"),
            "-v".to_owned(),
            self.mount_arg(&workflow_host(&self.temp_host), "/github/workflow"),
            "-v".to_owned(),
            format!("{}:ro", self.mount_arg(&self.actions_host, "/__a")),
            "-v".to_owned(),
            self.mount_arg(&self.tools_host, "/__tool"),
        ]);
        args.env("HOME", "/github/home");
        args.env("RUNNER_TOOL_CACHE", "/__tool");
        args.env("AGENT_TOOLSDIRECTORY", "/__tool");
        self.append_ownership_labels(args);
        self.append_docker_socket_mount(args);
        self.append_docker_cli_mounts(args);
        self.append_rust_acceleration(args);
        self.append_job_cgroup_parent(args);
        if let Some(entrypoint) = entrypoint {
            args.pair("--entrypoint", entrypoint.to_owned());
        }
        self.append_step_env(args, env);
        let prepared = command
            .image(&image)
            .operands(command_args.iter().cloned())
            .finish()?;
        audit_argv_for_secrets(prepared.args(), secret_masks)?;
        Ok(prepared)
    }

    pub fn remove_container_args(&self) -> Vec<String> {
        let mut args = DockerArgv::new(["rm"]);
        args.flag("--force");
        args.operands().operand(self.name.clone()).into_argv()
    }

    pub fn remove_network_args(&self) -> Vec<String> {
        DockerArgv::new(["network", "rm"])
            .operands()
            .operand(self.network.clone())
            .into_argv()
    }

    pub fn disconnect_network_args(&self) -> Vec<String> {
        let mut args = DockerArgv::new(["network", "disconnect"]);
        args.flag("--force");
        args.operands()
            .operand(self.network.clone())
            .operand(self.name.clone())
            .into_argv()
    }

    pub fn connect_network_args(&self) -> Vec<String> {
        DockerArgv::new(["network", "connect"])
            .operands()
            .operand(self.network.clone())
            .operand(self.name.clone())
            .into_argv()
    }

    pub fn inspect_network_args(&self) -> Vec<String> {
        DockerArgv::new(["network", "inspect"])
            .operands()
            .operand(self.network.clone())
            .into_argv()
    }

    /// `getent hosts <alias>` inside the job container. The alias is a
    /// workflow-controlled service key, so it stays an operand.
    pub fn service_dns_args(&self, alias: &str) -> Vec<String> {
        DockerArgv::new(["exec"])
            .operands()
            .operand(self.name.clone())
            .operands(["getent", "hosts", alias])
            .into_argv()
    }

    pub fn resolver_state_args(&self) -> Vec<String> {
        DockerArgv::new(["exec"])
            .operands()
            .operand(self.name.clone())
            .operands(["cat", "/etc/resolv.conf"])
            .into_argv()
    }

    pub fn guest_docker_socket_host(&self) -> PathBuf {
        crate::docker_lease::guest_docker_socket_host(&self.name, &self.temp_host)
    }

    fn append_docker_socket_mount(&self, args: &mut impl FlagSink) {
        if !self.mount_docker_socket {
            return;
        }
        args.pair(
            "-v",
            format!(
                "{}:/var/run/docker.sock",
                self.guest_docker_socket_host().display()
            ),
        );
    }

    fn append_docker_cli_mounts(&self, args: &mut impl FlagSink) {
        if !self.mount_docker_socket {
            return;
        }
        if let Some(path) = &self.docker_cli_host_path {
            args.pair("-v", format!("{}:/usr/local/bin/docker:ro", path.display()));
        }
        if let Some(path) = &self.docker_cli_plugin_host_dir {
            args.pair(
                "-v",
                format!("{}:/usr/local/lib/docker/cli-plugins:ro", path.display()),
            );
        }
    }

    fn mount_arg(&self, host_path: &Path, container_path: &str) -> String {
        mount(&self.docker_host_path(host_path), container_path)
    }

    fn repository_store_key(&self) -> Option<String> {
        self.repository
            .as_deref()
            .or_else(|| {
                self.env
                    .iter()
                    .find(|(name, _)| name == "GITHUB_REPOSITORY")
                    .map(|(_, value)| value.as_str())
            })
            .filter(|value| !value.is_empty())
            .map(sanitize_store_key)
    }

    fn cargo_executable_store_host(&self) -> PathBuf {
        self.repository_store_key().map_or_else(
            || {
                eprintln!(
                    "forensics.lifecycle: persistent cargo bin store refused: missing github.repository"
                );
                self.temp_host.join("_velnor/ephemeral/cargo-bin")
            },
            |repository| {
                cargo_executable_store_host_for_scope(
                    &self.temp_host,
                    store_trust_namespace(self.store_trust_class),
                    &repository,
                )
            },
        )
    }

    pub(crate) fn mise_executable_store_host(&self) -> PathBuf {
        match (self.repository_store_key(), slot_store_key(&self.temp_host)) {
            (Some(repository), Some(slot)) => mise_executable_store_host_for_scope(
                &self.temp_host,
                store_trust_namespace(self.store_trust_class),
                &repository,
            )
            .join("slots")
            .join(slot),
            _ => {
                eprintln!(
                    "forensics.lifecycle: persistent mise install store refused: missing github.repository or runner slot identity"
                );
                self.temp_host.join("_velnor/ephemeral/mise-installs")
            }
        }
    }

    /// Persistent per-version mise binary store for this job's trust/repository
    /// scope. Without a repository identity the store stays job-ephemeral, so
    /// persistence is never granted to an unidentified job.
    pub(crate) fn mise_binary_store_host(&self) -> PathBuf {
        self.repository_store_key().map_or_else(
            || {
                eprintln!(
                    "forensics.lifecycle: persistent mise binary store refused: missing github.repository"
                );
                self.temp_host.join("_velnor/ephemeral/mise-binaries")
            },
            |repository| {
                mise_binary_store_host_for_scope(
                    &self.temp_host,
                    store_trust_namespace(self.store_trust_class),
                    &repository,
                )
            },
        )
    }

    fn playwright_browser_store_host(&self) -> PathBuf {
        self.repository_store_key().map_or_else(
            || self.home_host.join(".cache/ms-playwright"),
            |repository| {
                playwright_browser_store_host_for_scope(
                    &self.temp_host,
                    store_trust_namespace(self.store_trust_class),
                    &repository,
                )
            },
        )
    }

    fn docker_host_path(&self, host_path: &Path) -> PathBuf {
        let Some(docker_work_dir) = &self.docker_host_work_dir else {
            return host_path.to_path_buf();
        };
        let Some(local_work_dir) = self.local_work_dir() else {
            return host_path.to_path_buf();
        };
        let Ok(relative) = host_path.strip_prefix(local_work_dir) else {
            return host_path.to_path_buf();
        };
        docker_work_dir.join(relative)
    }

    fn local_work_dir(&self) -> Option<PathBuf> {
        let job_dir = self.temp_host.parent()?;
        Some(daemon_shared_root(job_dir.parent()?.to_path_buf()))
    }

    fn sidecar_container_name(&self, kind: &str) -> String {
        format!("velnor-{kind}-{}", self.name)
    }

    fn append_ownership_labels(&self, args: &mut impl FlagSink) {
        args.pair("--label", format!("velnor.daemon-id={}", self.daemon_id));
        args.pair("--label", format!("velnor.job-id={}", self.name));
    }

    fn append_job_cgroup_parent(&self, args: &mut impl FlagSink) {
        args.pair("--cgroup-parent", crate::docker_lease::JOB_CGROUP_PARENT);
    }
}

/// Fail-closed audit: no finished Docker command line may contain a masked
/// secret. The builder already makes environment-on-argv unconstructible;
/// this catches a secret that reached argv through some other operand (an
/// action argument, a mount path) before the process is spawned.
fn audit_argv_for_secrets(args: &[String], secret_masks: &[String]) -> io::Result<()> {
    for mask in secret_masks.iter().filter(|mask| mask.len() >= 3) {
        if args.iter().any(|arg| arg.contains(mask.as_str())) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing to run docker: a masked secret reached the command line",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceContainerSpec {
    pub name: String,
    pub image: String,
    pub network_alias: String,
    pub network: String,
    pub env: Vec<(String, String)>,
    pub ports: Vec<String>,
    pub options: Vec<String>,
}

impl ServiceContainerSpec {
    /// `docker run` for a workflow service container.
    ///
    /// `env_dir` is the owning job's env-file directory: service environment
    /// is workflow-controlled and routinely holds credentials (for example
    /// `POSTGRES_PASSWORD`), so it is written to a mode-0600 file instead of
    /// the world-readable command line.
    ///
    /// # Errors
    /// The service image is not a valid OCI reference, or the env file could
    /// not be created.
    pub fn start_args(&self, env_dir: &Path) -> io::Result<PreparedDockerArgs> {
        let image = ImageReference::parse(&self.image)?;
        let mut command = DockerCommand::new(env_dir, ["run"]);
        let args = &mut command;
        args.flags([
            "--detach".to_owned(),
            "--name".to_owned(),
            self.name.clone(),
        ]);
        for (name, value) in &self.env {
            args.env(name.clone(), value.clone());
        }
        for port in &self.ports {
            args.pair("-p", port.clone());
        }
        args.flags(self.options.iter().cloned());
        args.pair("--cgroup-parent", crate::docker_lease::JOB_CGROUP_PARENT);
        // Runner-owned network policy must win over any network-shaped token
        // present in the expanded service options. Docker uses the final
        // occurrence, so append the per-job network and workflow service key
        // as its DNS alias after user options.
        args.pair("--network", self.network.clone());
        args.pair("--network-alias", self.network_alias.clone());
        command.image(&image).finish()
    }

    pub fn remove_args(&self) -> Vec<String> {
        let mut args = DockerArgv::new(["rm"]);
        args.flag("--force");
        args.operands().operand(self.name.clone()).into_argv()
    }

    pub fn disconnect_network_args(&self) -> Vec<String> {
        let mut args = DockerArgv::new(["network", "disconnect"]);
        args.flag("--force");
        args.operands()
            .operand(self.network.clone())
            .operand(self.name.clone())
            .into_argv()
    }

    pub fn connect_network_args(&self) -> Vec<String> {
        let mut args = DockerArgv::new(["network", "connect"]);
        args.pair("--alias", self.network_alias.clone());
        args.operands()
            .operand(self.network.clone())
            .operand(self.name.clone())
            .into_argv()
    }

    pub fn health_status_args(&self) -> Vec<String> {
        let mut args = DockerArgv::new(["inspect"]);
        args.flag(
            "--format={{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}",
        );
        args.operands().operand(self.name.clone()).into_argv()
    }

    pub fn id_args(&self) -> Vec<String> {
        let mut args = DockerArgv::new(["inspect"]);
        args.flag("--format={{.Id}}");
        args.operands().operand(self.name.clone()).into_argv()
    }

    pub fn mapped_ports_args(&self) -> Vec<String> {
        DockerArgv::new(["port"])
            .operands()
            .operand(self.name.clone())
            .into_argv()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Shell {
    /// Explicit `shell: bash` — GitHub runs `bash --noprofile --norc -e -o
    /// pipefail {0}` (actions/runner ScriptHandlerHelpers); omitting pipefail
    /// silently masks pipeline failures the hosted lane would catch.
    Bash,
    /// No shell specified anywhere — GitHub's fallback is plain `bash -e {0}`.
    BashDefault,
    Sh,
}

impl Shell {
    fn command_args(self, script_path: &str) -> Vec<String> {
        match self {
            Self::Bash => vec![
                "bash".into(),
                "--noprofile".into(),
                "--norc".into(),
                "-e".into(),
                "-o".into(),
                "pipefail".into(),
                script_path.into(),
            ],
            Self::BashDefault => vec!["bash".into(), "-e".into(), script_path.into()],
            Self::Sh => vec!["sh".into(), "-e".into(), script_path.into()],
        }
    }
}

fn mount(host: &Path, container: &str) -> String {
    format!("{}:{container}", host.display())
}

fn workflow_host(temp_host: &Path) -> PathBuf {
    temp_host.join("_github_workflow")
}

/// Climb from a per-slot work root (`…/work/slot-N`) to the daemon-shared
/// work root (`…/work`). Slot-fragmented caches were the top measured
/// performance defect: 10 slots × ~2 GB duplicate sccache dirs, and any job
/// landing on a cold slot misses caches its sibling slots already have.
/// Compilers' caches (sccache) and the actions-cache store are safe to share
/// across slots of one daemon (same repo trust domain).
pub(crate) fn daemon_shared_root(root: PathBuf) -> PathBuf {
    let is_slot_dir = root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("slot-"))
        .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()));
    if is_slot_dir {
        root.parent().map(Path::to_path_buf).unwrap_or(root)
    } else {
        root
    }
}

pub(crate) fn sccache_host(temp_host: &Path, trust_class: StoreTrustClass) -> PathBuf {
    crate::storage::cache_class_path_for_trust(
        &daemon_store_root(temp_host),
        store_trust_namespace(trust_class),
        "compiler/sccache",
        "_velnor_sccache",
    )
}

fn store_trust_namespace(trust_class: StoreTrustClass) -> &'static str {
    match trust_class {
        StoreTrustClass::Untrusted => "untrusted",
        StoreTrustClass::Trusted => "trusted",
        StoreTrustClass::Release => "release",
    }
}

/// Host-persistent Cargo download/index store, daemon-shared like sccache.
/// Extracted registry sources and git checkouts remain job-local because they
/// are mutable during materialization and are unsafe to share across slots.
pub(crate) fn cargo_store_host(temp_host: &Path) -> PathBuf {
    crate::storage::cache_class_path(&daemon_store_root(temp_host), "cargo", "_velnor_cargo")
}

/// Remove Cargo git checkouts whose same-named bare repository is absent.
/// Cargo cannot heal this state itself: it treats the checkout as reusable,
/// then fails metadata with `Repository .../git/db/<name> not found`.
pub(crate) fn repair_cargo_git_store(cargo_store: &Path) -> io::Result<usize> {
    let git = cargo_store.join("git");
    let lock = git.join(".velnor-repair-lock");
    match fs::create_dir(&lock) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(0),
        Err(error) => return Err(error),
    }

    let result = (|| {
        let checkouts = git.join("checkouts");
        let db = git.join("db");
        let entries = match fs::read_dir(&checkouts) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let mut repaired = 0;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || db.join(entry.file_name()).is_dir() {
                continue;
            }
            fs::remove_dir_all(entry.path())?;
            repaired += 1;
        }
        Ok(repaired)
    })();
    let _ = fs::remove_dir(&lock);
    result
}

/// Host-persistent cargo executable store, scoped by trust + repository.
pub(crate) fn cargo_executable_store_host(temp_host: &Path, repository: &str) -> PathBuf {
    cargo_executable_store_host_for_scope(
        temp_host,
        &crate::github_adapter::cargo_target_trust_scope(),
        repository,
    )
}

fn cargo_executable_store_host_for_scope(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    crate::storage::child_with_legacy_trust(cargo_store_host(temp_host), "bin", trust_scope)
        .join(sanitize_store_key(repository))
}

/// Host-persistent mise tool store (installs + cache subdirs are mounted).
pub(crate) fn mise_store_host(temp_host: &Path) -> PathBuf {
    crate::storage::cache_class_path(&daemon_store_root(temp_host), "mise", "_velnor_mise")
}

pub(crate) fn git_mirror_store_host(temp_host: &Path, trust_scope: &str) -> PathBuf {
    crate::git_mirror::store_root(&daemon_store_root(temp_host), trust_scope)
}

/// Host-persistent mise executable store, scoped by trust + repository.
pub(crate) fn mise_executable_store_host(temp_host: &Path, repository: &str) -> PathBuf {
    mise_executable_store_host_for_scope(
        temp_host,
        &crate::github_adapter::cargo_target_trust_scope(),
        repository,
    )
}

fn mise_executable_store_host_for_scope(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    crate::storage::child_with_legacy_trust(mise_store_host(temp_host), "installs", trust_scope)
        .join(sanitize_store_key(repository))
}

/// Host-persistent per-version mise BINARY store, scoped by trust + repository.
///
/// The setup script writes `<os-arch>/<exact-version>/mise` plus `metadata.json`
/// inside this scope (Plan 008 Step 2), so a fresh job reuses a verified binary
/// instead of mutating the read-only baked `/opt/mise/bin` bootstrap. Lives
/// under the same `mise` cache class as `installs`/`rustup`, so the mise GC
/// budget covers it and a per-scope lease protects it while a job holds it.
pub(crate) fn mise_binary_store_host(temp_host: &Path, repository: &str) -> PathBuf {
    mise_binary_store_host_for_scope(
        temp_host,
        &crate::github_adapter::cargo_target_trust_scope(),
        repository,
    )
}

fn mise_binary_store_host_for_scope(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    crate::storage::child_with_legacy_trust(mise_store_host(temp_host), "binaries", trust_scope)
        .join(sanitize_store_key(repository))
}

/// Root for opt-in persistent workspace target buckets (one per job class).
pub(crate) fn cargo_target_store_host(temp_host: &Path) -> PathBuf {
    crate::storage::cache_class_path(&daemon_store_root(temp_host), "targets", "_velnor_targets")
}

/// Host-persistent Playwright browser downloads, scoped by trust + repository.
pub(crate) fn playwright_browser_store_host(temp_host: &Path, repository: &str) -> PathBuf {
    playwright_browser_store_host_for_scope(
        temp_host,
        &crate::github_adapter::cargo_target_trust_scope(),
        repository,
    )
}

fn playwright_browser_store_host_for_scope(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    let root =
        crate::storage::cache_class_path(&daemon_store_root(temp_host), "caches", "_velnor_caches");
    crate::storage::append_legacy_trust(root, trust_scope)
        .join(sanitize_store_key(repository))
        .join("playwright")
}

/// Resolve the daemon-shared store root from a job temp dir
/// (`…/work/slot-N/<job>/temp` → `…/work`).
pub(crate) fn daemon_store_root(temp_host: &Path) -> PathBuf {
    let per_slot_root = if temp_host.file_name().is_some_and(|name| name == "temp") {
        if let Some(job_dir) = temp_host.parent() {
            if job_dir.file_name().is_some_and(|name| name == "tmp") {
                job_dir.to_path_buf()
            } else {
                job_dir.parent().unwrap_or(job_dir).to_path_buf()
            }
        } else {
            temp_host.to_path_buf()
        }
    } else {
        temp_host.to_path_buf()
    };
    daemon_shared_root(per_slot_root)
}

/// Resolve the stable runner slot from a production job temp path
/// (`…/slot-N/<job>/temp`). Executable stores must not fall back to a shared
/// scope when this identity is absent: that would recreate concurrent mutation.
fn slot_store_key(temp_host: &Path) -> Option<String> {
    let slot = temp_host.parent()?.parent()?.file_name()?.to_str()?;
    slot.starts_with("slot-").then(|| sanitize_store_key(slot))
}

/// Sanitize a job/store key into a filesystem-safe directory name.
pub(crate) fn sanitize_store_key(name: &str) -> String {
    let mut key: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    key.truncate(128);
    if key.is_empty() || matches!(key.as_str(), "." | "..") {
        key = "default".to_string();
    }
    key
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn split_container_options(options: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escape = false;
    for ch in options.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if let Some(quote_ch) = quote {
            if ch == quote_ch {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                values.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        values.push(current);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Render a prepared command the way Docker will read it: every
    /// `--env-file <path>` expanded in place into its `NAME=VALUE` lines. The
    /// argv itself never contains those pairs, so ordering assertions are
    /// written against this effective view.
    fn rendered(prepared: &PreparedDockerArgs) -> Vec<String> {
        let mut out = Vec::new();
        let mut args = prepared.args().iter();
        while let Some(arg) = args.next() {
            if arg == "--env-file" {
                let path = args.next().expect("env file path follows --env-file");
                for line in fs::read_to_string(path).unwrap().lines() {
                    out.push(line.to_owned());
                }
            } else {
                out.push(arg.clone());
            }
        }
        out
    }

    fn service_env_dir() -> PathBuf {
        container_test_temp("service").join("_velnor/exec-env")
    }

    fn has_mount(args: &[String], host: &Path, container: &str) -> bool {
        args.contains(&mount(host, container))
    }

    fn has_read_only_mount(args: &[String], host: &Path, container: &str) -> bool {
        args.contains(&format!("{}:ro", mount(host, container)))
    }

    fn spec() -> JobContainerSpec {
        let root = container_test_temp("spec");
        let work = root.join("work");
        let job = work.join("job-1");
        JobContainerSpec {
            name: "velnor-job-1".into(),
            image: "ubuntu:24.04".into(),
            network: "velnor-net-1".into(),
            workspace_host: job.join("workspace"),
            temp_host: job.join("temp"),
            home_host: job.join("home"),
            actions_host: job.join("actions"),
            tools_host: job.join("tools"),
            mount_docker_socket: true,
            slot_count: NonZeroU32::MIN,
            env: vec![("NODE_OPTIONS".into(), "--max-old-space-size=4096".into())],
            resource_options: vec!["--memory".into(), "8g".into()],
            options: vec!["--cpus".into(), "2".into()],
            services: Vec::new(),
            node_action_image: "node:24-bookworm".into(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            docker_host_work_dir: None,
            verify_bind_mounts: false,
            daemon_id: "test-daemon".into(),
            repository: Some("acme/repo".into()),
            cargo_target_host: None,
            store_trust_class: StoreTrustClass::Trusted,
            mbx_store_host: Some(work.join("_velnor_mbx/trusted")),
            sccache_store_host: None,
        }
    }

    #[test]
    fn slot_budget_uses_authoritative_count_for_nonstandard_job_paths() {
        let mut job = spec();
        job.temp_host = PathBuf::from("/not-a-slot-layout/jobs/job/temp");
        job.slot_count = NonZeroU32::new(2).unwrap();

        assert_eq!(job.slot_budget().slots, job.slot_count);
    }

    #[test]
    fn job_network_carries_daemon_and_job_ownership_labels() {
        assert_eq!(
            spec().create_network_args(),
            vec![
                "network",
                "create",
                "--label",
                "velnor.daemon-id=test-daemon",
                "--label",
                "velnor.job-id=velnor-job-1",
                "--",
                "velnor-net-1",
            ]
        );
    }

    #[test]
    fn default_job_mounts_only_mbx_with_bounded_gc() {
        let job = spec();
        let mbx_store = job.mbx_store_host.clone().unwrap();
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        assert!(args.contains(&format!("{}:/var/cache/mbx", mbx_store.display())));
        assert!(args.contains(&"MBX_CACHE_DIR=/var/cache/mbx".into()));
        assert!(args.contains(&"MBX_GC_MAX_TOTAL_SIZE=50GiB".into()));
        assert!(!args.iter().any(|arg| arg.contains("/var/cache/sccache")));
        assert!(!args.contains(&"MBX_DISABLE=1".into()));
    }

    #[test]
    fn daemon_acceleration_environment_follows_trusted_container_options() {
        let mut job = spec();
        job.options = vec!["-e".into(), "MBX_CACHE_DIR=/untrusted-override".into()];
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        let override_index = args
            .iter()
            .position(|arg| arg == "MBX_CACHE_DIR=/untrusted-override")
            .unwrap();
        let policy_index = args
            .iter()
            .position(|arg| arg == "MBX_CACHE_DIR=/var/cache/mbx")
            .unwrap();
        assert!(policy_index > override_index);
    }

    /// The budget the daemon derived for this job must reach the compilers
    /// that would otherwise each size themselves to the whole machine.
    #[test]
    fn the_job_is_told_its_share_of_the_machine() {
        let mut job = spec();
        job.options.clear();
        job.resource_options.clear();
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        let jobs = args
            .iter()
            .find_map(|arg| arg.strip_prefix("CARGO_BUILD_JOBS="))
            .expect("a derivable host budget must reach Cargo")
            .to_owned();
        assert!(args.contains(&format!("MAKEFLAGS=-j{jobs}")));
        assert!(args.contains(&format!("MBX_SCHEDULER_CPUS={jobs}")));
        // No declared limit, so the daemon pins the container to the share.
        assert_eq!(args.iter().filter(|arg| *arg == "--cpus").count(), 1);
        let cpus_index = args.iter().position(|arg| arg == "--cpus").unwrap();
        assert_eq!(args[cpus_index + 1], jobs);
        if let Some(memory) = job.slot_budget().memory_bytes.value().copied() {
            let memory_index = args.iter().position(|arg| arg == "--memory").unwrap();
            assert_eq!(args[memory_index + 1], memory.to_string());
        } else {
            assert!(!args.iter().any(|arg| arg == "--memory"));
        }
        assert!(args.iter().any(|arg| arg.starts_with("VELNOR_JOB_BUDGET=")));
    }

    /// A workflow cannot widen its own share of a host it cannot see, and the
    /// override is named rather than applied silently.
    #[test]
    fn a_workflow_set_job_count_loses_to_the_daemon_budget_and_is_named() {
        let mut job = spec();
        job.env = vec![("CARGO_BUILD_JOBS".into(), "64".into())];
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        let workflow_index = args
            .iter()
            .position(|arg| arg == "CARGO_BUILD_JOBS=64")
            .unwrap();
        let policy_index = args
            .iter()
            .rposition(|arg| arg.starts_with("CARGO_BUILD_JOBS="))
            .unwrap();
        assert!(policy_index > workflow_index);
        assert_ne!(args[policy_index], "CARGO_BUILD_JOBS=64");
        let notice = args
            .iter()
            .find(|arg| arg.starts_with("VELNOR_JOB_BUDGET="))
            .unwrap();
        assert!(notice.contains("overrides workflow-set CARGO_BUILD_JOBS"));
    }

    /// A declared container limit is folded into the runner-owned cap and is
    /// never joined by a second `--cpus`.
    #[test]
    fn a_declared_cpu_limit_narrows_the_share_and_stays_single() {
        let mut job = spec();
        job.options.clear();
        job.resource_options = vec!["--cpus".into(), "1".into()];
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        assert_eq!(args.iter().filter(|arg| *arg == "--cpus").count(), 1);
        assert!(args.contains(&"CARGO_BUILD_JOBS=1".to_owned()));
        assert!(args.contains(&"MAKEFLAGS=-j1".to_owned()));
    }

    /// The tightest declared limit wins, whichever side declared it, while the
    /// final argv contains one normalized runner-owned flag.
    #[test]
    fn the_tightest_declared_cpu_limit_sizes_the_job() {
        let mut job = spec();
        job.options = vec!["--cpus".into(), "2".into()];
        job.resource_options = vec!["--cpus".into(), "1".into()];
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        assert!(args.contains(&"CARGO_BUILD_JOBS=1".to_owned()));
        assert_eq!(args.iter().filter(|arg| *arg == "--cpus").count(), 1);
        let cpus_index = args.iter().position(|arg| arg == "--cpus").unwrap();
        assert_eq!(args[cpus_index + 1], "1");
    }

    #[test]
    fn normalized_cpu_limit_preserves_other_workflow_and_daemon_flags() {
        let mut job = spec();
        job.options = vec!["--cpus=64".into(), "--label".into(), "workflow".into()];
        job.resource_options = vec!["--memory".into(), "8g".into(), "--cpus".into(), "4".into()];
        let expected = job
            .slot_budget()
            .docker_cpu_option()
            .expect("the test host exposes a CPU budget");
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);

        assert_eq!(args.iter().filter(|arg| *arg == "--cpus").count(), 1);
        assert!(args.windows(2).any(|pair| pair == expected.as_slice()));
        assert!(!args.iter().any(|arg| arg == "--cpus=64"));
        if let Some(derived_memory) = job.slot_budget().memory_bytes.value().copied() {
            let expected_memory = derived_memory
                .min(parse_docker_memory_bytes("8g").unwrap())
                .to_string();
            assert_eq!(args.iter().filter(|arg| *arg == "--memory").count(), 1);
            assert!(args
                .windows(2)
                .any(|pair| pair[0] == "--memory" && pair[1] == expected_memory));
        } else {
            assert!(args.windows(2).any(|pair| pair == ["--memory", "8g"]));
        }
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--label" && pair[1] == "workflow"));
    }

    #[test]
    fn a_declared_memory_limit_narrows_the_share() {
        let mut job = spec();
        job.options = vec!["--memory=1m".into()];
        job.resource_options.clear();
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        if let Some(derived_memory) = job.slot_budget().memory_bytes.value().copied() {
            let expected = derived_memory.min(1 << 20).to_string();
            assert_eq!(args.iter().filter(|arg| *arg == "--memory").count(), 1);
            assert!(args
                .windows(2)
                .any(|pair| pair[0] == "--memory" && pair[1] == expected));
        } else {
            assert!(args.iter().any(|arg| arg == "--memory=1m"));
        }
    }

    #[test]
    fn a_large_declared_memory_limit_cannot_widen_the_share() {
        let mut job = spec();
        job.options = vec!["--memory".into(), "64t".into()];
        job.resource_options.clear();
        let expected = job.slot_budget().memory_bytes.value().copied();
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        if let Some(expected) = expected {
            let memory_index = args.iter().position(|arg| arg == "--memory").unwrap();
            assert_eq!(args[memory_index + 1], expected.to_string());
        } else {
            assert!(args.windows(2).any(|pair| pair == ["--memory", "64t"]));
        }
    }

    #[test]
    fn invalid_declared_memory_limit_fails_closed() {
        let mut job = spec();
        job.options = vec!["--memory".into(), "not-a-size".into()];
        job.resource_options.clear();
        let error = job.start_args().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn docker_memory_units_normalize_before_comparison() {
        assert_eq!(parse_docker_memory_bytes("123"), Some(123));
        assert_eq!(parse_docker_memory_bytes("1.5g"), Some(1_610_612_736));
        assert_eq!(parse_docker_memory_bytes("512MiB"), Some(536_870_912));
        assert_eq!(parse_docker_memory_bytes("0"), None);
        assert_eq!(parse_docker_memory_bytes("-1g"), None);
        assert_eq!(parse_docker_memory_bytes("garbage"), None);
    }

    #[test]
    fn explicit_sccache_is_mutually_exclusive_with_mbx() {
        let mut job = spec();
        job.mbx_store_host = None;
        let sccache_store = job.temp_host.join("_velnor_sccache/trusted");
        job.sccache_store_host = Some(sccache_store.clone());
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        assert!(args.contains(&format!("{}:/var/cache/sccache", sccache_store.display())));
        assert!(args.contains(&"RUSTC_WRAPPER=sccache".into()));
        assert!(args.contains(&"SCCACHE_DIR=/var/cache/sccache".into()));
        assert!(args.contains(&"SCCACHE_GHA_ENABLED=false".into()));
        assert!(args.contains(&"MBX_DISABLE=1".into()));
        assert!(!args.iter().any(|arg| arg.contains("/var/cache/mbx")));
    }

    #[test]
    fn exec_path_selects_mbx_only_for_the_default_accelerator() {
        let job = spec();
        assert_eq!(job.default_exec_path(), MBX_CONTAINER_EXEC_PATH);

        let mut sccache = job;
        sccache.mbx_store_host = None;
        sccache.sccache_store_host = Some(PathBuf::from("/var/cache/sccache"));
        assert_eq!(sccache.default_exec_path(), DEFAULT_CONTAINER_EXEC_PATH);
    }

    fn container_test_temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "velnor-container-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn concurrent_container_commands_use_disjoint_test_roots() {
        let commands = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    let job = spec();
                    let env_dir = job.env_dir();
                    let rendered = rendered(&job.start_args().unwrap());
                    (env_dir, rendered)
                })
            })
            .collect::<Vec<_>>();

        let commands = commands
            .into_iter()
            .map(|command| command.join().unwrap())
            .collect::<Vec<_>>();
        for (index, (env_dir, args)) in commands.iter().enumerate() {
            assert!(
                args.iter()
                    .any(|arg| arg == "NODE_OPTIONS=--max-old-space-size=4096"),
                "command {index} did not materialize its environment"
            );
            assert!(
                commands
                    .iter()
                    .skip(index + 1)
                    .all(|(other_env_dir, _)| other_env_dir != env_dir),
                "command {index} reused another test's env directory"
            );
        }
    }

    #[test]
    fn container_test_specs_and_service_env_dirs_are_disjoint() {
        let first = spec();
        let second = spec();
        assert_ne!(first.temp_host, second.temp_host);
        assert_ne!(first.env_dir(), second.env_dir());
        assert_ne!(first.mbx_store_host, second.mbx_store_host);

        let first_service = service_env_dir();
        let second_service = service_env_dir();
        assert_ne!(first_service, second_service);
    }

    #[test]
    fn repairs_orphaned_cargo_git_checkouts_as_one_coherent_store() {
        let root = container_test_temp("cargo-git-repair");
        let cargo = root.join("cargo");
        fs::create_dir_all(cargo.join("git/checkouts/orphan-123/rev")).unwrap();
        fs::create_dir_all(cargo.join("git/checkouts/healthy-456/rev")).unwrap();
        fs::create_dir_all(cargo.join("git/db/healthy-456")).unwrap();

        assert_eq!(repair_cargo_git_store(&cargo).unwrap(), 1);
        assert!(!cargo.join("git/checkouts/orphan-123").exists());
        assert!(cargo.join("git/checkouts/healthy-456").is_dir());
        assert_eq!(repair_cargo_git_store(&cargo).unwrap(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn daemon_shared_root_climbs_slot_dirs_only() {
        assert_eq!(
            daemon_shared_root(PathBuf::from("/var/lib/velnor-fixture/work/slot-2")),
            PathBuf::from("/var/lib/velnor-fixture/work")
        );
        assert_eq!(
            daemon_shared_root(PathBuf::from("/var/lib/velnor/work/slot-10")),
            PathBuf::from("/var/lib/velnor/work")
        );
        // Non-slot roots stay untouched.
        assert_eq!(
            daemon_shared_root(PathBuf::from("/daemon/work")),
            PathBuf::from("/daemon/work")
        );
        assert_eq!(
            daemon_shared_root(PathBuf::from("/work/slot-")),
            PathBuf::from("/work/slot-")
        );
        assert_eq!(
            daemon_shared_root(PathBuf::from("/work/slot-abc")),
            PathBuf::from("/work/slot-abc")
        );
    }

    #[test]
    fn sanitize_store_key_neutralizes_traversal() {
        assert_eq!(sanitize_store_key(".."), "default");
        assert_eq!(sanitize_store_key("."), "default");
        assert_eq!(sanitize_store_key(""), "default");
        assert_eq!(sanitize_store_key("normal-key.v2"), "normal-key.v2");
    }

    #[test]
    fn executable_tool_store_hosts_are_scoped_by_trust_and_repo() {
        let temp = Path::new("/var/lib/velnor/work/slot-3/job-9/temp");

        assert_eq!(
            cargo_executable_store_host_for_scope(temp, "trusted", "ChainArgos/java-monorepo"),
            PathBuf::from(
                "/var/lib/velnor/work/_velnor_cargo/bin/trusted/ChainArgos_java-monorepo"
            )
        );
        assert_eq!(
            mise_executable_store_host_for_scope(temp, "trusted", "ChainArgos/java-monorepo"),
            PathBuf::from(
                "/var/lib/velnor/work/_velnor_mise/installs/trusted/ChainArgos_java-monorepo"
            )
        );
        // Plan 008: the persistent mise binary store is a distinct `binaries`
        // subdir under the same trust/repository boundary as `installs`.
        assert_eq!(
            mise_binary_store_host_for_scope(temp, "trusted", "ChainArgos/java-monorepo"),
            PathBuf::from(
                "/var/lib/velnor/work/_velnor_mise/binaries/trusted/ChainArgos_java-monorepo"
            )
        );
        assert_ne!(
            mise_binary_store_host_for_scope(temp, "trusted", "ChainArgos/java-monorepo"),
            mise_executable_store_host_for_scope(temp, "trusted", "ChainArgos/java-monorepo"),
        );
    }

    #[test]
    fn executable_tool_store_hosts_differ_by_repo() {
        let temp = Path::new("/var/lib/velnor/work/slot-3/job-9/temp");

        assert_ne!(
            cargo_executable_store_host_for_scope(temp, "trusted", "org/one"),
            cargo_executable_store_host_for_scope(temp, "trusted", "org/two")
        );
        assert_ne!(
            mise_executable_store_host_for_scope(temp, "trusted", "org/one"),
            mise_executable_store_host_for_scope(temp, "trusted", "org/two")
        );
    }

    #[test]
    fn pure_data_tool_stores_stay_shared_across_repos() {
        let temp = Path::new("/var/lib/velnor/work/slot-3/job-9/temp");

        assert_eq!(
            cargo_store_host(temp).join("registry/cache"),
            PathBuf::from("/var/lib/velnor/work/_velnor_cargo/registry/cache")
        );
        assert_eq!(
            cargo_store_host(temp).join("git/db"),
            PathBuf::from("/var/lib/velnor/work/_velnor_cargo/git/db")
        );
        assert_eq!(
            mise_store_host(temp).join("cache"),
            PathBuf::from("/var/lib/velnor/work/_velnor_mise/cache")
        );
    }

    #[test]
    fn sccache_host_is_shared_across_daemon_slots() {
        // slot-N work roots collapse to one daemon-level sccache dir.
        assert_eq!(
            sccache_host(
                Path::new("/var/lib/velnor/work/slot-3/job-9/temp"),
                StoreTrustClass::Trusted,
            ),
            PathBuf::from("/var/lib/velnor/work/_velnor_sccache/trusted")
        );
        assert_eq!(
            sccache_host(
                Path::new("/var/lib/velnor/work/slot-7/job-1/temp"),
                StoreTrustClass::Trusted,
            ),
            PathBuf::from("/var/lib/velnor/work/_velnor_sccache/trusted")
        );
    }

    #[test]
    fn builds_start_container_args_with_mounts() {
        let job = spec();
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-job-1"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--add-host", "host.docker.internal:host-gateway"]),
            "job containers must map the standard host alias for daemon services"
        );
        assert!(has_mount(&args, &job.workspace_host, "/__w"));
        assert!(has_mount(&args, &job.temp_host, "/tmp"));
        assert!(has_mount(
            &args,
            job.mbx_store_host.as_ref().unwrap(),
            "/var/cache/mbx"
        ));
        assert!(has_mount(&args, &job.home_host, "/github/home"));
        assert!(has_mount(
            &args,
            &job.playwright_browser_store_host(),
            "/github/home/.cache/ms-playwright"
        ));
        assert!(has_mount(
            &args,
            &job.cargo_executable_store_host(),
            "/github/home/.cargo/bin"
        ));
        assert!(has_mount(
            &args,
            &job.mise_executable_store_host(),
            "/opt/mise/installs"
        ));
        assert!(has_mount(
            &args,
            &job.mise_binary_store_host(),
            "/opt/velnor/mise-binaries"
        ));
        assert!(!args.iter().any(|arg| arg.ends_with(":/root/.rustup")));
        assert!(has_mount(
            &args,
            &cargo_store_host(&job.temp_host).join("registry/cache"),
            "/github/home/.cargo/registry/cache"
        ));
        assert!(has_mount(
            &args,
            &cargo_store_host(&job.temp_host).join("registry/index"),
            "/github/home/.cargo/registry/index"
        ));
        assert!(has_mount(
            &args,
            &cargo_store_host(&job.temp_host).join("git/db"),
            "/github/home/.cargo/git/db"
        ));
        assert!(!args
            .iter()
            .any(|arg| arg.ends_with(":/github/home/.cargo/registry/src")));
        assert!(!args
            .iter()
            .any(|arg| arg.ends_with(":/github/home/.cargo/git/checkouts")));
        assert!(has_mount(
            &args,
            &mise_store_host(&job.temp_host).join("cache"),
            "/opt/mise/cache"
        ));
        assert!(has_mount(
            &args,
            &workflow_host(&job.temp_host),
            "/github/workflow"
        ));
        assert!(has_read_only_mount(&args, &job.actions_host, "/__a"));
        assert!(!has_mount(&args, &job.actions_host, "/__a"));
        assert!(args.contains(&"HOME=/github/home".into()));
        assert!(args.contains(&"MBX_CACHE_DIR=/var/cache/mbx".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"NODE_OPTIONS=--max-old-space-size=4096".into()));
        assert!(args.windows(2).any(|pair| pair == ["--cpus", "2"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--cgroup-parent", "velnor-jobs.slice"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--ulimit", "nofile=65536:65536"]));
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--sysctl", "net.ipv6.conf.all.disable_ipv6=1"] }));
        if let Some(derived_memory) = job.slot_budget().memory_bytes.value().copied() {
            let expected_memory = derived_memory.min(parse_docker_memory_bytes("8g").unwrap());
            assert!(args
                .windows(2)
                .any(|pair| { pair[0] == "--memory" && pair[1] == expected_memory.to_string() }));
        } else {
            assert!(args.windows(2).any(|pair| pair == ["--memory", "8g"]));
        }
        let lease_mount = format!(
            "{}:/var/run/docker.sock",
            job.guest_docker_socket_host().display()
        );
        assert!(
            args.contains(&lease_mount),
            "guest Docker must use the job lease socket {lease_mount}, got {args:?}"
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg == "/var/run/docker.sock:/var/run/docker.sock"),
            "host engine socket must not be mounted into the job"
        );
        // PID 1 tails the live console file (so `docker logs` mirrors the UI).
        assert_eq!(
            args.last().map(String::as_str),
            Some("mkdir -p /__t/_velnor && touch /__t/_velnor/console.log && exec tail -n +1 -F /__t/_velnor/console.log")
        );
    }

    #[test]
    fn mise_seed_keeps_rustup_isolated_in_the_job_image() {
        let job = spec();
        let args = job.seed_mise_store_args().unwrap();

        assert!(!args.iter().any(|arg| arg.contains("/__velnor_seed/rustup")));
        assert!(!args
            .last()
            .is_some_and(|script| script.contains("/root/.rustup")));
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--label", "velnor.daemon-id=test-daemon"] }));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--label", "velnor.job-id=velnor-job-1"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-mise-seed-velnor-job-1"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--cgroup-parent", "velnor-jobs.slice"]));
    }

    #[test]
    fn mise_installs_are_warm_per_slot_but_isolated_between_slots() {
        let mut first = spec();
        first.temp_host = "/var/lib/velnor/work/slot-3/job-a/temp".into();
        let mut same_slot = spec();
        same_slot.temp_host = "/var/lib/velnor/work/slot-3/job-b/temp".into();
        let mut other_slot = spec();
        other_slot.temp_host = "/var/lib/velnor/work/slot-4/job-c/temp".into();

        let expected = PathBuf::from(
            "/var/lib/velnor/work/_velnor_mise/installs/trusted/acme_repo/slots/slot-3",
        );
        assert_eq!(first.mise_executable_store_host(), expected);
        assert_eq!(same_slot.mise_executable_store_host(), expected);
        // Materializing the command writes an env file, so root this half of
        // the assertion in a real directory and derive the expectation.
        let root = container_test_temp("mise-slot");
        let mut warm = spec();
        warm.temp_host = root.join("work/slot-3/job-a/temp");
        let expected_mount = format!(
            "{}:/opt/mise/installs",
            warm.mise_executable_store_host().display()
        );
        assert!(rendered(&warm.start_args().unwrap()).contains(&expected_mount));
        assert_eq!(
            other_slot.mise_executable_store_host(),
            PathBuf::from(
                "/var/lib/velnor/work/_velnor_mise/installs/trusted/acme_repo/slots/slot-4"
            )
        );
        assert_ne!(
            first.mise_executable_store_host(),
            other_slot.mise_executable_store_host()
        );
    }

    #[test]
    fn maps_job_paths_to_docker_host_work_dir() {
        // The mapping only depends on the relative path below the work root,
        // so a real temp root keeps the expected daemon-side paths identical
        // while letting the command materialize its env file.
        let root = container_test_temp("host-work-dir");
        let mut spec = spec();
        spec.workspace_host = root.join("runner/work/job-1/workspace");
        spec.temp_host = root.join("runner/work/job-1/temp");
        spec.home_host = root.join("runner/work/job-1/home");
        spec.actions_host = root.join("runner/work/job-1/actions");
        spec.tools_host = root.join("runner/work/job-1/tools");
        spec.mbx_store_host = Some(root.join("runner/work/_velnor_mbx/trusted"));
        spec.docker_host_work_dir = Some("/daemon/work".into());

        let prepared = spec.start_args().unwrap();
        let args = rendered(&prepared);

        assert!(args.contains(&"/daemon/work/job-1/workspace:/__w".into()));
        assert!(args.contains(&"/daemon/work/job-1/temp:/__t".into()));
        assert!(args.contains(&"/daemon/work/job-1/temp:/daemon/work/job-1/temp".into()));
        assert!(args.contains(&"/daemon/work/job-1/workspace:/daemon/work/job-1/workspace".into()));
        assert!(args.contains(&"/daemon/work/_velnor_mbx/trusted:/var/cache/mbx".into()));
        assert!(args.contains(&"/daemon/work/job-1/home:/github/home".into()));
        assert!(args.contains(&"/daemon/work/job-1/temp/_github_workflow:/github/workflow".into()));
        assert!(args.contains(&"/daemon/work/job-1/actions:/__a:ro".into()));
        assert!(args.contains(&"/daemon/work/job-1/tools:/__tool".into()));
        assert!(args.contains(&"VELNOR_DOCKER_HOST_TEMP=/daemon/work/job-1/temp".into()));
        assert!(args.contains(&"VELNOR_DOCKER_HOST_WORKSPACE=/daemon/work/job-1/workspace".into()));
    }

    #[test]
    fn maps_slot_shared_paths_to_docker_host_work_dir() {
        let root = container_test_temp("slot-host-work-dir");
        let mut spec = spec();
        spec.workspace_host = root.join("runner/work/slot-1/job-1/workspace");
        spec.temp_host = root.join("runner/work/slot-1/job-1/temp");
        spec.mbx_store_host = Some(root.join("runner/work/_velnor_mbx/trusted"));
        spec.docker_host_work_dir = Some("/daemon/work".into());

        assert_eq!(
            spec.docker_host_path(&spec.workspace_host),
            PathBuf::from("/daemon/work/slot-1/job-1/workspace")
        );
        assert_eq!(
            spec.docker_host_path(spec.mbx_store_host.as_ref().unwrap()),
            PathBuf::from("/daemon/work/_velnor_mbx/trusted")
        );
    }

    #[test]
    fn builds_bash_exec_args() {
        let spec = spec();
        let prepared = spec
            .prepare_exec_script_args(
                "/__t/step.sh",
                Shell::Bash,
                "/__w/repo",
                &[("GITHUB_OUTPUT".into(), "/__t/out".into())],
                &[],
            )
            .unwrap();
        let temp_env = format!("VELNOR_DOCKER_HOST_TEMP={}", spec.temp_host.display());
        let workspace_env = format!(
            "VELNOR_DOCKER_HOST_WORKSPACE={}",
            spec.workspace_host.display()
        );

        assert_eq!(
            rendered(&prepared),
            vec![
                "exec",
                "--workdir",
                "/__w/repo",
                "HOME=/github/home",
                "DOCKER_HOST=unix:///var/run/docker.sock",
                "RUSTUP_HOME=/root/.rustup",
                "CARGO_HOME=/github/home/.cargo",
                "PATH=/opt/mbx/bin:/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                temp_env.as_str(),
                workspace_env.as_str(),
                "GITHUB_OUTPUT=/__t/out",
                "--",
                "velnor-job-1",
                "bash",
                "--noprofile",
                "--norc",
                "-e",
                "-o",
                "pipefail",
                "/__t/step.sh"
            ]
        );
    }

    #[test]
    fn builds_process_exec_args() {
        let spec = spec();
        let prepared = spec
            .prepare_exec_process_args(
                "/__w/repo",
                &[("INPUT_NAME".into(), "value".into())],
                &[],
                &["node".into(), "/__a/action/dist/index.js".into()],
            )
            .unwrap();
        let temp_env = format!("VELNOR_DOCKER_HOST_TEMP={}", spec.temp_host.display());
        let workspace_env = format!(
            "VELNOR_DOCKER_HOST_WORKSPACE={}",
            spec.workspace_host.display()
        );

        assert_eq!(
            rendered(&prepared),
            vec![
                "exec",
                "--workdir",
                "/__w/repo",
                "HOME=/github/home",
                "DOCKER_HOST=unix:///var/run/docker.sock",
                "RUSTUP_HOME=/root/.rustup",
                "CARGO_HOME=/github/home/.cargo",
                "PATH=/opt/mbx/bin:/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                temp_env.as_str(),
                workspace_env.as_str(),
                "INPUT_NAME=value",
                "--",
                "velnor-job-1",
                "node",
                "/__a/action/dist/index.js"
            ]
        );
    }

    #[test]
    fn docker_endpoint_environment_is_runner_owned() {
        let mut spec = spec();
        spec.env = vec![
            ("DOCKER_HOST".into(), "tcp://attacker.example:2376".into()),
            ("DOCKER_CONTEXT".into(), "attacker".into()),
            ("DOCKER_CONFIG".into(), "/tmp/attacker".into()),
            ("SAFE_ENV".into(), "kept".into()),
        ];
        let start_prepared = spec.start_args().unwrap();
        let start = rendered(&start_prepared);
        assert!(start.contains(&"DOCKER_HOST=unix:///var/run/docker.sock".into()));
        assert!(start.contains(&"SAFE_ENV=kept".into()));
        assert!(!start.iter().any(|arg| arg.contains("attacker.example")));
        assert!(!start.iter().any(|arg| arg == "DOCKER_CONTEXT=attacker"));
        assert!(!start.iter().any(|arg| arg == "DOCKER_CONFIG=/tmp/attacker"));

        let prepared = spec
            .prepare_exec_process_args(
                "/__w",
                &[
                    ("DOCKER_HOST".into(), "tcp://attacker.example:2376".into()),
                    ("DOCKER_CONTEXT".into(), "attacker".into()),
                    ("DOCKER_CONFIG".into(), "/tmp/attacker".into()),
                ],
                &[],
                &["docker".into(), "version".into()],
            )
            .unwrap();
        assert!(rendered(&prepared).contains(&"DOCKER_HOST=unix:///var/run/docker.sock".into()));
        assert!(!rendered(&prepared)
            .iter()
            .any(|arg| arg.contains("attacker")));
    }

    #[test]
    fn secret_env_is_not_on_exec_argv() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("secret-env");
        let prepared = spec
            .prepare_exec_process_args(
                "/__w",
                &[
                    ("TOKEN".into(), "PLACEHOLDER_SECRET".into()),
                    ("PLAIN".into(), "visible".into()),
                ],
                &["PLACEHOLDER_SECRET".into()],
                &["printenv".into()],
            )
            .unwrap();

        let joined = prepared.args().join("\0");
        assert!(prepared.args().contains(&"--env-file".into()));
        assert!(!joined.contains("PLACEHOLDER_SECRET"));
        assert!(!joined.contains("PLAIN=visible"));
        assert!(rendered(&prepared).contains(&"PLAIN=visible".into()));
    }

    #[test]
    fn runtime_tokens_are_not_on_exec_argv_without_masks() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("runtime-token-env");
        for name in [
            "ACTIONS_RUNTIME_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            "GITHUB_TOKEN",
        ] {
            let prepared = spec
                .prepare_exec_process_args(
                    "/__w",
                    &[(name.into(), "PLACEHOLDER_CREDENTIAL".into())],
                    &[],
                    &["printenv".into()],
                )
                .unwrap();
            let joined = prepared.args().join("\0");
            assert!(prepared.args().contains(&"--env-file".into()));
            assert!(!joined.contains("PLACEHOLDER_CREDENTIAL"));
        }
    }

    #[test]
    fn multiline_secret_uses_docker_process_env_without_argv_exposure() {
        let prepared = spec()
            .prepare_exec_process_args(
                "/__w",
                &[("ACTIONS_RUNTIME_TOKEN".into(), "line-one\nline-two".into())],
                &[],
                &["printenv".into()],
            )
            .unwrap();

        let joined = prepared.args().join("\0");
        assert!(joined.contains("-e\0ACTIONS_RUNTIME_TOKEN"));
        assert!(!joined.contains("line-one"));
        assert_eq!(
            prepared.process_env(),
            &[("ACTIONS_RUNTIME_TOKEN".into(), "line-one\nline-two".into())]
        );
    }

    #[test]
    fn action_sidecars_keep_runtime_tokens_off_argv() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("sidecar-token-env");
        let env = &[(
            "ACTIONS_RUNTIME_TOKEN".into(),
            "PLACEHOLDER_CREDENTIAL".into(),
        )];
        let node = spec
            .prepare_run_node_action_args("/__w", env, &[], &[], "node:24", "/__a/action/index.js")
            .unwrap();
        let docker = spec
            .prepare_run_docker_action_args("/__w", env, &[], "alpine:3.22", None, &["true".into()])
            .unwrap();
        for prepared in [node, docker] {
            let joined = prepared.args().join("\0");
            assert!(prepared.args().contains(&"--env-file".into()));
            assert!(!joined.contains("PLACEHOLDER_CREDENTIAL"));
            assert!(prepared
                .args()
                .windows(2)
                .any(|pair| pair == ["--cgroup-parent", "velnor-jobs.slice"]));
        }
    }

    /// Environment classification is gone: `/proc` is world-readable, so no
    /// variable is "safe enough" for argv. Every single-line variable goes to
    /// the mode-0600 env file, secret or not.
    #[test]
    fn no_environment_pair_reaches_argv() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("no-argv-env");
        let prepared = spec
            .prepare_exec_process_args(
                "/__w",
                &[("PLAIN".into(), "visible".into())],
                &["PLACEHOLDER_SECRET".into()],
                &["printenv".into()],
            )
            .unwrap();

        assert!(prepared.args().contains(&"--env-file".into()));
        assert!(!prepared.args().iter().any(|arg| arg.contains('=')));
        assert!(rendered(&prepared).contains(&"PLAIN=visible".into()));
    }

    #[test]
    fn override_ordering_preserved_with_secret_env_file() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("override-env");
        let prepared = spec
            .prepare_exec_process_args(
                "/__w",
                &[
                    ("HOME".into(), "PLACEHOLDER_SECRET".into()),
                    ("HOME".into(), "/override".into()),
                ],
                &["PLACEHOLDER_SECRET".into()],
                &["printenv".into()],
            )
            .unwrap();

        let effective = rendered(&prepared);
        let secret_pos = effective
            .iter()
            .position(|arg| arg == "HOME=PLACEHOLDER_SECRET")
            .unwrap();
        let override_pos = effective
            .iter()
            .position(|arg| arg == "HOME=/override")
            .unwrap();
        assert!(secret_pos < override_pos, "docker applies the last value");
        assert!(!prepared.args().iter().any(|arg| arg.contains("/override")));
    }

    #[test]
    fn secret_env_file_is_0600_and_unlinked_on_drop() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("mode-env");
        let prepared = spec
            .prepare_exec_process_args(
                "/__w",
                &[("TOKEN".into(), "PLACEHOLDER_SECRET".into())],
                &["PLACEHOLDER_SECRET".into()],
                &["printenv".into()],
            )
            .unwrap();
        let env_file_pos = prepared
            .args()
            .iter()
            .position(|arg| arg == "--env-file")
            .unwrap();
        let env_file = PathBuf::from(&prepared.args()[env_file_pos + 1]);

        assert!(fs::read_to_string(&env_file)
            .unwrap()
            .contains("TOKEN=PLACEHOLDER_SECRET\n"));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&env_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(prepared);
        assert!(!env_file.exists());
    }

    #[test]
    fn builds_node_action_run_args() {
        let spec = spec();
        let prepared = spec
            .prepare_run_node_action_args(
                "/__w",
                &[("GITHUB_OUTPUT".into(), "/__t/out".into())],
                &[],
                &[],
                "node:20-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let args = rendered(&prepared);

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-node-action-velnor-job-1"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--network", "velnor-net-1"]));
        assert!(args.windows(2).any(|pair| pair == ["--workdir", "/__w"]));
        assert!(has_mount(&args, &spec.workspace_host, "/__w"));
        assert!(has_mount(&args, &spec.workspace_host, "/github/workspace"));
        assert!(has_mount(&args, &spec.temp_host, "/tmp"));
        assert!(has_mount(
            &args,
            spec.mbx_store_host.as_ref().unwrap(),
            "/var/cache/mbx"
        ));
        assert!(has_mount(&args, &spec.temp_host, "/github/runner_temp"));
        assert!(has_mount(&args, &spec.temp_host, "/github/file_commands"));
        assert!(has_mount(&args, &spec.home_host, "/github/home"));
        assert!(has_mount(
            &args,
            &workflow_host(&spec.temp_host),
            "/github/workflow"
        ));
        assert!(has_read_only_mount(&args, &spec.actions_host, "/__a"));
        assert!(!has_mount(&args, &spec.actions_host, "/__a"));
        assert!(args.contains(&"HOME=/github/home".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"GITHUB_OUTPUT=/__t/out".into()));
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--label", "velnor.job-id=velnor-job-1"] }));
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--label", "velnor.daemon-id=test-daemon"] }));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--cgroup-parent", "velnor-jobs.slice"]));
        assert!(args.windows(2).any(|pair| pair == ["--entrypoint", "node"]));
        assert_eq!(
            &args[args.len() - 2..],
            ["node:20-bookworm", "/__a/action/dist/index.js"]
        );
    }

    #[test]
    fn builds_node_action_run_args_with_path_prelude() {
        let prepared = spec()
            .prepare_run_node_action_args(
                "/__w",
                &[("GITHUB_OUTPUT".into(), "/__t/out".into())],
                &[],
                &["/github/home/.cargo/bin".into(), "/path/with'quote".into()],
                "node:20-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let args = rendered(&prepared);

        assert!(args.windows(2).any(|pair| pair == ["--entrypoint", "node"]));
        assert!(args.contains(&"PATH=/github/home/.cargo/bin:/path/with'quote:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()));
        assert_eq!(
            &args[args.len() - 2..],
            ["node:20-bookworm", "/__a/action/dist/index.js"]
        );
    }

    #[test]
    fn mounts_host_docker_cli_when_socket_is_mounted() {
        let mut spec = spec();
        spec.docker_cli_host_path = Some("/usr/bin/docker".into());
        spec.docker_cli_plugin_host_dir = Some("/usr/libexec/docker/cli-plugins".into());

        let start_prepared = spec.start_args().unwrap();
        let start_args = rendered(&start_prepared);
        assert!(start_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(start_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));

        let node_prepared = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &[],
                "node:24-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let node_args = rendered(&node_prepared);
        assert!(node_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(node_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));

        let docker_action_prepared = spec
            .prepare_run_docker_action_args("/__w", &[], &[], "alpine:3.20", None, &[])
            .unwrap();
        let docker_action_args = rendered(&docker_action_prepared);
        assert!(docker_action_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(docker_action_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));
    }

    #[test]
    fn skips_host_docker_cli_when_socket_is_not_mounted() {
        let mut spec = spec();
        spec.mount_docker_socket = false;
        spec.docker_cli_host_path = Some("/usr/bin/docker".into());
        spec.docker_cli_plugin_host_dir = Some("/usr/libexec/docker/cli-plugins".into());

        let start_prepared = spec.start_args().unwrap();
        let start_args = rendered(&start_prepared);
        assert!(!start_args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert!(!start_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(!start_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));

        let node_prepared = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &[],
                "node:24-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let node_args = rendered(&node_prepared);
        assert!(!node_args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert!(!node_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(!node_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));

        let docker_action_prepared = spec
            .prepare_run_docker_action_args("/__w", &[], &[], "alpine:3.20", None, &[])
            .unwrap();
        let docker_action_args = rendered(&docker_action_prepared);
        assert!(!docker_action_args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert!(!docker_action_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(!docker_action_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));
    }

    #[test]
    fn builds_docker_action_args() {
        let spec = spec();
        let dockerfile = spec.actions_host.join("action/Dockerfile");
        let context = spec.actions_host.join("action");
        let dockerfile_string = dockerfile.display().to_string();
        let context_string = context.display().to_string();

        assert_eq!(
            spec.build_docker_action_args(
                "velnor-action-owner-repo-v1-root",
                &dockerfile,
                &context,
            )
            .unwrap(),
            vec![
                "build",
                "--cgroup-parent",
                "velnor-jobs.slice",
                "--tag",
                "velnor-action-owner-repo-v1-root",
                "--file",
                dockerfile_string.as_str(),
                "--",
                context_string.as_str()
            ]
        );

        let prepared = spec
            .prepare_run_docker_action_args(
                "/__w",
                &[("INPUT_NAME".into(), "value".into())],
                &[],
                "alpine:3.20",
                Some("/entrypoint.sh"),
                &["arg1".into()],
            )
            .unwrap();
        let args = rendered(&prepared);

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-docker-action-velnor-job-1"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--network", "velnor-net-1"]));
        assert!(has_mount(&args, &spec.workspace_host, "/__w"));
        assert!(has_mount(&args, &spec.workspace_host, "/github/workspace"));
        assert!(has_mount(&args, &spec.temp_host, "/tmp"));
        assert!(has_mount(
            &args,
            spec.mbx_store_host.as_ref().unwrap(),
            "/var/cache/mbx"
        ));
        assert!(has_mount(&args, &spec.temp_host, "/github/runner_temp"));
        assert!(has_mount(&args, &spec.temp_host, "/github/file_commands"));
        assert!(has_mount(&args, &spec.home_host, "/github/home"));
        assert!(has_mount(
            &args,
            &workflow_host(&spec.temp_host),
            "/github/workflow"
        ));
        assert!(has_read_only_mount(&args, &spec.actions_host, "/__a"));
        assert!(!has_mount(&args, &spec.actions_host, "/__a"));
        assert!(args.contains(&"HOME=/github/home".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"INPUT_NAME=value".into()));
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--label", "velnor.job-id=velnor-job-1"] }));
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--label", "velnor.daemon-id=test-daemon"] }));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--cgroup-parent", "velnor-jobs.slice"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--entrypoint", "/entrypoint.sh"]));
        assert_eq!(&args[args.len() - 2..], ["alpine:3.20", "arg1"]);
    }

    #[test]
    fn builds_service_container_start_args() {
        let service = ServiceContainerSpec {
            name: "velnor-service-postgres".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "velnor-net-1".into(),
            env: vec![("POSTGRES_PASSWORD".into(), "postgres".into())],
            ports: vec!["5432:5432".into()],
            options: vec!["--health-cmd".into(), "pg_isready".into()],
        };

        let prepared = service.start_args(&service_env_dir()).unwrap();
        assert_eq!(
            rendered(&prepared),
            vec![
                "run",
                "--detach",
                "--name",
                "velnor-service-postgres",
                "POSTGRES_PASSWORD=postgres",
                "-p",
                "5432:5432",
                "--health-cmd",
                "pg_isready",
                "--cgroup-parent",
                "velnor-jobs.slice",
                "--network",
                "velnor-net-1",
                "--network-alias",
                "postgres",
                "--",
                "postgres:16"
            ]
        );
        // The service password is workflow input and must never be argv.
        assert!(!prepared
            .args()
            .iter()
            .any(|arg| arg.contains("POSTGRES_PASSWORD")));
        assert_eq!(
            service.remove_args(),
            vec!["rm", "--force", "--", "velnor-service-postgres"]
        );
        assert_eq!(
            service.health_status_args(),
            vec![
                "inspect",
                "--format={{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}",
                "--",
                "velnor-service-postgres"
            ]
        );
    }

    #[test]
    fn service_runner_network_overrides_expanded_options() {
        let service = ServiceContainerSpec {
            name: "velnor-service-postgres".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "velnor-net-owned".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: vec!["--network".into(), "unexpected".into()],
        };
        let prepared = service.start_args(&service_env_dir()).unwrap();
        let args = rendered(&prepared);
        assert_eq!(
            &args[args.len() - 6..],
            [
                "--network",
                "velnor-net-owned",
                "--network-alias",
                "postgres",
                "--",
                "postgres:16"
            ]
        );
    }

    #[test]
    fn job_runner_network_overrides_expanded_options() {
        let mut job = spec();
        job.options = vec!["--network".into(), "unexpected".into()];
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        let network_pairs = args
            .windows(2)
            .filter(|pair| pair[0] == "--network")
            .collect::<Vec<_>>();
        assert_eq!(network_pairs.last().unwrap()[1], "velnor-net-1");
    }

    #[test]
    fn job_runner_cgroup_parent_overrides_expanded_options() {
        let mut job = spec();
        job.options = vec!["--cgroup-parent".into(), "unexpected.slice".into()];
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        let cgroup_pairs = args
            .windows(2)
            .filter(|pair| pair[0] == "--cgroup-parent")
            .collect::<Vec<_>>();
        assert_eq!(cgroup_pairs.last().unwrap()[1], "velnor-jobs.slice");
    }

    #[test]
    fn service_runner_cgroup_parent_overrides_expanded_options() {
        let service = ServiceContainerSpec {
            name: "velnor-service-postgres".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "velnor-net-1".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: vec!["--cgroup-parent".into(), "unexpected.slice".into()],
        };
        let prepared = service.start_args(&service_env_dir()).unwrap();
        let args = rendered(&prepared);
        let cgroup_pairs = args
            .windows(2)
            .filter(|pair| pair[0] == "--cgroup-parent")
            .collect::<Vec<_>>();
        assert_eq!(cgroup_pairs.last().unwrap()[1], "velnor-jobs.slice");
    }

    #[test]
    fn container_job_reaches_service_by_shared_network_alias() {
        let job = spec();
        let service = ServiceContainerSpec {
            name: "velnor-service-postgres".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: job.network.clone(),
            env: Vec::new(),
            ports: vec!["5432".into()],
            options: Vec::new(),
        };
        let job_prepared = job.start_args().unwrap();
        let job_args = rendered(&job_prepared);
        let service_prepared = service.start_args(&service_env_dir()).unwrap();
        let service_args = rendered(&service_prepared);
        assert!(job_args
            .windows(2)
            .any(|pair| pair == ["--network", "velnor-net-1"]));
        assert!(service_args
            .windows(2)
            .any(|pair| pair == ["--network", "velnor-net-1"]));
        assert!(service_args
            .windows(2)
            .any(|pair| pair == ["--network-alias", "postgres"]));
    }

    /// `runs.args` are repository content. Before the end-of-flags separator
    /// existed they were appended straight onto `docker run`, so
    /// `args: ["--privileged", "-v", "/:/host"]` was a host escape.
    #[test]
    fn docker_action_arguments_cannot_become_docker_flags() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("action-args");
        let prepared = spec
            .prepare_run_docker_action_args(
                "/__w",
                &[],
                &[],
                "alpine:3.20",
                None,
                &[
                    "--privileged".into(),
                    "-v".into(),
                    "/:/host".into(),
                    "--user=0:0".into(),
                ],
            )
            .unwrap();
        let args = prepared.args();
        let separator = args.iter().position(|arg| arg == "--").unwrap();
        for flagged in ["--privileged", "/:/host", "--user=0:0"] {
            let position = args.iter().position(|arg| arg == flagged).unwrap();
            assert!(
                position > separator,
                "{flagged} must sit after the end-of-flags separator"
            );
        }
        // `-v` is also a legitimate mount flag; the action's copy is the last.
        assert!(args.iter().rposition(|arg| arg == "-v").unwrap() > separator);
        assert_eq!(args[separator + 1], "alpine:3.20");
    }

    /// An image that Docker would read as a flag never reaches a command line.
    #[test]
    fn flag_shaped_images_are_refused_everywhere_a_command_is_built() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("flag-image");
        spec.image = "--privileged".into();
        assert!(spec.start_args().is_err());
        assert!(spec.seed_mise_store_args().is_err());
        assert!(spec
            .prepare_run_docker_action_args("/__w", &[], &[], "--privileged", None, &[])
            .is_err());
        assert!(spec
            .prepare_run_node_action_args("/__w", &[], &[], &[], "-v/:/host", "/__a/index.js")
            .is_err());
        assert!(spec
            .build_docker_action_args(
                "--tag=evil",
                &spec.actions_host.join("Dockerfile"),
                &spec.actions_host,
            )
            .is_err());

        let service = ServiceContainerSpec {
            name: "velnor-service-evil".into(),
            image: "--privileged".into(),
            network_alias: "evil".into(),
            network: "velnor-net-1".into(),
            env: Vec::new(),
            ports: Vec::new(),
            options: Vec::new(),
        };
        assert!(service.start_args(&service_env_dir()).is_err());
    }

    /// Job and service environment are the workflow's secrets. Neither may
    /// appear on a command line that `/proc` publishes to every co-tenant.
    #[test]
    fn job_and_service_environment_never_reach_argv() {
        let mut spec = spec();
        spec.temp_host = container_test_temp("job-env-argv");
        spec.env = vec![
            ("GITHUB_TOKEN".into(), "PLACEHOLDER_SECRET".into()),
            ("PLAIN".into(), "visible".into()),
        ];
        let prepared = spec.start_args().unwrap();
        assert!(!prepared
            .args()
            .iter()
            .any(|arg| arg.contains("PLACEHOLDER_SECRET")));
        assert!(!prepared.args().iter().any(|arg| arg == "PLAIN=visible"));
        assert!(!prepared
            .args()
            .iter()
            .any(|arg| arg.starts_with("NODE_OPTIONS=")));
        assert!(rendered(&prepared).contains(&"GITHUB_TOKEN=PLACEHOLDER_SECRET".into()));

        let service = ServiceContainerSpec {
            name: "velnor-service-postgres".into(),
            image: "postgres:16".into(),
            network_alias: "postgres".into(),
            network: "velnor-net-1".into(),
            env: vec![("POSTGRES_PASSWORD".into(), "PLACEHOLDER_SECRET".into())],
            ports: Vec::new(),
            options: Vec::new(),
        };
        let prepared = service.start_args(&spec.env_dir()).unwrap();
        assert!(!prepared
            .args()
            .iter()
            .any(|arg| arg.contains("PLACEHOLDER_SECRET")));
        assert!(rendered(&prepared).contains(&"POSTGRES_PASSWORD=PLACEHOLDER_SECRET".into()));
    }

    #[test]
    fn splits_container_options_with_quotes() {
        assert_eq!(
            split_container_options(r#"--cpus 2 --health-cmd "pg_isready -U postgres""#),
            vec!["--cpus", "2", "--health-cmd", "pg_isready -U postgres"]
        );
    }
}

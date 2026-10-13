#![allow(dead_code)]

use std::{
    fmt::Write as _,
    fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use sha2::{Digest, Sha256};

use crate::docker_argv::{DockerArgv, DockerCommand, FlagSink, ImageReference};

pub use crate::docker_argv::PreparedDockerArgs;

const NODE_ACTION_BASE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const JOB_NOFILE_LIMIT: &str = "65536:65536";
const JOB_DOCKER_HOST: &str = "unix:///var/run/docker.sock";
const JOB_WORKFLOW_CLI: &str = "/usr/local/bin/velnor-workflow";
const JOB_WORKFLOW_CLI_SHA256: &str = "/usr/local/share/velnor/velnor-workflow.sha256";
const JOB_DONE_CONTAINER_DIR: &str = "/__velnor";

/// PID 1: tail the live console so `docker logs` mirrors GitHub, then exit
/// when Velnor writes the runner-owned done sentinel. `exec tail -F` alone made
/// finished/cancelled jobs immortal — the tailer outlived every job process.
/// A virtiofs hiccup can kill `tail`; respawn it until `job.done` rather
/// than exiting PID 1 and tearing down in-flight `docker exec`s. The
/// completion directory is a separate read-only mount, not part of the
/// job-writable temp tree.
pub(crate) const JOB_CONTAINER_PID1: &str = "mkdir -p /__t/_velnor && touch /__t/_velnor/console.log && tail -n +1 -F /__t/_velnor/console.log & tail_pid=$!; while [ ! -f /__velnor/job.done ]; do if ! kill -0 \"$tail_pid\" 2>/dev/null; then echo '[velnor] console tail exited; restarting logger' >&2; tail -n +1 -F /__t/_velnor/console.log & tail_pid=$!; fi; sleep 1; done; kill \"$tail_pid\" 2>/dev/null; wait \"$tail_pid\" 2>/dev/null; exit 0";

/// Name of the host-relative sentinel that ends PID 1. The host path is in a
/// private sibling directory, never beneath the job's RW temp mount.
pub(crate) const JOB_DONE_SENTINEL: &str = "job.done";

/// Daemon-owned runner identity. Dropped from step env by exact match in
/// `append_step_env` and re-asserted after it on every exec/run path, so a
/// workflow can neither shadow these names via `env:`/`GITHUB_ENV` nor win a
/// `-e`-over-`--env-file` precedence race with a multiline spoof.
const AUTHORITATIVE_RUNNER_ENV: [&str; 6] = [
    "VELNOR_EXECUTION_BACKEND",
    "VELNOR_SOURCE_SHA",
    "VELNOR_MANIFEST_VERSION",
    "VELNOR_HOST",
    "VELNOR_INSTANCE",
    "VELNOR_SLOT",
];

fn is_reserved_mbx_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    (upper.starts_with("MBX_") && upper != "MBX_DISABLE") || upper == "CARGO_TARGET_DIR"
}

fn is_docker_control_env(name: &str) -> bool {
    name.eq_ignore_ascii_case("DOCKER_HOST")
        || name.eq_ignore_ascii_case("DOCKER_CONTEXT")
        || name.eq_ignore_ascii_case("DOCKER_CONFIG")
        || name.eq_ignore_ascii_case("VELNOR_DOCKER_HOST")
        || name.eq_ignore_ascii_case("VELNOR_DOCKER_CONTEXT")
}

/// Docker flags that would impose a CPU/RAM/PID ceiling on the workload.
/// Job containers run unbounded: these never reach `docker run`, however
/// they arrived. Workflow `container.options` are already filtered at
/// admission; this is the emission backstop. `--shm-size` is deliberately
/// absent: shared-memory sizing is not a CPU/RAM ceiling, and browsers
/// need it larger than Docker's default, not smaller.
///
/// Accepted risk (security audit F4; spec §4.3 mandates unbounded): host
/// capacity is count-capped via the permit ledger, never ceiling-capped,
/// so any admitted job can starve co-tenants or OOM the host (fork bomb,
/// malloc loop, OOM-heavy build) with no host access needed — one
/// malicious or buggy same-repo PR job suffices. Same-repo PRs are
/// therefore untrusted for capacity. Throughput-neutral backstops
/// (`--pids-limit`, oomd, `MemoryHigh`) were considered and rejected:
/// the spec forbids ceilings, and the installer fails closed on any
/// surviving one (postinst + preflight assert infinity).
const QUOTA_FLAGS: [&str; 12] = [
    "--cpus",
    "--cpu-period",
    "--cpu-quota",
    "--cpu-shares",
    "--cpuset-cpus",
    "--cpuset-mems",
    "-m",
    "--memory",
    "--memory-reservation",
    "--memory-swap",
    "--memory-swappiness",
    "--pids-limit",
];

fn is_quota_flag(option: &str) -> bool {
    let name = option.split_once('=').map_or(option, |(name, _)| name);
    QUOTA_FLAGS.contains(&name)
}

fn append_options_without_quota(command: &mut impl FlagSink, options: &[String]) {
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        if is_quota_flag(option) {
            index += 1;
            // `--flag value` form: skip the value too. `--flag=value` and a
            // bare flag carry nothing further.
            if !option.contains('=')
                && options
                    .get(index)
                    .is_some_and(|value| !value.starts_with('-'))
            {
                index += 1;
            }
            continue;
        }
        command.flag(option.clone());
        index += 1;
    }
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
    /// The owning daemon slot's stable store key (`slot-N`), carried from
    /// the slot's own configuration. It scopes the
    /// per-slot persistent stores (mise installs, the mbx cache and target
    /// tree). `None` means no slot owns this job (standalone `run`), and
    /// those stores stay job-ephemeral: persistence is never granted to a
    /// job without a slot identity, because two unidentified jobs would
    /// share one mutable store. Never derived from the work-dir layout:
    /// that inference held only for `--work-dir <root>` with several
    /// slots, and silently lost every store on the default `_work` layout
    /// and on single-slot hosts.
    pub slot_store_key: Option<String>,
    pub env: Vec<(String, String)>,
    /// Admitted workflow `container.options`. Quota flags never survive to
    /// emission: job containers run unbounded.
    pub options: Vec<String>,
    pub services: Vec<ServiceContainerSpec>,
    pub node_action_image: String,
    pub docker_cli_host_path: Option<PathBuf>,
    pub docker_cli_plugin_host_dir: Option<PathBuf>,
    /// Host path of the apt-packaged `velnor-workflow` CLI. Jobs bind-mount it
    /// over the image copy so `apt install velnor-runner` is enough to update
    /// plan/run. `None` skips the mount (tests, macOS/dev hosts without apt).
    pub packaged_workflow_cli_host: Option<PathBuf>,
    pub docker_host_work_dir: Option<PathBuf>,
    pub verify_bind_mounts: bool,
    pub daemon_id: String,
    pub repository: Option<String>,
    /// The job's admitted scope (the pool ceiling narrowed by the job's
    /// trust class), normalized once at admission. This is the one spelling
    /// shared by the container mounts, the storage leases, and GC: every
    /// trust-scoped store path in the spec derives from it, so a custom pool
    /// scope (or a case variant of a known one) names the same namespace on
    /// the lease side and the mount side instead of collapsing to `untrusted`
    /// on one of them.
    pub store_trust_scope: String,
    /// Docker-only Mr Boxington store. `None` for MicroVM jobs.
    pub mbx_store_host: Option<PathBuf>,
    /// Docker-only explicit sccache action store.
    pub sccache_store_host: Option<PathBuf>,
}

/// The two filesystem views of a job Docker lease.
///
/// The runner binds the proxy on `host_visible`; Docker receives
/// `daemon_visible` as the bind-mount source. They are equal for a native
/// Linux daemon and may differ when Docker Desktop/OrbStack maps a host work
/// root into its Linux VM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DockerLeasePaths {
    pub(crate) host_visible: PathBuf,
    pub(crate) daemon_visible: PathBuf,
}

const DEFAULT_CONTAINER_EXEC_PATH: &str =
    "/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const MBX_CONTAINER_EXEC_PATH: &str =
    "/opt/mbx/bin:/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// Container mount point of the job's mbx store.
const MBX_CONTAINER_STORE: &str = "/var/cache/mbx";

impl JobContainerSpec {
    /// The `-v` operands of the daemon-shared Cargo subtrees: the persistent
    /// store of [`Self::store_trust_scope`], bind-mounted read-write like
    /// every other trust-scoped store. A `pr`-scope job's store was seeded
    /// from `trusted` by copy before the container started
    /// ([`crate::storage::seed_cargo_store`]); its writes persist in `pr`
    /// and are shared by every slot on the host.
    fn cargo_store_mount_operands(&self) -> Vec<String> {
        let store = cargo_store_host(&self.temp_host, self.store_trust_scope.as_str());
        CARGO_STORE_SUBTREES
            .iter()
            .map(|(subpath, target)| self.mount_arg(&store.join(subpath), target))
            .collect()
    }

    /// Append admitted workflow container options. Quota flags are stripped
    /// unconditionally: the container runs unbounded and no option spelling
    /// may reintroduce a ceiling at emission.
    fn append_container_options(&self, command: &mut DockerCommand) {
        append_options_without_quota(command, &self.options);
    }

    fn append_rust_acceleration(&self, command: &mut DockerCommand) -> io::Result<()> {
        if let Some(host) = &self.mbx_store_host {
            command.pair("-v", self.mount_arg(host, MBX_CONTAINER_STORE));
            let cache_dir = self.mbx_cache_container_dir();
            let target_root = self.mbx_target_container_dir();
            command.envs([
                ("MBX_CACHE_DIR", cache_dir.as_str()),
                ("MBX_TARGET_ROOT", target_root.as_str()),
                ("CARGO_TARGET_DIR", target_root.as_str()),
                ("MBX_GC_AUTO", "true"),
                ("MBX_GC_MAX_SIZE", "20GiB"),
                ("MBX_GC_INCREMENTAL_MAX_SIZE", "20GiB"),
                ("MBX_GC_INCREMENTAL_MAX_AGE", "30d"),
                ("MBX_TARGET_MAX_SIZE", "30GiB"),
                ("MBX_GC_MAX_TOTAL_SIZE", "50GiB"),
            ]);
        } else if let Some(host) = &self.sccache_store_host {
            // Explicit sccache compatibility mode: mount and env owned by the
            // compat module; the default mbx branch above is untouched.
            command.pair(
                "-v",
                self.mount_arg(host, crate::sccache_compat::CONTAINER_DIR),
            );
            command.envs(crate::sccache_compat::container_env());
        }
        Ok(())
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
        self.validate_docker_host_path_mapping()?;
        let image = self.image_reference()?;
        self.prepare_job_done_mount()?;
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
            format!(
                "{}:ro",
                self.mount_arg(&self.job_done_host_dir(), JOB_DONE_CONTAINER_DIR)
            ),
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
            // Package-manager download caches are not workspace output. Persist
            // them per trust/repository so bun, npm, and OpenTofu jobs stay warm
            // without hitting the hosted actions cache from self-hosted runners.
            "-v".into(),
            self.mount_arg(
                &self.bun_install_cache_store_host(),
                "/github/home/.bun/install/cache",
            ),
            "-v".into(),
            self.mount_arg(&self.npm_cache_store_host(), "/github/home/.npm"),
            "-v".into(),
            self.mount_arg(
                &self.terraform_plugin_cache_store_host(),
                "/github/home/.terraform.d/plugin-cache",
            ),
        ]);
        // Share immutable Cargo downloads and indexes across the daemon,
        // but keep extracted registry sources and git checkouts in the
        // job home. Separate containers can otherwise race while creating
        // `.cargo-ok` in the same extracted crate (Cargo's package-cache
        // lock does not serialize that mutation across container jobs).
        for operand in self.cargo_store_mount_operands() {
            args.flags(["-v".into(), operand]);
        }
        args.flags([
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
                &mise_store_host(&self.temp_host, self.store_trust_scope.as_str()).join("cache"),
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
        self.append_container_options(args);
        // Docker creates the actual workload in dockerd's cgroup, not in the
        // Velnor worker process. Place the outer job under the runner-owned
        // identity cgroup (no ceiling); the job lease proxy applies the same
        // placement — and the same unbounded policy — to containers created
        // from inside the job.
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

        self.append_docker_socket_mount(args)?;
        self.append_docker_cli_mounts(args);
        self.append_packaged_workflow_cli_mounts(args)?;

        // The per-job network is runner policy. Keep it after expanded job
        // options so the job cannot be displaced from the network shared
        // with its workflow services.
        args.pair("--network", self.network.clone());

        // Daemon-owned acceleration mounts and variables must follow both
        // workflow environment and expanded container options. A trusted job
        // may add options, but cannot redirect a persistent store or stack
        // sccache with the image's mbx shim.
        self.append_rust_acceleration(args)?;

        // PID 1 supervises the live console tail. `tail -F` alone would
        // keep the container alive after the job is terminal; the supervisor
        // exits when Velnor writes the runner-owned `/__velnor/job.done`.
        command
            .image(&image)
            .operands(["sh", "-c", JOB_CONTAINER_PID1])
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
        self.validate_docker_host_path_mapping()?;
        let image = self.image_reference()?;
        let store = mise_store_host(&self.temp_host, self.store_trust_scope.as_str());
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

    /// The ordered environment for one Engine-API script exec: base exec
    /// env, step env, authoritative runner env — the same three appends in
    /// the same order `exec_command` feeds the CLI
    /// builder, so daemon last-wins resolves identically on both legs. The
    /// scratch builder is never finished, so no env file is written; control
    /// and authoritative filtering is shared, not duplicated, with the CLI
    /// leg by construction.
    pub fn script_exec_env(&self, env: &[(String, String)]) -> Vec<(String, String)> {
        let mut builder = DockerCommand::new(self.env_dir(), ["exec"]);
        self.append_base_exec_env(&mut builder);
        self.append_step_env(&mut builder, env);
        self.append_authoritative_runner_env(&mut builder);
        builder.recorded_env()
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
        self.append_authoritative_runner_env(&mut builder);
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
    /// Daemon-owned identity names are dropped by exact match, so a spoof
    /// never reaches the command builder on any path — a multiline spoof
    /// would otherwise route to `-e` while the clean value goes to
    /// `--env-file`, and Docker resolves `-e` over `--env-file`.
    fn append_step_env(&self, command: &mut DockerCommand, env: &[(String, String)]) {
        for (name, value) in env {
            if is_docker_control_env(name) {
                continue;
            }
            if AUTHORITATIVE_RUNNER_ENV.contains(&name.as_str()) {
                continue;
            }
            if is_reserved_mbx_env(name) {
                continue;
            }
            command.env(name.clone(), value.clone());
        }
    }

    /// Re-assert the daemon-owned backend and build identity after workflow
    /// and step env. The values are injected by `backend_advertising_env`; a
    /// workflow must not be able to switch a Velnor job to the GitHub command
    /// contract through `env:` or `GITHUB_ENV`, nor spoof the release it runs
    /// under. Defense in depth behind the exact-match drop in
    /// `append_step_env`.
    fn append_authoritative_runner_env(&self, command: &mut DockerCommand) {
        for name in AUTHORITATIVE_RUNNER_ENV {
            if let Some((_, value)) = self.env.iter().find(|(env_name, _)| env_name == name) {
                command.env(name, value.clone());
            }
        }
        self.append_authoritative_mbx_env(command);
    }

    /// Authoritative mbx paths. `GITHUB_ENV` / step `env:` cannot replace the
    /// daemon-owned cache or managed-target roots. Re-asserted after step env,
    /// same last-wins rule as `VELNOR_*`.
    fn append_authoritative_mbx_env(&self, command: &mut DockerCommand) {
        if self.mbx_store_host.is_none() {
            return;
        }
        command.env("MBX_CACHE_DIR", self.mbx_cache_container_dir());
        command.env("MBX_TARGET_ROOT", self.mbx_target_container_dir());
        command.env("CARGO_TARGET_DIR", self.mbx_target_container_dir());
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
        self.validate_docker_host_path_mapping()?;
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
        ]);
        let mut mounts = vec![
            self.mount_arg(&self.workspace_host, "/__w"),
            self.mount_arg(&self.workspace_host, "/github/workspace"),
            self.mount_arg(&self.temp_host, "/__t"),
            self.mount_arg(&self.temp_host, "/tmp"),
            self.mount_arg(&self.temp_host, "/github/runner_temp"),
            self.mount_arg(&self.temp_host, "/github/file_commands"),
            self.mount_arg(&self.home_host, "/github/home"),
            self.mount_arg(&workflow_host(&self.temp_host), "/github/workflow"),
            format!("{}:ro", self.mount_arg(&self.actions_host, "/__a")),
            self.mount_arg(&self.tools_host, "/__tool"),
        ];
        mounts.extend(self.node_action_path_mounts(path_prepend, &mounts));
        for mount in mounts {
            args.flag("-v");
            args.flag(mount);
        }
        args.env("HOME", "/github/home");
        args.env("RUNNER_TOOL_CACHE", "/__tool");
        args.env("AGENT_TOOLSDIRECTORY", "/__tool");
        // The Node image entrypoint/shell drops env names with '-', but
        // @actions/core reads inputs like INPUT_PUSH-TO-REGISTRY.
        args.pair("--entrypoint", "node");
        self.append_ownership_labels(args);
        self.append_docker_socket_mount(args)?;
        self.append_docker_cli_mounts(args);
        self.append_rust_acceleration(args)?;
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
        self.append_authoritative_runner_env(args);
        let prepared = command
            .image(&image)
            .operand(entrypoint_container_path.to_owned())
            .finish()?;
        audit_argv_for_secrets(prepared.args(), secret_masks)?;
        Ok(prepared)
    }

    /// Bind mounts that make job-container PATH directories resolvable inside
    /// the Node action sidecar.
    ///
    /// actions/runner executes node actions — main and post — on the runner
    /// host, so every PATH entry an earlier step recorded is inherently
    /// visible to them. Velnor runs those actions in a sidecar that sees only
    /// what this argv mounts, so an entry recorded inside the job container
    /// named a directory that does not exist there: a post step failed with
    /// `Unable to locate executable file: cargo` while every main step of the
    /// same job resolved it. Each entry is therefore projected at the same
    /// absolute path it has in the job container, from the daemon-host
    /// directory that backs that container path in `start_args`; any other
    /// absolute entry binds the host path of the same name (Docker creates a
    /// missing host directory, exactly as it does for the job's own mounts).
    /// Entries the sidecar already sees through one of `existing_mounts` are
    /// dropped, so a PATH entry can never shadow the runner-owned /__w, /__t
    /// and /github views, and identical host paths are mounted once.
    fn node_action_path_mounts(
        &self,
        path_prepend: &[String],
        existing_mounts: &[String],
    ) -> Vec<String> {
        let mut hosts: Vec<String> = existing_mounts
            .iter()
            .map(|mount| mount_host(mount).to_owned())
            .collect();
        let mut mounts = Vec::new();
        for entry in path_prepend {
            let Some(source) = self.node_action_path_source(entry, existing_mounts) else {
                continue;
            };
            let mount = self.mount_arg(&source, entry);
            let host = mount_host(&mount).to_owned();
            if hosts.contains(&host) {
                continue;
            }
            hosts.push(host);
            mounts.push(mount);
        }
        mounts
    }

    /// Daemon-host directory that backs a job-container PATH entry, or `None`
    /// when the sidecar already sees that container path.
    fn node_action_path_source(&self, entry: &str, existing_mounts: &[String]) -> Option<PathBuf> {
        let entry = entry.trim();
        // Not a usable mount destination: the base PATH entries are already
        // spelled out below, and `:`, `..` or a relative form would corrupt
        // the `-v` argument rather than describe a job-container directory.
        if entry.is_empty()
            || !entry.starts_with('/')
            || entry.contains(':')
            || entry.split('/').any(|component| component == "..")
            || NODE_ACTION_BASE_PATH.split(':').any(|base| base == entry)
        {
            return None;
        }
        // Container paths whose daemon-host backing directory `start_args`
        // mounts at that same container path. The mise adapter records tool
        // bin directories inside these stores, and no directory of that name
        // exists on the daemon host: only the store holds the installed tools.
        let store_backed: [(&str, PathStoreResolver); 3] = [
            ("/opt/mise/installs", Self::mise_executable_store_host),
            ("/opt/velnor/mise-binaries", Self::mise_binary_store_host),
            ("/github/home/.cargo/bin", Self::cargo_executable_store_host),
        ];
        for (container_prefix, store) in store_backed {
            if let Some(rest) = entry.strip_prefix(container_prefix) {
                let rest = rest.strip_prefix('/').unwrap_or(rest);
                let source = store(self);
                return Some(if rest.is_empty() {
                    source
                } else {
                    source.join(rest)
                });
            }
        }
        // Already visible through a mount above. Those views are load-bearing:
        // a PATH entry landing on /__w, /__t or /github must not rebind them
        // to a host path of its own choosing.
        if existing_mounts
            .iter()
            .any(|mount| container_path_under(entry, mount_container(mount)))
        {
            return None;
        }
        Some(PathBuf::from(entry))
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
        self.validate_docker_host_path_mapping()?;
        let dockerfile_host = self.docker_host_path_checked(dockerfile_host, "Dockerfile")?;
        let context_host = self.docker_host_path_checked(context_host, "Docker build context")?;
        let image = ImageReference::parse(image)?;
        let mut args = DockerArgv::new(["build"]);
        args.flags([
            "--cgroup-parent".to_owned(),
            crate::docker_lease::JOB_CGROUP_PARENT.to_owned(),
            "--tag".to_owned(),
            image.as_str().to_owned(),
            "--file".to_owned(),
            dockerfile_host.display().to_string(),
        ]);
        Ok(args
            .operands()
            .operand(context_host.display().to_string())
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
        self.validate_docker_host_path_mapping()?;
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
        self.append_docker_socket_mount(args)?;
        self.append_docker_cli_mounts(args);
        self.append_rust_acceleration(args)?;
        self.append_job_cgroup_parent(args);
        if let Some(entrypoint) = entrypoint {
            args.pair("--entrypoint", entrypoint.to_owned());
        }
        self.append_step_env(args, env);
        self.append_authoritative_runner_env(args);
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

    /// Resolve both filesystem views of the job lease.
    pub(crate) fn docker_lease_paths(&self) -> io::Result<DockerLeasePaths> {
        let host_visible = self.guest_docker_socket_host();
        let daemon_visible =
            self.docker_host_path_checked(&host_visible, "job Docker lease socket")?;
        Ok(DockerLeasePaths {
            host_visible,
            daemon_visible,
        })
    }

    /// Validate every work-tree path that Docker mounts or passes to the
    /// daemon when a VM path mapping is configured. A lexical fallback to the
    /// runner path would make Docker Desktop/OrbStack mount a different file,
    /// so mapping failures are fatal and explain which roots must agree.
    pub(crate) fn validate_docker_host_path_mapping(&self) -> io::Result<()> {
        if self.docker_host_work_dir.is_none() {
            return Ok(());
        }

        let paths = [
            ("workspace", self.workspace_host.clone()),
            ("temp", self.temp_host.clone()),
            ("home", self.home_host.clone()),
            ("actions", self.actions_host.clone()),
            ("tools", self.tools_host.clone()),
            ("workflow", workflow_host(&self.temp_host)),
            ("Playwright store", self.playwright_browser_store_host()),
            ("Bun install cache", self.bun_install_cache_store_host()),
            ("npm cache", self.npm_cache_store_host()),
            (
                "OpenTofu plugin cache",
                self.terraform_plugin_cache_store_host(),
            ),
            (
                "Cargo registry cache",
                cargo_store_host(&self.temp_host, self.store_trust_scope.as_str())
                    .join("registry/cache"),
            ),
            (
                "Cargo registry index",
                cargo_store_host(&self.temp_host, self.store_trust_scope.as_str())
                    .join("registry/index"),
            ),
            (
                "Cargo git database",
                cargo_store_host(&self.temp_host, self.store_trust_scope.as_str()).join("git/db"),
            ),
            ("Cargo executable store", self.cargo_executable_store_host()),
            ("mise executable store", self.mise_executable_store_host()),
            ("mise binary store", self.mise_binary_store_host()),
            (
                "mise cache",
                mise_store_host(&self.temp_host, self.store_trust_scope.as_str()).join("cache"),
            ),
        ];
        for (label, path) in paths {
            self.docker_host_path_checked(&path, label)?;
        }
        if let Some(path) = &self.mbx_store_host {
            self.docker_host_path_checked(path, "MBX store")?;
        }
        if let Some(path) = &self.sccache_store_host {
            self.docker_host_path_checked(path, "sccache store")?;
        }
        // This also rejects a lease path shortened outside the mapped work
        // root; the host listener and daemon mount must refer to one socket.
        self.docker_lease_paths()?;
        Ok(())
    }

    fn append_docker_socket_mount(&self, args: &mut impl FlagSink) -> io::Result<()> {
        if !self.mount_docker_socket {
            return Ok(());
        }
        let daemon_visible = self.guest_docker_socket_bind_source()?;
        args.pair(
            "-v",
            format!("{}:/var/run/docker.sock", daemon_visible.display()),
        );
        Ok(())
    }

    /// Unix socket the Linux job container should see as `/var/run/docker.sock`.
    ///
    /// On a native Linux host that is the per-job lease proxy. On macOS the
    /// runner binds that proxy on a host path the OrbStack/Docker Desktop VM
    /// can *see* as a socket inode, but `connect()` is `ECONNREFUSED`
    /// (virtiofs). The daemon's own socket is special-cased and connectable.
    /// A TCP lease proxy is the root fix; until then trusted macOS jobs
    /// mount the resolved host socket.
    pub(crate) fn guest_can_connect_host_bound_unix_lease() -> bool {
        !cfg!(target_os = "macos")
    }

    fn guest_docker_socket_bind_source(&self) -> io::Result<PathBuf> {
        if Self::guest_can_connect_host_bound_unix_lease() {
            return Ok(self.docker_lease_paths()?.daemon_visible);
        }
        let endpoint = crate::docker::engine::resolve_docker_endpoint().map_err(|error| {
            io::Error::other(format!(
                "resolve host Docker socket for macOS job mount: {error}"
            ))
        })?;
        Ok(endpoint.socket.canonicalize().unwrap_or(endpoint.socket))
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

    fn append_packaged_workflow_cli_mounts(&self, args: &mut impl FlagSink) -> io::Result<()> {
        let Some(host) = self.packaged_workflow_cli_host.as_deref() else {
            return Ok(());
        };
        if !host.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "packaged velnor-workflow missing at {}; install velnor-runner from apt",
                    host.display()
                ),
            ));
        }
        fs::create_dir_all(&self.temp_host)?;
        let digest = Sha256::digest(fs::read(host)?);
        let mut hex = String::with_capacity(64);
        for byte in digest {
            let _ = write!(&mut hex, "{byte:02x}");
        }
        let sidecar = self.temp_host.join("velnor-workflow.sha256");
        fs::write(&sidecar, format!("{hex}  {JOB_WORKFLOW_CLI}\n"))?;
        args.pair(
            "-v",
            format!("{}:ro", self.mount_arg(host, JOB_WORKFLOW_CLI)),
        );
        args.pair(
            "-v",
            format!("{}:ro", self.mount_arg(&sidecar, JOB_WORKFLOW_CLI_SHA256)),
        );
        Ok(())
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
                cargo_executable_store_host(
                    &self.temp_host,
                    self.store_trust_scope.as_str(),
                    &repository,
                )
            },
        )
    }

    pub(crate) fn mise_executable_store_host(&self) -> PathBuf {
        match (self.repository_store_key(), self.slot_store_key.as_deref()) {
            (Some(repository), Some(slot)) => mise_executable_store_host(
                &self.temp_host,
                self.store_trust_scope.as_str(),
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

    /// Container-side mbx cache dir: a per-slot subdir of the shared mount.
    ///
    /// Every job container mounts the same host store at `/var/cache/mbx`, and
    /// mbx takes its registrar flock(EX) then a per-hash lease flock(EX) with
    /// no timeouts. Every container runs mbx as the same pid, so all of them
    /// serialize on one `registrar.lock` while the holder blocks on the shared
    /// lease file — an observed cross-container ABBA deadlock (1 holder + N
    /// waiters, 0% CPU, jobs hang to the GitHub timeout). Each slot therefore
    /// gets a disjoint `slots/slot-N` subdir — the same `slots/<slot>`
    /// precedent as the mise installs mount — which relocates mbx's registrar,
    /// leases, and checkouts (`MBX_CACHE_DIR` → `cache_dir` →
    /// `cache_dir/incremental`). The mount itself is unchanged, so the store
    /// root stays shared and each slot's cache stays warm across its own jobs.
    /// Without a slot identity (standalone `run`) the subdir isolates per
    /// job name instead of falling back to the shared root.
    pub(crate) fn mbx_cache_container_dir(&self) -> String {
        crate::mbx_store::slot_cache_dir(Path::new(MBX_CONTAINER_STORE), &self.mbx_slot_key())
            .to_string_lossy()
            .into_owned()
    }

    /// Host-side path of this job's mbx cache subdir, created before start.
    /// `None` exactly when the mbx store is disabled (explicit sccache mode).
    pub(crate) fn mbx_cache_store_host(&self) -> Option<PathBuf> {
        self.mbx_store_host
            .as_ref()
            .map(|store| crate::mbx_store::slot_cache_dir(store, &self.mbx_slot_key()))
    }

    /// Container-side managed-target root: a per-slot subdir of the shared
    /// mount. Cargo's `.cargo-lock` lives under `MBX_TARGET_ROOT`; a shared
    /// root globally serializes concurrent Rust jobs. Sequential jobs on the
    /// same slot keep one warm tree. Missing slot identity isolates per job
    /// name, never the global `/var/cache/mbx/targets` root.
    pub(crate) fn mbx_target_container_dir(&self) -> String {
        crate::mbx_store::slot_target_dir(Path::new(MBX_CONTAINER_STORE), &self.mbx_slot_key())
            .to_string_lossy()
            .into_owned()
    }

    pub(crate) fn mbx_target_store_host(&self) -> Option<PathBuf> {
        self.mbx_store_host
            .as_ref()
            .map(|store| crate::mbx_store::slot_target_dir(store, &self.mbx_slot_key()))
    }

    /// Host directory of the sentinel that ends the container's PID 1
    /// supervisor. It is a sibling of the job temp directory, so the job's
    /// RW `/__t` mount cannot create or replace it.
    pub(crate) fn job_done_host_dir(&self) -> PathBuf {
        let temp_name = self
            .temp_host
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("temp");
        self.temp_host
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".{temp_name}-velnor-control"))
    }

    /// Ensure Docker receives an existing, private directory for the
    /// read-only completion mount. Docker otherwise creates a missing `-v`
    /// source as a directory with daemon-dependent semantics.
    pub(crate) fn prepare_job_done_mount(&self) -> io::Result<()> {
        if !self.temp_host.is_absolute() || has_parent_component(&self.temp_host) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "job temp path is not a normalized absolute path",
            ));
        }
        if self.temp_host.parent().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "job temp path has no parent for private completion control",
            ));
        }
        fs::create_dir_all(&self.temp_host)?;
        let dir = self.job_done_host_dir();
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(&dir)?;
        if !metadata.file_type().is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "private completion control path is not a directory: {}",
                    dir.display()
                ),
            ));
        }
        #[cfg(unix)]
        {
            if metadata.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "private completion control directory is not runner-owned: {}",
                        dir.display()
                    ),
                ));
            }
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Host path of the sentinel that ends the container's PID 1 supervisor.
    pub(crate) fn job_done_host_path(&self) -> PathBuf {
        self.job_done_host_dir().join(JOB_DONE_SENTINEL)
    }

    fn mbx_slot_key(&self) -> String {
        self.slot_store_key.clone().unwrap_or_else(|| {
            eprintln!(
                "forensics.lifecycle: mbx cache isolated per job: job {} has no runner slot identity",
                self.name
            );
            sanitize_store_key(&self.name)
        })
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
                mise_binary_store_host(
                    &self.temp_host,
                    self.store_trust_scope.as_str(),
                    &repository,
                )
            },
        )
    }

    fn playwright_browser_store_host(&self) -> PathBuf {
        self.repository_scoped_home_cache_store_host("playwright", ".cache/ms-playwright")
    }

    fn bun_install_cache_store_host(&self) -> PathBuf {
        self.repository_scoped_home_cache_store_host("bun-install-cache", ".bun/install/cache")
    }

    fn npm_cache_store_host(&self) -> PathBuf {
        self.repository_scoped_home_cache_store_host("npm", ".npm")
    }

    fn terraform_plugin_cache_store_host(&self) -> PathBuf {
        self.repository_scoped_home_cache_store_host(
            "terraform-plugin-cache",
            ".terraform.d/plugin-cache",
        )
    }

    fn repository_scoped_home_cache_store_host(
        &self,
        store_leaf: &str,
        home_relative: &str,
    ) -> PathBuf {
        self.repository_store_key().map_or_else(
            || self.home_host.join(home_relative),
            |repository| {
                repository_scoped_home_cache_store_host(
                    &self.temp_host,
                    self.store_trust_scope.as_str(),
                    &repository,
                    store_leaf,
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

    fn docker_host_path_checked(&self, host_path: &Path, label: &str) -> io::Result<PathBuf> {
        let Some(docker_work_dir) = &self.docker_host_work_dir else {
            return Ok(host_path.to_path_buf());
        };
        let local_work_dir = self.local_work_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "cannot map Docker {label} '{}': derive the runner work root from an absolute job temp path",
                    host_path.display()
                ),
            )
        })?;
        validate_docker_mapping_root(&local_work_dir, "runner work root")?;
        validate_docker_mapping_root(docker_work_dir, "Docker daemon work root")?;
        if !host_path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "cannot map Docker {label} '{}': host-visible paths must be absolute when docker_host_work_dir is set",
                    host_path.display()
                ),
            ));
        }
        if has_parent_component(host_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "cannot map Docker {label} '{}': parent ('..') path components are unsafe; use normalized paths below the runner work root",
                    host_path.display()
                ),
            ));
        }
        let relative = host_path.strip_prefix(&local_work_dir).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "cannot map Docker {label} '{}': it escapes host-visible runner work root '{}'; set docker_host_work_dir to the daemon-visible equivalent of that root",
                    host_path.display(),
                    local_work_dir.display()
                ),
            )
        })?;
        Ok(docker_work_dir.join(relative))
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

fn validate_docker_mapping_root(path: &Path, label: &str) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Docker {label} must be absolute for a VM-backed daemon; got '{}'",
                path.display()
            ),
        ));
    }
    if has_parent_component(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Docker {label} '{}' contains an unsafe parent ('..') component; use a normalized absolute path",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| component == Component::ParentDir)
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
        // Quota flags never reach `docker run`, however they arrived
        // (admission already filtered workflow options; this is the
        // emission backstop, same as the job container).
        append_options_without_quota(args, &self.options);
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
    /// The argv a script step runs under this shell. Shared by the CLI exec
    /// leg and the Engine-API exec leg so the executed command is identical.
    pub(crate) fn command_args(self, script_path: &str) -> Vec<String> {
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

/// Daemon-host store directory resolver for one job container.
type PathStoreResolver = fn(&JobContainerSpec) -> PathBuf;

/// Host side of a rendered `-v host:container[:ro]` argument. Host paths on
/// Linux cannot contain `:`, so the first field is authoritative.
fn mount_host(mount: &str) -> &str {
    mount.split(':').next().unwrap_or_default()
}

/// Container side of a rendered `-v host:container[:ro]` argument.
fn mount_container(mount: &str) -> &str {
    mount.split(':').nth(1).unwrap_or_default()
}

/// Component-wise containment, so `/__w/repo` is under `/__w` while
/// `/__w/repo-sibling` is not.
fn container_path_under(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return false;
    }
    prefix == "/"
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
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

/// The daemon-shared Cargo store subtrees and their container mount points.
/// Registry sources and git checkouts stay in the job home (see
/// [`JobContainerSpec::start_args`]); these three are the immutable-by-key
/// downloads and indexes worth sharing, and the ones the `pr`-scope seed
/// copies from `trusted` (D18, [`crate::storage::seed_cargo_store`]).
pub(crate) const CARGO_STORE_SUBTREES: [(&str, &str); 3] = [
    ("registry/cache", "/github/home/.cargo/registry/cache"),
    ("registry/index", "/github/home/.cargo/registry/index"),
    ("git/db", "/github/home/.cargo/git/db"),
];

/// Host-persistent Cargo download/index store, daemon-shared like the
/// compiler stores.
/// Extracted registry sources and git checkouts remain job-local because they
/// are mutable during materialization and are unsafe to share across slots.
///
/// `trust_scope` is the scope in effect for the caller (the job's admitted
/// scope on the execution path). It selects the canonical namespace; the
/// legacy root carries no trust segment.
pub(crate) fn cargo_store_host(temp_host: &Path, trust_scope: &str) -> PathBuf {
    crate::storage::cache_class_path(
        &daemon_store_root(temp_host),
        trust_scope,
        "cargo",
        "_velnor_cargo",
    )
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
///
/// `trust_scope` is the scope in effect for the caller (the job's admitted
/// scope on the execution path): the namespace below the root in the legacy
/// layout, the root namespace in the canonical layout.
pub(crate) fn cargo_executable_store_host(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    crate::storage::child_with_legacy_trust(
        cargo_store_host(temp_host, trust_scope),
        "bin",
        trust_scope,
    )
    .join(sanitize_store_key(repository))
}

/// Host-persistent mise tool store (installs + cache subdirs are mounted).
///
/// `trust_scope` is the scope in effect for the caller (the job's admitted
/// scope on the execution path). It selects the canonical namespace; the
/// legacy root carries no trust segment.
pub(crate) fn mise_store_host(temp_host: &Path, trust_scope: &str) -> PathBuf {
    crate::storage::cache_class_path(
        &daemon_store_root(temp_host),
        trust_scope,
        "mise",
        "_velnor_mise",
    )
}

pub(crate) fn git_mirror_store_host(temp_host: &Path, trust_scope: &str) -> PathBuf {
    crate::git_mirror::store_root(&daemon_store_root(temp_host), trust_scope)
}

/// Host-persistent mise executable store, scoped by trust + repository.
///
/// `trust_scope` is the scope in effect for the caller (the job's admitted
/// scope on the execution path): the namespace below the root in the legacy
/// layout, the root namespace in the canonical layout.
pub(crate) fn mise_executable_store_host(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    crate::storage::child_with_legacy_trust(
        mise_store_host(temp_host, trust_scope),
        "installs",
        trust_scope,
    )
    .join(sanitize_store_key(repository))
}

/// Host-persistent per-version mise BINARY store, scoped by trust + repository.
///
/// The setup script writes `<os-arch>/<exact-version>/mise` plus `metadata.json`
/// inside this scope (Plan 008 Step 2), so a fresh job reuses a verified binary
/// instead of mutating the read-only baked `/opt/mise/bin` bootstrap. Lives
/// under the same `mise` cache class as `installs`/`rustup`, so the mise GC
/// budget covers it and a per-scope lease protects it while a job holds it.
pub(crate) fn mise_binary_store_host(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    crate::storage::child_with_legacy_trust(
        mise_store_host(temp_host, trust_scope),
        "binaries",
        trust_scope,
    )
    .join(sanitize_store_key(repository))
}

/// Host-persistent Playwright browser downloads, scoped by trust + repository.
///
/// `trust_scope` is the scope in effect for the caller (the job's admitted
/// scope on the execution path): the namespace below the root in the legacy
/// layout, the root namespace in the canonical layout.
pub(crate) fn playwright_browser_store_host(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    repository_scoped_home_cache_store_host(temp_host, trust_scope, repository, "playwright")
}

pub(crate) fn bun_install_cache_store_host(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    repository_scoped_home_cache_store_host(temp_host, trust_scope, repository, "bun-install-cache")
}

pub(crate) fn npm_cache_store_host(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    repository_scoped_home_cache_store_host(temp_host, trust_scope, repository, "npm")
}

pub(crate) fn terraform_plugin_cache_store_host(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    repository_scoped_home_cache_store_host(
        temp_host,
        trust_scope,
        repository,
        "terraform-plugin-cache",
    )
}

fn repository_scoped_home_cache_store_host(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
    store_leaf: &str,
) -> PathBuf {
    let root = crate::storage::cache_class_path(
        &daemon_store_root(temp_host),
        trust_scope,
        "caches",
        "_velnor_caches",
    );
    crate::storage::append_legacy_trust(root, trust_scope)
        .join(sanitize_store_key(repository))
        .join(store_leaf)
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

/// Store key of daemon slot `slot_index` (1-based): the `slot-N` name the
/// daemon gives the slot everywhere else (its config dir, its logs), so a
/// store warmed under one layout stays warm under the next.
#[must_use]
pub(crate) fn slot_store_key(slot_index: usize) -> String {
    sanitize_store_key(&format!("slot-{slot_index}"))
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

    /// Every `-v host:container[:ro]` argument of a rendered command.
    fn mount_args(prepared: &PreparedDockerArgs) -> Vec<String> {
        rendered(prepared)
            .windows(2)
            .filter(|pair| pair[0] == "-v")
            .map(|pair| pair[1].clone())
            .collect()
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
            slot_store_key: None,
            env: vec![("NODE_OPTIONS".into(), "--max-old-space-size=4096".into())],
            options: Vec::new(),
            services: Vec::new(),
            node_action_image: "node:24-bookworm".into(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            packaged_workflow_cli_host: None,
            docker_host_work_dir: None,
            verify_bind_mounts: false,
            daemon_id: "test-daemon".into(),
            repository: Some("acme/repo".into()),
            store_trust_scope: "trusted".to_owned(),
            mbx_store_host: Some(work.join("_velnor_mbx/trusted")),
            sccache_store_host: None,
        }
    }

    #[test]
    fn pr_scope_jobs_bind_the_persistent_pr_cargo_store_read_write() {
        // A PR job on a trusted pool (D18) mounts the `pr` scope's Cargo
        // subtrees exactly like a trusted job mounts `trusted`'s: plain
        // read-write binds of the persistent host store, shared by every
        // slot. No overlay volume, no scratch, and never the trusted store.
        let mut job = spec();
        job.store_trust_scope = crate::trust_scope::PR_STORE_SCOPE.to_owned();
        let args = rendered(&job.start_args().unwrap());
        // The test work root has no `VELNOR_STORAGE_ROOT`, so the paths are
        // the legacy layout's; the scope selects the store root exactly as
        // it does for every other trust-scoped store in this spec.
        let pr_store = cargo_store_host(&job.temp_host, crate::trust_scope::PR_STORE_SCOPE);
        let trusted_store = cargo_store_host(&job.temp_host, crate::trust_scope::TRUSTED);
        let cargo_mounts = mount_args(&job.start_args().unwrap())
            .into_iter()
            .filter(|mount| mount.contains("/.cargo/"))
            .collect::<Vec<_>>();
        for (subpath, target) in CARGO_STORE_SUBTREES {
            assert!(
                has_mount(&args, &pr_store.join(subpath), target),
                "expected a read-write pr bind for {target}, got {args:?}"
            );
            assert!(!has_read_only_mount(&args, &pr_store.join(subpath), target));
            assert_eq!(
                cargo_mounts
                    .iter()
                    .filter(|mount| mount.ends_with(&format!(":{target}")))
                    .count(),
                1,
                "exactly one bind serves {target}: {cargo_mounts:?}"
            );
        }
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("overlay") || arg.contains("upperdir")),
            "{args:?}"
        );

        job.store_trust_scope = crate::trust_scope::TRUSTED.to_owned();
        let args = rendered(&job.start_args().unwrap());
        for (subpath, target) in CARGO_STORE_SUBTREES {
            assert!(has_mount(&args, &trusted_store.join(subpath), target));
        }
    }

    #[test]
    fn packaged_workflow_cli_is_bind_mounted_over_the_image_copy() {
        let mut job = spec();
        fs::create_dir_all(&job.temp_host).unwrap();
        let cli = job.temp_host.join("packaged-velnor-workflow");
        fs::write(&cli, b"workflow-cli").unwrap();
        job.packaged_workflow_cli_host = Some(cli.clone());
        let prepared = job.start_args().unwrap();
        assert!(has_read_only_mount(
            &rendered(&prepared),
            &cli,
            "/usr/local/bin/velnor-workflow"
        ));
        let sidecar = job.temp_host.join("velnor-workflow.sha256");
        assert!(has_read_only_mount(
            &rendered(&prepared),
            &sidecar,
            "/usr/local/share/velnor/velnor-workflow.sha256"
        ));
        let sidecar_text = fs::read_to_string(&sidecar).unwrap();
        assert!(
            sidecar_text.ends_with("  /usr/local/bin/velnor-workflow\n"),
            "{sidecar_text}"
        );
        assert_eq!(
            sidecar_text.len(),
            64 + "  /usr/local/bin/velnor-workflow\n".len()
        );
    }

    #[test]
    fn packaged_workflow_cli_missing_from_apt_fails_closed() {
        let mut job = spec();
        job.packaged_workflow_cli_host = Some(job.temp_host.join("missing-velnor-workflow"));
        let error = job.start_args().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains("install velnor-runner from apt"));
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
        let job = slotted_spec("mbx-default-gc");
        let mbx_store = job.mbx_store_host.clone().unwrap();
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        assert!(args.contains(&format!("{}:/var/cache/mbx", mbx_store.display())));
        assert!(args.contains(&"MBX_CACHE_DIR=/var/cache/mbx/slots/slot-1".into()));
        assert!(args.contains(&"MBX_TARGET_ROOT=/var/cache/mbx/targets/slots/slot-1".into()));
        assert!(args.contains(&"CARGO_TARGET_DIR=/var/cache/mbx/targets/slots/slot-1".into()));
        assert!(args.contains(&"MBX_GC_MAX_TOTAL_SIZE=50GiB".into()));
        assert!(!args.iter().any(|arg| arg.contains("/var/cache/sccache")));
        assert!(!args.contains(&"MBX_DISABLE=1".into()));
        // Default path carries no sccache presence at all: no wrapper, no
        // sccache env, no provisioned binary on PATH.
        assert!(!args.iter().any(|arg| arg.contains("RUSTC_WRAPPER")));
        assert!(!args
            .iter()
            .any(|arg| arg.to_ascii_lowercase().contains("sccache")));
    }

    #[test]
    fn daemon_acceleration_environment_follows_trusted_container_options() {
        let mut job = slotted_spec("mbx-policy-order");
        job.options = vec!["-e".into(), "MBX_CACHE_DIR=/untrusted-override".into()];
        let prepared = job.start_args().unwrap();
        let args = rendered(&prepared);
        let override_index = args
            .iter()
            .position(|arg| arg == "MBX_CACHE_DIR=/untrusted-override")
            .unwrap();
        let policy_index = args
            .iter()
            .position(|arg| arg == "MBX_CACHE_DIR=/var/cache/mbx/slots/slot-1")
            .unwrap();
        assert!(policy_index > override_index);
    }

    /// Job containers run unbounded: no CPU/RAM/PID ceiling flag may reach
    /// `docker run`, however it arrived. Admission filters workflow options;
    /// emission strips them again as a backstop.
    #[test]
    fn job_containers_emit_no_cpu_ram_pid_ceiling() {
        let mut job = spec();
        job.options = vec![
            "--cpus".into(),
            "2".into(),
            "--memory=1g".into(),
            "-m".into(),
            "1g".into(),
            "-m=2g".into(),
            "--cpu-quota".into(),
            "50000".into(),
            "--cpuset-cpus".into(),
            "0-1".into(),
            "--memory-swap".into(),
            "2g".into(),
            "--pids-limit".into(),
            "512".into(),
            "--label".into(),
            "workflow".into(),
        ];
        let args = rendered(&job.start_args().unwrap());
        for quota in [
            "--cpus",
            "--cpu-period",
            "--cpu-quota",
            "--cpu-shares",
            "--cpuset-cpus",
            "--cpuset-mems",
            "--memory",
            "--memory-reservation",
            "--memory-swap",
            "--memory-swappiness",
            "--pids-limit",
        ] {
            assert!(
                !args
                    .iter()
                    .any(|arg| arg == quota || arg.starts_with(&format!("{quota}="))),
                "unbounded emission must strip {quota}, got {args:?}"
            );
        }
        // The stripped values vanish with their flags; neighbors survive.
        assert!(
            !args
                .iter()
                .any(|arg| matches!(arg.as_str(), "0-1" | "512" | "1g" | "2g")),
            "quota values must be stripped with their flags: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--label" && pair[1] == "workflow"),
            "{args:?}"
        );
        // Placement and identity survive: cgroup parent, ulimit, sysctl.
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--cgroup-parent", "velnor-jobs.slice"]),
            "{args:?}"
        );
    }

    /// The emission strip list covers every Docker CPU/RAM/PID ceiling
    /// flag, however spelled: shrinking this list would silently admit a
    /// ceiling, so the list itself is pinned, not just the stripping
    /// behavior. (Accepted risk, audit F4: no ceilings by spec §4.3.)
    #[test]
    fn quota_strip_list_covers_every_ceiling_flag() {
        const CEILINGS: [&str; 12] = [
            "--cpus",
            "--cpu-period",
            "--cpu-quota",
            "--cpu-shares",
            "--cpuset-cpus",
            "--cpuset-mems",
            "-m",
            "--memory",
            "--memory-reservation",
            "--memory-swap",
            "--memory-swappiness",
            "--pids-limit",
        ];
        assert_eq!(QUOTA_FLAGS, CEILINGS);
        for flag in CEILINGS {
            assert!(is_quota_flag(flag), "{flag} must be stripped");
            assert!(
                is_quota_flag(&format!("{flag}=1")),
                "{flag}=1 must be stripped"
            );
        }
        // `--shm-size` is deliberately not a ceiling (see `QUOTA_FLAGS`).
        assert!(!is_quota_flag("--shm-size"));
        assert!(!is_quota_flag("--shm-size=256m"));
    }

    /// No disguised resource partition is injected into the build
    /// environment: no `CARGO_BUILD_JOBS`, `MAKEFLAGS`, `MBX_SCHEDULER_*`,
    /// or budget notice. A workflow that sets such a variable keeps its own
    /// spelling — the daemon no longer overrides it with a share.
    #[test]
    fn job_environment_carries_no_build_partition() {
        let mut job = spec();
        job.env = vec![
            ("CARGO_BUILD_JOBS".into(), "64".into()),
            ("MAKEFLAGS".into(), "-j64".into()),
        ];
        let args = rendered(&job.start_args().unwrap());
        assert_eq!(
            args.iter()
                .filter(|arg| arg.starts_with("CARGO_BUILD_JOBS="))
                .count(),
            1,
            "only the workflow's own spelling survives: {args:?}"
        );
        assert!(args.contains(&"CARGO_BUILD_JOBS=64".to_owned()), "{args:?}");
        assert!(args.contains(&"MAKEFLAGS=-j64".to_owned()), "{args:?}");
        assert!(
            !args.iter().any(|arg| arg.starts_with("MBX_SCHEDULER_")),
            "{args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg.starts_with("VELNOR_JOB_BUDGET=")),
            "{args:?}"
        );
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
            cargo_executable_store_host(temp, "trusted", "ChainArgos/java-monorepo"),
            PathBuf::from(
                "/var/lib/velnor/work/_velnor_cargo/bin/trusted/ChainArgos_java-monorepo"
            )
        );
        assert_eq!(
            mise_executable_store_host(temp, "trusted", "ChainArgos/java-monorepo"),
            PathBuf::from(
                "/var/lib/velnor/work/_velnor_mise/installs/trusted/ChainArgos_java-monorepo"
            )
        );
        // Plan 008: the persistent mise binary store is a distinct `binaries`
        // subdir under the same trust/repository boundary as `installs`.
        assert_eq!(
            mise_binary_store_host(temp, "trusted", "ChainArgos/java-monorepo"),
            PathBuf::from(
                "/var/lib/velnor/work/_velnor_mise/binaries/trusted/ChainArgos_java-monorepo"
            )
        );
        assert_ne!(
            mise_binary_store_host(temp, "trusted", "ChainArgos/java-monorepo"),
            mise_executable_store_host(temp, "trusted", "ChainArgos/java-monorepo"),
        );
    }

    #[test]
    fn executable_tool_store_hosts_differ_by_repo() {
        let temp = Path::new("/var/lib/velnor/work/slot-3/job-9/temp");

        assert_ne!(
            cargo_executable_store_host(temp, "trusted", "org/one"),
            cargo_executable_store_host(temp, "trusted", "org/two")
        );
        assert_ne!(
            mise_executable_store_host(temp, "trusted", "org/one"),
            mise_executable_store_host(temp, "trusted", "org/two")
        );
    }

    #[test]
    fn pure_data_tool_stores_stay_shared_across_repos() {
        let temp = Path::new("/var/lib/velnor/work/slot-3/job-9/temp");

        assert_eq!(
            cargo_store_host(temp, "trusted").join("registry/cache"),
            PathBuf::from("/var/lib/velnor/work/_velnor_cargo/registry/cache")
        );
        assert_eq!(
            cargo_store_host(temp, "trusted").join("git/db"),
            PathBuf::from("/var/lib/velnor/work/_velnor_cargo/git/db")
        );
        assert_eq!(
            mise_store_host(temp, "trusted").join("cache"),
            PathBuf::from("/var/lib/velnor/work/_velnor_mise/cache")
        );
    }

    #[test]
    fn builds_start_container_args_with_mounts() {
        let job = slotted_spec("start-container-mounts");
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
            &job.bun_install_cache_store_host(),
            "/github/home/.bun/install/cache"
        ));
        assert!(has_mount(
            &args,
            &job.npm_cache_store_host(),
            "/github/home/.npm"
        ));
        assert!(has_mount(
            &args,
            &job.terraform_plugin_cache_store_host(),
            "/github/home/.terraform.d/plugin-cache"
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
            &cargo_store_host(&job.temp_host, job.store_trust_scope.as_str())
                .join("registry/cache"),
            "/github/home/.cargo/registry/cache"
        ));
        assert!(has_mount(
            &args,
            &cargo_store_host(&job.temp_host, job.store_trust_scope.as_str())
                .join("registry/index"),
            "/github/home/.cargo/registry/index"
        ));
        assert!(has_mount(
            &args,
            &cargo_store_host(&job.temp_host, job.store_trust_scope.as_str()).join("git/db"),
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
            &mise_store_host(&job.temp_host, job.store_trust_scope.as_str()).join("cache"),
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
        assert!(args.contains(&"MBX_CACHE_DIR=/var/cache/mbx/slots/slot-1".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"NODE_OPTIONS=--max-old-space-size=4096".into()));
        // Unbounded: no CPU/RAM/PID ceiling flag is emitted. Placement and
        // identity (cgroup parent, ulimit, sysctl) are not ceilings.
        for quota in [
            "--cpus",
            "--cpu-period",
            "--cpu-quota",
            "--cpu-shares",
            "--cpuset-cpus",
            "--cpuset-mems",
            "--memory",
            "--memory-reservation",
            "--memory-swap",
            "--memory-swappiness",
            "--pids-limit",
        ] {
            assert!(
                !args
                    .iter()
                    .any(|arg| arg == quota || arg.starts_with(&format!("{quota}="))),
                "unbounded emission must not contain {quota}: {args:?}"
            );
        }
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--cgroup-parent", "velnor-jobs.slice"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--ulimit", "nofile=65536:65536"]));
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--sysctl", "net.ipv6.conf.all.disable_ipv6=1"] }));
        if JobContainerSpec::guest_can_connect_host_bound_unix_lease() {
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
        } else {
            assert!(
                args.iter()
                    .any(|arg| arg.ends_with(".sock:/var/run/docker.sock")
                        && !arg.contains("vdl-")),
                "macOS guest Docker must mount the resolved host socket, got {args:?}"
            );
        }
        // PID 1 supervises the console tail and exits only on the private
        // read-only completion sentinel.
        assert_eq!(args.last().map(String::as_str), Some(JOB_CONTAINER_PID1));
        assert!(
            JOB_CONTAINER_PID1.contains("/__velnor/job.done"),
            "PID 1 must terminate when the runner-owned done sentinel appears"
        );
        assert!(!JOB_CONTAINER_PID1.contains("/__t/_velnor/job.done"));
        assert!(has_read_only_mount(
            &args,
            &job.job_done_host_dir(),
            "/__velnor"
        ));
        assert!(!job.job_done_host_path().starts_with(&job.temp_host));
        assert!(
            !JOB_CONTAINER_PID1.contains("exec tail"),
            "exec tail -F as PID 1 keeps finished containers alive"
        );
        assert!(
            JOB_CONTAINER_PID1.contains("restarting logger"),
            "a dead console tail must respawn until job.done, not exit PID 1"
        );
        assert!(
            !JOB_CONTAINER_PID1.contains("exit 2"),
            "tail death must not kill in-flight docker exec"
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
        first.slot_store_key = Some(slot_store_key(3));
        let mut same_slot = spec();
        same_slot.temp_host = "/var/lib/velnor/work/slot-3/job-b/temp".into();
        same_slot.slot_store_key = Some(slot_store_key(3));
        let mut other_slot = spec();
        other_slot.temp_host = "/var/lib/velnor/work/slot-4/job-c/temp".into();
        other_slot.slot_store_key = Some(slot_store_key(4));

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
        warm.slot_store_key = Some(slot_store_key(3));
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

    /// The per-slot stores follow the slot's carried identity, not the shape
    /// of its work dir. Regression: with the identity parsed from
    /// `…/slot-N/<job>/temp`, the daemon's default `<slot>/_work/<job>/temp`
    /// layout (`velnorctl host start`) and the single-slot fleet layout
    /// (`<work>/<job>/temp`) both lost every persistent mise install and
    /// every warm mbx cache, on every job, with only a forensics line to
    /// show for it.
    #[test]
    fn per_slot_stores_follow_the_carried_slot_identity_not_the_work_dir_shape() {
        let mut default_layout = spec();
        default_layout.temp_host =
            "/home/ci/.local/state/velnor/lib/velnor/runner/hosts/mac-1/slots/slot-2/_work/job-a/temp"
                .into();
        default_layout.slot_store_key = Some(slot_store_key(2));
        let mut single_slot = spec();
        single_slot.temp_host = "/var/lib/velnor/work/job-b/temp".into();
        single_slot.slot_store_key = Some(slot_store_key(1));

        assert!(default_layout
            .mise_executable_store_host()
            .ends_with("slots/slot-2"));
        assert_eq!(
            default_layout.mbx_cache_container_dir(),
            "/var/cache/mbx/slots/slot-2"
        );
        assert!(single_slot
            .mise_executable_store_host()
            .ends_with("slots/slot-1"));
        assert_eq!(
            single_slot.mbx_cache_container_dir(),
            "/var/cache/mbx/slots/slot-1"
        );

        // A job no slot owns gets job-ephemeral stores, whatever its path
        // looks like: persistence is never granted on a lookalike layout.
        let mut unowned = spec();
        unowned.temp_host = "/var/lib/velnor/work/slot-3/job-c/temp".into();
        unowned.slot_store_key = None;
        assert_eq!(
            unowned.mise_executable_store_host(),
            unowned.temp_host.join("_velnor/ephemeral/mise-installs")
        );
        assert_eq!(
            unowned.mbx_cache_container_dir(),
            "/var/cache/mbx/slots/velnor-job-1"
        );
    }

    #[test]
    fn mbx_cache_is_warm_per_slot_but_isolated_between_slots() {
        let mut first = spec();
        first.temp_host = "/var/lib/velnor/work/slot-3/job-a/temp".into();
        first.slot_store_key = Some(slot_store_key(3));
        let mut same_slot = spec();
        same_slot.temp_host = "/var/lib/velnor/work/slot-3/job-b/temp".into();
        same_slot.slot_store_key = Some(slot_store_key(3));
        let mut other_slot = spec();
        other_slot.temp_host = "/var/lib/velnor/work/slot-4/job-c/temp".into();
        other_slot.slot_store_key = Some(slot_store_key(4));

        // Same slot, successive jobs: one warm subdir. Different slots:
        // disjoint subdirs, so mbx's registrar/lease flocks never cross.
        assert_eq!(
            first.mbx_cache_container_dir(),
            "/var/cache/mbx/slots/slot-3"
        );
        assert_eq!(
            same_slot.mbx_cache_container_dir(),
            "/var/cache/mbx/slots/slot-3"
        );
        assert_eq!(
            other_slot.mbx_cache_container_dir(),
            "/var/cache/mbx/slots/slot-4"
        );

        // Mount/env consistency: the shared mount is unchanged, and each env
        // value names the mounted subdir the host pre-creates for that slot.
        // (Each fixture owns its temp root, so expectations derive per spec.)
        for (job, slot) in [
            (&first, "slot-3"),
            (&same_slot, "slot-3"),
            (&other_slot, "slot-4"),
        ] {
            let store = job.mbx_store_host.clone().unwrap();
            assert_eq!(
                job.mbx_cache_store_host().unwrap(),
                store.join("slots").join(slot)
            );
        }

        // Materializing the command writes an env file, so root the argv half
        // of the assertion in a real directory.
        let job = slotted_spec("mbx-slot-argv");
        let store = job.mbx_store_host.clone().unwrap();
        let args = rendered(&job.start_args().unwrap());
        assert!(args.contains(&format!("{}:/var/cache/mbx", store.display())));
        assert!(args.contains(&"MBX_CACHE_DIR=/var/cache/mbx/slots/slot-1".into()));
        assert!(args.contains(&"MBX_TARGET_ROOT=/var/cache/mbx/targets/slots/slot-1".into()));
        assert!(args.contains(&"CARGO_TARGET_DIR=/var/cache/mbx/targets/slots/slot-1".into()));
    }

    #[test]
    fn concurrent_slots_do_not_share_mbx_target_roots() {
        let mut first = spec();
        first.temp_host = "/var/lib/velnor/work/slot-3/job-a/temp".into();
        first.slot_store_key = Some(slot_store_key(3));
        let mut other = spec();
        other.temp_host = "/var/lib/velnor/work/slot-4/job-c/temp".into();
        other.slot_store_key = Some(slot_store_key(4));

        assert_eq!(
            first.mbx_target_container_dir(),
            "/var/cache/mbx/targets/slots/slot-3"
        );
        assert_eq!(
            other.mbx_target_container_dir(),
            "/var/cache/mbx/targets/slots/slot-4"
        );
        assert_ne!(
            first.mbx_target_container_dir(),
            other.mbx_target_container_dir()
        );
        assert_eq!(
            first.mbx_target_store_host().unwrap(),
            first
                .mbx_store_host
                .as_ref()
                .unwrap()
                .join("targets/slots/slot-3")
        );
        assert_eq!(
            other.mbx_target_store_host().unwrap(),
            other
                .mbx_store_host
                .as_ref()
                .unwrap()
                .join("targets/slots/slot-4")
        );
        let first_cargo = first
            .script_exec_env(&[])
            .into_iter()
            .find(|(name, _)| name == "CARGO_TARGET_DIR")
            .map(|(_, value)| value);
        let other_cargo = other
            .script_exec_env(&[])
            .into_iter()
            .find(|(name, _)| name == "CARGO_TARGET_DIR")
            .map(|(_, value)| value);
        assert_eq!(
            first_cargo.as_deref(),
            Some(first.mbx_target_container_dir().as_str())
        );
        assert_eq!(
            other_cargo.as_deref(),
            Some(other.mbx_target_container_dir().as_str())
        );
        assert_ne!(first_cargo, other_cargo);
        let mut first_run = spec();
        first_run.temp_host = container_test_temp("concurrent-slots-a")
            .join("slots")
            .join("slot-3")
            .join("job-a")
            .join("temp");
        first_run.slot_store_key = Some(slot_store_key(3));
        let mut other_run = spec();
        other_run.temp_host = container_test_temp("concurrent-slots-b")
            .join("slots")
            .join("slot-4")
            .join("job-c")
            .join("temp");
        other_run.slot_store_key = Some(slot_store_key(4));
        let first_args = rendered(&first_run.start_args().unwrap());
        let other_args = rendered(&other_run.start_args().unwrap());
        assert!(first_args.contains(&"CARGO_TARGET_DIR=/var/cache/mbx/targets/slots/slot-3".into()));
        assert!(other_args.contains(&"CARGO_TARGET_DIR=/var/cache/mbx/targets/slots/slot-4".into()));
    }

    #[test]
    fn sequential_same_slot_jobs_reuse_mbx_target_root() {
        let mut first = spec();
        first.temp_host = "/var/lib/velnor/work/slot-3/job-a/temp".into();
        first.slot_store_key = Some(slot_store_key(3));
        let mut second = spec();
        second.temp_host = "/var/lib/velnor/work/slot-3/job-b/temp".into();
        second.slot_store_key = Some(slot_store_key(3));

        assert_eq!(
            first.mbx_target_container_dir(),
            second.mbx_target_container_dir()
        );
        assert_eq!(
            first.mbx_target_store_host().unwrap().file_name(),
            second.mbx_target_store_host().unwrap().file_name()
        );
        assert_eq!(
            first.mbx_target_container_dir(),
            "/var/cache/mbx/targets/slots/slot-3"
        );
        assert_eq!(
            first
                .script_exec_env(&[])
                .into_iter()
                .find(|(name, _)| name == "CARGO_TARGET_DIR"),
            second
                .script_exec_env(&[])
                .into_iter()
                .find(|(name, _)| name == "CARGO_TARGET_DIR")
        );
        assert_eq!(
            first
                .script_exec_env(&[])
                .into_iter()
                .find(|(name, _)| name == "CARGO_TARGET_DIR")
                .map(|(_, value)| value)
                .as_deref(),
            Some(first.mbx_target_container_dir().as_str())
        );
    }

    #[test]
    fn missing_slot_identity_never_falls_back_to_shared_mbx_target_root() {
        let mut first = spec();
        first.temp_host = "/var/lib/velnor/work/job-a/temp".into();
        let mut second = spec();
        second.name = "velnor-job-2".into();
        second.temp_host = "/var/lib/velnor/work/job-b/temp".into();

        assert_eq!(
            first.mbx_target_container_dir(),
            "/var/cache/mbx/targets/slots/velnor-job-1"
        );
        assert_eq!(
            second.mbx_target_container_dir(),
            "/var/cache/mbx/targets/slots/velnor-job-2"
        );
        assert_ne!(first.mbx_target_container_dir(), "/var/cache/mbx/targets");
        let first_cargo = first
            .script_exec_env(&[])
            .into_iter()
            .find(|(name, _)| name == "CARGO_TARGET_DIR")
            .map(|(_, value)| value);
        assert_eq!(
            first_cargo.as_deref(),
            Some(first.mbx_target_container_dir().as_str())
        );
        assert_ne!(first_cargo.as_deref(), Some("/var/cache/mbx/targets"));
    }

    #[test]
    fn step_env_cannot_override_authoritative_mbx_roots() {
        let job = slotted_spec("mbx-target-spoof");
        let env = vec![
            ("MBX_TARGET_ROOT".into(), "/var/cache/mbx/targets".into()),
            ("MBX_CACHE_DIR".into(), "/var/cache/mbx".into()),
            ("CARGO_TARGET_DIR".into(), "/var/cache/mbx/targets".into()),
            ("MBX_DISABLE".into(), "1".into()),
        ];
        let recorded = job.script_exec_env(&env);
        assert!(recorded.iter().any(|(name, value)| {
            name == "MBX_TARGET_ROOT" && value == "/var/cache/mbx/targets/slots/slot-1"
        }));
        assert!(recorded.iter().any(|(name, value)| {
            name == "MBX_CACHE_DIR" && value == "/var/cache/mbx/slots/slot-1"
        }));
        assert!(recorded.iter().any(|(name, value)| {
            name == "CARGO_TARGET_DIR" && value == "/var/cache/mbx/targets/slots/slot-1"
        }));
        assert!(
            recorded
                .iter()
                .any(|(name, value)| name == "MBX_DISABLE" && value == "1"),
            "MBX_DISABLE is the one mbx name a job may set"
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|(name, _)| name == "MBX_TARGET_ROOT")
                .count(),
            1
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|(name, _)| name == "CARGO_TARGET_DIR")
                .count(),
            1
        );
    }

    #[test]
    fn mbx_cache_without_slot_identity_isolates_per_job() {
        let mut first = spec();
        first.temp_host = "/var/lib/velnor/work/job-a/temp".into();
        let mut second = spec();
        second.name = "velnor-job-2".into();
        second.temp_host = "/var/lib/velnor/work/job-b/temp".into();

        // No `slot-N` segment: isolate per job name, never share the root.
        assert_eq!(
            first.mbx_cache_container_dir(),
            "/var/cache/mbx/slots/velnor-job-1"
        );
        assert_eq!(
            second.mbx_cache_container_dir(),
            "/var/cache/mbx/slots/velnor-job-2"
        );
        let store = first.mbx_store_host.clone().unwrap();
        assert_eq!(
            first.mbx_cache_store_host().unwrap(),
            store.join("slots/velnor-job-1")
        );
    }

    #[test]
    fn mbx_cache_subdir_is_absent_when_the_store_is_disabled() {
        let mut job = slotted_spec("mbx-slot-disabled");
        job.mbx_store_host = None;
        job.sccache_store_host = Some(PathBuf::from("/var/cache/sccache"));
        assert_eq!(job.mbx_cache_store_host(), None);
        assert_eq!(job.mbx_target_store_host(), None);
        let args = rendered(&job.start_args().unwrap());
        assert!(!args.iter().any(|arg| arg.starts_with("MBX_CACHE_DIR=")));
        assert!(!args.iter().any(|arg| arg.starts_with("MBX_TARGET_ROOT=")));
        assert!(!args.iter().any(|arg| arg.starts_with("CARGO_TARGET_DIR=")));
        assert!(!job
            .script_exec_env(&[])
            .iter()
            .any(|(name, _)| name == "CARGO_TARGET_DIR"));
        assert!(args.contains(&"SCCACHE_DIR=/var/cache/sccache".into()));
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
        let lease_paths = spec.docker_lease_paths().unwrap();
        assert_ne!(lease_paths.host_visible, lease_paths.daemon_visible);
        if JobContainerSpec::guest_can_connect_host_bound_unix_lease() {
            let lease_mount = format!(
                "{}:/var/run/docker.sock",
                lease_paths.daemon_visible.display()
            );
            assert!(
                args.contains(&lease_mount),
                "the lease listener must use the Docker-daemon-visible path {lease_mount}, got {args:?}"
            );
        } else {
            assert!(
                args.iter()
                    .any(|arg| arg.ends_with(".sock:/var/run/docker.sock")
                        && !arg.contains("vdl-")),
                "macOS guest Docker must mount the resolved host socket, got {args:?}"
            );
        }
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
        assert_eq!(
            spec.docker_lease_paths().unwrap().daemon_visible.parent(),
            Some(Path::new("/daemon/work/slot-1/job-1/temp/_velnor"))
        );
    }

    #[test]
    fn rejects_docker_mapping_that_escapes_the_host_work_root() {
        let root = container_test_temp("reject-escaping-host-path");
        let mut spec = spec();
        spec.temp_host = root.join("work/job-1/temp");
        spec.workspace_host = root.join("outside/workspace");
        spec.home_host = root.join("work/job-1/home");
        spec.actions_host = root.join("work/job-1/actions");
        spec.tools_host = root.join("work/job-1/tools");
        spec.docker_host_work_dir = Some("/daemon/work".into());

        let error = spec.validate_docker_host_path_mapping().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("workspace"), "{message}");
        assert!(
            message.contains("escapes host-visible runner work root"),
            "{message}"
        );
        assert!(message.contains("docker_host_work_dir"), "{message}");
    }

    #[test]
    fn rejects_parent_components_in_docker_mapping_roots() {
        let mut spec = spec();
        spec.docker_host_work_dir = Some("/daemon/work/../escape".into());

        let error = spec.validate_docker_host_path_mapping().unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Docker daemon work root"), "{message}");
        assert!(message.contains("unsafe parent"), "{message}");
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
        let mbx_cache_env = format!("MBX_CACHE_DIR={}", spec.mbx_cache_container_dir());
        let mbx_target_env = format!("MBX_TARGET_ROOT={}", spec.mbx_target_container_dir());
        let cargo_target_env = format!("CARGO_TARGET_DIR={}", spec.mbx_target_container_dir());

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
                mbx_cache_env.as_str(),
                mbx_target_env.as_str(),
                cargo_target_env.as_str(),
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
        let mbx_cache_env = format!("MBX_CACHE_DIR={}", spec.mbx_cache_container_dir());
        let mbx_target_env = format!("MBX_TARGET_ROOT={}", spec.mbx_target_container_dir());
        let cargo_target_env = format!("CARGO_TARGET_DIR={}", spec.mbx_target_container_dir());

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
                mbx_cache_env.as_str(),
                mbx_target_env.as_str(),
                cargo_target_env.as_str(),
                "--",
                "velnor-job-1",
                "node",
                "/__a/action/dist/index.js"
            ]
        );
    }

    #[test]
    fn runner_backend_cannot_be_overridden_by_step_environment() {
        let mut spec = spec();
        spec.env
            .push(("VELNOR_EXECUTION_BACKEND".into(), "docker".into()));
        let prepared = spec
            .prepare_exec_process_args(
                "/__w/repo",
                &[("VELNOR_EXECUTION_BACKEND".into(), "spoofed".into())],
                &[],
                &["sh".into(), "-c".into(), "true".into()],
            )
            .unwrap();
        let rendered = rendered(&prepared);
        let backend = rendered
            .iter()
            .rfind(|argument| argument.starts_with("VELNOR_EXECUTION_BACKEND="));
        assert_eq!(backend, Some(&"VELNOR_EXECUTION_BACKEND=docker".to_owned()));
    }

    #[test]
    fn build_identity_env_cannot_be_overridden_by_step_environment() {
        let mut spec = spec();
        spec.env
            .push(("VELNOR_SOURCE_SHA".into(), env!("VELNOR_SOURCE_SHA").into()));
        spec.env.push((
            "VELNOR_MANIFEST_VERSION".into(),
            crate::manifest::MANIFEST_VERSION.to_string(),
        ));
        let prepared = spec
            .prepare_exec_process_args(
                "/__w/repo",
                &[
                    ("VELNOR_SOURCE_SHA".into(), "spoofed".into()),
                    ("VELNOR_MANIFEST_VERSION".into(), "spoofed".into()),
                ],
                &[],
                &["sh".into(), "-c".into(), "true".into()],
            )
            .unwrap();
        let rendered = rendered(&prepared);
        let sha = rendered
            .iter()
            .rfind(|argument| argument.starts_with("VELNOR_SOURCE_SHA="));
        assert_eq!(
            sha,
            Some(&format!("VELNOR_SOURCE_SHA={}", env!("VELNOR_SOURCE_SHA")))
        );
        let version = rendered
            .iter()
            .rfind(|argument| argument.starts_with("VELNOR_MANIFEST_VERSION="));
        assert_eq!(
            version,
            Some(&format!(
                "VELNOR_MANIFEST_VERSION={}",
                crate::manifest::MANIFEST_VERSION
            ))
        );
    }

    #[test]
    fn docker_action_sidecar_reasserts_authoritative_runner_env() {
        let mut spec = spec();
        spec.env
            .push(("VELNOR_EXECUTION_BACKEND".into(), "docker".into()));
        spec.env
            .push(("VELNOR_SOURCE_SHA".into(), env!("VELNOR_SOURCE_SHA").into()));
        spec.env.push((
            "VELNOR_MANIFEST_VERSION".into(),
            crate::manifest::MANIFEST_VERSION.to_string(),
        ));
        let prepared = spec
            .prepare_run_docker_action_args(
                "/__w",
                &[
                    ("VELNOR_EXECUTION_BACKEND".into(), "spoofed".into()),
                    ("VELNOR_SOURCE_SHA".into(), "spoofed".into()),
                    ("VELNOR_MANIFEST_VERSION".into(), "spoofed".into()),
                ],
                &[],
                "alpine:3.22",
                None,
                &["true".into()],
            )
            .unwrap();
        let effective = rendered(&prepared);
        assert_eq!(
            effective
                .iter()
                .rfind(|arg| arg.starts_with("VELNOR_EXECUTION_BACKEND=")),
            Some(&"VELNOR_EXECUTION_BACKEND=docker".to_owned())
        );
        assert_eq!(
            effective
                .iter()
                .rfind(|arg| arg.starts_with("VELNOR_SOURCE_SHA=")),
            Some(&format!("VELNOR_SOURCE_SHA={}", env!("VELNOR_SOURCE_SHA")))
        );
        assert_eq!(
            effective
                .iter()
                .rfind(|arg| arg.starts_with("VELNOR_MANIFEST_VERSION=")),
            Some(&format!(
                "VELNOR_MANIFEST_VERSION={}",
                crate::manifest::MANIFEST_VERSION
            ))
        );
        assert!(!effective.iter().any(|arg| arg.contains("spoofed")));
    }

    #[test]
    fn multiline_authoritative_spoof_cannot_escape_via_process_env() {
        let mut spec = spec();
        spec.env
            .push(("VELNOR_EXECUTION_BACKEND".into(), "docker".into()));
        let spoof = &[(
            "VELNOR_EXECUTION_BACKEND".into(),
            "spoofed\nINJECTED=x".into(),
        )];
        let prepared = [
            spec.prepare_exec_process_args("/__w", spoof, &[], &["printenv".into()])
                .unwrap(),
            spec.prepare_run_docker_action_args(
                "/__w",
                spoof,
                &[],
                "alpine:3.22",
                None,
                &["true".into()],
            )
            .unwrap(),
        ];
        for command in &prepared {
            // A multiline value routes to `-e NAME` plus process env, which
            // Docker resolves over `--env-file`: the spoof must be dropped
            // before the builder, not merely out-ordered by the clean value.
            assert!(command
                .process_env()
                .iter()
                .all(|(name, _)| name != "VELNOR_EXECUTION_BACKEND"));
            assert!(!command
                .args()
                .windows(2)
                .any(|pair| pair[0] == "-e" && pair[1] == "VELNOR_EXECUTION_BACKEND"));
            let effective = rendered(command);
            assert_eq!(
                effective
                    .iter()
                    .rfind(|arg| arg.starts_with("VELNOR_EXECUTION_BACKEND=")),
                Some(&"VELNOR_EXECUTION_BACKEND=docker".to_owned())
            );
            assert!(!effective.iter().any(|arg| arg.contains("spoofed")));
        }
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

    /// Spec owned by daemon slot 1, so the persistent per-slot stores resolve
    /// their slot scope like a real job. The temp root deliberately uses the
    /// default `_work` layout, which carries no slot name of its own.
    fn slotted_spec(name: &str) -> JobContainerSpec {
        let mut spec = spec();
        spec.temp_host = container_test_temp(name)
            .join("slots")
            .join("slot-1")
            .join("_work")
            .join("job-1")
            .join("temp");
        spec.slot_store_key = Some(slot_store_key(1));
        spec
    }

    /// PATH entries name job-container directories, so the sidecar gets a bind
    /// mount for each at the very same absolute path: a post step resolving
    /// `cargo` through a PATH dir must not depend on that dir existing only in
    /// the job container.
    #[test]
    fn node_action_path_entries_are_mounted_at_the_same_container_path() {
        let spec = slotted_spec("node-path-mounts");
        let mise_installs = spec.mise_executable_store_host();
        let mise_binaries = spec.mise_binary_store_host();
        let cargo_bin = spec.cargo_executable_store_host();
        let prepared = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &[
                    "/opt/mise/installs/node/22.18.0/bin".into(),
                    "/opt/velnor/mise-binaries/linux-x64/25.6.0".into(),
                    "/github/home/.cargo/bin".into(),
                    "/root/.cargo/bin".into(),
                ],
                "node:20-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let args = rendered(&prepared);

        assert!(has_mount(
            &args,
            &mise_installs.join("node/22.18.0/bin"),
            "/opt/mise/installs/node/22.18.0/bin"
        ));
        assert!(has_mount(
            &args,
            &mise_binaries.join("linux-x64/25.6.0"),
            "/opt/velnor/mise-binaries/linux-x64/25.6.0"
        ));
        // Nested store mount: /github/home alone does not carry the cargo bin
        // store, and it must not be swallowed by the broader /github/home view.
        assert!(has_mount(&args, &cargo_bin, "/github/home/.cargo/bin"));
        assert!(has_mount(
            &args,
            Path::new("/root/.cargo/bin"),
            "/root/.cargo/bin"
        ));
        assert!(args.contains(&"PATH=/opt/mise/installs/node/22.18.0/bin:/opt/velnor/mise-binaries/linux-x64/25.6.0:/github/home/.cargo/bin:/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()));
    }

    #[test]
    fn node_action_path_mounts_are_deduplicated() {
        let spec = slotted_spec("node-path-dedupe");
        let prepared = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &[
                    "/root/.cargo/bin".into(),
                    "/root/.cargo/bin".into(),
                    "/opt/mise/installs/node/22.18.0/bin".into(),
                    "/opt/mise/installs/node/22.18.0/bin".into(),
                ],
                "node:20-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let args = rendered(&prepared);

        assert_eq!(
            args.iter()
                .filter(|arg| **arg == mount(Path::new("/root/.cargo/bin"), "/root/.cargo/bin"))
                .count(),
            1
        );
        assert_eq!(
            args.iter()
                .filter(|arg| **arg
                    == mount(
                        &spec.mise_executable_store_host().join("node/22.18.0/bin"),
                        "/opt/mise/installs/node/22.18.0/bin"
                    ))
                .count(),
            1
        );
    }

    /// A PATH entry landing on /__w, /__t or /github is already visible, and
    /// those runner-owned views are load-bearing: the entry is dropped rather
    /// than rebound to a host path of its own choosing.
    #[test]
    fn node_action_path_entries_never_shadow_runner_owned_mounts() {
        let spec = slotted_spec("node-path-required");
        let prepared = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &[
                    "/__w".into(),
                    "/__w/repo/target/debug".into(),
                    "/__w/repo-sibling/bin".into(),
                    "/__t".into(),
                    "/tmp/velnor".into(),
                    "/github/workspace/bin".into(),
                    "/github/workflow".into(),
                    "/github/home".into(),
                    "/__a".into(),
                    "/__tool".into(),
                    "/usr/local/tools/bin".into(),
                ],
                "node:20-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let args = rendered(&prepared);

        // The required views survive, exactly once, from their own hosts.
        assert_eq!(
            args.iter()
                .filter(|arg| **arg == mount(&spec.workspace_host, "/__w"))
                .count(),
            1
        );
        assert_eq!(
            args.iter()
                .filter(|arg| **arg == mount(&spec.temp_host, "/__t"))
                .count(),
            1
        );
        assert_eq!(
            args.iter()
                .filter(|arg| **arg == mount(&spec.home_host, "/github/home"))
                .count(),
            1
        );
        assert_eq!(
            args.iter()
                .filter(|arg| **arg == mount(&spec.workspace_host, "/github/workspace"))
                .count(),
            1
        );
        assert_eq!(
            args.iter()
                .filter(|arg| **arg == mount(&workflow_host(&spec.temp_host), "/github/workflow"))
                .count(),
            1
        );
        assert!(has_read_only_mount(&args, &spec.actions_host, "/__a"));
        assert!(has_mount(&args, &spec.tools_host, "/__tool"));
        // Every mount touching a runner-owned container path is the required
        // one, present exactly once: no PATH entry rebinds it.
        let mounts = mount_args(&prepared);
        let required = [
            mount(&spec.workspace_host, "/__w"),
            mount(&spec.workspace_host, "/github/workspace"),
            mount(&spec.temp_host, "/__t"),
            mount(&spec.temp_host, "/tmp"),
            mount(&spec.temp_host, "/github/runner_temp"),
            mount(&spec.temp_host, "/github/file_commands"),
            mount(&spec.home_host, "/github/home"),
            mount(&workflow_host(&spec.temp_host), "/github/workflow"),
            format!("{}:ro", mount(&spec.actions_host, "/__a")),
            mount(&spec.tools_host, "/__tool"),
        ];
        for required_mount in &required {
            assert_eq!(
                mounts.iter().filter(|m| *m == required_mount).count(),
                1,
                "{required_mount}"
            );
        }
        for entry in &mounts {
            if ["/__w", "/__t", "/tmp", "/github", "/__a", "/__tool"]
                .iter()
                .any(|owned| container_path_under(mount_container(entry), owned))
            {
                assert!(
                    required.contains(entry),
                    "runner-owned path rebound: {entry}"
                );
            }
        }
        // A PATH entry outside the runner-owned views still gets its mount.
        assert!(has_mount(
            &args,
            Path::new("/usr/local/tools/bin"),
            "/usr/local/tools/bin"
        ));
    }

    #[test]
    fn node_action_run_without_path_entries_adds_no_path_env_or_mounts() {
        let spec = slotted_spec("node-path-empty");
        let prepared = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &[],
                "node:20-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let args = rendered(&prepared);

        assert!(!args.iter().any(|arg| arg.starts_with("PATH=")));
        let mut containers: Vec<String> = mount_args(&prepared)
            .into_iter()
            .map(|entry| mount_container(&entry).to_owned())
            .collect();
        containers.sort_unstable();
        assert_eq!(
            containers,
            [
                "/__a",
                "/__t",
                "/__tool",
                "/__w",
                "/github/file_commands",
                "/github/home",
                "/github/runner_temp",
                "/github/workflow",
                "/github/workspace",
                "/tmp",
                "/var/cache/mbx",
                "/var/run/docker.sock",
            ]
        );
    }

    /// Main and post node actions are prepared by the same call with the same
    /// recorded PATH, so both must carry the identical mount set.
    #[test]
    fn node_action_post_steps_receive_the_same_path_mounts() {
        let spec = slotted_spec("node-path-post");
        let path_prepend = [
            "/opt/mise/installs/node/22.18.0/bin".to_owned(),
            "/root/.cargo/bin".to_owned(),
        ];
        let main = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &path_prepend,
                "node:20-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let post = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &path_prepend,
                "node:20-bookworm",
                "/__a/action/dist/save.js",
            )
            .unwrap();
        let mounts = |prepared: &PreparedDockerArgs| {
            mount_args(prepared)
                .into_iter()
                .filter(|entry| {
                    ["/opt/mise/installs", "/root/.cargo"]
                        .iter()
                        .any(|prefix| mount_container(entry).starts_with(prefix))
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(mounts(&main), mounts(&post));
        assert_eq!(mounts(&post).len(), 2);
    }

    #[test]
    fn relative_and_base_path_entries_mount_nothing() {
        let spec = slotted_spec("node-path-junk");
        let prepared = spec
            .prepare_run_node_action_args(
                "/__w",
                &[],
                &[],
                &[
                    String::new(),
                    "bin/tools".into(),
                    "/opt/mise/shims/../..".into(),
                    "/usr/local/bin:/opt/mise/bin".into(),
                    "/opt/mise/bin".into(),
                ],
                "node:20-bookworm",
                "/__a/action/dist/index.js",
            )
            .unwrap();
        let args = rendered(&prepared);

        // Only the plain absolute entry mounts; unusable entries are skipped
        // while the recorded PATH itself stays verbatim.
        assert_eq!(
            args.iter()
                .filter(|arg| **arg == mount(Path::new("/opt/mise/bin"), "/opt/mise/bin"))
                .count(),
            1
        );
        assert!(args.contains(&"PATH=:bin/tools:/opt/mise/shims/../..:/usr/local/bin:/opt/mise/bin:/opt/mise/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()));
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
    fn rejects_docker_action_paths_outside_mapped_work_root() {
        let root = container_test_temp("reject-docker-action-path");
        let mut spec = spec();
        spec.docker_host_work_dir = Some("/daemon/work".into());
        let dockerfile = root.join("outside/Dockerfile");
        let context = root.join("outside");

        let error = spec
            .build_docker_action_args("alpine:3.20", &dockerfile, &context)
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Dockerfile"), "{message}");
        assert!(
            message.contains("escapes host-visible runner work root"),
            "{message}"
        );
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
            options: vec![
                "--cpus".into(),
                "2".into(),
                "--memory=1g".into(),
                "--health-cmd".into(),
                "pg_isready".into(),
            ],
        };

        // Quota flags are stripped at emission, like the job container.
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

    #[test]
    fn script_exec_env_matches_prepared_argv_env_exactly() {
        let mut job = spec();
        job.env
            .push(("VELNOR_EXECUTION_BACKEND".into(), "velnor".into()));
        let step_env = vec![
            ("STEP_VAR".into(), "step".into()),
            ("PATH".into(), "/step/path".into()),
            ("DOCKER_HOST".into(), "tcp://evil:2376".into()),
            ("MULTILINE".into(), "a\nb".into()),
            ("VELNOR_EXECUTION_BACKEND".into(), "spoof".into()),
        ];
        let engine = job.script_exec_env(&step_env);
        let prepared = job
            .prepare_exec_script_args("/__t/step.sh", Shell::Sh, "/__w", &step_env, &[])
            .unwrap();
        // The CLI leg's effective env in argv order: env files expanded in
        // place, bare `-e NAME` resolved from the client process env. Stops
        // at `--`: command operands are opaque (`sh -e` is not a flag).
        let mut cli = Vec::new();
        let mut args = prepared.args().iter();
        while let Some(arg) = args.next() {
            if arg == "--" {
                break;
            }
            if arg == "--env-file" {
                let path = args.next().expect("env file path follows --env-file");
                cli.extend(
                    fs::read_to_string(path)
                        .unwrap()
                        .lines()
                        .map(str::to_string),
                );
            } else if arg == "-e" {
                let name = args.next().expect("variable name follows -e");
                let (_, value) = prepared
                    .process_env()
                    .iter()
                    .find(|(key, _)| key == name)
                    .expect("forwarded value exists");
                cli.push(format!("{name}={value}"));
            }
        }
        let engine: Vec<String> = engine
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        assert_eq!(engine, cli, "engine list must equal CLI effective env");
        // The semantics the shared appends own: base first, the control
        // spoof dropped, step PATH after base PATH (last-wins), the
        // authoritative value re-asserted last, multiline intact.
        assert_eq!(engine[0], "HOME=/github/home");
        assert!(
            !engine.iter().any(|entry| entry.contains("tcp://evil")),
            "{engine:?}"
        );
        assert_eq!(
            engine
                .iter()
                .filter(|entry| entry.starts_with("DOCKER_HOST="))
                .count(),
            1
        );
        let base_path = engine
            .iter()
            .position(|entry| entry.starts_with("PATH="))
            .unwrap();
        let step_path = engine
            .iter()
            .position(|entry| entry == "PATH=/step/path")
            .unwrap();
        assert!(base_path < step_path);
        assert!(!engine.iter().any(|entry| entry.ends_with("=spoof")));
        assert!(engine
            .iter()
            .any(|entry| entry == "VELNOR_EXECUTION_BACKEND=velnor"));
        assert_eq!(
            engine.last().unwrap(),
            &format!("CARGO_TARGET_DIR={}", job.mbx_target_container_dir())
        );
        assert!(engine.contains(&"MULTILINE=a\nb".to_string()));
        // The CLI leg can only carry that value via the client process env;
        // the engine leg carries it directly — same daemon value.
        assert!(prepared
            .process_env()
            .contains(&("MULTILINE".to_string(), "a\nb".to_string())));
    }
}

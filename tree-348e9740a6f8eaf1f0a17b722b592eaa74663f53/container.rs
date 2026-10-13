#![allow(dead_code)]

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::compiler_cache::CompilerCacheBackend;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

static EXEC_ENV_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

const NODE_ACTION_BASE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const JOB_NOFILE_LIMIT: &str = "65536:65536";

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
    pub env: Vec<(String, String)>,
    /// Daemon-enforced Docker resource limits. Appended after workflow
    /// createOptions so operator policy wins for shared warm-runner hosts.
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
    /// Exactly one compiler-cache store is exposed to a job.
    pub compiler_cache_backend: CompilerCacheBackend,
}

impl JobContainerSpec {
    fn append_compiler_cache_mount(&self, args: &mut Vec<String>) {
        let (host, container, env) = match self.compiler_cache_backend {
            CompilerCacheBackend::Sccache => (
                sccache_host(&self.temp_host),
                "/var/cache/sccache",
                vec!["SCCACHE_DIR=/var/cache/sccache"],
            ),
            CompilerCacheBackend::Kache => (
                kache_host(&self.temp_host),
                "/var/cache/kache",
                vec!["KACHE_CACHE_DIR=/var/cache/kache", "KACHE_MAX_SIZE=20GiB"],
            ),
            CompilerCacheBackend::Off => return,
        };
        args.extend(["-v".into(), self.mount_arg(&host, container)]);
        for value in env {
            args.extend(["-e".into(), value.into()]);
        }
    }

    pub fn create_network_args(&self) -> Vec<String> {
        vec!["network".into(), "create".into(), self.network.clone()]
    }

    pub fn start_args(&self) -> Vec<String> {
        let mut args = vec![
            "run".into(),
            "--detach".into(),
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
            // Host-persistent mise tool store: installed tools are executable,
            // so `installs` is scoped by trust/repository. The mise binary,
            // shims and global config stay baked in the image. `cache` is
            // download data and remains daemon-shared for warmth; mise uses
            // its own file locks.
            "-v".into(),
            self.mount_arg(&self.mise_executable_store_host(), "/opt/mise/installs"),
            // mise's Rust backend stores compiler payloads and selection state
            // in rustup, not in /opt/mise. Keep it in the same trust/repository
            // scope or an ephemeral container loses the selected toolchain.
            "-v".into(),
            self.mount_arg(&self.rustup_executable_store_host(), "/root/.rustup"),
            "-v".into(),
            self.mount_arg(
                &mise_store_host(&self.temp_host).join("cache"),
                "/opt/mise/cache",
            ),
            "-v".into(),
            self.mount_arg(&workflow_host(&self.temp_host), "/github/workflow"),
            "-v".into(),
            self.mount_arg(&self.actions_host, "/__a"),
            "-v".into(),
            self.mount_arg(&self.tools_host, "/__tool"),
            "-e".into(),
            "HOME=/github/home".into(),
            "-e".into(),
            "RUSTUP_HOME=/root/.rustup".into(),
            "-e".into(),
            "CARGO_HOME=/github/home/.cargo".into(),
            "-e".into(),
            "RUNNER_TEMP=/__t".into(),
            "-e".into(),
            "RUNNER_TOOL_CACHE=/__tool".into(),
            "-e".into(),
            "AGENT_TOOLSDIRECTORY=/__tool".into(),
            "-e".into(),
            format!(
                "VELNOR_DOCKER_HOST_TEMP={}",
                self.docker_host_path(&self.temp_host).display()
            ),
            "-e".into(),
            format!(
                "VELNOR_DOCKER_HOST_WORKSPACE={}",
                self.docker_host_path(&self.workspace_host).display()
            ),
        ];
        self.append_compiler_cache_mount(&mut args);
        for (name, value) in &self.env {
            args.extend(["-e".into(), format!("{name}={value}")]);
        }
        args.extend([
            "--label".into(),
            format!("velnor.daemon-id={}", self.daemon_id),
        ]);
        args.extend(self.options.iter().cloned());
        args.extend(self.resource_options.iter().cloned());

        // Docker Engine 29 inherits systemd's 1024-file descriptor default
        // when no container limit is explicit. Large Rust/Zig links open one
        // descriptor per object and fail with ProcessFdQuotaExceeded. Make the
        // job contract deterministic and large enough for GitHub-scale builds.
        args.extend(["--ulimit".into(), format!("nofile={JOB_NOFILE_LIMIT}")]);

        // GitHub-hosted Ubuntu jobs expose localhost over IPv4. Docker also
        // assigns localhost to ::1, which can split same-process servers and
        // clients across address families (for example Vite binds ::1 while
        // Bun fetches 127.0.0.1). Keep loopback behavior lane-identical.
        args.extend(["--sysctl".into(), "net.ipv6.conf.all.disable_ipv6=1".into()]);

        if self.mount_docker_socket {
            args.extend([
                "-v".into(),
                "/var/run/docker.sock:/var/run/docker.sock".into(),
            ]);
        }
        self.append_docker_cli_mounts(&mut args);

        // The per-job network is runner policy. Keep it after expanded job
        // and daemon resource options so the job cannot be displaced from the
        // network shared with its workflow services.
        args.extend(["--network".into(), self.network.clone()]);

        // PID 1 tails a live console file instead of /dev/null, so
        // `docker logs <job-container>` mirrors the GitHub UI step output.
        // Velnor appends each masked step's lines to this file (mounted at
        // /__t). `tail -F` waits for the file if it does not exist yet.
        args.extend([
            self.image.clone(),
            "sh".into(),
            "-c".into(),
            "mkdir -p /__t/_velnor && touch /__t/_velnor/console.log && exec tail -n +1 -F /__t/_velnor/console.log".into(),
        ]);
        args
    }

    /// `docker run` args that copy the job image's baked /opt/mise installs +
    /// cache into the shared host store without clobbering newer entries.
    /// Mounting an (initially empty) shared store over /opt/mise/installs
    /// shadows the image-baked tools while the baked shims keep pointing at
    /// them — observed live as `mise ERROR gh is not a valid shim` on a fresh
    /// store. Seeding once per image digest removes that class.
    pub fn seed_mise_store_args(&self) -> Vec<String> {
        let store = mise_store_host(&self.temp_host);
        vec![
            "run".into(),
            "--rm".into(),
            "--entrypoint".into(),
            "sh".into(),
            "-v".into(),
            self.mount_arg(
                &self.mise_executable_store_host(),
                "/__velnor_seed/installs",
            ),
            "-v".into(),
            self.mount_arg(&store.join("cache"), "/__velnor_seed/cache"),
            "-v".into(),
            self.mount_arg(
                &self.rustup_executable_store_host(),
                "/__velnor_seed/rustup",
            ),
            self.image.clone(),
            "-c".into(),
            "cp -an /opt/mise/installs/. /__velnor_seed/installs/ 2>/dev/null || true; \
             cp -an /opt/mise/cache/. /__velnor_seed/cache/ 2>/dev/null || true; \
             cp -an /root/.rustup/. /__velnor_seed/rustup/ 2>/dev/null || true"
                .into(),
        ]
    }

    fn exec_script_args(
        &self,
        script_path_in_container: &str,
        shell: Shell,
        working_directory: &str,
        env: &[(String, String)],
    ) -> Vec<String> {
        self.exec_process_args(
            working_directory,
            env,
            &shell.command_args(script_path_in_container),
        )
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

    fn exec_process_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        command: &[String],
    ) -> Vec<String> {
        let mut args = vec!["exec".into(), "--workdir".into(), working_directory.into()];
        self.append_base_exec_env(&mut args);
        for (name, value) in env {
            args.extend(["-e".into(), format!("{name}={value}")]);
        }
        args.push(self.name.clone());
        args.extend(command.iter().cloned());
        args
    }

    pub fn prepare_exec_process_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        command: &[String],
    ) -> io::Result<PreparedDockerArgs> {
        let mut prepared = PreparedDockerArgs::new(vec![
            "exec".into(),
            "--workdir".into(),
            working_directory.into(),
        ]);
        self.append_base_exec_env(&mut prepared.args);
        self.append_step_env(&mut prepared, env, secret_masks)?;
        prepared.args.push(self.name.clone());
        prepared.args.extend(command.iter().cloned());
        Ok(prepared)
    }

    /// Like prepare_exec_process_args, but with stdin kept open (`docker exec
    /// -i`) so the caller can stream data (for example a registry password).
    pub fn prepare_exec_process_stdin_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        command: &[String],
    ) -> io::Result<PreparedDockerArgs> {
        let mut prepared = PreparedDockerArgs::new(vec![
            "exec".into(),
            "-i".into(),
            "--workdir".into(),
            working_directory.into(),
        ]);
        self.append_base_exec_env(&mut prepared.args);
        self.append_step_env(&mut prepared, env, secret_masks)?;
        prepared.args.push(self.name.clone());
        prepared.args.extend(command.iter().cloned());
        Ok(prepared)
    }

    fn append_step_env(
        &self,
        prepared: &mut PreparedDockerArgs,
        env: &[(String, String)],
        secret_masks: &[String],
    ) -> io::Result<()> {
        for (name, value) in env {
            let is_secret = env_name_is_secret(name) || env_value_is_secret(value, secret_masks);
            if is_secret && value.contains('\n') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("secret environment variable {name} contains a newline"),
                ));
            }
            if is_secret {
                let env_file = write_exec_env_file(&self.temp_host, name, value)?;
                prepared
                    .args
                    .extend(["--env-file".into(), env_file.path().display().to_string()]);
                prepared.env_files.push(env_file);
            } else {
                prepared
                    .args
                    .extend(["-e".into(), format!("{name}={value}")]);
            }
        }
        Ok(())
    }

    /// Truthful base env for every exec'd process: the job home is the
    /// bind-mounted /github/home (so `~` caches and docker client state
    /// persist on the host), the rustup toolchain store stays at the
    /// image-baked /root/.rustup, and cargo's registry/git live under the
    /// job home (backed by the host-persistent cargo store mounts).
    /// Re-asserted per exec because OrbStack (macOS dev hosts) injects the
    /// host user's HOME into exec'd processes; explicit -e wins. Step env
    /// (GITHUB_ENV and `env:` blocks) is appended after these, and docker
    /// applies the last duplicate -e, so steps can still override.
    fn append_base_exec_env(&self, args: &mut Vec<String>) {
        for kv in [
            "HOME=/github/home".to_string(),
            "RUSTUP_HOME=/root/.rustup".to_string(),
            "CARGO_HOME=/github/home/.cargo".to_string(),
            format!(
                "VELNOR_DOCKER_HOST_TEMP={}",
                self.docker_host_path(&self.temp_host).display()
            ),
            format!(
                "VELNOR_DOCKER_HOST_WORKSPACE={}",
                self.docker_host_path(&self.workspace_host).display()
            ),
        ] {
            args.extend(["-e".into(), kv]);
        }
    }

    fn run_node_action_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        path_prepend: &[String],
        node_image: &str,
        entrypoint_container_path: &str,
    ) -> Vec<String> {
        let mut args = vec![
            "run".into(),
            "--rm".into(),
            "--name".into(),
            self.sidecar_container_name("node-action"),
            "--network".into(),
            self.network.clone(),
            "--workdir".into(),
            working_directory.into(),
            "-v".into(),
            self.mount_arg(&self.workspace_host, "/__w"),
            "-v".into(),
            self.mount_arg(&self.workspace_host, "/github/workspace"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/__t"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/tmp"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/github/runner_temp"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/github/file_commands"),
            "-v".into(),
            self.mount_arg(&self.home_host, "/github/home"),
            "-v".into(),
            self.mount_arg(&workflow_host(&self.temp_host), "/github/workflow"),
            "-v".into(),
            self.mount_arg(&self.actions_host, "/__a"),
            "-v".into(),
            self.mount_arg(&self.tools_host, "/__tool"),
            "-e".into(),
            "HOME=/github/home".into(),
            "-e".into(),
            "RUNNER_TOOL_CACHE=/__tool".into(),
            "-e".into(),
            "AGENT_TOOLSDIRECTORY=/__tool".into(),
            // The Node image entrypoint/shell drops env names with '-', but
            // @actions/core reads inputs like INPUT_PUSH-TO-REGISTRY.
            "--entrypoint".into(),
            "node".into(),
        ];
        self.append_compiler_cache_mount(&mut args);
        if self.mount_docker_socket {
            args.extend([
                "-v".into(),
                "/var/run/docker.sock:/var/run/docker.sock".into(),
            ]);
        }
        self.append_docker_cli_mounts(&mut args);
        for (name, value) in env {
            args.extend(["-e".into(), format!("{name}={value}")]);
        }
        if !path_prepend.is_empty() {
            let path = path_prepend
                .iter()
                .cloned()
                .chain(std::iter::once(NODE_ACTION_BASE_PATH.to_string()))
                .collect::<Vec<_>>()
                .join(":");
            args.extend(["-e".into(), format!("PATH={path}")]);
        }
        args.push(node_image.into());
        args.push(entrypoint_container_path.into());
        args
    }

    pub fn prepare_run_node_action_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        path_prepend: &[String],
        node_image: &str,
        entrypoint_container_path: &str,
    ) -> io::Result<PreparedDockerArgs> {
        let mut prepared = PreparedDockerArgs::new(self.run_node_action_args(
            working_directory,
            &[],
            path_prepend,
            node_image,
            entrypoint_container_path,
        ));
        let image_position = prepared.args.len() - 2;
        let trailing = prepared.args.split_off(image_position);
        self.append_step_env(&mut prepared, env, secret_masks)?;
        prepared.args.extend(trailing);
        Ok(prepared)
    }

    pub fn build_docker_action_args(
        &self,
        image: &str,
        dockerfile_host: &Path,
        context_host: &Path,
    ) -> Vec<String> {
        vec![
            "build".into(),
            "--tag".into(),
            image.into(),
            "--file".into(),
            self.docker_host_path(dockerfile_host).display().to_string(),
            self.docker_host_path(context_host).display().to_string(),
        ]
    }

    fn run_docker_action_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        image: &str,
        entrypoint: Option<&str>,
        command_args: &[String],
    ) -> Vec<String> {
        let mut args = vec![
            "run".into(),
            "--rm".into(),
            "--name".into(),
            self.sidecar_container_name("docker-action"),
            "--network".into(),
            self.network.clone(),
            "--workdir".into(),
            working_directory.into(),
            "-v".into(),
            self.mount_arg(&self.workspace_host, "/__w"),
            "-v".into(),
            self.mount_arg(&self.workspace_host, "/github/workspace"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/__t"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/tmp"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/github/runner_temp"),
            "-v".into(),
            self.mount_arg(&self.temp_host, "/github/file_commands"),
            "-v".into(),
            self.mount_arg(&self.home_host, "/github/home"),
            "-v".into(),
            self.mount_arg(&workflow_host(&self.temp_host), "/github/workflow"),
            "-v".into(),
            self.mount_arg(&self.actions_host, "/__a"),
            "-v".into(),
            self.mount_arg(&self.tools_host, "/__tool"),
            "-e".into(),
            "HOME=/github/home".into(),
            "-e".into(),
            "RUNNER_TOOL_CACHE=/__tool".into(),
            "-e".into(),
            "AGENT_TOOLSDIRECTORY=/__tool".into(),
        ];
        self.append_compiler_cache_mount(&mut args);
        if self.mount_docker_socket {
            args.extend([
                "-v".into(),
                "/var/run/docker.sock:/var/run/docker.sock".into(),
            ]);
        }
        self.append_docker_cli_mounts(&mut args);
        for (name, value) in env {
            args.extend(["-e".into(), format!("{name}={value}")]);
        }
        if let Some(entrypoint) = entrypoint {
            args.extend(["--entrypoint".into(), entrypoint.into()]);
        }
        args.push(image.into());
        args.extend(command_args.iter().cloned());
        args
    }

    pub fn prepare_run_docker_action_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        secret_masks: &[String],
        image: &str,
        entrypoint: Option<&str>,
        command_args: &[String],
    ) -> io::Result<PreparedDockerArgs> {
        let mut prepared = PreparedDockerArgs::new(self.run_docker_action_args(
            working_directory,
            &[],
            image,
            entrypoint,
            command_args,
        ));
        let image_position = prepared.args.len() - command_args.len() - 1;
        let trailing = prepared.args.split_off(image_position);
        self.append_step_env(&mut prepared, env, secret_masks)?;
        prepared.args.extend(trailing);
        Ok(prepared)
    }

    pub fn remove_container_args(&self) -> Vec<String> {
        vec!["rm".into(), "--force".into(), self.name.clone()]
    }

    pub fn remove_network_args(&self) -> Vec<String> {
        vec!["network".into(), "rm".into(), self.network.clone()]
    }

    pub fn disconnect_network_args(&self) -> Vec<String> {
        vec![
            "network".into(),
            "disconnect".into(),
            "--force".into(),
            self.network.clone(),
            self.name.clone(),
        ]
    }

    pub fn connect_network_args(&self) -> Vec<String> {
        vec![
            "network".into(),
            "connect".into(),
            self.network.clone(),
            self.name.clone(),
        ]
    }

    pub fn inspect_network_args(&self) -> Vec<String> {
        vec!["network".into(), "inspect".into(), self.network.clone()]
    }

    pub fn service_dns_args(&self, alias: &str) -> Vec<String> {
        vec![
            "exec".into(),
            self.name.clone(),
            "getent".into(),
            "hosts".into(),
            alias.into(),
        ]
    }

    pub fn resolver_state_args(&self) -> Vec<String> {
        vec![
            "exec".into(),
            self.name.clone(),
            "cat".into(),
            "/etc/resolv.conf".into(),
        ]
    }

    fn append_docker_cli_mounts(&self, args: &mut Vec<String>) {
        if !self.mount_docker_socket {
            return;
        }
        if let Some(path) = &self.docker_cli_host_path {
            args.extend([
                "-v".into(),
                format!("{}:/usr/local/bin/docker:ro", path.display()),
            ]);
        }
        if let Some(path) = &self.docker_cli_plugin_host_dir {
            args.extend([
                "-v".into(),
                format!("{}:/usr/local/lib/docker/cli-plugins:ro", path.display()),
            ]);
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
            |repository| cargo_executable_store_host(&self.temp_host, &repository),
        )
    }

    pub(crate) fn mise_executable_store_host(&self) -> PathBuf {
        self.repository_store_key().map_or_else(
            || {
                eprintln!(
                    "forensics.lifecycle: persistent mise install store refused: missing github.repository"
                );
                self.temp_host.join("_velnor/ephemeral/mise-installs")
            },
            |repository| mise_executable_store_host(&self.temp_host, &repository),
        )
    }

    fn rustup_executable_store_host(&self) -> PathBuf {
        self.repository_store_key().map_or_else(
            || {
                eprintln!(
                    "forensics.lifecycle: persistent rustup store refused: missing github.repository"
                );
                self.temp_host.join("_velnor/ephemeral/rustup")
            },
            |repository| rustup_executable_store_host(&self.temp_host, &repository),
        )
    }

    fn playwright_browser_store_host(&self) -> PathBuf {
        self.repository_store_key().map_or_else(
            || self.home_host.join(".cache/ms-playwright"),
            |repository| playwright_browser_store_host(&self.temp_host, &repository),
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

    fn local_work_dir(&self) -> Option<&Path> {
        let job_dir = self.temp_host.parent()?;
        job_dir.parent()
    }

    fn sidecar_container_name(&self, kind: &str) -> String {
        format!("velnor-{kind}-{}", self.name)
    }
}

#[derive(Debug)]
pub struct PreparedDockerArgs {
    pub args: Vec<String>,
    env_files: Vec<ExecEnvFile>,
}

impl PreparedDockerArgs {
    fn new(args: Vec<String>) -> Self {
        Self {
            args,
            env_files: Vec::new(),
        }
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }
}

#[derive(Debug)]
struct ExecEnvFile {
    path: PathBuf,
}

impl ExecEnvFile {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ExecEnvFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn env_value_is_secret(value: &str, secret_masks: &[String]) -> bool {
    !value.is_empty()
        && secret_masks
            .iter()
            .filter(|mask| mask.len() >= 3)
            .any(|mask| value.contains(mask))
}

fn env_name_is_secret(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "ACTIONS_RUNTIME_TOKEN" | "ACTIONS_ID_TOKEN_REQUEST_TOKEN" | "GITHUB_TOKEN"
    ) || name.ends_with("_TOKEN")
        || name.ends_with("_PASSWORD")
        || name.ends_with("_SECRET")
        || name.ends_with("_PRIVATE_KEY")
}

fn write_exec_env_file(temp_host: &Path, name: &str, value: &str) -> io::Result<ExecEnvFile> {
    let dir = temp_host.join("_velnor").join("exec-env");
    fs::create_dir_all(&dir)?;
    let counter = EXEC_ENV_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("env-{}-{counter}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&path)?;
    writeln!(file, "{name}={value}")?;
    Ok(ExecEnvFile { path })
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
    pub fn start_args(&self) -> Vec<String> {
        let mut args = vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            self.name.clone(),
        ];
        for (name, value) in &self.env {
            args.extend(["-e".into(), format!("{name}={value}")]);
        }
        for port in &self.ports {
            args.extend(["-p".into(), port.clone()]);
        }
        args.extend(self.options.iter().cloned());
        // Runner-owned network policy must win over any network-shaped token
        // present in the expanded service options. Docker uses the final
        // occurrence, so append the per-job network and workflow service key
        // as its DNS alias after user options.
        args.extend([
            "--network".into(),
            self.network.clone(),
            "--network-alias".into(),
            self.network_alias.clone(),
        ]);
        args.extend([self.image.clone()]);
        args
    }

    pub fn remove_args(&self) -> Vec<String> {
        vec!["rm".into(), "--force".into(), self.name.clone()]
    }

    pub fn disconnect_network_args(&self) -> Vec<String> {
        vec![
            "network".into(),
            "disconnect".into(),
            "--force".into(),
            self.network.clone(),
            self.name.clone(),
        ]
    }

    pub fn connect_network_args(&self) -> Vec<String> {
        vec![
            "network".into(),
            "connect".into(),
            "--alias".into(),
            self.network_alias.clone(),
            self.network.clone(),
            self.name.clone(),
        ]
    }

    pub fn health_status_args(&self) -> Vec<String> {
        vec![
            "inspect".into(),
            "--format={{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}"
                .into(),
            self.name.clone(),
        ]
    }

    pub fn id_args(&self) -> Vec<String> {
        vec![
            "inspect".into(),
            "--format={{.Id}}".into(),
            self.name.clone(),
        ]
    }

    pub fn mapped_ports_args(&self) -> Vec<String> {
        vec!["port".into(), self.name.clone()]
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

pub(crate) fn sccache_host(temp_host: &Path) -> PathBuf {
    crate::storage::cache_class_path(
        &daemon_store_root(temp_host),
        "compiler/sccache",
        "_velnor_sccache",
    )
}

pub(crate) fn kache_host(temp_host: &Path) -> PathBuf {
    crate::storage::cache_class_path(
        &daemon_store_root(temp_host),
        "compiler/kache",
        "_velnor_kache",
    )
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

/// Host-persistent rustup state used by mise's Rust backend, scoped by the
/// same trust/repository boundary as executable mise installs.
pub(crate) fn rustup_executable_store_host(temp_host: &Path, repository: &str) -> PathBuf {
    rustup_executable_store_host_for_scope(
        temp_host,
        &crate::github_adapter::cargo_target_trust_scope(),
        repository,
    )
}

fn rustup_executable_store_host_for_scope(
    temp_host: &Path,
    trust_scope: &str,
    repository: &str,
) -> PathBuf {
    crate::storage::child_with_legacy_trust(mise_store_host(temp_host), "rustup", trust_scope)
        .join(sanitize_store_key(repository))
}

/// Root for opt-in persistent workspace target buckets (one per job class).
pub(crate) fn cargo_target_store_host(temp_host: &Path) -> PathBuf {
    crate::storage::cache_class_path(&daemon_store_root(temp_host), "targets", "_velnor_targets")
}

/// Host-persistent Playwright browser downloads, scoped by trust + repository.
pub(crate) fn playwright_browser_store_host(temp_host: &Path, repository: &str) -> PathBuf {
    let root =
        crate::storage::cache_class_path(&daemon_store_root(temp_host), "caches", "_velnor_caches");
    crate::storage::append_legacy_trust(root, &crate::github_adapter::cargo_target_trust_scope())
        .join(sanitize_store_key(repository))
        .join("playwright")
}

/// Resolve the daemon-shared store root from a job temp dir
/// (`…/work/slot-N/<job>/temp` → `…/work`).
fn daemon_store_root(temp_host: &Path) -> PathBuf {
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

    fn spec() -> JobContainerSpec {
        JobContainerSpec {
            name: "velnor-job-1".into(),
            image: "ubuntu:24.04".into(),
            network: "velnor-net-1".into(),
            workspace_host: "/tmp/work".into(),
            temp_host: "/tmp/temp".into(),
            home_host: "/tmp/home".into(),
            actions_host: "/tmp/actions".into(),
            tools_host: "/tmp/tools".into(),
            mount_docker_socket: true,
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
            compiler_cache_backend: CompilerCacheBackend::Sccache,
        }
    }

    fn container_test_temp(name: &str) -> PathBuf {
        let counter = EXEC_ENV_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "velnor-container-{name}-{}-{counter}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
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
    fn compiler_cache_mounts_are_mutually_exclusive() {
        let mut job = spec();
        let sccache = job.start_args().join(" ");
        assert!(sccache.contains("/var/cache/sccache"));
        assert!(!sccache.contains("/var/cache/kache"));

        job.compiler_cache_backend = CompilerCacheBackend::Kache;
        let kache = job.start_args().join(" ");
        assert!(kache.contains("/var/cache/kache"));
        assert!(!kache.contains("/var/cache/sccache"));

        job.compiler_cache_backend = CompilerCacheBackend::Off;
        let off = job.start_args().join(" ");
        assert!(!off.contains("/var/cache/sccache"));
        assert!(!off.contains("/var/cache/kache"));
    }

    #[test]
    fn compiler_cache_stores_have_distinct_canonical_classes() {
        let temp = Path::new("/var/lib/velnor/work/slot-3/job-9/temp");
        assert_eq!(
            sccache_host(temp),
            PathBuf::from("/var/lib/velnor/work/_velnor_sccache")
        );
        assert_eq!(
            kache_host(temp),
            PathBuf::from("/var/lib/velnor/work/_velnor_kache")
        );
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
        assert_eq!(
            rustup_executable_store_host_for_scope(temp, "trusted", "ChainArgos/java-monorepo"),
            PathBuf::from(
                "/var/lib/velnor/work/_velnor_mise/rustup/trusted/ChainArgos_java-monorepo"
            )
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
        assert_ne!(
            rustup_executable_store_host_for_scope(temp, "trusted", "org/one"),
            rustup_executable_store_host_for_scope(temp, "trusted", "org/two")
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
            sccache_host(Path::new("/var/lib/velnor/work/slot-3/job-9/temp")),
            PathBuf::from("/var/lib/velnor/work/_velnor_sccache")
        );
        assert_eq!(
            sccache_host(Path::new("/var/lib/velnor/work/slot-7/job-1/temp")),
            PathBuf::from("/var/lib/velnor/work/_velnor_sccache")
        );
    }

    #[test]
    fn builds_start_container_args_with_mounts() {
        let args = spec().start_args();

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-job-1"]));
        assert!(args.contains(&"/tmp/work:/__w".into()));
        assert!(args.contains(&"/tmp/temp:/tmp".into()));
        assert!(args.contains(&"/tmp/_velnor_sccache:/var/cache/sccache".into()));
        assert!(args.contains(&"/tmp/home:/github/home".into()));
        assert!(args.contains(
            &"/tmp/_velnor_caches/trusted/acme_repo/playwright:/github/home/.cache/ms-playwright"
                .into()
        ));
        assert!(args
            .contains(&"/tmp/_velnor_cargo/bin/trusted/acme_repo:/github/home/.cargo/bin".into()));
        assert!(args
            .contains(&"/tmp/_velnor_mise/installs/trusted/acme_repo:/opt/mise/installs".into()));
        assert!(args.contains(&"/tmp/_velnor_mise/rustup/trusted/acme_repo:/root/.rustup".into()));
        assert!(args.contains(
            &"/tmp/_velnor_cargo/registry/cache:/github/home/.cargo/registry/cache".into()
        ));
        assert!(args.contains(
            &"/tmp/_velnor_cargo/registry/index:/github/home/.cargo/registry/index".into()
        ));
        assert!(args.contains(&"/tmp/_velnor_cargo/git/db:/github/home/.cargo/git/db".into()));
        assert!(!args
            .iter()
            .any(|arg| arg.ends_with(":/github/home/.cargo/registry/src")));
        assert!(!args
            .iter()
            .any(|arg| arg.ends_with(":/github/home/.cargo/git/checkouts")));
        assert!(args.contains(&"/tmp/_velnor_mise/cache:/opt/mise/cache".into()));
        assert!(args.contains(&"/tmp/temp/_github_workflow:/github/workflow".into()));
        assert!(args.contains(&"HOME=/github/home".into()));
        assert!(args.contains(&"SCCACHE_DIR=/var/cache/sccache".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"NODE_OPTIONS=--max-old-space-size=4096".into()));
        assert!(args.windows(2).any(|pair| pair == ["--cpus", "2"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--ulimit", "nofile=65536:65536"]));
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["--sysctl", "net.ipv6.conf.all.disable_ipv6=1"] }));
        assert!(args.windows(2).any(|pair| pair == ["--memory", "8g"]));
        let cpus_pos = args.iter().position(|arg| arg == "--cpus").unwrap();
        let memory_pos = args.iter().position(|arg| arg == "--memory").unwrap();
        assert!(cpus_pos < memory_pos);
        assert!(args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        // PID 1 tails the live console file (so `docker logs` mirrors the UI).
        assert_eq!(
            args.last().map(String::as_str),
            Some("mkdir -p /__t/_velnor && touch /__t/_velnor/console.log && exec tail -n +1 -F /__t/_velnor/console.log")
        );
    }

    #[test]
    fn mise_seed_copies_repo_scoped_rustup_state() {
        let args = spec().seed_mise_store_args();

        assert!(args
            .contains(&"/tmp/_velnor_mise/rustup/trusted/acme_repo:/__velnor_seed/rustup".into()));
        assert!(
            args.last().is_some_and(
                |script| script.contains("cp -an /root/.rustup/. /__velnor_seed/rustup/")
            )
        );
    }

    #[test]
    fn maps_job_paths_to_docker_host_work_dir() {
        let mut spec = spec();
        spec.workspace_host = "/runner/work/job-1/workspace".into();
        spec.temp_host = "/runner/work/job-1/temp".into();
        spec.home_host = "/runner/work/job-1/home".into();
        spec.actions_host = "/runner/work/job-1/actions".into();
        spec.tools_host = "/runner/work/job-1/tools".into();
        spec.docker_host_work_dir = Some("/daemon/work".into());

        let args = spec.start_args();

        assert!(args.contains(&"/daemon/work/job-1/workspace:/__w".into()));
        assert!(args.contains(&"/daemon/work/job-1/temp:/__t".into()));
        assert!(args.contains(&"/daemon/work/job-1/temp:/daemon/work/job-1/temp".into()));
        assert!(args.contains(&"/daemon/work/job-1/workspace:/daemon/work/job-1/workspace".into()));
        assert!(args.contains(&"/daemon/work/_velnor_sccache:/var/cache/sccache".into()));
        assert!(args.contains(&"/daemon/work/job-1/home:/github/home".into()));
        assert!(args.contains(&"/daemon/work/job-1/temp/_github_workflow:/github/workflow".into()));
        assert!(args.contains(&"/daemon/work/job-1/actions:/__a".into()));
        assert!(args.contains(&"/daemon/work/job-1/tools:/__tool".into()));
        assert!(args.contains(&"VELNOR_DOCKER_HOST_TEMP=/daemon/work/job-1/temp".into()));
        assert!(args.contains(&"VELNOR_DOCKER_HOST_WORKSPACE=/daemon/work/job-1/workspace".into()));
    }

    #[test]
    fn builds_bash_exec_args() {
        let args = spec().exec_script_args(
            "/__t/step.sh",
            Shell::Bash,
            "/__w/repo",
            &[("GITHUB_OUTPUT".into(), "/__t/out".into())],
        );

        assert_eq!(
            args,
            vec![
                "exec",
                "--workdir",
                "/__w/repo",
                "-e",
                "HOME=/github/home",
                "-e",
                "RUSTUP_HOME=/root/.rustup",
                "-e",
                "CARGO_HOME=/github/home/.cargo",
                "-e",
                "VELNOR_DOCKER_HOST_TEMP=/tmp/temp",
                "-e",
                "VELNOR_DOCKER_HOST_WORKSPACE=/tmp/work",
                "-e",
                "GITHUB_OUTPUT=/__t/out",
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
        let args = spec().exec_process_args(
            "/__w/repo",
            &[("INPUT_NAME".into(), "value".into())],
            &["node".into(), "/__a/action/dist/index.js".into()],
        );

        assert_eq!(
            args,
            vec![
                "exec",
                "--workdir",
                "/__w/repo",
                "-e",
                "HOME=/github/home",
                "-e",
                "RUSTUP_HOME=/root/.rustup",
                "-e",
                "CARGO_HOME=/github/home/.cargo",
                "-e",
                "VELNOR_DOCKER_HOST_TEMP=/tmp/temp",
                "-e",
                "VELNOR_DOCKER_HOST_WORKSPACE=/tmp/work",
                "-e",
                "INPUT_NAME=value",
                "velnor-job-1",
                "node",
                "/__a/action/dist/index.js"
            ]
        );
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
        assert!(joined.contains("PLAIN=visible"));
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
    fn multiline_secret_is_rejected_instead_of_exposed() {
        let error = spec()
            .prepare_exec_process_args(
                "/__w",
                &[("ACTIONS_RUNTIME_TOKEN".into(), "line-one\nline-two".into())],
                &[],
                &["printenv".into()],
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!error.to_string().contains("line-one"));
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
        }
    }

    #[test]
    fn nonsecret_env_stays_inline() {
        let prepared = spec()
            .prepare_exec_process_args(
                "/__w",
                &[("PLAIN".into(), "visible".into())],
                &["PLACEHOLDER_SECRET".into()],
                &["printenv".into()],
            )
            .unwrap();

        assert!(prepared.args().contains(&"-e".into()));
        assert!(prepared.args().contains(&"PLAIN=visible".into()));
        assert!(!prepared.args().contains(&"--env-file".into()));
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

        let env_file_pos = prepared
            .args()
            .iter()
            .position(|arg| arg == "--env-file")
            .unwrap();
        let override_pos = prepared
            .args()
            .windows(2)
            .position(|pair| pair == ["-e", "HOME=/override"])
            .unwrap();
        assert!(env_file_pos < override_pos);
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

        assert_eq!(
            fs::read_to_string(&env_file).unwrap(),
            "TOKEN=PLACEHOLDER_SECRET\n"
        );
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
        let args = spec().run_node_action_args(
            "/__w",
            &[("GITHUB_OUTPUT".into(), "/__t/out".into())],
            &[],
            "node:20-bookworm",
            "/__a/action/dist/index.js",
        );

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-node-action-velnor-job-1"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--network", "velnor-net-1"]));
        assert!(args.windows(2).any(|pair| pair == ["--workdir", "/__w"]));
        assert!(args.contains(&"/tmp/work:/__w".into()));
        assert!(args.contains(&"/tmp/work:/github/workspace".into()));
        assert!(args.contains(&"/tmp/temp:/tmp".into()));
        assert!(args.contains(&"/tmp/_velnor_sccache:/var/cache/sccache".into()));
        assert!(args.contains(&"/tmp/temp:/github/runner_temp".into()));
        assert!(args.contains(&"/tmp/temp:/github/file_commands".into()));
        assert!(args.contains(&"/tmp/home:/github/home".into()));
        assert!(args.contains(&"/tmp/temp/_github_workflow:/github/workflow".into()));
        assert!(args.contains(&"HOME=/github/home".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"GITHUB_OUTPUT=/__t/out".into()));
        assert!(args.windows(2).any(|pair| pair == ["--entrypoint", "node"]));
        assert_eq!(
            &args[args.len() - 2..],
            ["node:20-bookworm", "/__a/action/dist/index.js"]
        );
    }

    #[test]
    fn builds_node_action_run_args_with_path_prelude() {
        let args = spec().run_node_action_args(
            "/__w",
            &[("GITHUB_OUTPUT".into(), "/__t/out".into())],
            &["/github/home/.cargo/bin".into(), "/path/with'quote".into()],
            "node:20-bookworm",
            "/__a/action/dist/index.js",
        );

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

        let start_args = spec.start_args();
        assert!(start_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(start_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));

        let node_args = spec.run_node_action_args(
            "/__w",
            &[],
            &[],
            "node:24-bookworm",
            "/__a/action/dist/index.js",
        );
        assert!(node_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(node_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));

        let docker_action_args = spec.run_docker_action_args("/__w", &[], "alpine:3.20", None, &[]);
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

        let start_args = spec.start_args();
        assert!(!start_args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert!(!start_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(!start_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));

        let node_args = spec.run_node_action_args(
            "/__w",
            &[],
            &[],
            "node:24-bookworm",
            "/__a/action/dist/index.js",
        );
        assert!(!node_args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert!(!node_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(!node_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));

        let docker_action_args = spec.run_docker_action_args("/__w", &[], "alpine:3.20", None, &[]);
        assert!(!docker_action_args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert!(!docker_action_args.contains(&"/usr/bin/docker:/usr/local/bin/docker:ro".into()));
        assert!(!docker_action_args.contains(
            &"/usr/libexec/docker/cli-plugins:/usr/local/lib/docker/cli-plugins:ro".into()
        ));
    }

    #[test]
    fn builds_docker_action_args() {
        let spec = spec();

        assert_eq!(
            spec.build_docker_action_args(
                "velnor-action-owner-repo-v1-root",
                Path::new("/tmp/actions/action/Dockerfile"),
                Path::new("/tmp/actions/action"),
            ),
            vec![
                "build",
                "--tag",
                "velnor-action-owner-repo-v1-root",
                "--file",
                "/tmp/actions/action/Dockerfile",
                "/tmp/actions/action"
            ]
        );

        let args = spec.run_docker_action_args(
            "/__w",
            &[("INPUT_NAME".into(), "value".into())],
            "alpine:3.20",
            Some("/entrypoint.sh"),
            &["arg1".into()],
        );

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-docker-action-velnor-job-1"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--network", "velnor-net-1"]));
        assert!(args.contains(&"/tmp/work:/__w".into()));
        assert!(args.contains(&"/tmp/work:/github/workspace".into()));
        assert!(args.contains(&"/tmp/temp:/tmp".into()));
        assert!(args.contains(&"/tmp/_velnor_sccache:/var/cache/sccache".into()));
        assert!(args.contains(&"/tmp/temp:/github/runner_temp".into()));
        assert!(args.contains(&"/tmp/temp:/github/file_commands".into()));
        assert!(args.contains(&"/tmp/home:/github/home".into()));
        assert!(args.contains(&"/tmp/temp/_github_workflow:/github/workflow".into()));
        assert!(args.contains(&"HOME=/github/home".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"INPUT_NAME=value".into()));
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

        assert_eq!(
            service.start_args(),
            vec![
                "run",
                "--detach",
                "--name",
                "velnor-service-postgres",
                "-e",
                "POSTGRES_PASSWORD=postgres",
                "-p",
                "5432:5432",
                "--health-cmd",
                "pg_isready",
                "--network",
                "velnor-net-1",
                "--network-alias",
                "postgres",
                "postgres:16"
            ]
        );
        assert_eq!(
            service.remove_args(),
            vec!["rm", "--force", "velnor-service-postgres"]
        );
        assert_eq!(
            service.health_status_args(),
            vec![
                "inspect",
                "--format={{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}",
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
        let args = service.start_args();
        assert_eq!(
            &args[args.len() - 5..],
            [
                "--network",
                "velnor-net-owned",
                "--network-alias",
                "postgres",
                "postgres:16"
            ]
        );
    }

    #[test]
    fn job_runner_network_overrides_expanded_options() {
        let mut job = spec();
        job.options = vec!["--network".into(), "unexpected".into()];
        let args = job.start_args();
        let network_pairs = args
            .windows(2)
            .filter(|pair| pair[0] == "--network")
            .collect::<Vec<_>>();
        assert_eq!(network_pairs.last().unwrap()[1], "velnor-net-1");
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
        let job_args = job.start_args();
        let service_args = service.start_args();
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

    #[test]
    fn splits_container_options_with_quotes() {
        assert_eq!(
            split_container_options(r#"--cpus 2 --health-cmd "pg_isready -U postgres""#),
            vec!["--cpus", "2", "--health-cmd", "pg_isready -U postgres"]
        );
    }
}

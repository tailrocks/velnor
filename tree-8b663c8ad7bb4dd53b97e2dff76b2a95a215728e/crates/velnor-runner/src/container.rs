#![allow(dead_code)]

use std::path::{Path, PathBuf};

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
    pub options: Vec<String>,
    pub services: Vec<ServiceContainerSpec>,
    pub node_action_image: String,
    pub docker_cli_host_path: Option<PathBuf>,
    pub docker_cli_plugin_host_dir: Option<PathBuf>,
    pub verify_bind_mounts: bool,
}

impl JobContainerSpec {
    pub fn create_network_args(&self) -> Vec<String> {
        vec!["network".into(), "create".into(), self.network.clone()]
    }

    pub fn start_args(&self) -> Vec<String> {
        let mut args = vec![
            "run".into(),
            "--detach".into(),
            "--name".into(),
            self.name.clone(),
            "--network".into(),
            self.network.clone(),
            "--workdir".into(),
            "/__w".into(),
            "-v".into(),
            mount(&self.workspace_host, "/__w"),
            "-v".into(),
            mount(&self.temp_host, "/__t"),
            "-v".into(),
            mount(&self.home_host, "/github/home"),
            "-v".into(),
            mount(&self.actions_host, "/__a"),
            "-v".into(),
            mount(&self.tools_host, "/__tool"),
            "-e".into(),
            "HOME=/github/home".into(),
            "-e".into(),
            "RUNNER_TEMP=/__t".into(),
            "-e".into(),
            "RUNNER_TOOL_CACHE=/__tool".into(),
            "-e".into(),
            "AGENT_TOOLSDIRECTORY=/__tool".into(),
        ];
        for (name, value) in &self.env {
            args.extend(["-e".into(), format!("{name}={value}")]);
        }
        args.extend(self.options.iter().cloned());

        if self.mount_docker_socket {
            args.extend([
                "-v".into(),
                "/var/run/docker.sock:/var/run/docker.sock".into(),
            ]);
        }
        self.append_docker_cli_mounts(&mut args);

        args.extend([
            self.image.clone(),
            "tail".into(),
            "-f".into(),
            "/dev/null".into(),
        ]);
        args
    }

    pub fn exec_script_args(
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

    pub fn exec_process_args(
        &self,
        working_directory: &str,
        env: &[(String, String)],
        command: &[String],
    ) -> Vec<String> {
        let mut args = vec!["exec".into(), "--workdir".into(), working_directory.into()];
        for (name, value) in env {
            args.extend(["-e".into(), format!("{name}={value}")]);
        }
        args.push(self.name.clone());
        args.extend(command.iter().cloned());
        args
    }

    pub fn run_node_action_args(
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
            "--network".into(),
            self.network.clone(),
            "--workdir".into(),
            working_directory.into(),
            "-v".into(),
            mount(&self.workspace_host, "/__w"),
            "-v".into(),
            mount(&self.temp_host, "/__t"),
            "-v".into(),
            mount(&self.home_host, "/github/home"),
            "-v".into(),
            mount(&self.actions_host, "/__a"),
            "-v".into(),
            mount(&self.tools_host, "/__tool"),
            "-e".into(),
            "HOME=/github/home".into(),
            "-e".into(),
            "RUNNER_TOOL_CACHE=/__tool".into(),
            "-e".into(),
            "AGENT_TOOLSDIRECTORY=/__tool".into(),
        ];
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
        args.push(node_image.into());
        if path_prepend.is_empty() {
            args.extend(["node".into(), entrypoint_container_path.into()]);
        } else {
            args.extend([
                "sh".into(),
                "-lc".into(),
                node_action_shell_command(path_prepend, entrypoint_container_path),
            ]);
        }
        args
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
            dockerfile_host.display().to_string(),
            context_host.display().to_string(),
        ]
    }

    pub fn run_docker_action_args(
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
            "--network".into(),
            self.network.clone(),
            "--workdir".into(),
            working_directory.into(),
            "-v".into(),
            mount(&self.workspace_host, "/__w"),
            "-v".into(),
            mount(&self.temp_host, "/__t"),
            "-v".into(),
            mount(&self.home_host, "/github/home"),
            "-v".into(),
            mount(&self.actions_host, "/__a"),
            "-v".into(),
            mount(&self.tools_host, "/__tool"),
            "-e".into(),
            "HOME=/github/home".into(),
            "-e".into(),
            "RUNNER_TOOL_CACHE=/__tool".into(),
            "-e".into(),
            "AGENT_TOOLSDIRECTORY=/__tool".into(),
        ];
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

    pub fn remove_container_args(&self) -> Vec<String> {
        vec!["rm".into(), "--force".into(), self.name.clone()]
    }

    pub fn remove_network_args(&self) -> Vec<String> {
        vec!["network".into(), "rm".into(), self.network.clone()]
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
            "--network".into(),
            self.network.clone(),
            "--network-alias".into(),
            self.network_alias.clone(),
        ];
        for (name, value) in &self.env {
            args.extend(["-e".into(), format!("{name}={value}")]);
        }
        for port in &self.ports {
            args.extend(["-p".into(), port.clone()]);
        }
        args.extend(self.options.iter().cloned());
        args.extend([self.image.clone()]);
        args
    }

    pub fn remove_args(&self) -> Vec<String> {
        vec!["rm".into(), "--force".into(), self.name.clone()]
    }

    pub fn health_status_args(&self) -> Vec<String> {
        vec![
            "inspect".into(),
            "--format={{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}"
                .into(),
            self.name.clone(),
        ]
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Shell {
    Bash,
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
                script_path.into(),
            ],
            Self::Sh => vec!["sh".into(), "-e".into(), script_path.into()],
        }
    }
}

fn mount(host: &Path, container: &str) -> String {
    format!("{}:{container}", host.display())
}

fn node_action_shell_command(path_prepend: &[String], entrypoint_container_path: &str) -> String {
    let joined = path_prepend
        .iter()
        .map(|path| shell_single_quote(path))
        .collect::<Vec<_>>()
        .join(":");
    format!(
        "export PATH={joined}:\"$PATH\"\nexec node {}",
        shell_single_quote(entrypoint_container_path)
    )
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
            options: vec!["--cpus".into(), "2".into()],
            services: Vec::new(),
            node_action_image: "node:24-bookworm".into(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            verify_bind_mounts: false,
        }
    }

    #[test]
    fn builds_start_container_args_with_mounts() {
        let args = spec().start_args();

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-job-1"]));
        assert!(args.contains(&"/tmp/work:/__w".into()));
        assert!(args.contains(&"/tmp/home:/github/home".into()));
        assert!(args.contains(&"HOME=/github/home".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"NODE_OPTIONS=--max-old-space-size=4096".into()));
        assert!(args.windows(2).any(|pair| pair == ["--cpus", "2"]));
        assert!(args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert_eq!(args.last().map(String::as_str), Some("/dev/null"));
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
                "GITHUB_OUTPUT=/__t/out",
                "velnor-job-1",
                "bash",
                "--noprofile",
                "--norc",
                "-e",
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
                "INPUT_NAME=value",
                "velnor-job-1",
                "node",
                "/__a/action/dist/index.js"
            ]
        );
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
            .any(|pair| pair == ["--network", "velnor-net-1"]));
        assert!(args.contains(&"/tmp/work:/__w".into()));
        assert!(args.contains(&"/tmp/home:/github/home".into()));
        assert!(args.contains(&"HOME=/github/home".into()));
        assert!(args.contains(&"RUNNER_TOOL_CACHE=/__tool".into()));
        assert!(args.contains(&"AGENT_TOOLSDIRECTORY=/__tool".into()));
        assert!(args.contains(&"GITHUB_OUTPUT=/__t/out".into()));
        assert_eq!(
            &args[args.len() - 3..],
            ["node:20-bookworm", "node", "/__a/action/dist/index.js"]
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

        assert_eq!(
            &args[args.len() - 4..args.len() - 1],
            ["node:20-bookworm", "sh", "-lc"]
        );
        assert_eq!(
            args.last().unwrap(),
            "export PATH='/github/home/.cargo/bin':'/path/with'\\''quote':\"$PATH\"\nexec node '/__a/action/dist/index.js'"
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
            .any(|pair| pair == ["--network", "velnor-net-1"]));
        assert!(args.contains(&"/tmp/work:/__w".into()));
        assert!(args.contains(&"/tmp/home:/github/home".into()));
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
                "--network",
                "velnor-net-1",
                "--network-alias",
                "postgres",
                "-e",
                "POSTGRES_PASSWORD=postgres",
                "-p",
                "5432:5432",
                "--health-cmd",
                "pg_isready",
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
    fn splits_container_options_with_quotes() {
        assert_eq!(
            split_container_options(r#"--cpus 2 --health-cmd "pg_isready -U postgres""#),
            vec!["--cpus", "2", "--health-cmd", "pg_isready -U postgres"]
        );
    }
}

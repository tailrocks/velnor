#![allow(dead_code)]

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct JobContainerSpec {
    pub name: String,
    pub image: String,
    pub network: String,
    pub workspace_host: PathBuf,
    pub temp_host: PathBuf,
    pub actions_host: PathBuf,
    pub tools_host: PathBuf,
    pub mount_docker_socket: bool,
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
            mount(&self.actions_host, "/__a"),
            "-v".into(),
            mount(&self.tools_host, "/__tool"),
            "-e".into(),
            "RUNNER_TEMP=/__t".into(),
            "-e".into(),
            "RUNNER_TOOL_CACHE=/__tool".into(),
        ];

        if self.mount_docker_socket {
            args.extend([
                "-v".into(),
                "/var/run/docker.sock:/var/run/docker.sock".into(),
            ]);
        }

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
    ) -> Vec<String> {
        let mut args = vec![
            "exec".into(),
            "--workdir".into(),
            working_directory.into(),
            self.name.clone(),
        ];
        args.extend(shell.command_args(script_path_in_container));
        args
    }

    pub fn remove_container_args(&self) -> Vec<String> {
        vec!["rm".into(), "--force".into(), self.name.clone()]
    }

    pub fn remove_network_args(&self) -> Vec<String> {
        vec!["network".into(), "rm".into(), self.network.clone()]
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
            actions_host: "/tmp/actions".into(),
            tools_host: "/tmp/tools".into(),
            mount_docker_socket: true,
        }
    }

    #[test]
    fn builds_start_container_args_with_mounts() {
        let args = spec().start_args();

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-job-1"]));
        assert!(args.contains(&"/tmp/work:/__w".into()));
        assert!(args.contains(&"/var/run/docker.sock:/var/run/docker.sock".into()));
        assert_eq!(args.last().map(String::as_str), Some("/dev/null"));
    }

    #[test]
    fn builds_bash_exec_args() {
        let args = spec().exec_script_args("/__t/step.sh", Shell::Bash, "/__w/repo");

        assert_eq!(
            args,
            vec![
                "exec",
                "--workdir",
                "/__w/repo",
                "velnor-job-1",
                "bash",
                "--noprofile",
                "--norc",
                "-e",
                "/__t/step.sh"
            ]
        );
    }
}

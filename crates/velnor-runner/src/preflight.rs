use crate::{
    cli::PreflightArgs,
    executor::{CommandRunner, ProcessCommandRunner},
};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const DOCKER_MOUNT_CHECK_FILE: &str = ".velnor-mount-check";

pub fn preflight(args: PreflightArgs) -> Result<()> {
    let mut runner = ProcessCommandRunner;
    preflight_with_runner(args, &mut runner)
}

fn preflight_with_runner(args: PreflightArgs, runner: &mut dyn CommandRunner) -> Result<()> {
    let work_dir = preflight_work_dir(args.work_dir)?;
    let docker_host_work_dir = args.docker_host_work_dir;
    let temp_dir = work_dir.join("preflight").join("temp");
    let workspace_dir = work_dir.join("preflight").join("workspace");
    for path in [&temp_dir, &workspace_dir] {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    }

    run_required(runner, "git", &["--version".to_string()], "Host git")?;
    run_required(runner, "docker", &["version".to_string()], "Docker daemon")?;
    if args.require_buildx {
        run_required(
            runner,
            "docker",
            &["buildx".to_string(), "version".to_string()],
            "Docker Buildx",
        )?;
    }
    if args.require_docker_socket {
        if !Path::new("/var/run/docker.sock").exists() {
            bail!("{}", missing_docker_socket_error());
        }
        verify_container_docker_client(runner, &args.docker_image, args.require_buildx)?;
    }

    verify_job_image_tools(runner, &args.docker_image)?;
    verify_script_execution(
        runner,
        &temp_dir,
        &workspace_dir,
        &work_dir,
        docker_host_work_dir.as_deref(),
        &args.docker_image,
    )?;
    verify_bind_mount(
        runner,
        &temp_dir,
        &work_dir,
        docker_host_work_dir.as_deref(),
        &args.docker_image,
    )?;

    println!("Docker preflight passed.");
    println!("Work dir: {}", work_dir.display());
    if let Some(path) = &docker_host_work_dir {
        println!("Docker host work dir: {}", path.display());
    }
    println!("Image: {}", args.docker_image);
    Ok(())
}

fn run_required(
    runner: &mut dyn CommandRunner,
    program: &str,
    args: &[String],
    label: &str,
) -> Result<()> {
    let result = runner.run(program, args)?;
    if result.code != 0 {
        bail!(
            "{label} check failed with code {}: {}",
            result.code,
            result.stderr
        );
    }
    Ok(())
}

fn verify_script_execution(
    runner: &mut dyn CommandRunner,
    temp_dir: &Path,
    workspace_dir: &Path,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
    docker_image: &str,
) -> Result<()> {
    let script = temp_dir.join("velnor-preflight.sh");
    let output = temp_dir.join("velnor-preflight-output");
    fs::write(
        &script,
        "set -euo pipefail\n\
         test \"$PWD\" = /__w\n\
         echo velnor-script-ok > /__t/velnor-preflight-output\n",
    )
    .with_context(|| format!("write preflight script {}", script.display()))?;
    fs::remove_file(&output).ok();

    let args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        preflight_container_name("script"),
        "--workdir".to_string(),
        "/__w".to_string(),
        "-v".to_string(),
        format!(
            "{}:/__t",
            docker_mount_path(temp_dir, work_dir, docker_host_work_dir)?.display()
        ),
        "-v".to_string(),
        format!(
            "{}:/__w",
            docker_mount_path(workspace_dir, work_dir, docker_host_work_dir)?.display()
        ),
        docker_image.to_string(),
        "bash".to_string(),
        "/__t/velnor-preflight.sh".to_string(),
    ];
    let result = runner.run("docker", &args);
    fs::remove_file(&script).ok();

    let result = result?;
    if result.code != 0 {
        fs::remove_file(&output).ok();
        bail!(
            "Docker image '{}' could not execute a Velnor temp script from /__t with /__w workdir. {}",
            docker_image,
            bind_mount_error_detail(&temp_dir.display().to_string(), &result.stderr)
        );
    }

    let output_value = fs::read_to_string(&output)
        .with_context(|| format!("read preflight output {}", output.display()))?;
    fs::remove_file(&output).ok();
    if output_value.trim() != "velnor-script-ok" {
        bail!(
            "Docker script preflight wrote unexpected output '{}'",
            output_value.trim()
        );
    }
    Ok(())
}

fn verify_container_docker_client(
    runner: &mut dyn CommandRunner,
    docker_image: &str,
    require_buildx: bool,
) -> Result<()> {
    let args = container_docker_client_args(docker_image, require_buildx);
    let result = runner.run("docker", &args)?;
    if result.code != 0 {
        bail!(
            "Docker CLI/Buildx is not usable inside job image '{}'. Use Velnor's Ubuntu job image or provide a --docker-image with Docker CLI and Buildx installed. stderr: {}",
            docker_image,
            result.stderr
        );
    }
    Ok(())
}

fn container_docker_client_args(docker_image: &str, require_buildx: bool) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        preflight_container_name("docker-client"),
        "-v".to_string(),
        "/var/run/docker.sock:/var/run/docker.sock".to_string(),
    ];
    args.extend([
        docker_image.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        if require_buildx {
            "docker version && docker buildx version"
        } else {
            "docker version"
        }
        .to_string(),
    ]);
    args
}

fn verify_job_image_tools(runner: &mut dyn CommandRunner, docker_image: &str) -> Result<()> {
    let args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        preflight_container_name("image-tools"),
        docker_image.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        "command -v sh >/dev/null && command -v bash >/dev/null && command -v git >/dev/null"
            .to_string(),
    ];
    let result = runner.run("docker", &args)?;
    if result.code != 0 {
        bail!(
            "Docker image '{}' is missing target job tools sh, bash, or git. stderr: {}",
            docker_image,
            result.stderr
        );
    }
    Ok(())
}

fn verify_bind_mount(
    runner: &mut dyn CommandRunner,
    temp_dir: &Path,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
    docker_image: &str,
) -> Result<()> {
    let marker = temp_dir.join(DOCKER_MOUNT_CHECK_FILE);
    fs::write(&marker, "velnor\n")
        .with_context(|| format!("write Docker bind-mount marker {}", marker.display()))?;

    let args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        preflight_container_name("bind-mount"),
        "-v".to_string(),
        format!(
            "{}:/__t",
            docker_mount_path(temp_dir, work_dir, docker_host_work_dir)?.display()
        ),
        docker_image.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        format!("test -f /__t/{DOCKER_MOUNT_CHECK_FILE}"),
    ];
    let result = runner.run("docker", &args);
    fs::remove_file(&marker).ok();

    let result = result?;
    if result.code != 0 {
        bail!(
            "Docker daemon cannot see Velnor bind-mounted work directory '{}'. {}",
            temp_dir.display(),
            bind_mount_error_detail(&temp_dir.display().to_string(), &result.stderr)
        );
    }
    Ok(())
}

fn docker_mount_path(
    path: &Path,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
) -> Result<PathBuf> {
    let Some(host_work_dir) = docker_host_work_dir else {
        return Ok(path.to_path_buf());
    };
    let relative = path.strip_prefix(work_dir).with_context(|| {
        format!(
            "path '{}' is not under work dir '{}'",
            path.display(),
            work_dir.display()
        )
    })?;
    Ok(host_work_dir.join(relative))
}

fn preflight_container_name(kind: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("velnor-preflight-{kind}-{}-{nanos}", std::process::id())
}

fn bind_mount_error_detail(path: &str, stderr: &str) -> String {
    let docker_host = std::env::var("DOCKER_HOST").unwrap_or_default();
    let remote_hint = if docker_host.starts_with("tcp://") || docker_host.starts_with("ssh://") {
        format!(
            "Detected DOCKER_HOST={docker_host}; Velnor live jobs need a Docker daemon that can see the host work directory '{path}'. Use a local Docker socket or a --work-dir path mounted into the remote daemon."
        )
    } else {
        format!(
            "Use a local Docker daemon or pass --work-dir to a path visible to the daemon at '{path}'."
        )
    };
    format!("{remote_hint} stderr: {stderr}")
}

fn missing_docker_socket_error() -> String {
    let docker_host = std::env::var("DOCKER_HOST").unwrap_or_default();
    if docker_host.starts_with("tcp://") || docker_host.starts_with("ssh://") {
        format!(
            "required Docker socket /var/run/docker.sock does not exist on this host. Detected DOCKER_HOST={docker_host}; Phase 0 target Docker/Buildx jobs need a local Docker socket mounted into Velnor job containers. Use a Linux host with /var/run/docker.sock for target proof, or set VELNOR_REQUIRE_DOCKER_SOCKET=false only for fixture checks that do not need Docker from inside the job container."
        )
    } else {
        "required Docker socket /var/run/docker.sock does not exist on this host".to_string()
    }
}

fn preflight_work_dir(work_dir: Option<PathBuf>) -> Result<PathBuf> {
    match work_dir {
        Some(path) => Ok(path),
        None => Ok(std::env::current_dir()?.join(".velnor-work")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::CommandResult;

    #[derive(Default)]
    struct RecordingRunner {
        calls: Vec<(String, Vec<String>)>,
        codes: Vec<i32>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            self.calls.push((program.to_string(), args.to_vec()));
            let code = if self.codes.is_empty() {
                0
            } else {
                self.codes.remove(0)
            };
            if code == 0 && args.contains(&"/__t/velnor-preflight.sh".to_string()) {
                if let Some(temp_mount) = args.windows(2).find_map(|items| {
                    if items[0] == "-v" {
                        items[1].strip_suffix(":/__t")
                    } else {
                        None
                    }
                }) {
                    fs::write(
                        Path::new(temp_mount).join("velnor-preflight-output"),
                        "velnor-script-ok\n",
                    )?;
                }
            }
            Ok(CommandResult {
                code,
                stdout: String::new(),
                stderr: "failed".to_string(),
            })
        }
    }

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "velnor-preflight-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn docker_client_args_require_cli_inside_job_image() {
        let args = container_docker_client_args("ubuntu:24.04", true);

        assert!(args.windows(2).any(|pair| {
            pair[0] == "--name" && pair[1].starts_with("velnor-preflight-docker-client-")
        }));
        assert!(args.contains(&"/var/run/docker.sock:/var/run/docker.sock".to_string()));
        assert!(!args.iter().any(|arg| arg.contains("/usr/local/bin/docker")));
        assert!(!args.iter().any(|arg| arg.contains("cli-plugins")));
        assert!(args.contains(&"docker version && docker buildx version".to_string()));
    }

    #[test]
    fn preflight_checks_docker_buildx_and_bind_mount_visibility() {
        let temp = temp_dir();
        let args = PreflightArgs {
            work_dir: Some(temp.clone()),
            docker_host_work_dir: None,
            docker_image: "ubuntu:24.04".to_string(),
            require_docker_socket: false,
            require_buildx: true,
        };
        let mut runner = RecordingRunner::default();

        preflight_with_runner(args, &mut runner).unwrap();

        assert_eq!(
            runner.calls[0],
            ("git".to_string(), vec!["--version".to_string()])
        );
        assert_eq!(
            runner.calls[1],
            ("docker".to_string(), vec!["version".to_string()])
        );
        assert_eq!(
            runner.calls[2],
            (
                "docker".to_string(),
                vec!["buildx".to_string(), "version".to_string()]
            )
        );
        let image_tools_call = &runner.calls[3];
        assert_eq!(image_tools_call.0, "docker");
        assert_eq!(image_tools_call.1[0], "run");
        assert!(image_tools_call.1.windows(2).any(|pair| {
            pair[0] == "--name" && pair[1].starts_with("velnor-preflight-image-tools-")
        }));
        assert_eq!(
            image_tools_call
                .1
                .iter()
                .filter(|value| value.as_str() == "--rm")
                .count(),
            1
        );
        assert!(image_tools_call.1.contains(
            &"command -v sh >/dev/null && command -v bash >/dev/null && command -v git >/dev/null"
                .to_string()
        ));
        let script_call = &runner.calls[4];
        assert_eq!(script_call.0, "docker");
        assert!(script_call.1.windows(2).any(|pair| {
            pair[0] == "--name" && pair[1].starts_with("velnor-preflight-script-")
        }));
        assert!(script_call
            .1
            .contains(&"/__t/velnor-preflight.sh".to_string()));
        assert!(script_call.1.contains(&format!(
            "{}:/__w",
            temp.join("preflight").join("workspace").display()
        )));
        let bind_mount_call = &runner.calls[5];
        assert_eq!(bind_mount_call.0, "docker");
        assert_eq!(bind_mount_call.1[0], "run");
        assert!(bind_mount_call.1.windows(2).any(|pair| {
            pair[0] == "--name" && pair[1].starts_with("velnor-preflight-bind-mount-")
        }));
        assert!(bind_mount_call.1.contains(&format!(
            "{}:/__t",
            temp.join("preflight").join("temp").display()
        )));
        assert!(bind_mount_call
            .1
            .contains(&format!("test -f /__t/{DOCKER_MOUNT_CHECK_FILE}")));
        assert!(!temp
            .join("preflight")
            .join("temp")
            .join(DOCKER_MOUNT_CHECK_FILE)
            .exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn docker_mount_path_maps_work_dir_to_docker_host_path() {
        let temp = temp_dir();
        let docker_root = PathBuf::from("/daemon/velnor-work");

        let mapped = docker_mount_path(
            &temp.join("preflight").join("temp"),
            &temp,
            Some(&docker_root),
        )
        .unwrap();

        assert_eq!(mapped, docker_root.join("preflight").join("temp"));
    }

    #[test]
    fn preflight_reports_bind_mount_failure() {
        let temp = temp_dir();
        let args = PreflightArgs {
            work_dir: Some(temp.clone()),
            docker_host_work_dir: None,
            docker_image: "ubuntu:24.04".to_string(),
            require_docker_socket: false,
            require_buildx: false,
        };
        let mut runner = RecordingRunner {
            calls: Vec::new(),
            codes: vec![0, 0, 0, 0, 1],
        };

        let error = preflight_with_runner(args, &mut runner).unwrap_err();

        assert!(error.to_string().contains("Docker daemon cannot see"));
        assert!(!temp
            .join("preflight")
            .join("temp")
            .join(DOCKER_MOUNT_CHECK_FILE)
            .exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn preflight_reports_script_execution_failure() {
        let temp = temp_dir();
        let args = PreflightArgs {
            work_dir: Some(temp.clone()),
            docker_host_work_dir: None,
            docker_image: "ubuntu:24.04".to_string(),
            require_docker_socket: false,
            require_buildx: false,
        };
        let mut runner = RecordingRunner {
            calls: Vec::new(),
            codes: vec![0, 0, 0, 1],
        };

        let error = preflight_with_runner(args, &mut runner).unwrap_err();

        assert!(error
            .to_string()
            .contains("could not execute a Velnor temp script"));
        assert!(!temp
            .join("preflight")
            .join("temp")
            .join("velnor-preflight.sh")
            .exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn preflight_reports_missing_job_image_tools() {
        let temp = temp_dir();
        let args = PreflightArgs {
            work_dir: Some(temp.clone()),
            docker_host_work_dir: None,
            docker_image: "minimal:latest".to_string(),
            require_docker_socket: false,
            require_buildx: false,
        };
        let mut runner = RecordingRunner {
            calls: Vec::new(),
            codes: vec![0, 0, 1],
        };

        let error = preflight_with_runner(args, &mut runner).unwrap_err();

        assert!(error.to_string().contains("missing target job tools"));
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn missing_socket_error_explains_remote_docker_host_target_limit() {
        let previous = std::env::var_os("DOCKER_HOST");
        std::env::set_var("DOCKER_HOST", "tcp://docker.example:2376");

        let error = missing_docker_socket_error();

        assert!(error.contains("Detected DOCKER_HOST=tcp://docker.example:2376"));
        assert!(error.contains("target Docker/Buildx jobs need a local Docker socket"));
        assert!(error.contains("VELNOR_REQUIRE_DOCKER_SOCKET=false only for fixture checks"));

        if let Some(previous) = previous {
            std::env::set_var("DOCKER_HOST", previous);
        } else {
            std::env::remove_var("DOCKER_HOST");
        }
    }
}

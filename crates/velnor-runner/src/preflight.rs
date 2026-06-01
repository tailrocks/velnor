use crate::{
    cli::PreflightArgs,
    executor::{CommandRunner, ProcessCommandRunner},
};
use anyhow::{bail, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

const DOCKER_MOUNT_CHECK_FILE: &str = ".velnor-mount-check";

pub fn preflight(args: PreflightArgs) -> Result<()> {
    let mut runner = ProcessCommandRunner;
    preflight_with_runner(args, &mut runner)
}

fn preflight_with_runner(args: PreflightArgs, runner: &mut dyn CommandRunner) -> Result<()> {
    let work_dir = preflight_work_dir(args.work_dir)?;
    let temp_dir = work_dir.join("preflight").join("temp");
    fs::create_dir_all(&temp_dir).with_context(|| format!("create {}", temp_dir.display()))?;

    run_required(runner, "docker", &["version".to_string()], "Docker daemon")?;
    if args.require_buildx {
        run_required(
            runner,
            "docker",
            &["buildx".to_string(), "version".to_string()],
            "Docker Buildx",
        )?;
    }
    if args.require_docker_socket && !Path::new("/var/run/docker.sock").exists() {
        bail!("required Docker socket /var/run/docker.sock does not exist on this host");
    }

    verify_bind_mount(runner, &temp_dir, &args.docker_image)?;

    println!("Docker preflight passed.");
    println!("Work dir: {}", work_dir.display());
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

fn verify_bind_mount(
    runner: &mut dyn CommandRunner,
    temp_dir: &Path,
    docker_image: &str,
) -> Result<()> {
    let marker = temp_dir.join(DOCKER_MOUNT_CHECK_FILE);
    fs::write(&marker, "velnor\n")
        .with_context(|| format!("write Docker bind-mount marker {}", marker.display()))?;

    let args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "-v".to_string(),
        format!("{}:/__t", temp_dir.display()),
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
            "Docker daemon cannot see Velnor bind-mounted work directory '{}'. \
             Use a local Docker daemon or pass --work-dir to a path visible to the daemon. stderr: {}",
            temp_dir.display(),
            result.stderr
        );
    }
    Ok(())
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
    fn preflight_checks_docker_buildx_and_bind_mount_visibility() {
        let temp = temp_dir();
        let args = PreflightArgs {
            work_dir: Some(temp.clone()),
            docker_image: "ubuntu:24.04".to_string(),
            require_docker_socket: false,
            require_buildx: true,
        };
        let mut runner = RecordingRunner::default();

        preflight_with_runner(args, &mut runner).unwrap();

        assert_eq!(
            runner.calls[0],
            ("docker".to_string(), vec!["version".to_string()])
        );
        assert_eq!(
            runner.calls[1],
            (
                "docker".to_string(),
                vec!["buildx".to_string(), "version".to_string()]
            )
        );
        let bind_mount_call = &runner.calls[2];
        assert_eq!(bind_mount_call.0, "docker");
        assert_eq!(bind_mount_call.1[0], "run");
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
    fn preflight_reports_bind_mount_failure() {
        let temp = temp_dir();
        let args = PreflightArgs {
            work_dir: Some(temp.clone()),
            docker_image: "ubuntu:24.04".to_string(),
            require_docker_socket: false,
            require_buildx: false,
        };
        let mut runner = RecordingRunner {
            calls: Vec::new(),
            codes: vec![0, 1],
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
}

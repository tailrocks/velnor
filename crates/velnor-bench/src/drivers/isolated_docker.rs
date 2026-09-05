//! A disposable Docker daemon for benchmark workloads that must not touch the
//! host daemon.

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::FileTypeExt as _;

use anyhow::{bail, Context as _, Result};

use crate::sys::{Invocation, Runner};

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);
// Unix domain socket paths are platform-limited. Leave room for the scheme
// and Docker's own socket handling rather than starting a daemon that cannot
// bind the requested endpoint.
const MAX_SOCKET_PATH_BYTES: usize = 100;
const DOCKER_CLIENT_ENV_TO_CLEAR: &[&str] = &[
    "DOCKER_CONTEXT",
    "DOCKER_TLS",
    "DOCKER_TLS_VERIFY",
    "DOCKER_CERT_PATH",
];

#[derive(Debug)]
struct DaemonLayout {
    root: PathBuf,
    data_root: PathBuf,
    exec_root: PathBuf,
    pidfile: PathBuf,
    socket: PathBuf,
    docker_host: String,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
}

impl DaemonLayout {
    fn create(parent: &Path, owner: &str, iteration: u64) -> Result<Self> {
        if !parent.is_absolute() {
            bail!(
                "isolated Docker daemon requires an absolute scratch path: {}",
                parent.display()
            );
        }
        if owner.is_empty()
            || !owner
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character))
        {
            bail!("isolated Docker daemon owner token is not path-safe");
        }

        let parent = fs::canonicalize(parent).with_context(|| {
            format!("canonicalising isolated Docker parent {}", parent.display())
        })?;
        if !parent.is_dir() {
            bail!(
                "isolated Docker daemon parent is not a directory: {}",
                parent.display()
            );
        }

        let root = parent.join(format!("image-pull-daemon-{owner}-{iteration}"));
        fs::create_dir(&root).with_context(|| {
            format!(
                "creating unique isolated Docker daemon root {}; refusing reuse",
                root.display()
            )
        })?;

        let result = (|| {
            let data_root = root.join("data-root");
            let exec_root = root.join("exec-root");
            fs::create_dir(&data_root).context("creating isolated Docker data-root")?;
            fs::create_dir(&exec_root).context("creating isolated Docker exec-root")?;

            let pidfile = root.join("dockerd.pid");
            let socket = root.join("docker.sock");
            let socket_text = path_text(&socket)?;
            if socket_text.len() > MAX_SOCKET_PATH_BYTES {
                bail!(
                    "isolated Docker Unix socket path is too long ({} bytes; limit {})",
                    socket_text.len(),
                    MAX_SOCKET_PATH_BYTES
                );
            }

            Ok(Self {
                root: root.clone(),
                data_root,
                exec_root,
                pidfile,
                socket,
                docker_host: format!("unix://{socket_text}"),
                stdout_log: root.join("dockerd.stdout.log"),
                stderr_log: root.join("dockerd.stderr.log"),
            })
        })();

        match result {
            Ok(layout) => Ok(layout),
            Err(error) => Err(with_cleanup(error, remove_root(&root))),
        }
    }

    fn command_args(&self) -> Result<Vec<String>> {
        Ok(vec![
            "--data-root".to_owned(),
            path_text(&self.data_root)?,
            "--exec-root".to_owned(),
            path_text(&self.exec_root)?,
            "--pidfile".to_owned(),
            path_text(&self.pidfile)?,
            "--host".to_owned(),
            self.docker_host.clone(),
            // Image pulls do not need a bridge. These flags keep the disposable
            // daemon from changing host firewall or bridge state.
            "--bridge".to_owned(),
            "none".to_owned(),
            "--iptables".to_owned(),
            "false".to_owned(),
        ])
    }
}

/// A running, private Docker daemon and the filesystem that proves its scope.
pub(super) struct IsolatedDockerDaemon {
    layout: Option<DaemonLayout>,
    child: Option<Child>,
}

impl IsolatedDockerDaemon {
    /// Start and prove one isolated daemon before returning it to the caller.
    pub(super) fn start(
        parent: &Path,
        owner: &str,
        iteration: u64,
        runner: &mut Runner,
    ) -> Result<Self> {
        let layout = DaemonLayout::create(parent, owner, iteration)?;
        let spawn_result = (|| {
            let stdout =
                File::create(&layout.stdout_log).context("creating isolated dockerd stdout log")?;
            let stderr =
                File::create(&layout.stderr_log).context("creating isolated dockerd stderr log")?;
            let args = layout.command_args()?;
            let mut command = Command::new("dockerd");
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            command.env_remove("DOCKER_HOST");
            for key in DOCKER_CLIENT_ENV_TO_CLEAR {
                command.env_remove(key);
            }
            command.spawn().context("starting isolated dockerd")
        })();

        let child = match spawn_result {
            Ok(child) => child,
            Err(error) => return Err(with_cleanup(error, remove_root(&layout.root))),
        };
        let mut daemon = Self {
            layout: Some(layout),
            child: Some(child),
        };

        if let Err(error) = daemon.wait_ready(runner) {
            return Err(with_cleanup(error, daemon.shutdown()));
        }
        Ok(daemon)
    }

    /// Run a Docker CLI command against this daemon and no inherited endpoint.
    pub(super) fn run<'runner>(
        &self,
        runner: &'runner mut Runner,
        args: &[&str],
    ) -> io::Result<&'runner Invocation> {
        let layout = self
            .layout
            .as_ref()
            .ok_or_else(|| io::Error::other("isolated Docker daemon is shut down"))?;
        runner.exec_without(
            "docker",
            args,
            None,
            &[("DOCKER_HOST".to_owned(), layout.docker_host.clone())],
            DOCKER_CLIENT_ENV_TO_CLEAR,
        )
    }

    /// Stop the daemon and remove every private path. A failed cleanup leaves
    /// the layout owned so `Drop` can make a second best-effort attempt.
    pub(super) fn shutdown(&mut self) -> Result<()> {
        let child_result = match self.child.as_mut() {
            Some(child) => stop_child(child),
            None => Ok(()),
        };
        if child_result.is_ok() {
            self.child = None;
        }

        let cleanup_result = match (child_result.is_ok(), self.layout.as_ref()) {
            (true, Some(layout)) => remove_root(&layout.root),
            (false, Some(_)) => Err(anyhow::anyhow!(
                "isolated Docker daemon root retained because the process did not stop"
            )),
            (true, None) => Ok(()),
            (false, None) => Err(anyhow::anyhow!(
                "isolated Docker daemon process did not stop and has no retained layout"
            )),
        };
        if child_result.is_ok() && cleanup_result.is_ok() {
            self.layout = None;
        }

        match (child_result, cleanup_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup_error)) => Err(error.context(format!(
                "isolated Docker daemon cleanup also failed: {cleanup_error:#}"
            ))),
        }
    }

    fn wait_ready(&mut self, runner: &mut Runner) -> Result<()> {
        let expected_pid = self
            .child
            .as_ref()
            .context("isolated Docker daemon child was not retained")?
            .id();
        let deadline = Instant::now() + READY_TIMEOUT;

        while Instant::now() < deadline {
            let child_status = self
                .child
                .as_mut()
                .context("isolated Docker daemon child was lost")?
                .try_wait()?;
            if let Some(status) = child_status {
                bail!(
                    "isolated dockerd exited before readiness with code {:?}; stderr log: {}",
                    status.code(),
                    self.stderr_log_path().display()
                );
            }

            let pid_ready = self.pidfile_matches(expected_pid)?;
            let socket_ready = self.socket_ready()?;
            if pid_ready && socket_ready && self.endpoint_proves_data_root(runner)? {
                return Ok(());
            }
            std::thread::sleep(READY_POLL_INTERVAL);
        }

        bail!(
            "isolated dockerd readiness deadline exceeded after {}s; stderr log: {}",
            READY_TIMEOUT.as_secs(),
            self.stderr_log_path().display()
        )
    }

    fn pidfile_matches(&self, expected_pid: u32) -> Result<bool> {
        let pidfile = &self
            .layout
            .as_ref()
            .context("isolated Docker daemon layout was lost")?
            .pidfile;
        let contents = match fs::read_to_string(pidfile) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).context("reading isolated dockerd pidfile"),
        };
        let pid = contents
            .trim()
            .parse::<u32>()
            .context("isolated dockerd pidfile is not a numeric process ID")?;
        if pid != expected_pid {
            bail!("isolated dockerd pidfile identifies process {pid}, expected {expected_pid}");
        }
        Ok(true)
    }

    fn socket_ready(&self) -> Result<bool> {
        let socket = &self
            .layout
            .as_ref()
            .context("isolated Docker daemon layout was lost")?
            .socket;
        match fs::symlink_metadata(socket) {
            Ok(metadata) => {
                #[cfg(unix)]
                if metadata.file_type().is_socket() {
                    return Ok(true);
                }
                bail!(
                    "isolated Docker endpoint exists but is not a Unix socket: {}",
                    socket.display()
                )
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).context("checking isolated Docker socket"),
        }
    }

    fn endpoint_proves_data_root(&self, runner: &mut Runner) -> Result<bool> {
        let expected_root = path_text(
            &self
                .layout
                .as_ref()
                .context("isolated Docker daemon layout was lost")?
                .data_root,
        )?;
        let invocation = match self.run(runner, &["info", "--format", "{{.DockerRootDir}}"]) {
            Ok(invocation) => invocation,
            Err(_) => return Ok(false),
        };
        if !invocation.ok() {
            return Ok(false);
        }
        let reported_root = invocation.stdout.trim();
        if reported_root.is_empty() {
            return Ok(false);
        }
        if reported_root != expected_root {
            bail!(
                "Docker endpoint did not prove isolated data-root: expected {expected_root:?}, got {reported_root:?}"
            );
        }
        Ok(true)
    }

    fn stderr_log_path(&self) -> &Path {
        &self
            .layout
            .as_ref()
            .expect("layout is retained while the daemon is running")
            .stderr_log
    }
}

impl Drop for IsolatedDockerDaemon {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = stop_child(child);
        }
        if let Some(layout) = self.layout.as_ref() {
            let _ = remove_root(&layout.root);
        }
    }
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str().map(str::to_owned).with_context(|| {
        format!(
            "isolated Docker path is not valid UTF-8: {}",
            path.display()
        )
    })
}

fn remove_root(root: &Path) -> Result<()> {
    match fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("removing isolated Docker daemon root {}", root.display())),
    }
}

fn stop_child(child: &mut Child) -> Result<()> {
    let mut failures = Vec::new();
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => {
            if let Err(error) = child.kill()
                && error.kind() != io::ErrorKind::NotFound
            {
                failures.push(format!("killing dockerd: {error}"));
            }
            if let Err(error) = child.wait()
                && error.kind() != io::ErrorKind::NotFound
            {
                failures.push(format!("waiting for dockerd: {error}"));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!(
            "isolated Docker daemon stop failed: {}",
            failures.join("; ")
        )
    }
}

fn with_cleanup(primary: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup_error) => primary.context(format!(
            "isolated Docker cleanup also failed: {cleanup_error:#}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_parent(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let base = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        base.join(format!("vb-ip-{label}-{}-{nonce:x}", std::process::id()))
    }

    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn layout_proves_unique_roots_and_exact_endpoint_arguments() {
        let parent = test_parent("layout");
        fs::create_dir(&parent).expect("create test parent");
        let first = DaemonLayout::create(&parent, "owner-1", 1).expect("first layout");
        let second = DaemonLayout::create(&parent, "owner-1", 2).expect("second layout");

        assert_ne!(first.root, second.root);
        assert_ne!(first.data_root, first.exec_root);
        assert!(first.data_root.starts_with(&first.root));
        assert!(first.exec_root.starts_with(&first.root));
        assert!(first.pidfile.starts_with(&first.root));
        assert!(first.socket.starts_with(&first.root));

        let args = first.command_args().expect("daemon arguments");
        assert!(args.windows(2).any(|pair| pair
            == [
                "--data-root",
                first.data_root.to_str().expect("UTF-8 data root")
            ]));
        assert!(args.windows(2).any(|pair| pair
            == [
                "--exec-root",
                first.exec_root.to_str().expect("UTF-8 exec root")
            ]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--pidfile", first.pidfile.to_str().expect("UTF-8 pidfile")]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--host", first.docker_host.as_str()]));

        remove_root(&first.root).expect("remove first layout");
        remove_root(&second.root).expect("remove second layout");
        remove_root(&parent).expect("remove test parent");
    }

    #[test]
    fn layout_refuses_reuse_of_an_existing_iteration_root() {
        let parent = test_parent("collision");
        fs::create_dir(&parent).expect("create test parent");
        let first = DaemonLayout::create(&parent, "owner-1", 1).expect("first layout");
        let error = DaemonLayout::create(&parent, "owner-1", 1).expect_err("collision");
        assert!(error.to_string().contains("refusing reuse"), "{error}");
        remove_root(&first.root).expect("remove first layout");
        remove_root(&parent).expect("remove test parent");
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_stops_the_child_before_removing_the_private_root() {
        let parent = test_parent("shutdown");
        fs::create_dir(&parent).expect("create test parent");
        let layout = DaemonLayout::create(&parent, "owner-1", 1).expect("layout");
        let child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("spawn test child");
        let root = layout.root.clone();
        let mut daemon = IsolatedDockerDaemon {
            layout: Some(layout),
            child: Some(child),
        };

        daemon.shutdown().expect("shutdown");
        assert!(!root.exists());
        remove_root(&parent).expect("remove test parent");
    }
}

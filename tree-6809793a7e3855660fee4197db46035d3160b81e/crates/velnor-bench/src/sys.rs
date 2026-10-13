//! Host process execution and kernel resource accounting.
//!
//! Every host command the harness runs goes through [`Runner`], so the process
//! count, the per-invocation latency, and the Docker invocation census are
//! byproducts of execution rather than something a scenario has to remember to
//! report. This mirrors the role `CommandRunner`
//! (`crates/velnor-runner/src/executor.rs`) plays inside the runner itself.

use std::{
    io,
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io::Read;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::process::ExitStatusExt;

/// Outcome of one host process invocation.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub wall: Duration,
}

impl Invocation {
    /// True when the process exited zero.
    #[must_use]
    pub const fn ok(&self) -> bool {
        self.code == 0
    }

    /// Trimmed stdout, or the reason the invocation cannot be trusted.
    ///
    /// # Errors
    /// Non-zero exit or empty output.
    pub fn text(&self) -> Result<String, String> {
        if !self.ok() {
            return Err(format!(
                "`{} {}` exited {}: {}",
                self.program,
                self.args.join(" "),
                self.code,
                self.stderr.trim()
            ));
        }
        let text = self.stdout.trim().to_owned();
        if text.is_empty() {
            return Err(format!(
                "`{} {}` produced no output",
                self.program,
                self.args.join(" ")
            ));
        }
        Ok(text)
    }
}

struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    usage: Rusage,
}

/// Counting process runner. Records every spawn so a scenario's process count
/// and Docker invocation count are measured, never estimated.
#[derive(Debug, Default)]
pub struct Runner {
    invocations: Vec<Invocation>,
    usage: Rusage,
}

impl Runner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Run a command to completion, capturing both streams.
    ///
    /// # Errors
    /// The process could not be spawned or waited on.
    pub fn run<S: AsRef<str>>(&mut self, program: &str, args: &[S]) -> io::Result<&Invocation> {
        self.exec(program, args, None, &[])
    }

    /// Run a command in a working directory with extra environment.
    ///
    /// # Errors
    /// The process could not be spawned or waited on.
    pub fn exec<S: AsRef<str>>(
        &mut self,
        program: &str,
        args: &[S],
        dir: Option<&std::path::Path>,
        env: &[(String, String)],
    ) -> io::Result<&Invocation> {
        self.exec_inner(program, args, dir, env, &[])
    }

    /// Run a command while removing selected inherited environment variables.
    ///
    /// The removals happen after `env` is applied, so the caller cannot
    /// accidentally reintroduce a variable it meant to clear. This is useful
    /// for measurements whose workload must not inherit compiler or target
    /// overrides from the shell that launched the harness.
    ///
    /// # Errors
    /// The process could not be spawned or waited on.
    pub fn exec_without<S: AsRef<str>>(
        &mut self,
        program: &str,
        args: &[S],
        dir: Option<&std::path::Path>,
        env: &[(String, String)],
        unset: &[&str],
    ) -> io::Result<&Invocation> {
        self.exec_inner(program, args, dir, env, unset)
    }

    fn exec_inner<S: AsRef<str>>(
        &mut self,
        program: &str,
        args: &[S],
        dir: Option<&std::path::Path>,
        env: &[(String, String)],
        unset: &[&str],
    ) -> io::Result<&Invocation> {
        let owned: Vec<String> = args.iter().map(|arg| arg.as_ref().to_owned()).collect();
        let mut command = Command::new(program);
        command.args(&owned).stdin(Stdio::null());
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        for key in unset {
            command.env_remove(key);
        }
        let started = Instant::now();
        let output = command_output(&mut command)?;
        let wall = started.elapsed();
        self.usage = self.usage.accumulate(output.usage);
        self.invocations.push(Invocation {
            program: program.to_owned(),
            args: owned,
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            wall,
        });
        Ok(self.invocations.last().expect("just pushed"))
    }

    /// Run and return trimmed stdout, collapsing spawn and exit failures into a
    /// single reason string suitable for a [`crate::fact::Fact`].
    pub fn capture<S: AsRef<str>>(&mut self, program: &str, args: &[S]) -> Result<String, String> {
        match self.run(program, args) {
            Ok(invocation) => invocation.text(),
            Err(error) => Err(format!("`{program}` could not be executed: {error}")),
        }
    }

    /// Total host processes spawned so far.
    #[must_use]
    pub fn process_count(&self) -> usize {
        self.invocations.len()
    }

    /// Processes spawned for a given program name.
    #[must_use]
    pub fn count_of(&self, program: &str) -> usize {
        self.invocations
            .iter()
            .filter(|invocation| invocation.program == program)
            .count()
    }

    /// Every invocation recorded so far.
    #[must_use]
    pub fn invocations(&self) -> &[Invocation] {
        &self.invocations
    }

    /// Resource usage for commands recorded since the last reset.
    #[must_use]
    pub(crate) fn rusage(&self) -> Rusage {
        self.usage
    }

    /// Merge invocations collected by a completed worker.
    pub(crate) fn merge(&mut self, worker: Self) {
        self.usage = self.usage.accumulate(worker.usage);
        self.invocations.extend(worker.invocations);
    }

    /// Forget the recorded invocations, keeping the runner for the next sample.
    pub fn reset(&mut self) {
        self.invocations.clear();
        self.usage = Rusage::default();
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn command_output(command: &mut Command) -> io::Result<CommandOutput> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("child stdout pipe was not captured"));
    };
    let Some(mut stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other("child stderr pipe was not captured"));
    };

    let stdout_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).map(|_| output)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).map(|_| output)
    });

    let (status, usage) = match wait4_child(&mut child) {
        Ok(result) => result,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(error);
        }
    };
    let stdout = join_output(stdout_reader)?;
    let stderr = join_output(stderr_reader)?;

    Ok(CommandOutput {
        status,
        stdout,
        stderr,
        usage,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait4_child(child: &mut std::process::Child) -> io::Result<(ExitStatus, Rusage)> {
    let pid = libc::pid_t::try_from(child.id())
        .map_err(|_| io::Error::other("child process id does not fit the platform pid type"))?;
    let mut status = 0;
    // SAFETY: `libc::rusage` contains only integer/time fields, so an
    // all-zero bit pattern is valid C struct storage.
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    loop {
        // SAFETY: `pid` is the live child returned by `Command::spawn`; the
        // status and usage pointers are valid writable storage for wait4.
        let waited = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
        if waited == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if waited != pid {
            return Err(io::Error::other("wait4 returned an unexpected child"));
        }
        return Ok((ExitStatus::from_raw(status), Rusage::from_raw(usage)));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn join_output(reader: std::thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("child output reader panicked"))?
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn command_output(command: &mut Command) -> io::Result<CommandOutput> {
    let output = command.output()?;
    Ok(CommandOutput {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
        usage: Rusage::default(),
    })
}

/// Kernel resource accounting for one interval of child processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rusage {
    pub user_us: u64,
    pub system_us: u64,
    pub max_rss_bytes: u64,
    pub block_input_ops: u64,
    pub block_output_ops: u64,
}

impl Rusage {
    fn accumulate(self, sample: Self) -> Self {
        Self {
            user_us: self.user_us.saturating_add(sample.user_us),
            system_us: self.system_us.saturating_add(sample.system_us),
            max_rss_bytes: self.max_rss_bytes.max(sample.max_rss_bytes),
            block_input_ops: self.block_input_ops.saturating_add(sample.block_input_ops),
            block_output_ops: self
                .block_output_ops
                .saturating_add(sample.block_output_ops),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn from_raw(usage: libc::rusage) -> Self {
        Self {
            user_us: timeval_us(usage.ru_utime),
            system_us: timeval_us(usage.ru_stime),
            max_rss_bytes: max_rss_bytes(usage.ru_maxrss),
            block_input_ops: u64::try_from(usage.ru_inblock).unwrap_or(0),
            block_output_ops: u64::try_from(usage.ru_oublock).unwrap_or(0),
        }
    }
}

fn timeval_us(value: libc::timeval) -> u64 {
    let seconds = u64::try_from(value.tv_sec).unwrap_or(0);
    let micros = u64::try_from(value.tv_usec).unwrap_or(0);
    seconds.saturating_mul(1_000_000).saturating_add(micros)
}

/// `ru_maxrss` is kilobytes on Linux and bytes on the Darwin/BSD family.
fn max_rss_bytes(raw: libc::c_long) -> u64 {
    let value = u64::try_from(raw).unwrap_or(0);
    if cfg!(target_os = "linux") {
        value.saturating_mul(1024)
    } else {
        value
    }
}

/// Recursive size in bytes of a directory tree, following no symlinks.
#[must_use]
pub fn tree_bytes(root: &std::path::Path) -> u64 {
    let Ok(root_metadata) = std::fs::symlink_metadata(root) else {
        return 0;
    };
    if root_metadata.file_type().is_symlink() {
        return 0;
    }
    if !root_metadata.is_dir() {
        return root_metadata.len();
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut total = 0_u64;
    for entry in entries.flatten() {
        // `DirEntry::metadata()` follows links. Use lstat semantics at every
        // level so a job-controlled link cannot make benchmark accounting walk
        // outside the measured root or recurse through a link cycle.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            total = total.saturating_add(tree_bytes(&entry.path()));
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_counts_every_spawn_and_captures_output() {
        let mut runner = Runner::new();
        let text = runner
            .capture("/bin/echo", &["velnor"])
            .expect("echo must succeed");
        assert_eq!(text, "velnor");
        assert_eq!(runner.process_count(), 1);
        assert_eq!(runner.count_of("/bin/echo"), 1);
        assert_eq!(runner.count_of("docker"), 0);

        let _ = runner.capture("/bin/echo", &["again"]);
        assert_eq!(runner.process_count(), 2);
        runner.reset();
        assert_eq!(runner.process_count(), 0);
    }

    #[test]
    fn runner_merges_completed_worker_invocations() {
        let mut parent = Runner::new();
        let mut worker = Runner::new();
        let _ = worker.capture("/bin/echo", &["worker"]);

        parent.merge(worker);

        assert_eq!(parent.process_count(), 1);
        assert_eq!(parent.count_of("/bin/echo"), 1);
        assert_eq!(parent.invocations()[0].stdout.trim(), "worker");
    }

    #[test]
    fn exec_honours_working_directory_and_environment() {
        let mut runner = Runner::new();
        let dir = std::env::temp_dir();
        let invocation = runner
            .exec(
                "/bin/sh",
                &["-c", "pwd; printf %s \"$VELNOR_BENCH_PROBE\""],
                Some(&dir),
                &[("VELNOR_BENCH_PROBE".to_owned(), "set".to_owned())],
            )
            .expect("exec");
        assert!(invocation.ok());
        assert!(invocation.stdout.ends_with("set"), "{}", invocation.stdout);
        assert_eq!(runner.process_count(), 1);
    }

    #[test]
    fn exec_without_removes_a_variable_after_environment_is_applied() {
        let mut runner = Runner::new();
        let invocation = runner
            .exec_without(
                "/bin/sh",
                &["-c", "test -z \"${VELNOR_BENCH_UNSET+x}\""],
                None,
                &[("VELNOR_BENCH_UNSET".to_owned(), "present".to_owned())],
                &["VELNOR_BENCH_UNSET"],
            )
            .expect("exec");
        assert!(invocation.ok(), "{}", invocation.stderr);
        assert_eq!(runner.process_count(), 1);
    }

    #[test]
    fn a_missing_program_becomes_a_reason_not_a_panic() {
        let mut runner = Runner::new();
        let reason = runner
            .capture("/nonexistent/velnor-bench-probe", &["--version"])
            .expect_err("missing program");
        assert!(reason.contains("could not be executed"), "{reason}");
        assert_eq!(runner.process_count(), 0);
    }

    #[test]
    fn a_nonzero_exit_becomes_a_reason() {
        let mut runner = Runner::new();
        let reason = runner
            .capture("/bin/sh", &["-c", "echo boom >&2; exit 3"])
            .expect_err("failing program");
        assert!(reason.contains("exited 3"), "{reason}");
        assert!(reason.contains("boom"), "{reason}");
    }

    #[test]
    fn rusage_aggregates_and_resets_rss_high_water() {
        let high = Rusage {
            user_us: 11,
            system_us: 13,
            max_rss_bytes: 64 * 1024 * 1024,
            block_input_ops: 17,
            block_output_ops: 19,
        };
        let low = Rusage {
            user_us: 2,
            system_us: 3,
            max_rss_bytes: 1024,
            block_input_ops: 5,
            block_output_ops: 7,
        };

        let mut runner = Runner::new();
        runner.usage = runner.usage.accumulate(high);
        runner.usage = runner.usage.accumulate(low);
        assert_eq!(
            runner.rusage(),
            Rusage {
                user_us: 13,
                system_us: 16,
                max_rss_bytes: 64 * 1024 * 1024,
                block_input_ops: 22,
                block_output_ops: 26,
            }
        );

        runner.reset();
        runner.usage = runner.usage.accumulate(low);
        assert_eq!(runner.rusage().max_rss_bytes, low.max_rss_bytes);
    }

    #[test]
    fn wait_time_rusage_is_recorded_without_cumulative_rss() {
        let mut runner = Runner::new();
        let _ = runner.capture(
            "/bin/sh",
            &["-c", "i=0; while [ $i -lt 40000 ]; do i=$((i+1)); done"],
        );
        let usage = runner.rusage();
        assert!(
            usage.user_us + usage.system_us > 0,
            "expected measurable child cpu time"
        );
        assert!(usage.max_rss_bytes > 0);
    }

    #[test]
    fn tree_bytes_sums_a_directory() {
        let dir = std::env::temp_dir().join(format!("velnor-bench-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("nested")).expect("create");
        std::fs::write(dir.join("a"), b"0123456789").expect("write");
        std::fs::write(dir.join("nested").join("b"), b"01234").expect("write");
        assert_eq!(tree_bytes(&dir), 15);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn tree_bytes_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("velnor-bench-links-{suffix}"));
        let outside = root.with_extension("outside");
        let root_link = root.with_extension("link");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_file(&root_link);
        std::fs::create_dir_all(root.join("nested")).expect("create root");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::write(root.join("inside"), b"12345").expect("write inside");
        std::fs::write(outside.join("outside-file"), b"this must not count")
            .expect("write outside");
        symlink(&outside, root.join("linked-directory")).expect("link directory");
        symlink(outside.join("outside-file"), root.join("linked-file")).expect("link file");
        symlink(&root, &root_link).expect("link root");

        assert_eq!(tree_bytes(&root), 5);
        assert_eq!(tree_bytes(&root_link), 0);

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_file(root_link);
    }
}

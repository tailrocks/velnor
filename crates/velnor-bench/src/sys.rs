//! Host process execution and kernel resource accounting.
//!
//! Every host command the harness runs goes through [`Runner`], so the process
//! count, the per-invocation latency, and the Docker invocation census are
//! byproducts of execution rather than something a scenario has to remember to
//! report. This mirrors the role `CommandRunner`
//! (`crates/velnor-runner/src/executor.rs`) plays inside the runner itself.

use std::{
    io,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

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

/// Counting process runner. Records every spawn so a scenario's process count
/// and Docker invocation count are measured, never estimated.
#[derive(Debug, Default)]
pub struct Runner {
    invocations: Vec<Invocation>,
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
        let owned: Vec<String> = args.iter().map(|arg| arg.as_ref().to_owned()).collect();
        let mut command = Command::new(program);
        command.args(&owned).stdin(Stdio::null());
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let started = Instant::now();
        let output = command.output()?;
        let wall = started.elapsed();
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

    /// Forget the recorded invocations, keeping the runner for the next sample.
    pub fn reset(&mut self) {
        self.invocations.clear();
    }
}

/// Kernel resource accounting for child processes, read from `getrusage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rusage {
    pub user_us: u64,
    pub system_us: u64,
    pub max_rss_bytes: u64,
    pub block_input_ops: u64,
    pub block_output_ops: u64,
}

impl Rusage {
    /// Cumulative usage of all reaped children of this process.
    #[must_use]
    pub fn children() -> Self {
        // SAFETY: `getrusage` writes a fully-initialised `rusage` into the
        // out-parameter and reads nothing from it.
        let usage = unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            if libc::getrusage(libc::RUSAGE_CHILDREN, &raw mut usage) != 0 {
                return Self::default();
            }
            usage
        };
        Self {
            user_us: timeval_us(usage.ru_utime),
            system_us: timeval_us(usage.ru_stime),
            max_rss_bytes: max_rss_bytes(usage.ru_maxrss),
            block_input_ops: u64::try_from(usage.ru_inblock).unwrap_or(0),
            block_output_ops: u64::try_from(usage.ru_oublock).unwrap_or(0),
        }
    }

    /// Usage accumulated between two observations.
    #[must_use]
    pub fn since(&self, earlier: Self) -> Self {
        Self {
            user_us: self.user_us.saturating_sub(earlier.user_us),
            system_us: self.system_us.saturating_sub(earlier.system_us),
            // Peak RSS is a high-water mark, not a counter: the larger of the
            // two is the only meaningful value for the interval.
            max_rss_bytes: self.max_rss_bytes.max(earlier.max_rss_bytes),
            block_input_ops: self.block_input_ops.saturating_sub(earlier.block_input_ops),
            block_output_ops: self
                .block_output_ops
                .saturating_sub(earlier.block_output_ops),
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
    fn rusage_accumulates_across_children() {
        let before = Rusage::children();
        let mut runner = Runner::new();
        let _ = runner.capture(
            "/bin/sh",
            &["-c", "i=0; while [ $i -lt 40000 ]; do i=$((i+1)); done"],
        );
        let delta = Rusage::children().since(before);
        assert!(
            delta.user_us + delta.system_us > 0,
            "expected measurable child cpu time"
        );
        assert!(delta.max_rss_bytes > 0);
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

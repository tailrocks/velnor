//! Child commands whose working directory must stay bound to a captured root.
//!
//! `std::process::Command::current_dir` accepts only a path. A pathname can be
//! rebound after validation, so this helper changes the child directory from
//! the still-open root descriptor immediately before `exec`.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Output};

use super::SafeRoot;

pub(crate) const OUTPUT_ROOT_FD_ENV: &str = "VELNOR_INTERNAL_OUTPUT_ROOT_FD";
pub(crate) const SOURCE_ROOT_FD_ENV: &str = "VELNOR_INTERNAL_SOURCE_ROOT_FD";
pub(crate) const SOURCE_ROOT_PATH_ENV: &str = "VELNOR_INTERNAL_SOURCE_ROOT_PATH";

/// Keep a validated renderer handoff descriptor in this process but prevent
/// any later child from inheriting it. The caller must first confirm that the
/// descriptor is open and remains owned by this process for the duration of
/// this call.
pub(crate) fn mark_close_on_exec(raw_fd: std::os::fd::RawFd) -> io::Result<()> {
    // SAFETY: callers pass the still-open descriptor parsed from the renderer
    // handoff only after `SafeRoot::from_inherited_directory_fd` validated and
    // duplicated it; no code closes it concurrently before this call returns.
    let descriptor = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
    let mut flags = rustix::io::fcntl_getfd(descriptor).map_err(io::Error::from)?;
    flags.insert(rustix::io::FdFlags::CLOEXEC);
    rustix::io::fcntl_setfd(descriptor, flags).map_err(io::Error::from)
}

/// Spawn a streaming child with `root` as its working directory. Unlike
/// `output`, this preserves inherited stdio so callers can monitor output and
/// terminate a stalled child without buffering its complete output.
pub(crate) fn spawn(root: &SafeRoot, mut command: Command) -> io::Result<Child> {
    let directory = root
        .clone_directory_handle()
        .map_err(|error| io::Error::other(error.to_string()))?;
    remove_git_environment(&mut command);
    command
        .env_remove(OUTPUT_ROOT_FD_ENV)
        .env_remove(SOURCE_ROOT_FD_ENV)
        .env_remove(SOURCE_ROOT_PATH_ENV)
        // The display path may now name a replacement directory. A relative
        // value resolves through the descriptor-bound current directory.
        .env("PWD", ".")
        .env("GITHUB_WORKSPACE", ".")
        .current_dir("/");
    if Path::new(command.get_program()).file_name() == Some(OsStr::new("git")) {
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_LFS_SKIP_SMUDGE", "1");
    }

    // SAFETY: the child hook only performs `fchdir`, which is async-signal-
    // safe. The cloned descriptor is owned by the closure through spawn.
    unsafe {
        command.pre_exec(move || {
            rustix::process::fchdir(&directory).map_err(io::Error::from)?;
            Ok(())
        });
    }

    command.spawn()
}

/// Remove Git's repository-redirection variables without changing the
/// caller's other inherited or explicitly configured environment. Git reads
/// `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, and related variables before
/// honoring a descriptor-bound current directory.
fn remove_git_environment(command: &mut Command) {
    let inherited_names = std::env::vars_os().map(|(name, _)| name);
    remove_git_environment_with_names(command, inherited_names);
}

fn remove_git_environment_with_names(
    command: &mut Command,
    inherited_names: impl IntoIterator<Item = OsString>,
) {
    let mut names = inherited_names
        .into_iter()
        .filter(|name| is_git_environment_variable(name))
        .collect::<Vec<_>>();
    names.extend(
        command
            .get_envs()
            .filter(|(name, _)| is_git_environment_variable(name))
            .map(|(name, _)| name.to_os_string()),
    );
    names.sort();
    names.dedup();
    for name in names {
        command.env_remove(name);
    }
}

/// Run a command with `root` as its working directory, using the captured
/// directory handle rather than reopening its display path.
pub(crate) fn output<P, I, A>(root: &SafeRoot, program: P, args: I) -> io::Result<Output>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    output_with_environment(
        root,
        program,
        args,
        std::iter::empty::<(OsString, OsString)>(),
    )
}

fn output_with_environment<P, I, A, E>(
    root: &SafeRoot,
    program: P,
    args: I,
    extra_environment: E,
) -> io::Result<Output>
where
    P: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
    E: IntoIterator<Item = (OsString, OsString)>,
{
    let is_git = program.as_ref() == OsStr::new("git");
    let directory = root.directory.try_clone()?;
    let environment = std::env::vars_os()
        .chain(extra_environment)
        .filter(|(name, _)| {
            !is_git_environment_variable(name) && !is_internal_root_environment_variable(name)
        });
    let mut command = Command::new(program.as_ref());
    command
        .env_clear()
        .envs(environment)
        .env("PWD", ".")
        .env("GITHUB_WORKSPACE", ".")
        .current_dir("/")
        .args(args);
    if is_git {
        // Snapshot materialization runs trusted Git against untrusted tree
        // data. Disable system/global config and prompting so a host filter,
        // hook, URL rewrite, or credential helper cannot execute while the
        // checkout is copied into its isolated sandbox.
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_LFS_SKIP_SMUDGE", "1");
    }

    // SAFETY: the child hook runs between fork and exec and performs only
    // `fchdir`, which is async-signal-safe. The cloned directory descriptor is
    // owned by this closure and stays open through the spawn operation. The
    // hook captures no borrowed state and does not allocate or acquire locks.
    unsafe {
        command.pre_exec(move || {
            rustix::process::fchdir(&directory).map_err(io::Error::from)?;
            Ok(())
        });
    }

    command.output()
}

/// Run an opened executable with a captured root as its working directory.
/// The descriptor supplies `fchdir` in the child, so proof commands such as
/// `--closure` inspect the same checkout captured by the caller.
pub(crate) fn output_executable_in_safe_root<I, A>(
    root: &SafeRoot,
    executable: &File,
    args: I,
) -> io::Result<Output>
where
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    output_executable_inner(root, executable, args, None)
}

/// Run an already-open executable from a captured checkout root and hand the
/// captured checkout and output roots to the child through
/// [`SOURCE_ROOT_FD_ENV`], [`SOURCE_ROOT_PATH_ENV`], and
/// [`OUTPUT_ROOT_FD_ENV`]. All inherited descriptors remain close-on-exec in
/// the parent; the child clears that flag only in `pre_exec`.
pub(crate) fn output_with_executable_and_output_root<I, A>(
    source_root: &SafeRoot,
    executable: &File,
    output_root: &SafeRoot,
    args: I,
) -> io::Result<Output>
where
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    output_executable_inner(source_root, executable, args, Some(output_root))
}

fn output_executable_inner<I, A>(
    source_root: &SafeRoot,
    executable: &File,
    args: I,
    output_root: Option<&SafeRoot>,
) -> io::Result<Output>
where
    I: IntoIterator<Item = A>,
    A: AsRef<OsStr>,
{
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source_root, executable, args, output_root);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "descriptor-bound renderer execution is unavailable on this platform; refusing path-based execution",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let executable = executable.try_clone()?;
        ensure_close_on_exec(&executable)?;
        let program = OsString::from(format!("/proc/self/fd/{}", executable.as_raw_fd()));

        let source_directory = clone_directory_handle(source_root)?;
        let source_path = source_root.command_directory();
        let output_directory = output_root.map(clone_directory_handle).transpose()?;
        ensure_close_on_exec(&source_directory)?;
        if let Some(directory) = &output_directory {
            ensure_close_on_exec(directory)?;
        }

        let environment = std::env::vars_os().filter(|(name, _)| {
            !is_git_environment_variable(name) && !is_internal_root_environment_variable(name)
        });
        let mut command = Command::new(program);
        command
            .env_clear()
            .envs(environment)
            .args(args)
            .current_dir("/")
            .env("PWD", ".")
            .env("GITHUB_WORKSPACE", ".");
        if let Some(directory) = &output_directory {
            command.env(OUTPUT_ROOT_FD_ENV, directory.as_raw_fd().to_string());
        }
        command
            .env(SOURCE_ROOT_FD_ENV, source_directory.as_raw_fd().to_string())
            .env(SOURCE_ROOT_PATH_ENV, source_path.as_os_str());

        // SAFETY: the hook only performs fchdir and fcntl, which are
        // async-signal-safe. All descriptors and environment strings are
        // prepared in the parent. The executable and output-root descriptors
        // stay CLOEXEC in the parent; the child clears CLOEXEC immediately
        // before exec so `/proc/self/fd/N` and the renderer's inherited root
        // remain bound to these open objects.
        unsafe {
            command.pre_exec(move || {
                make_inheritable(&executable)?;
                make_inheritable(&source_directory)?;
                if let Some(directory) = &output_directory {
                    make_inheritable(directory)?;
                }
                rustix::process::fchdir(&source_directory).map_err(io::Error::from)?;
                Ok(())
            });
        }

        command.output()
    }
}

#[cfg(target_os = "linux")]
fn clone_directory_handle(root: &SafeRoot) -> io::Result<File> {
    root.clone_directory_handle()
        .map_err(|error| io::Error::other(error.to_string()))
}

#[cfg(target_os = "linux")]
fn ensure_close_on_exec(file: &File) -> io::Result<()> {
    let mut flags = rustix::io::fcntl_getfd(file).map_err(io::Error::from)?;
    flags.insert(rustix::io::FdFlags::CLOEXEC);
    rustix::io::fcntl_setfd(file, flags).map_err(io::Error::from)
}

#[cfg(target_os = "linux")]
fn make_inheritable(file: &File) -> io::Result<()> {
    let mut flags = rustix::io::fcntl_getfd(file).map_err(io::Error::from)?;
    flags.remove(rustix::io::FdFlags::CLOEXEC);
    rustix::io::fcntl_setfd(file, flags).map_err(io::Error::from)
}

fn is_git_environment_variable(name: &OsStr) -> bool {
    name.as_bytes().starts_with(b"GIT_")
}

fn is_internal_root_environment_variable(name: &OsStr) -> bool {
    name == OsStr::new(OUTPUT_ROOT_FD_ENV)
        || name == OsStr::new(SOURCE_ROOT_FD_ENV)
        || name == OsStr::new(SOURCE_ROOT_PATH_ENV)
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use super::*;
    use crate::s2::safe_fs::SafeRoot;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "velnor-pinned-command-{}-{}",
                std::process::id(),
                super::super::unique_suffix()
            ));
            fs::create_dir(&path).expect("create pinned-command test directory");
            Self(fs::canonicalize(&path).expect("canonicalize test directory"))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn output_ignores_git_repository_redirection_environment() {
        let source = TestDirectory::new();
        let external = TestDirectory::new();
        initialize_git(&source.0, "source.txt");
        initialize_git(&external.0, "external.txt");
        let safe_root = SafeRoot::open(&source.0).expect("open captured source root");

        let environment = [
            (
                OsString::from("GIT_DIR"),
                external.0.join(".git").into_os_string(),
            ),
            (
                OsString::from("GIT_WORK_TREE"),
                external.0.clone().into_os_string(),
            ),
            (
                OsString::from("GIT_INDEX_FILE"),
                external.0.join(".git/index").into_os_string(),
            ),
        ];

        let output = output_with_environment(&safe_root, "git", ["ls-files", "-z"], environment)
            .expect("run Git from the captured source root");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"source.txt\0");
    }

    #[test]
    fn spawn_ignores_explicit_git_repository_redirection_environment() {
        let source = TestDirectory::new();
        let external = TestDirectory::new();
        initialize_git(&source.0, "source.txt");
        initialize_git(&external.0, "external.txt");
        let safe_root = SafeRoot::open(&source.0).expect("open captured source root");
        let captured_output = output(&safe_root, "git", ["ls-files", "-z"])
            .expect("read the fixture index through captured output");
        assert_eq!(captured_output.stdout, b"source.txt\0");

        let mut command = Command::new("git");
        command
            .args(["ls-files", "-z"])
            .env("GIT_DIR", external.0.join(".git"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut root_probe = Command::new("git");
        root_probe
            .args(["rev-parse", "--show-toplevel"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let repository = spawn(&safe_root, root_probe)
            .expect("spawn Git root probe")
            .wait_with_output()
            .expect("wait for Git root probe");
        assert!(
            repository.status.success(),
            "Git root probe failed: {}",
            String::from_utf8_lossy(&repository.stderr)
        );
        assert_eq!(
            repository.stdout.trim_ascii(),
            source.0.as_os_str().as_encoded_bytes()
        );
        let output = spawn(&safe_root, command)
            .expect("spawn Git from captured source root")
            .wait_with_output()
            .expect("wait for Git");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"source.txt\0");
    }

    #[test]
    fn spawn_environment_filter_preserves_non_git_overrides_and_removals() {
        let mut command = Command::new("sh");
        command
            .env("GIT_DIR", "/external/.git")
            .env("VELNOR_TEST_KEEP", "explicit")
            .env_remove("VELNOR_TEST_REMOVE");

        remove_git_environment_with_names(
            &mut command,
            [OsString::from("GIT_WORK_TREE"), OsString::from("PATH")],
        );

        let environment = command
            .get_envs()
            .map(|(name, value)| (name.to_os_string(), value.map(OsStr::to_os_string)))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            environment.get(OsStr::new("GIT_DIR")),
            Some(&None),
            "explicit GIT_DIR is removed"
        );
        assert_eq!(
            environment.get(OsStr::new("GIT_WORK_TREE")),
            Some(&None),
            "inherited GIT_WORK_TREE is removed"
        );
        assert_eq!(
            environment.get(OsStr::new("VELNOR_TEST_KEEP")),
            Some(&Some(OsString::from("explicit"))),
            "non-Git explicit values are preserved"
        );
        assert_eq!(
            environment.get(OsStr::new("VELNOR_TEST_REMOVE")),
            Some(&None),
            "non-Git removals are preserved"
        );
        assert!(
            !environment.contains_key(OsStr::new("PATH")),
            "non-Git inherited names are not removed"
        );
    }

    #[test]
    fn generic_children_do_not_inherit_renderer_root_handoff_environment() {
        let directory = TestDirectory::new();
        let safe_root = SafeRoot::open(&directory.0).expect("open command root");
        let environment = [
            (OsString::from(OUTPUT_ROOT_FD_ENV), OsString::from("41")),
            (OsString::from(SOURCE_ROOT_FD_ENV), OsString::from("42")),
            (
                OsString::from(SOURCE_ROOT_PATH_ENV),
                OsString::from("/captured/source"),
            ),
        ];
        let script = format!(
            "test -z \"${{{OUTPUT_ROOT_FD_ENV}+x}}\" && test -z \"${{{SOURCE_ROOT_FD_ENV}+x}}\" && test -z \"${{{SOURCE_ROOT_PATH_ENV}+x}}\""
        );

        let output = output_with_environment(
            &safe_root,
            "sh",
            ["-c", script.as_str()],
            environment.clone(),
        )
        .expect("run generic output child");
        assert!(
            output.status.success(),
            "root handoff leaked to output child"
        );

        let mut command = Command::new("sh");
        command.args(["-c", script.as_str()]);
        command.envs(environment);
        let output = spawn(&safe_root, command)
            .expect("spawn generic child")
            .wait_with_output()
            .expect("wait for generic child");
        assert!(
            output.status.success(),
            "root handoff leaked to spawned child"
        );
    }

    #[test]
    fn spawn_uses_captured_root_after_display_path_replacement() {
        let fixture = TestDirectory::new();
        let source = fixture.0.join("source");
        let moved_source = fixture.0.join("source-captured");
        fs::create_dir(&source).expect("create source root A");
        fs::write(source.join("tree.txt"), "captured A\n").expect("write A marker");
        let safe_root = SafeRoot::open(&source).expect("capture source root A");

        fs::rename(&source, &moved_source).expect("move captured root A");
        fs::create_dir(&source).expect("place replacement root B");
        fs::write(source.join("tree.txt"), "replacement B\n").expect("write B marker");

        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "test \"$GITHUB_WORKSPACE\" = . && test \"$(cat tree.txt)\" = 'captured A' && touch \"$GITHUB_WORKSPACE/marker\" && touch \"$PWD/pwd-marker\"",
            ])
            .env("GITHUB_WORKSPACE", &source)
            .env("PWD", &source);
        let status = spawn(&safe_root, command)
            .expect("spawn from captured root A")
            .wait()
            .expect("wait for captured-root child");
        assert!(status.success(), "child did not use captured root A");
        assert!(moved_source.join("marker").is_file());
        assert!(moved_source.join("pwd-marker").is_file());
        assert!(!source.join("marker").exists());
        assert!(!source.join("pwd-marker").exists());
    }

    fn initialize_git(root: &Path, file: &str) {
        let status = clean_git_command(root, ["init", "--quiet"])
            .status()
            .expect("initialize test repository");
        assert!(status.success(), "git init failed");
        fs::write(root.join(file), "tracked\n").expect("write test file");
        let status = clean_git_command(root, ["add", file])
            .status()
            .expect("stage test file");
        assert!(status.success(), "git add failed");
    }

    fn clean_git_command<'a>(root: &Path, args: impl IntoIterator<Item = &'a str>) -> Command {
        let mut command = Command::new("git");
        command.current_dir(root).args(args);
        for (name, _) in env::vars_os() {
            if is_git_environment_variable(&name) {
                command.env_remove(name);
            }
        }
        command
    }
}

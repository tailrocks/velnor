//! The deliberately narrow Docker compiler-cache execution boundary.
//!
//! A GitHub `run:` step is normally opaque shell text. This module admits only
//! one structured command shape, so cache lookup can never skip an arbitrary
//! script that happens to contain compiler-looking text.

use crate::{
    container::JobContainerSpec,
    executor::{CommandResult, CommandRunner, CommandStream},
    script_step::COMPILER_ACTION_PATH_ENV,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use velnor_action_model::{
    canonical_json_bytes, ActionKey, ActionResult, ActionTiming, Digest, ExecutionPolicy,
    PlatformIdentity, ProducerLease, Provenance, TrustClass,
};
use velnor_cache_service::ProductionCompilerCache;
use velnor_model::guest_plan::GuestCompilerCacheTrustClass;

const MAX_INPUT_FILES: usize = 100_000;
const MAX_INPUT_DIRECTORIES: usize = 100_000;
const MAX_INPUT_NODES: usize = 200_000;
const MAX_INPUT_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_INPUT_PATH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_INPUT_PATH_DEPTH: usize = 256;
const MAX_INPUT_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_INPUT_TOTAL_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const CACHEABLE_TARGET_DIRECTORY: &str = "target";
const DEFAULT_EXECUTION_PATH: &str =
    "/root/.cargo/bin:/opt/mise/bin:/opt/mise/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const COMMAND_FILE_ENV_NAMES: [&str; 5] = [
    "GITHUB_OUTPUT",
    "GITHUB_ENV",
    "GITHUB_PATH",
    "GITHUB_STATE",
    "GITHUB_STEP_SUMMARY",
];

/// One exact, shell-free compiler command admitted to the cache boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StructuredCompileAction {
    command: String,
    args: Vec<String>,
}

impl StructuredCompileAction {
    fn parse(script: &str) -> Option<Self> {
        if script.is_empty()
            || script.trim() != script
            || script.contains('\n')
            || script.contains('\r')
            || script.chars().any(|character| {
                matches!(
                    character,
                    ';' | '|' | '&' | '>' | '<' | '$' | '`' | '(' | ')' | '{' | '}' | '#'
                )
            })
        {
            return None;
        }
        let tokens = script
            .split_ascii_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if tokens.first().map(String::as_str) != Some("cargo")
            || tokens.get(1).map(String::as_str) != Some("build")
        {
            return None;
        }

        let mut index = 2;
        while index < tokens.len() {
            let token = &tokens[index];
            let needs_value = matches!(
                token.as_str(),
                "--package" | "-p" | "--bin" | "--features" | "--target"
            );
            let allowed = matches!(
                token.as_str(),
                "--release"
                    | "--locked"
                    | "--frozen"
                    | "--offline"
                    | "--all-targets"
                    | "--all-features"
            ) || needs_value;
            if !allowed {
                return None;
            }
            if needs_value {
                let value = tokens.get(index + 1)?;
                if value.is_empty()
                    || value.starts_with('-')
                    || value.chars().any(|character| {
                        !(character.is_ascii_alphanumeric()
                            || matches!(character, '_' | '-' | ',' | '/'))
                    })
                {
                    return None;
                }
                index += 1;
            }
            index += 1;
        }

        Some(Self {
            command: tokens.join(" "),
            args: tokens.into_iter().skip(1).collect(),
        })
    }
}

/// Outcome of one admitted compiler action.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CompilerActionExecution {
    pub result: CommandResult,
    pub cache_hit: bool,
    pub bypassed: bool,
}

/// Execution-scoped owner of a canonical compiler action and its producer
/// lease. It has no destructor cleanup: every terminal transition is explicit.
pub(crate) struct CompilerActionSession {
    service: Arc<ProductionCompilerCache>,
    action: StructuredCompileAction,
    key: ActionKey,
    input_directory: PathBuf,
    output_directory: PathBuf,
    timeout: Duration,
}

impl CompilerActionSession {
    /// Construct a session only for an admitted structured Docker command.
    pub(crate) fn new(
        service: Arc<ProductionCompilerCache>,
        container: &JobContainerSpec,
        script: &str,
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<Option<Self>> {
        let Some(action) = StructuredCompileAction::parse(script) else {
            return Ok(None);
        };
        let Some(image_digest) = immutable_image_digest(&container.image) else {
            return Ok(None);
        };
        let input_root = digest_input_tree(&container.workspace_host)
            .with_context(|| "compute compiler-cache input identity")?;
        let environment = canonical_compiler_environment(env)?;
        let environment_digest = digest_json(&environment)?;
        // The input tree includes workspace Cargo configuration and toolchain
        // files. Bind the rest of compiler resolution to the immutable image
        // and the complete effective execution environment, including PATH.
        let toolchain_digest = digest_json(&(image_digest.clone(), &environment))?;
        let trust_class = trust_class(container.compiler_cache_trust_class);
        let key = ActionKey {
            command_digest: Digest::from_bytes(action.command.as_bytes()),
            input_root,
            image_digest,
            toolchain_digest,
            platform: PlatformIdentity::new(
                "linux",
                std::env::consts::ARCH,
                Some("gnu".to_owned()),
            ),
            environment_digest,
            dependency_outputs: Vec::new(),
            execution_policy: ExecutionPolicy {
                trust_class,
                network: true,
                privileged: container
                    .options
                    .iter()
                    .any(|option| option == "--privileged"),
                timeout_ms: timeout.as_millis().try_into().unwrap_or(u64::MAX),
                adoptable: true,
            },
        };
        Ok(Some(Self {
            service,
            action,
            key,
            input_directory: container.workspace_host.clone(),
            output_directory: container.workspace_host.join(CACHEABLE_TARGET_DIRECTORY),
            timeout,
        }))
    }

    /// Warm the workspace from a validated hit, then always run the physical
    /// compiler command. A Cargo build can execute build scripts, proc macros,
    /// linker wrappers, and other user-controlled side effects; a cache hit is
    /// never permission to skip those effects.
    pub(crate) fn execute(
        self,
        runner: &mut impl CommandRunner,
        args: &[String],
        env: &[(String, String)],
        on_output: &mut dyn FnMut(CommandStream, &str),
    ) -> Result<CompilerActionExecution> {
        if let Some(entry) = self
            .service
            .lookup_with_publication_accounting_blocking(&self.key)
            .context("lookup compiler action")?
        {
            self.service
                .materialize_output_tree(&entry.result().output_root, &self.output_directory)
                .context("restore compiler-cache output")?;
            let result = runner
                .run_streaming_timeout_with_env("docker", args, env, self.timeout, on_output)
                .context("run compiler after cache warm restore")?;
            return Ok(CompilerActionExecution {
                result,
                cache_hit: true,
                bypassed: false,
            });
        }

        let lease = match self.service.begin_blocking(&self.key) {
            Ok(lease) => lease,
            Err(error) if error.is_lease_contention() => {
                return Ok(CompilerActionExecution {
                    result: runner
                        .run_streaming_timeout_with_env(
                            "docker",
                            args,
                            env,
                            self.timeout,
                            on_output,
                        )
                        .context("run compiler after cache contention")?,
                    cache_hit: false,
                    bypassed: true,
                });
            }
            Err(error) => return Err(error).context("begin compiler action lease"),
        };
        let lease_state = Arc::new(Mutex::new(Some(lease)));
        let heartbeat = Heartbeat::start(Arc::clone(&self.service), Arc::clone(&lease_state));
        let command_result = runner
            .run_streaming_timeout_with_env("docker", args, env, self.timeout, on_output)
            .context("run structured compiler action");
        let command_result = match command_result {
            Ok(result) => result,
            Err(error) => {
                let _ = heartbeat.stop();
                let cleanup = abandon_lease(&self.service, &lease_state);
                return Err(with_cleanup_context(error, cleanup));
            }
        };
        if let Some(error) = heartbeat.error() {
            let _ = heartbeat.stop();
            let cleanup = abandon_lease(&self.service, &lease_state);
            return Err(with_cleanup_context(
                anyhow::anyhow!("compiler-cache lease renewal failed: {error}"),
                cleanup,
            ));
        }
        if command_result.code != 0 {
            let _ = heartbeat.stop();
            let cleanup = abandon_lease(&self.service, &lease_state);
            if let Err(cleanup) = cleanup {
                return Err(cleanup.context("abandon failed compiler action"));
            }
            return Ok(CompilerActionExecution {
                result: command_result,
                cache_hit: false,
                bypassed: false,
            });
        }

        let lease_snapshot = lease_snapshot(&lease_state)?.ok_or_else(|| {
            anyhow::anyhow!("compiler-cache lease disappeared during output capture")
        })?;
        let output_root = match self
            .service
            .store_output_tree_for_lease(&lease_snapshot, &self.output_directory)
        {
            Ok(root) => root,
            Err(error) => {
                let _ = heartbeat.stop();
                let cleanup = abandon_lease(&self.service, &lease_state);
                return Err(with_cleanup_context(
                    anyhow::anyhow!(error).context("capture compiler output tree"),
                    cleanup,
                ));
            }
        };
        let current_input = match digest_input_tree(&self.input_directory) {
            Ok(digest) => digest,
            Err(error) => {
                let _ = heartbeat.stop();
                let cleanup = abandon_lease(&self.service, &lease_state);
                return Err(with_cleanup_context(
                    error.context("revalidate compiler-cache input identity"),
                    cleanup,
                ));
            }
        };
        if current_input != self.key.input_root {
            let _ = heartbeat.stop();
            let cleanup = abandon_lease(&self.service, &lease_state);
            return Err(with_cleanup_context(
                anyhow::anyhow!("compiler-cache input changed during compilation"),
                cleanup,
            ));
        }
        if let Some(error) = heartbeat.error() {
            let _ = heartbeat.stop();
            let cleanup = abandon_lease(&self.service, &lease_state);
            return Err(with_cleanup_context(
                anyhow::anyhow!("compiler-cache lease renewal failed: {error}"),
                cleanup,
            ));
        }
        let started_at_ms = unix_now_ms();
        let key = self.key;
        let source_digest = key.input_root.clone();
        let result = ActionResult {
            action_key: key,
            output_root,
            stdout_digest: Digest::from_bytes(command_result.stdout.as_bytes()),
            stderr_digest: Digest::from_bytes(command_result.stderr.as_bytes()),
            exit_code: command_result.code,
            provenance: Provenance {
                builder: "velnor-runner/docker-structured-compiler".to_owned(),
                source_digest,
                metadata: BTreeMap::from([
                    ("command".to_owned(), self.action.command),
                    ("argv".to_owned(), self.action.args.join("\u{0}")),
                ]),
            },
            timing: ActionTiming {
                started_at_ms,
                duration_ms: 0,
                cpu_ms: None,
            },
        };
        let lease = lease_snapshot(&lease_state)?.ok_or_else(|| {
            anyhow::anyhow!("compiler-cache lease disappeared before publication")
        })?;
        if let Err(error) = self
            .service
            .publish_with_accounting_blocking(lease, result)
        {
            let _ = heartbeat.stop();
            let cleanup = abandon_lease(&self.service, &lease_state);
            return Err(with_cleanup_context(
                anyhow::anyhow!(error).context("publish compiler action"),
                cleanup,
            ));
        }
        clear_lease(&lease_state)?;
        let _ = heartbeat.stop();
        Ok(CompilerActionExecution {
            result: command_result,
            cache_hit: false,
            bypassed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn key(&self) -> &ActionKey {
        &self.key
    }
}

struct Heartbeat {
    control: Arc<HeartbeatControl>,
    error: Arc<Mutex<Option<String>>>,
    thread: Option<thread::JoinHandle<()>>,
}

struct HeartbeatControl {
    stop: Mutex<bool>,
    wake: Condvar,
}

impl Heartbeat {
    fn start(
        service: Arc<ProductionCompilerCache>,
        state: Arc<Mutex<Option<ProducerLease>>>,
    ) -> Self {
        let control = Arc::new(HeartbeatControl {
            stop: Mutex::new(false),
            wake: Condvar::new(),
        });
        let error = Arc::new(Mutex::new(None));
        let thread_control = Arc::clone(&control);
        let thread_state = Arc::clone(&state);
        let thread_error = Arc::clone(&error);
        let thread = thread::spawn(move || loop {
            let guard = match thread_control.stop.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            let (guard, _) = match thread_control
                .wake
                .wait_timeout(guard, heartbeat_interval(&thread_state))
            {
                Ok(result) => result,
                Err(_) => return,
            };
            if *guard {
                return;
            }
            drop(guard);
            let mut lease = match thread_state.lock() {
                Ok(lease) => lease,
                Err(_) => return,
            };
            let Some(lease) = lease.as_mut() else {
                return;
            };
            if let Err(error) = service.renew_blocking(lease) {
                if let Ok(mut slot) = thread_error.lock() {
                    *slot = Some(error.to_string());
                }
                return;
            }
        });
        Self {
            control,
            error,
            thread: Some(thread),
        }
    }

    fn stop(mut self) -> Option<String> {
        if let Ok(mut stop) = self.control.stop.lock() {
            *stop = true;
            self.control.wake.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.error()
    }

    fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|error| error.clone())
    }
}

fn heartbeat_interval(state: &Mutex<Option<ProducerLease>>) -> Duration {
    state
        .lock()
        .ok()
        .and_then(|lease| lease.as_ref().map(|lease| lease.heartbeat_every))
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(1))
}

fn lease_snapshot(state: &Mutex<Option<ProducerLease>>) -> Result<Option<ProducerLease>> {
    state
        .lock()
        .map(|state| state.clone())
        .map_err(|_| anyhow::anyhow!("compiler-cache lease mutex poisoned"))
}

fn clear_lease(state: &Mutex<Option<ProducerLease>>) -> Result<()> {
    state
        .lock()
        .map(|mut state| *state = None)
        .map_err(|_| anyhow::anyhow!("compiler-cache lease mutex poisoned"))
}

fn abandon_lease(
    service: &ProductionCompilerCache,
    state: &Mutex<Option<ProducerLease>>,
) -> Result<()> {
    let Some(lease) = lease_snapshot(state)? else {
        return Ok(());
    };
    clear_lease(state)?;
    match service.abandon_blocking(&lease) {
        Ok(()) => Ok(()),
        Err(velnor_cache_service::CacheError::Journal(
            velnor_action_journal::JournalError::LeaseFenced
            | velnor_action_journal::JournalError::LeaseReleased { .. }
            | velnor_action_journal::JournalError::LeaseNotFound { .. },
        )) => Ok(()),
        Err(error) => Err(anyhow::anyhow!(error).context("abandon compiler action lease")),
    }
}

fn with_cleanup_context(primary: anyhow::Error, cleanup: Result<()>) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(error) => primary.context(format!("cache cleanup also failed: {error:#}")),
    }
}

fn immutable_image_digest(image: &str) -> Option<Digest> {
    let (_, sha256) = image.rsplit_once("@sha256:")?;
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    Some(Digest::from_bytes(image.as_bytes()))
}

fn trust_class(value: GuestCompilerCacheTrustClass) -> TrustClass {
    match value {
        GuestCompilerCacheTrustClass::Untrusted => TrustClass::Untrusted,
        GuestCompilerCacheTrustClass::Trusted => TrustClass::Trusted,
        GuestCompilerCacheTrustClass::Release => TrustClass::Release,
    }
}

fn digest_json<T: Serialize>(value: &T) -> Result<Digest> {
    Ok(Digest::from_bytes(&canonical_json_bytes(value)?))
}

fn canonical_compiler_environment(env: &[(String, String)]) -> Result<BTreeMap<String, String>> {
    let mut environment = env.iter().cloned().collect::<BTreeMap<_, _>>();
    let path_prepend = environment
        .remove(COMPILER_ACTION_PATH_ENV)
        .map(|value| {
            serde_json::from_str::<Vec<String>>(&value)
                .with_context(|| "parse effective GITHUB_PATH entries")
        })
        .transpose()?
        .unwrap_or_default();

    for name in COMMAND_FILE_ENV_NAMES {
        environment.remove(name);
    }

    let base_path = environment
        .get("PATH")
        .cloned()
        .unwrap_or_else(|| DEFAULT_EXECUTION_PATH.to_owned());
    let effective_path = if path_prepend.is_empty() {
        base_path
    } else if base_path.is_empty() {
        path_prepend.join(":")
    } else {
        format!("{}:{base_path}", path_prepend.join(":"))
    };
    environment.insert("PATH".to_owned(), effective_path);
    Ok(environment)
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn digest_input_tree(root: &Path) -> Result<Digest> {
    #[cfg(unix)]
    {
        let root = SecureInputDirectory::open_absolute(root)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"velnor-compiler-input-v1\0");
        let mut files = 0_usize;
        let mut directories = 0_usize;
        let mut nodes = 0_usize;
        let mut path_bytes = 0_u64;
        let mut total_bytes = 0_u64;
        digest_input_directory(
            &root,
            Path::new(""),
            &mut hasher,
            &mut files,
            &mut directories,
            &mut nodes,
            &mut path_bytes,
            &mut total_bytes,
            0,
        )?;
        Ok(Digest::from_hash(hasher.finalize()))
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        bail!(
            "structured compiler-cache input snapshots require descriptor-relative filesystem APIs"
        )
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct SecureInputDirectory {
    file: fs::File,
    display_path: PathBuf,
}

#[cfg(unix)]
impl SecureInputDirectory {
    fn open_absolute(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            bail!("compiler input root must be absolute: {}", path.display());
        }
        let path = normalized_secure_root_path(path);

        let root = rustix::fs::openat(
            rustix::fs::CWD,
            Path::new("/"),
            directory_flags(),
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .context("open filesystem root for compiler input")?;
        let mut current = Self {
            file: root.into(),
            display_path: PathBuf::from("/"),
        };

        for component in path.components() {
            let name = match component {
                std::path::Component::RootDir | std::path::Component::CurDir => continue,
                std::path::Component::Normal(name) => name,
                std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                    bail!("compiler input root is not normalized: {}", path.display())
                }
            };
            let display_path = current.display_path.join(name);
            current = current.open_directory(name, &display_path)?;
        }
        Ok(current)
    }

    fn open_directory(&self, name: &OsStr, display_path: &Path) -> Result<Self> {
        let expected = stat_input_entry(&self.file, name, display_path)?;
        if rustix::fs::FileType::from_raw_mode(expected.st_mode) == rustix::fs::FileType::Symlink {
            bail!(
                "compiler input contains symlink: {}",
                display_path.display()
            );
        }
        if !is_directory(&expected) {
            bail!(
                "compiler input path has a non-directory ancestor: {}",
                display_path.display()
            );
        }
        let child = rustix::fs::openat(
            &self.file,
            name,
            directory_flags(),
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!(
                "open compiler input directory without following links: {}",
                display_path.display()
            )
        })?;
        let opened = rustix::fs::fstat(&child)
            .map_err(std::io::Error::from)
            .with_context(|| format!("inspect opened compiler input directory {display_path:?}"))?;
        ensure_same_input_entry(&expected, &opened, display_path)?;
        let directory: fs::File = child.into();
        verify_input_entry(&self.file, name, &expected, display_path)?;
        Ok(Self {
            file: directory,
            display_path: display_path.to_path_buf(),
        })
    }
}

#[cfg(unix)]
fn digest_input_directory(
    root: &SecureInputDirectory,
    relative_root: &Path,
    hasher: &mut blake3::Hasher,
    files: &mut usize,
    directories: &mut usize,
    nodes: &mut usize,
    path_bytes: &mut u64,
    total_bytes: &mut u64,
    depth: usize,
) -> Result<()> {
    if depth > MAX_INPUT_PATH_DEPTH {
        bail!("compiler input exceeds {MAX_INPUT_PATH_DEPTH}-component depth");
    }
    *directories = directories.saturating_add(1);
    if *directories > MAX_INPUT_DIRECTORIES {
        bail!("compiler input exceeds {MAX_INPUT_DIRECTORIES} directories");
    }
    let names = read_input_names(
        &root.file,
        &root.display_path,
        MAX_INPUT_DIRECTORY_ENTRIES,
    )?;
    for name in &names {
        if name == OsStr::new(".") || name == OsStr::new("..") {
            continue;
        }
        let relative = relative_root.join(name);
        let relative_text = relative.to_str().ok_or_else(|| {
            anyhow::anyhow!("compiler input path is not UTF-8: {}", relative.display())
        })?;
        *nodes = nodes.saturating_add(1);
        if *nodes > MAX_INPUT_NODES {
            bail!("compiler input exceeds {MAX_INPUT_NODES} nodes");
        }
        *path_bytes = path_bytes
            .checked_add(relative_text.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("compiler input path byte count overflowed"))?;
        if *path_bytes > MAX_INPUT_PATH_BYTES {
            bail!("compiler input paths exceed {MAX_INPUT_PATH_BYTES} bytes");
        }
        let display_path = root.display_path.join(name);
        let expected = stat_input_entry(&root.file, name, &display_path)?;
        let file_type = rustix::fs::FileType::from_raw_mode(expected.st_mode);
        if relative_root.as_os_str().is_empty()
            && matches!(name.to_str(), Some("target" | ".git" | ".velnor"))
        {
            if file_type == rustix::fs::FileType::Symlink {
                bail!("compiler input contains symlink: {relative_text}");
            }
            if file_type != rustix::fs::FileType::Directory {
                bail!("compiler input contains unsupported entry: {relative_text}");
            }
            verify_input_entry(&root.file, name, &expected, &display_path)?;
            continue;
        }
        match file_type {
            rustix::fs::FileType::Directory => {
                let child = root.open_directory(name, &display_path)?;
                digest_input_directory(
                    &child,
                    &relative,
                    hasher,
                    files,
                    directories,
                    nodes,
                    path_bytes,
                    total_bytes,
                    depth.saturating_add(1),
                )?;
            }
            rustix::fs::FileType::RegularFile => {
                *files = files.saturating_add(1);
                if *files > MAX_INPUT_FILES {
                    bail!("compiler input exceeds {MAX_INPUT_FILES} files");
                }
                let file_size = opened_input_size(&expected, &relative_text)?;
                *total_bytes = total_bytes
                    .checked_add(file_size)
                    .ok_or_else(|| anyhow::anyhow!("compiler input byte count overflowed"))?;
                if *total_bytes > MAX_INPUT_TOTAL_BYTES {
                    bail!("compiler input exceeds {MAX_INPUT_TOTAL_BYTES} bytes");
                }
                hasher.update(&(relative_text.len() as u64).to_be_bytes());
                hasher.update(relative_text.as_bytes());
                hasher.update(&file_size.to_be_bytes());
                hash_input_file(
                    &root.file,
                    name,
                    &expected,
                    &display_path,
                    file_size,
                    hasher,
                )?;
            }
            rustix::fs::FileType::Symlink => {
                bail!("compiler input contains symlink: {relative_text}")
            }
            _ => bail!("compiler input contains unsupported entry: {relative_text}"),
        }
        verify_input_entry(&root.file, name, &expected, &display_path)?;
    }

    let final_names = read_input_names(
        &root.file,
        &root.display_path,
        MAX_INPUT_DIRECTORY_ENTRIES,
    )?;
    if names != final_names {
        bail!(
            "compiler input directory changed during secure snapshot: {}",
            root.display_path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn read_input_names(
    directory: &fs::File,
    display_path: &Path,
    max_entries: usize,
) -> Result<Vec<OsString>> {
    let mut names = rustix::fs::Dir::read_from(directory)
        .map_err(std::io::Error::from)
        .with_context(|| format!("read compiler input directory {}", display_path.display()))?
        .map(|entry| {
            entry
                .map_err(std::io::Error::from)
                .map(|entry| OsString::from_vec(entry.file_name().to_bytes().to_vec()))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if names.len() > max_entries {
        bail!(
            "compiler input directory {} exceeds {max_entries} entries",
            display_path.display()
        );
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
fn directory_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW
}

fn normalized_secure_root_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        for alias in [Path::new("/var"), Path::new("/tmp")] {
            if let Ok(remainder) = path.strip_prefix(alias) {
                return Path::new("/private")
                    .join(alias.strip_prefix("/").unwrap_or(alias))
                    .join(remainder);
            }
        }
    }
    path.to_path_buf()
}

#[cfg(unix)]
fn stat_input_entry(
    parent: &fs::File,
    name: &OsStr,
    display_path: &Path,
) -> Result<rustix::fs::Stat> {
    rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)
        .with_context(|| format!("inspect compiler input {}", display_path.display()))
}

#[cfg(unix)]
fn is_directory(stat: &rustix::fs::Stat) -> bool {
    rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Directory
}

#[cfg(unix)]
fn stat_identity(stat: &rustix::fs::Stat) -> (u64, u64, u32) {
    (
        stat.st_dev as u64,
        stat.st_ino,
        u32::from(stat.st_mode & 0o170_000),
    )
}

#[cfg(unix)]
fn ensure_same_input_entry(
    expected: &rustix::fs::Stat,
    actual: &rustix::fs::Stat,
    display_path: &Path,
) -> Result<()> {
    if stat_identity(expected) != stat_identity(actual) {
        bail!(
            "compiler input entry was replaced during secure open: {}",
            display_path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn verify_input_entry(
    parent: &fs::File,
    name: &OsStr,
    expected: &rustix::fs::Stat,
    display_path: &Path,
) -> Result<()> {
    let current = stat_input_entry(parent, name, display_path)?;
    ensure_same_input_entry(expected, &current, display_path)
}

#[cfg(unix)]
fn opened_input_size(stat: &rustix::fs::Stat, relative: &str) -> Result<u64> {
    let size = u64::try_from(stat.st_size)
        .map_err(|_| anyhow::anyhow!("compiler input file has a negative size: {relative}"))?;
    if size > MAX_INPUT_FILE_BYTES {
        bail!(
            "compiler input file exceeds {MAX_INPUT_FILE_BYTES} bytes: {relative}"
        );
    }
    Ok(size)
}

#[cfg(unix)]
fn hash_input_file(
    parent: &fs::File,
    name: &OsStr,
    expected: &rustix::fs::Stat,
    display_path: &Path,
    expected_size: u64,
    hasher: &mut blake3::Hasher,
) -> Result<()> {
    let file = rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .with_context(|| {
        format!(
            "open compiler input without following links: {}",
            display_path.display()
        )
    })?;
    let opened = rustix::fs::fstat(&file)
        .map_err(std::io::Error::from)
        .with_context(|| format!("inspect opened compiler input {}", display_path.display()))?;
    if !is_regular_file(&opened) {
        bail!(
            "compiler input changed to a non-file: {}",
            display_path.display()
        );
    }
    ensure_same_input_entry(expected, &opened, display_path)?;
    let mut file: fs::File = file.into();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| anyhow::anyhow!("compiler input byte count overflowed"))?;
        if total > MAX_INPUT_FILE_BYTES {
            bail!(
                "compiler input file exceeds {MAX_INPUT_FILE_BYTES} bytes: {}",
                display_path.display()
            );
        }
        hasher.update(&buffer[..count]);
    }
    let after = rustix::fs::fstat(&file)
        .map_err(std::io::Error::from)
        .with_context(|| format!("reinspect compiler input {}", display_path.display()))?;
    ensure_same_input_entry(expected, &after, display_path)?;
    if total != expected_size
        || u64::try_from(after.st_size).ok() != Some(expected_size)
    {
        bail!(
            "compiler input file changed during secure snapshot: {}",
            display_path.display()
        );
    }
    verify_input_entry(parent, name, expected, display_path)?;
    Ok(())
}

#[cfg(unix)]
fn is_regular_file(stat: &rustix::fs::Stat) -> bool {
    rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::RegularFile
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::ServiceContainerSpec;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ROOT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TestRunner {
        target: PathBuf,
        code: i32,
        fail: bool,
        calls: usize,
    }

    impl CommandRunner for TestRunner {
        fn run(&mut self, _program: &str, _args: &[String]) -> Result<CommandResult> {
            self.calls += 1;
            if self.fail {
                bail!("simulated command cancellation")
            }
            if self.code == 0 {
                fs::create_dir_all(&self.target)?;
                fs::write(self.target.join("libdemo.rlib"), b"compiler output")?;
            }
            Ok(CommandResult {
                code: self.code,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        fn run_streaming_timeout_with_env(
            &mut self,
            program: &str,
            args: &[String],
            _env: &[(String, String)],
            _timeout: Duration,
            _on_output: &mut dyn FnMut(CommandStream, &str),
        ) -> Result<CommandResult> {
            self.run(program, args)
        }
    }

    fn test_root(name: &str) -> PathBuf {
        let sequence = TEST_ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "velnor-compiler-action-{name}-{}-{sequence}",
            std::process::id()
        ))
    }

    fn container(root: &Path) -> JobContainerSpec {
        JobContainerSpec {
            name: "compiler-job".into(),
            image: "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
            network: "compiler-net".into(),
            workspace_host: root.join("workspace"),
            temp_host: root.join("temp"),
            home_host: root.join("home"),
            actions_host: root.join("actions"),
            tools_host: root.join("tools"),
            mount_docker_socket: false,
            env: Vec::new(),
            resource_options: Vec::new(),
            options: Vec::new(),
            services: Vec::<ServiceContainerSpec>::new(),
            node_action_image: "node:24-bookworm".into(),
            docker_cli_host_path: None,
            docker_cli_plugin_host_dir: None,
            docker_host_work_dir: None,
            verify_bind_mounts: false,
            daemon_id: "compiler-daemon".into(),
            repository: Some("acme/compiler".into()),
            cargo_target_host: None,
            compiler_cache_backend: velnor_cache_service::CompilerCacheBackend::Off,
            compiler_cache_trust_class: GuestCompilerCacheTrustClass::Trusted,
            compiler_cache_service: true,
            compiler_cache_service_root: Some(root.join("cache")),
        }
    }

    fn service(root: &Path) -> Arc<ProductionCompilerCache> {
        let mut config = velnor_cache_service::CompilerCacheConfig::new(
            root.join("cache"),
            "compiler-test-worker",
        );
        config.policy = velnor_cache_service::CompilerCachePolicy::Kache;
        config.trust_class = TrustClass::Trusted;
        Arc::new(
            ProductionCompilerCache::open_production(
                config,
                velnor_cache_service::WrapperDeclaration::default(),
            )
            .expect("open compiler test cache"),
        )
    }

    fn prepare(root: &Path) -> JobContainerSpec {
        let container = container(root);
        fs::create_dir_all(&container.workspace_host).expect("workspace");
        fs::write(
            container.workspace_host.join("Cargo.toml"),
            b"[package]\nname='demo'\n",
        )
        .expect("input");
        container
    }

    fn execute(session: CompilerActionSession, runner: &mut TestRunner) -> CompilerActionExecution {
        session
            .execute(
                runner,
                &["exec".into(), "compiler".into()],
                &[("CARGO_INCREMENTAL".into(), "0".into())],
                &mut |_, _| {},
            )
            .expect("compiler session")
    }

    #[test]
    fn parser_admits_only_one_structured_cargo_build() {
        assert!(StructuredCompileAction::parse("cargo build --release --locked").is_some());
        assert!(StructuredCompileAction::parse("cargo build --package app --bin app").is_some());
        assert!(StructuredCompileAction::parse("cargo build; echo unsafe").is_none());
        assert!(StructuredCompileAction::parse("echo cargo build").is_none());
        assert!(StructuredCompileAction::parse("cargo test").is_none());
        assert!(StructuredCompileAction::parse("cargo build $(touch pwned)").is_none());
    }

    #[test]
    fn mutable_image_is_not_cacheable() {
        assert!(immutable_image_digest("ubuntu:24.04").is_none());
        assert!(immutable_image_digest(
            "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_some());
        assert!(immutable_image_digest(
            "ubuntu@sha256:0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_none());
    }

    #[test]
    fn cache_miss_publishes_and_hit_warms_then_runs_compiler() {
        let root = test_root("hit-miss");
        let container = prepare(&root);
        let service = service(&root);
        let session = CompilerActionSession::new(
            Arc::clone(&service),
            &container,
            "cargo build --locked",
            &[("CARGO_INCREMENTAL".into(), "0".into())],
            Duration::from_secs(5),
        )
        .expect("session construction")
        .expect("structured action");
        let mut producer = TestRunner {
            target: container.workspace_host.join("target"),
            code: 0,
            fail: false,
            calls: 0,
        };
        let miss = execute(session, &mut producer);
        assert!(!miss.cache_hit);
        assert!(!miss.bypassed);
        assert_eq!(producer.calls, 1);

        let session = CompilerActionSession::new(
            Arc::clone(&service),
            &container,
            "cargo build --locked",
            &[("CARGO_INCREMENTAL".into(), "0".into())],
            Duration::from_secs(5),
        )
        .expect("session construction")
        .expect("structured action");
        let mut consumer = TestRunner {
            target: container.workspace_host.join("target"),
            code: 17,
            fail: false,
            calls: 0,
        };
        let hit = execute(session, &mut consumer);
        assert!(hit.cache_hit);
        assert_eq!(consumer.calls, 1);
        assert_eq!(hit.result.code, 17);
        assert_eq!(
            fs::read(container.workspace_host.join("target/libdemo.rlib"))
                .expect("restored output"),
            b"compiler output"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn action_key_binds_effective_path_and_compiler_environment_canonically() {
        let root = test_root("key-identity");
        let container = prepare(&root);
        let service = service(&root);
        let path_a = serde_json::to_string(&["/opt/tool-a"]).expect("path metadata");
        let path_b = serde_json::to_string(&["/opt/tool-b"]).expect("path metadata");
        let first = CompilerActionSession::new(
            Arc::clone(&service),
            &container,
            "cargo build --locked",
            &[
                ("CC".into(), "clang-a".into()),
                ("GITHUB_PATH".into(), "/__t/first_path".into()),
                (COMPILER_ACTION_PATH_ENV.into(), path_a.clone()),
                ("GITHUB_ENV".into(), "/__t/first_env".into()),
            ],
            Duration::from_secs(5),
        )
        .expect("first session")
        .expect("structured action");
        let same_effective_environment = CompilerActionSession::new(
            Arc::clone(&service),
            &container,
            "cargo build --locked",
            &[
                ("GITHUB_ENV".into(), "/__t/another_env".into()),
                (COMPILER_ACTION_PATH_ENV.into(), path_a),
                ("CC".into(), "clang-a".into()),
                ("GITHUB_PATH".into(), "/__t/another_path".into()),
            ],
            Duration::from_secs(5),
        )
        .expect("same environment session")
        .expect("structured action");
        assert_eq!(
            first.key().canonical_bytes().expect("first canonical key"),
            same_effective_environment
                .key()
                .canonical_bytes()
                .expect("same canonical key")
        );

        let different_path = CompilerActionSession::new(
            Arc::clone(&service),
            &container,
            "cargo build --locked",
            &[
                ("CC".into(), "clang-a".into()),
                (COMPILER_ACTION_PATH_ENV.into(), path_b),
            ],
            Duration::from_secs(5),
        )
        .expect("different path session")
        .expect("structured action");
        assert_ne!(
            first.key().environment_digest,
            different_path.key().environment_digest
        );

        let different_compiler = CompilerActionSession::new(
            service,
            &container,
            "cargo build --locked",
            &[("CC".into(), "clang-b".into())],
            Duration::from_secs(5),
        )
        .expect("different compiler session")
        .expect("structured action");
        assert_ne!(
            first.key().environment_digest,
            different_compiler.key().environment_digest
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn input_snapshot_rejects_symlinked_parent_without_escaping_root() {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink-parent");
        let workspace = root.join("workspace");
        let outside = root.join("outside");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(outside.join("secret.txt"), b"must not be hashed").expect("outside input");
        symlink(&outside, workspace.join("source")).expect("symlink parent");

        let error = digest_input_tree(&workspace).expect_err("symlink must fail closed");
        assert!(error.to_string().contains("symlink"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn busy_or_stale_lease_bypasses_without_competing_publication() {
        let root = test_root("busy");
        let container = prepare(&root);
        let service = service(&root);
        let session = CompilerActionSession::new(
            Arc::clone(&service),
            &container,
            "cargo build",
            &[],
            Duration::from_secs(5),
        )
        .expect("session construction")
        .expect("structured action");
        let lease = service.begin_blocking(session.key()).expect("hold lease");
        let mut runner = TestRunner {
            target: container.workspace_host.join("target"),
            code: 0,
            fail: false,
            calls: 0,
        };
        let result = execute(session, &mut runner);
        assert!(result.bypassed);
        assert_eq!(runner.calls, 1);
        service
            .abandon_blocking(&lease)
            .expect("abandon held lease");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn command_failure_and_cancellation_abandon_the_lease() {
        for (name, fail, code) in [("failure", false, 1), ("cancel", true, 0)] {
            let root = test_root(name);
            let container = prepare(&root);
            let service = service(&root);
            let session = CompilerActionSession::new(
                Arc::clone(&service),
                &container,
                "cargo build",
                &[],
                Duration::from_secs(5),
            )
            .expect("session construction")
            .expect("structured action");
            let mut runner = TestRunner {
                target: container.workspace_host.join("target"),
                code,
                fail,
                calls: 0,
            };
            let result = session.execute(
                &mut runner,
                &["exec".into(), "compiler".into()],
                &[],
                &mut |_, _| {},
            );
            if fail {
                assert!(result.is_err());
            } else {
                assert_eq!(result.expect("failed compile result").result.code, 1);
            }
            let retry = CompilerActionSession::new(
                Arc::clone(&service),
                &container,
                "cargo build",
                &[],
                Duration::from_secs(5),
            )
            .expect("retry construction")
            .expect("structured retry");
            let retry_error = service
                .begin_blocking(retry.key())
                .expect_err("lease abandoned");
            assert!(retry_error.is_lease_contention());
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn ambiguous_shell_is_bypassed_and_never_becomes_a_cache_action() {
        let root = test_root("ambiguous");
        let container = prepare(&root);
        let session = CompilerActionSession::new(
            service(&root),
            &container,
            "cargo build && echo side effect",
            &[],
            Duration::from_secs(5),
        )
        .expect("session construction");
        assert!(session.is_none());
        let _ = fs::remove_dir_all(root);
    }
}

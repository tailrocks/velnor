//! The deliberately narrow Docker compiler-cache execution boundary.
//!
//! A GitHub `run:` step is normally opaque shell text. This module admits only
//! one structured command shape, so cache lookup can never skip an arbitrary
//! script that happens to contain compiler-looking text.

use crate::{
    container::JobContainerSpec,
    executor::{CommandResult, CommandRunner, CommandStream},
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
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
const MAX_INPUT_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_INPUT_TOTAL_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const CACHEABLE_TARGET_DIRECTORY: &str = "target";

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
        let environment = env.iter().cloned().collect::<BTreeMap<_, _>>();
        let environment_digest = digest_json(&environment)?;
        let toolchain_inputs = environment
            .iter()
            .filter(|(name, _)| {
                name.starts_with("CARGO")
                    || name.starts_with("RUST")
                    || name.starts_with("MISE")
                    || name == &"PATH"
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let toolchain_digest = digest_json(&(image_digest.clone(), toolchain_inputs))?;
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
            output_directory: container.workspace_host.join(CACHEABLE_TARGET_DIRECTORY),
            timeout,
        }))
    }

    /// Run the physical compiler command, or restore a validated hit.
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
            return Ok(CompilerActionExecution {
                result: CommandResult {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                },
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
        let heartbeat_error = heartbeat.stop();

        if let Some(error) = heartbeat_error {
            let cleanup = abandon_lease(&self.service, &lease_state);
            return Err(with_cleanup_context(
                anyhow::anyhow!("compiler-cache lease renewal failed: {error}"),
                cleanup,
            ));
        }
        let command_result = match command_result {
            Ok(result) => result,
            Err(error) => {
                let cleanup = abandon_lease(&self.service, &lease_state);
                return Err(with_cleanup_context(error, cleanup));
            }
        };
        if command_result.code != 0 {
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

        let output_root = match self.service.store_output_tree(&self.output_directory) {
            Ok(root) => root,
            Err(error) => {
                let cleanup = abandon_lease(&self.service, &lease_state);
                return Err(with_cleanup_context(
                    anyhow::anyhow!(error).context("capture compiler output tree"),
                    cleanup,
                ));
            }
        };
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
        let lease = take_lease(&lease_state)?;
        let Some(lease) = lease else {
            bail!("compiler-cache lease disappeared before publication")
        };
        if let Err(error) = self.service.publish_with_accounting_blocking(lease, result) {
            let cleanup = abandon_lease(&self.service, &lease_state);
            return Err(with_cleanup_context(
                anyhow::anyhow!(error).context("publish compiler action"),
                cleanup,
            ));
        }
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

fn take_lease(state: &Mutex<Option<ProducerLease>>) -> Result<Option<ProducerLease>> {
    state
        .lock()
        .map(|mut state| state.take())
        .map_err(|_| anyhow::anyhow!("compiler-cache lease mutex poisoned"))
}

fn abandon_lease(
    service: &ProductionCompilerCache,
    state: &Mutex<Option<ProducerLease>>,
) -> Result<()> {
    let Some(lease) = take_lease(state)? else {
        return Ok(());
    };
    service
        .abandon_blocking(&lease)
        .map_err(|error| anyhow::anyhow!(error).context("abandon compiler action lease"))
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

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn digest_input_tree(root: &Path) -> Result<Digest> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"velnor-compiler-input-v1\0");
    let mut files = 0_usize;
    let mut total_bytes = 0_u64;
    digest_input_directory(
        root,
        Path::new(""),
        &mut hasher,
        &mut files,
        &mut total_bytes,
    )?;
    Ok(Digest::from_hash(hasher.finalize()))
}

fn digest_input_directory(
    root: &Path,
    relative_root: &Path,
    hasher: &mut blake3::Hasher,
    files: &mut usize,
    total_bytes: &mut u64,
) -> Result<()> {
    let mut entries = fs::read_dir(root)
        .with_context(|| format!("read compiler input directory {}", root.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        if relative_root.as_os_str().is_empty()
            && matches!(name.to_str(), Some("target" | ".git" | ".velnor"))
        {
            continue;
        }
        let path = entry.path();
        let relative = relative_root.join(&name);
        let relative_text = relative.to_str().ok_or_else(|| {
            anyhow::anyhow!("compiler input path is not UTF-8: {}", relative.display())
        })?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!("compiler input contains symlink: {relative_text}");
        }
        if metadata.is_dir() {
            digest_input_directory(&path, &relative, hasher, files, total_bytes)?;
            continue;
        }
        if !metadata.is_file() {
            bail!("compiler input contains unsupported entry: {relative_text}");
        }
        *files = files.saturating_add(1);
        if *files > MAX_INPUT_FILES {
            bail!("compiler input exceeds {MAX_INPUT_FILES} files");
        }
        let bytes = read_input_file(&path)?;
        if bytes.len() as u64 > MAX_INPUT_FILE_BYTES {
            bail!("compiler input file exceeds {MAX_INPUT_FILE_BYTES} bytes: {relative_text}");
        }
        *total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("compiler input byte count overflowed"))?;
        if *total_bytes > MAX_INPUT_TOTAL_BYTES {
            bail!("compiler input exceeds {MAX_INPUT_TOTAL_BYTES} bytes");
        }
        hasher.update(&(relative_text.len() as u64).to_be_bytes());
        hasher.update(relative_text.as_bytes());
        hasher.update(&(bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    }
    Ok(())
}

fn read_input_file(path: &Path) -> Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("compiler input changed to a non-file: {}", path.display());
    }
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok(bytes)
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
    fn cache_miss_publishes_and_hit_restores_without_running_compiler() {
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
            code: 0,
            fail: false,
            calls: 0,
        };
        let hit = execute(session, &mut consumer);
        assert!(hit.cache_hit);
        assert_eq!(consumer.calls, 0);
        assert_eq!(
            fs::read(container.workspace_host.join("target/libdemo.rlib"))
                .expect("restored output"),
            b"compiler output"
        );
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

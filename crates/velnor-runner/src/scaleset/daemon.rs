//! Daemon wiring: config, startup, session management, shutdown.
//!
//! [`ScaleSetDaemon`] is the production caller the protocol/worker/loop
//! parts were missing: it loads key material, connects the admin client
//! ([`new_with_app`][app]/[`new_with_pat`][pat]), reconciles the GitHub
//! registration, adopts recorded workers, creates the message session,
//! runs the poll→Scale→ACK loop against the shared host-wide ledger, and
//! on shutdown closes the session and adopt-or-fails every worker.
//!
//! Startup splits into [`ScaleSetDaemon::open`] (local only: config,
//! keys, client construction) and [`ScaleSetDaemon::start`] (network:
//! registration, adoption, session creation) so the daemon fails a pass
//! fast *before* supervising native slots when the lane cannot start.
//! [`ScaleSetDaemon::run`] only returns on shutdown.
//!
//! [app]: crate::scaleset::ScaleSetClient::new_with_app
//! [pat]: crate::scaleset::ScaleSetClient::new_with_pat

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::scaleset::demand::OfferAdmission;
use crate::scaleset::key_material::{load_app_auth, load_pat, AppKeyConfig, KeySource};
use crate::scaleset::lane::{AdoptReport, DaemonWorkerLane, LaneConfig, ShutdownReport};
use crate::scaleset::registration::{reconcile_registration, ReconciledSet, RegistrationPlan};
use crate::scaleset::worker::{HomogeneousProfile, ToolContentHook, WorkerRunner};
use crate::scaleset::{
    AcquireBatchStore, ClientSession, DemandStore, IdlePolicy, Listener, LoopConfig, Metrics,
    Processor, ProcessorConfig, ProvisionImages, ProvisionIntentStore, RetryPolicy, ScaleSetClient,
    SessionStore, SharedLedger, SystemInfo, MAX_ACQUIRE_BATCH,
};

/// Default DinD readiness probes (× 5s interval ≈ 120s, the worker timeout).
const DEFAULT_READY_ATTEMPTS: u32 = 24;
/// Default gap between opportunistic supervision sweeps.
const DEFAULT_SWEEP_INTERVAL_SECS: u64 = 30;
/// Default long-poll window. Shorter than the protocol's 5min upstream
/// mirror: an in-flight poll is the worst-case graceful-stop latency, and
/// 60s idle polls cost nothing (nil polls run bounded reconcile anyway).
const DEFAULT_POLL_TIMEOUT_SECS: u64 = 60;
/// Crate version stamped into the session's `User-Agent` identity block.
const ADAPTER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Scale-set lane configuration file (TOML).
///
/// Secrets travel by reference only: key files or env var names. There is
/// no field that takes an inline secret, so a config file can never carry
/// key material by construction.
#[derive(Debug, Clone, Deserialize)]
pub struct ScaleSetFileConfig {
    /// Enterprise, org, or repository URL, e.g. `https://github.com/octo-org`.
    pub scope_url: String,
    /// Session owner name (upstream passes the org/login here).
    pub owner: String,
    /// Pin the runner group by ID (wins over `group_name`).
    pub group_id: Option<i32>,
    /// Resolve the runner group by name.
    pub group_name: Option<String>,
    /// Adopt this set ID and verify it (never auto-created when missing).
    pub set_id: Option<i32>,
    /// Get-or-create the set under this name.
    pub set_name: Option<String>,
    /// Desired label names; reconciled onto the set when drifted.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Credentials (App xor PAT, each by file or env reference).
    pub auth: AuthFileConfig,
    /// Every offer identity dimension must be explicitly allowlisted.
    #[serde(default)]
    pub admission: OfferAdmission,
    /// State database (defaults to the daemon's operational state db).
    pub state_db: Option<PathBuf>,
    /// Host-wide permit ledger (defaults to the daemon's ledger path).
    pub ledger_path: Option<PathBuf>,
    /// Worker state root (defaults to `<config-dir>/scaleset-workers`).
    pub worker_state_dir: Option<PathBuf>,
    /// DinD readiness probes before giving up.
    pub ready_attempts: Option<u32>,
    /// Seconds between opportunistic supervision sweeps.
    pub sweep_interval_secs: Option<u64>,
    /// HTTP retry budget override.
    pub max_retries: Option<u32>,
    /// Long-poll timeout override, seconds.
    pub poll_timeout_secs: Option<u64>,
    /// Nil-poll floor override, seconds.
    pub nil_delay_secs: Option<u64>,
}

/// Credential references: exactly one of `app` / `pat`.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthFileConfig {
    pub app: Option<AppAuthFile>,
    pub pat: Option<PatAuthFile>,
}

/// GitHub App credentials by reference.
#[derive(Debug, Clone, Deserialize)]
pub struct AppAuthFile {
    pub client_id: String,
    pub installation_id: i64,
    pub private_key_file: Option<PathBuf>,
    pub private_key_env: Option<String>,
}

/// PAT credential by reference.
#[derive(Debug, Clone, Deserialize)]
pub struct PatAuthFile {
    pub token_file: Option<PathBuf>,
    pub token_env: Option<String>,
}

/// Load + validate the lane config file. Fails closed on unreadable
/// files, TOML errors, missing identity, or ambiguous credentials.
pub fn load_file_config(path: &Path) -> Result<ScaleSetFileConfig> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read scale-set config {}", path.display()))?;
    let config: ScaleSetFileConfig = toml::from_str(&raw)
        .with_context(|| format!("parse scale-set config {}", path.display()))?;
    config.validate()?;
    Ok(config)
}

impl ScaleSetFileConfig {
    fn validate(&self) -> Result<()> {
        if self.scope_url.is_empty() {
            anyhow::bail!("scale-set config: scope_url is required");
        }
        if self.owner.is_empty() {
            anyhow::bail!("scale-set config: owner is required");
        }
        crate::platform::validate_self_hosted_runner_labels(&self.labels)?;
        // Registration validates the rest at reconcile time; fail fast on
        // the shape here too so a bad file never reaches the network.
        if self.group_id.is_none() && self.group_name.as_ref().is_none_or(String::is_empty) {
            anyhow::bail!("scale-set config: needs group_id or group_name");
        }
        match (self.set_id, self.set_name.as_ref()) {
            (Some(_), Some(_)) => {
                anyhow::bail!("scale-set config: takes either set_id or set_name, not both");
            }
            (None, None) => {
                anyhow::bail!("scale-set config: needs set_id or set_name");
            }
            _ => {}
        }
        self.admission
            .validate()
            .context("validate [admission] allowlists")?;
        if self
            .ledger_path
            .as_deref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            anyhow::bail!("scale-set config: ledger_path must not be empty");
        }
        self.auth.resolve()?;
        if self.ready_attempts == Some(0) {
            anyhow::bail!("scale-set config: ready_attempts must be positive");
        }
        Ok(())
    }

    fn registration_plan(&self) -> RegistrationPlan {
        RegistrationPlan {
            group_id: self.group_id,
            group_name: self.group_name.clone(),
            set_id: self.set_id,
            set_name: self.set_name.clone(),
            labels: self.labels.clone(),
        }
    }
}

impl AuthFileConfig {
    /// Resolve exactly one credential source. Errors name the file shape,
    /// never key material.
    fn resolve(&self) -> Result<ResolvedAuth> {
        match (&self.app, &self.pat) {
            (Some(_), Some(_)) => {
                anyhow::bail!("scale-set config: takes either auth.app or auth.pat, not both");
            }
            (None, None) => {
                anyhow::bail!("scale-set config: needs auth.app or auth.pat");
            }
            (Some(app), None) => {
                let key = exactly_one_source(
                    app.private_key_file.clone(),
                    app.private_key_env.clone(),
                    "auth.app.private_key_file",
                    "auth.app.private_key_env",
                )?;
                Ok(ResolvedAuth::App(AppKeyConfig {
                    client_id: app.client_id.clone(),
                    installation_id: app.installation_id,
                    key,
                }))
            }
            (None, Some(pat)) => {
                let source = exactly_one_source(
                    pat.token_file.clone(),
                    pat.token_env.clone(),
                    "auth.pat.token_file",
                    "auth.pat.token_env",
                )?;
                Ok(ResolvedAuth::Pat(source))
            }
        }
    }
}

enum ResolvedAuth {
    App(AppKeyConfig),
    Pat(KeySource),
}

fn exactly_one_source(
    file: Option<PathBuf>,
    env: Option<String>,
    file_name: &str,
    env_name: &str,
) -> Result<KeySource> {
    match (file, env) {
        (Some(_), Some(_)) => {
            anyhow::bail!("scale-set config: takes either {file_name} or {env_name}, not both");
        }
        (None, None) => {
            anyhow::bail!("scale-set config: needs {file_name} or {env_name}");
        }
        (Some(path), None) => Ok(KeySource::File(path)),
        (None, Some(name)) => {
            if name.is_empty() {
                anyhow::bail!("scale-set config: {env_name} is empty");
            }
            Ok(KeySource::Env(name))
        }
    }
}

/// Daemon-resolved defaults for paths the config file leaves unset.
#[derive(Debug, Clone)]
pub struct DaemonDefaults {
    /// The daemon's operational state db (stores live here).
    pub state_db: PathBuf,
    /// The daemon's host-wide permit ledger file.
    pub ledger_path: PathBuf,
    /// The daemon's config dir (worker state defaults under it).
    pub config_dir: PathBuf,
}

/// The running lane: loop + session. Built by `start`, driven by `run`.
struct Running {
    listener: Listener<ClientSession, SharedLedger, DaemonWorkerLane>,
    session: crate::scaleset::MessageSessionClient,
}

/// The scale-set lane as the daemon runs it.
pub struct ScaleSetDaemon {
    plan: RegistrationPlan,
    client: ScaleSetClient,
    system_info: SystemInfo,
    retry: RetryPolicy,
    idle: IdlePolicy,
    owner: String,
    profile: HomogeneousProfile,
    lane_config: LaneConfig,
    state_db: PathBuf,
    ledger_path: PathBuf,
    admission: OfferAdmission,
    runner: Option<Box<dyn WorkerRunner + Send>>,
    hook: Option<Box<dyn ToolContentHook + Send>>,
    metrics: Metrics,
    running: Option<Running>,
    reconciled: Option<ReconciledSet>,
    adopt_report: Option<AdoptReport>,
}

impl ScaleSetDaemon {
    /// Open the lane from a config file: validate, load key material,
    /// connect the admin client. Local I/O only — no GitHub calls, so
    /// this is safe to run before the daemon supervises native slots.
    ///
    /// `runner`/`hook` are the Docker seams: production passes
    /// [`ProcessCommandRunner`][pcr] + [`DockerToolContentHook`][hook]
    /// (see [`ScaleSetDaemon::open_production`]); tests pass scripted
    /// doubles.
    ///
    /// [pcr]: crate::executor::ProcessCommandRunner
    /// [hook]: crate::scaleset::worker::DockerToolContentHook
    pub fn open(
        config_path: &Path,
        defaults: &DaemonDefaults,
        runner: Box<dyn WorkerRunner + Send>,
        hook: Box<dyn ToolContentHook + Send>,
    ) -> Result<Self> {
        let file = load_file_config(config_path)?;
        let profile = HomogeneousProfile::host().with_context(|| {
            format!(
                "no homogeneous scale-set profile for arch {}",
                std::env::consts::ARCH
            )
        })?;
        let retry = RetryPolicy {
            max_retries: file
                .max_retries
                .unwrap_or(RetryPolicy::default().max_retries),
            timeout: Duration::from_secs(
                file.poll_timeout_secs.unwrap_or(DEFAULT_POLL_TIMEOUT_SECS),
            ),
            ..RetryPolicy::default()
        };
        if retry.timeout.is_zero() {
            anyhow::bail!("scale-set config: poll_timeout_secs must be positive");
        }
        let idle = IdlePolicy {
            nil_delay: Duration::from_secs(
                file.nil_delay_secs
                    .unwrap_or(IdlePolicy::default().nil_delay.as_secs()),
            ),
        };
        // THE production constructors: App keys / PAT enter here and stay
        // in the in-process credential provider (refresh included). Jobs
        // receive JIT configs, never this material.
        let client = match file.auth.resolve()? {
            ResolvedAuth::App(app) => {
                let auth = load_app_auth(&app)?;
                ScaleSetClient::new_with_app(&file.scope_url, &auth, system_info(0), retry.clone())
                    .context("connect scale-set admin client (App)")?
            }
            ResolvedAuth::Pat(source) => {
                let token = load_pat(&source)?;
                ScaleSetClient::new_with_pat(&file.scope_url, &token, system_info(0), retry.clone())
                    .context("connect scale-set admin client (PAT)")?
            }
        };
        let state_db = file.state_db.clone().unwrap_or(defaults.state_db.clone());
        let ledger_path = resolve_ledger_path(file.ledger_path.as_deref(), &defaults.ledger_path)?;
        let worker_state_dir = file
            .worker_state_dir
            .clone()
            .unwrap_or_else(|| defaults.config_dir.join("scaleset-workers"));
        // The set id is unknown until registration reconciles; the id 0
        // below only seeds the pre-registration identity block and is
        // replaced before the first poll.
        let lane_config = LaneConfig {
            scale_set_id: 0,
            profile: profile.clone(),
            state_root: worker_state_dir,
            ready_attempts: file.ready_attempts.unwrap_or(DEFAULT_READY_ATTEMPTS),
            sweep_interval: Duration::from_secs(
                file.sweep_interval_secs
                    .unwrap_or(DEFAULT_SWEEP_INTERVAL_SECS),
            ),
        };
        Ok(Self {
            plan: file.registration_plan(),
            client,
            system_info: system_info(0),
            retry,
            idle,
            owner: file.owner.clone(),
            profile,
            lane_config,
            state_db,
            ledger_path,
            admission: file.admission,
            runner: Some(runner),
            hook: Some(hook),
            metrics: Metrics::new(),
            running: None,
            reconciled: None,
            adopt_report: None,
        })
    }

    /// [`ScaleSetDaemon::open`] with the production Docker seams.
    pub fn open_production(config_path: &Path, defaults: &DaemonDefaults) -> Result<Self> {
        Self::open(
            config_path,
            defaults,
            Box::new(crate::executor::ProcessCommandRunner),
            Box::new(crate::scaleset::worker::DockerToolContentHook),
        )
    }

    /// Adapter metrics handle (shared with the loop + lane).
    #[must_use]
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Network startup: registration reconcile → worker adoption →
    /// session creation → loop construction. Fails fast so the daemon
    /// retries the pass before supervising native slots.
    ///
    /// (Upstream-drift protection lives in the fixture-manifest verifier
    /// [`require_pin`][crate::scaleset::require_pin], which gates tests and
    /// the canary — the daemon runs the compiled pin, so there is no second
    /// pin to compare at startup.)
    pub async fn start(&mut self) -> Result<StartReport> {
        let reconciled = reconcile_registration(&self.client, &self.plan)
            .await
            .context("reconcile scale-set registration")?;
        let set_id = reconciled.set.id;
        self.client
            .set_system_info(self.system_info_for(set_id))
            .await;
        self.lane_config.scale_set_id = set_id;

        let mut lane = DaemonWorkerLane::open(
            self.client.clone(),
            self.lane_config.clone(),
            &self.state_db,
            &self.ledger_path,
            self.runner
                .take()
                .context("scale-set lane already started")?,
            self.hook.take().context("scale-set lane already started")?,
        )
        .context("open scale-set worker lane")?;
        let adopt_report = lane
            .adopt_live_workers()
            .context("adopt scale-set workers")?;
        tracing::info!(
            set_id,
            adopted = adopt_report.adopted,
            failed = adopt_report.failed,
            resumed_cleanup = adopt_report.resumed_cleanup,
            awaiting_provision = adopt_report.awaiting_provision,
            "scale-set workers adopted"
        );

        let cursors = SessionStore::open(&self.state_db).context("open scale-set session store")?;
        if let Ok(Some(existing_session)) = cursors.get(set_id) {
            tracing::info!(
                set_id,
                session_id = %existing_session.session_id,
                "cleaning up previous scale-set session before acquiring new session"
            );
            let _ = self
                .client
                .actions_service_request(
                    reqwest::Method::DELETE,
                    &format!(
                        "/_apis/runtime/runnerscalesets/{set_id}/sessions/{}",
                        existing_session.session_id
                    ),
                    &[],
                    None,
                )
                .await;
        }

        let mut session_attempt = 0;
        let session = loop {
            match crate::scaleset::MessageSessionClient::create(&self.client, set_id, &self.owner)
                .await
            {
                Ok(s) => break s,
                Err(error) if session_attempt < 3 && error.to_string().contains("409 Conflict") => {
                    session_attempt += 1;
                    tracing::warn!(
                        set_id,
                        attempt = session_attempt,
                        "session conflict encountered, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                Err(error) => {
                    return Err(anyhow::anyhow!("create scale-set message session: {error}"))
                }
            }
        };
        let queue = ClientSession::new(session.clone());
        let ledger = SharedLedger::open(&self.ledger_path).context("open shared permit ledger")?;
        let processor = Processor::new(
            queue.clone(),
            ledger,
            lane,
            DemandStore::open_with_admission(&self.state_db, self.admission.clone())
                .context("open scale-set demand store")?,
            AcquireBatchStore::open(&self.state_db).context("open acquire-batch store")?,
            ProvisionIntentStore::open(&self.state_db).context("open provision-intent store")?,
            self.metrics.clone(),
            ProcessorConfig {
                scale_set_id: set_id,
                images: ProvisionImages {
                    runner_digest: self.profile.runner().digest().to_owned(),
                    dind_digest: self.profile.dind().digest().to_owned(),
                },
                max_acquire_batch: MAX_ACQUIRE_BATCH,
            },
        );
        let listener = Listener::new(
            queue,
            processor,
            cursors,
            self.metrics.clone(),
            LoopConfig {
                scale_set_id: set_id,
                retry: self.retry.clone(),
                idle: self.idle.clone(),
            },
        );
        let session_id = session.session().await.session_id.clone();
        self.running = Some(Running { listener, session });
        self.reconciled = Some(reconciled);
        self.adopt_report = Some(adopt_report.clone());
        Ok(StartReport {
            scale_set_id: set_id,
            session_id,
            adopt_report,
        })
    }

    /// Run the poll→Scale→ACK loop until `shutdown` is set, then close the
    /// session and adopt-or-fail every worker. Only returns on shutdown
    /// (transient failures back off inside the loop, never exit).
    pub async fn run(&mut self, shutdown: &AtomicBool) -> Result<AdapterReport> {
        let running = self
            .running
            .as_mut()
            .context("scale-set lane runs only after start")?;
        running.listener.run(shutdown).await;
        // Graceful shutdown: stop taking work (loop exited), close the
        // session, then triage workers. A failed session close is traced
        // but never fatal: the server reaps expired sessions, and the
        // workers below are the state that must converge.
        if let Err(error) = running.session.close().await {
            tracing::warn!(
                error = error.to_string(),
                "scale-set session close failed; server-side expiry will reap it"
            );
        }
        let shutdown_report = running
            .listener
            .processor_mut()
            .lane_mut()
            .shutdown_pass()
            .context("scale-set shutdown pass")?;
        tracing::info!(
            adopted_across_restart = shutdown_report.adopted_across_restart,
            failed = shutdown_report.failed,
            recorded_total = shutdown_report.recorded_total,
            "scale-set lane shut down"
        );
        Ok(AdapterReport {
            scale_set_id: self.lane_config.scale_set_id,
            adopt_report: self.adopt_report.clone().unwrap_or_default(),
            shutdown_report,
            metrics: self.metrics.snapshot(),
        })
    }

    /// Whether [`ScaleSetDaemon::start`] completed.
    #[must_use]
    pub fn started(&self) -> bool {
        self.running.is_some()
    }

    /// The reconciled registration, once [`ScaleSetDaemon::start`] ran.
    #[must_use]
    pub fn reconciled(&self) -> Option<&ReconciledSet> {
        self.reconciled.as_ref()
    }

    fn system_info_for(&self, scale_set_id: i32) -> SystemInfo {
        SystemInfo {
            scale_set_id,
            ..self.system_info.clone()
        }
    }
}

/// Build the `User-Agent` identity block for the admin client.
fn system_info(scale_set_id: i32) -> SystemInfo {
    SystemInfo {
        system: "velnor".to_owned(),
        version: ADAPTER_VERSION.to_owned(),
        commit_sha: env!("CARGO_PKG_VERSION").to_owned(),
        scale_set_id,
        subsystem: "scaleset-listener".to_owned(),
    }
}

/// Resolve the lane ledger without permitting a second host-wide ledger.
///
/// The daemon receives the already-resolved host ledger from `runner.rs`.
/// A config override is therefore valid only when it names that exact path;
/// otherwise the Scale Set lane could reserve capacity outside the native
/// controller's ledger.
fn resolve_ledger_path(configured: Option<&Path>, host_ledger: &Path) -> Result<PathBuf> {
    let Some(configured) = configured else {
        return Ok(host_ledger.to_path_buf());
    };
    if configured != host_ledger {
        anyhow::bail!(
            "scale-set ledger path {} diverges from the host-wide permit ledger {}",
            configured.display(),
            host_ledger.display()
        );
    }
    Ok(host_ledger.to_path_buf())
}

/// What [`ScaleSetDaemon::start`] established.
#[derive(Debug, Clone)]
pub struct StartReport {
    pub scale_set_id: i32,
    pub session_id: String,
    pub adopt_report: AdoptReport,
}

/// What one [`ScaleSetDaemon::run`] did, end to end.
#[derive(Debug, Clone)]
pub struct AdapterReport {
    pub scale_set_id: i32,
    pub adopt_report: AdoptReport,
    pub shutdown_report: ShutdownReport,
    pub metrics: crate::scaleset::MetricSnapshot,
}

/// True when the daemon args enable the scale-set lane.
#[must_use]
pub fn lane_configured(scale_set_config: Option<&Path>) -> bool {
    scale_set_config.is_some()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    fn write_config(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-scaleset-config-{name}-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, body).unwrap();
        path
    }

    fn app_body(key_line: &str) -> String {
        format!(
            "scope_url = \"https://github.com/octo-org\"\n\
             owner = \"octo-org\"\n\
             group_name = \"velnor\"\n\
             set_name = \"velnor-set\"\n\
             labels = [\"velnor\", \"linux\"]\n\
             [admission]\n\
             owner = [\"octo-org\"]\n\
             repository = [\"octo-org/velnor\"]\n\
             ref = [\"main\"]\n\
             source = [\"octo-org/velnor\"]\n\
             workflow = [\".github/workflows/ci.yml\"]\n\
             event = [\"push\"]\n\
             [auth.app]\n\
             client_id = \"Iv1.abc\"\n\
             installation_id = 42\n\
             {key_line}\n"
        )
    }

    #[test]
    fn app_config_parses_and_resolves() {
        let path = write_config(
            "app",
            &app_body("private_key_env = \"NEVER_SET_VELNOR_TEST\""),
        );
        let config = load_file_config(&path).unwrap();
        assert_eq!(config.owner, "octo-org");
        assert_eq!(config.labels, vec!["velnor".to_owned(), "linux".into()]);
        assert!(matches!(
            config.auth.resolve().unwrap(),
            ResolvedAuth::App(_)
        ));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn config_rejects_github_hosted_selector_as_a_self_hosted_label() {
        let body = app_body("private_key_env = \"NEVER_SET_VELNOR_TEST\"").replace(
            "labels = [\"velnor\", \"linux\"]",
            "labels = [\"UbUnTu-24.04\"]",
        );
        let path = write_config("reserved-label", &body);

        let error = load_file_config(&path).unwrap_err().to_string();

        assert!(
            error.contains("Velnor self-hosted label 'UbUnTu-24.04'"),
            "{error}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn pat_config_parses_and_resolves() {
        let body = "scope_url = \"https://github.com/octo-org\"\n\
             owner = \"octo-org\"\n\
             group_id = 3\n\
             set_id = 7\n\
             [admission]\n\
             owner = [\"octo-org\"]\n\
             repository = [\"octo-org/velnor\"]\n\
             ref = [\"main\"]\n\
             source = [\"octo-org/velnor\"]\n\
             workflow = [\".github/workflows/ci.yml\"]\n\
             event = [\"push\"]\n\
             [auth.pat]\n\
             token_env = \"NEVER_SET_VELNOR_TEST\"\n";
        let path = write_config("pat", body);
        let config = load_file_config(&path).unwrap();
        assert_eq!(config.group_id, Some(3));
        assert!(matches!(
            config.auth.resolve().unwrap(),
            ResolvedAuth::Pat(_)
        ));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn config_rejects_ambiguity_and_gaps() {
        // Both app and PAT.
        let both = app_body("private_key_env = \"X\"") + "[auth.pat]\ntoken_env = \"Y\"\n";
        let path = write_config("both", &both);
        assert!(load_file_config(&path).is_err());
        std::fs::remove_file(&path).unwrap();

        // Neither app nor PAT.
        let neither = "scope_url = \"https://github.com/octo-org\"\n\
             owner = \"octo-org\"\n\
             group_id = 3\n\
             set_id = 7\n\
             [auth]\n";
        let path = write_config("neither", neither);
        assert!(load_file_config(&path).is_err());
        std::fs::remove_file(&path).unwrap();

        // Both set id and set name.
        let path = write_config(
            "both-sets",
            "scope_url = \"https://github.com/octo-org\"\n\
             owner = \"octo-org\"\n\
             group_id = 3\n\
             set_id = 7\n\
             set_name = \"x\"\n\
             [auth.pat]\n\
             token_env = \"X\"\n",
        );
        assert!(load_file_config(&path).is_err());
        std::fs::remove_file(&path).unwrap();

        // No group at all.
        let path = write_config(
            "no-group",
            "scope_url = \"https://github.com/octo-org\"\n\
             owner = \"octo-org\"\n\
             set_id = 7\n\
             [auth.pat]\n\
             token_env = \"X\"\n",
        );
        assert!(load_file_config(&path).is_err());
        std::fs::remove_file(&path).unwrap();

        // Zero ready attempts.
        let path = write_config(
            "zero-ready",
            "scope_url = \"https://github.com/octo-org\"\n\
             owner = \"octo-org\"\n\
             group_id = 3\n\
             set_id = 7\n\
             ready_attempts = 0\n\
             [auth.pat]\n\
             token_env = \"X\"\n",
        );
        assert!(load_file_config(&path).is_err());
        std::fs::remove_file(&path).unwrap();

        // Missing file.
        assert!(load_file_config(Path::new("/nonexistent/velnor-scaleset-test.toml")).is_err());
    }

    #[test]
    fn credential_source_requires_exactly_one() {
        assert!(exactly_one_source(Some(PathBuf::from("/k")), Some("E".into()), "f", "e").is_err());
        assert!(exactly_one_source(None, None, "f", "e").is_err());
        assert!(exactly_one_source(None, Some(String::new()), "f", "e").is_err());
        assert!(exactly_one_source(Some(PathBuf::from("/k")), None, "f", "e").is_ok());
        assert!(exactly_one_source(None, Some("E".into()), "f", "e").is_ok());
    }

    #[test]
    fn lane_configured_is_explicit() {
        assert!(!lane_configured(None));
        assert!(lane_configured(Some(Path::new(
            "/etc/velnor/scaleset.toml"
        ))));
    }

    #[test]
    fn scale_set_ledger_must_match_host_ledger() {
        let host = Path::new("/var/lib/velnor/permit-ledger.db");
        assert_eq!(resolve_ledger_path(None, host).unwrap(), host.to_path_buf());
        assert_eq!(
            resolve_ledger_path(Some(host), host).unwrap(),
            host.to_path_buf()
        );
        let error =
            resolve_ledger_path(Some(Path::new("/tmp/other-permit-ledger.db")), host).unwrap_err();
        assert!(error.to_string().contains("diverges"));
    }
}

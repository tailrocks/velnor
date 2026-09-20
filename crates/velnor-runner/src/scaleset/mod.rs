//! Scale-set adapter (D1 parts A + B + C + D): protocol + worker lane +
//! §5.1 loop + daemon wiring.
//!
//! Rust port of the `actions/scaleset` wire protocol at
//! [`upstream_pin::UPSTREAM_COMMIT`]: admin-plane client, message-session
//! client (long poll, ACK, AcquireJobs, JIT config), credential chain, and
//! recorded fixtures (part A), the homogeneous worker lane
//! ([`worker`]: pinned official runner + private DinD, supervision, owned
//! cleanup) and the shared [`allocator`] binding to the ONE host-wide
//! `max_jobs=N` ledger (part B), the poll→Scale→ACK loop with its durable
//! demand queue, intent stores, shared-grant capacity port, population
//! convergence, and reconcile paths (part C), and the daemon wiring (part
//! D): key material ([`key_material`]), registration reconciliation
//! ([`registration`]), the production [`WorkerLane`][converge::WorkerLane]
//! ([`lane`]), the shared-ledger adapter ([`shared_ledger`]), and the
//! [`ScaleSetDaemon`][daemon::ScaleSetDaemon] the `velnor-runner` daemon
//! supervises next to its native slots.

pub mod allocator;
pub mod backoff;
pub mod capacity;
pub mod client;
pub mod config;
pub mod converge;
pub mod credentials;
pub mod daemon;
pub mod demand;
pub mod errors;
pub mod fixtures;
pub mod intents;
pub mod key_material;
pub mod lane;
pub mod listener;
pub mod metrics;
pub mod reconcile;
pub mod registration;
pub mod scale;
pub mod session;
pub mod shared_ledger;
pub mod upstream_pin;
pub mod worker;

pub use allocator::{AllocatorError, ScaleSetAllocator, ScaleSetPermitGuard};
pub use backoff::{IdlePolicy, PollOutcomeClass, RetryPolicy};
pub use capacity::{
    advertise_free, reserve_for_offer, AcquireOutcome as LedgerAcquireOutcome, CapacityLedger,
    LedgerError, LedgerHolder, LedgerLane, LedgerPermitState, MemLedger, ReconcileReport,
    ReserveError, ReserveOutcome,
};
pub use client::{ScaleSetClient, SystemInfo};
pub use config::{GitHubConfig, GitHubScope};
pub use converge::{PopulationDecision, ProvisionImages, WorkerLane};
pub use credentials::{ActionsAuth, FnJwtProvider, GitHubAppAuth, JwtProvider, PemJwtProvider};
pub use daemon::{
    lane_configured, load_file_config, AdapterReport, AuthFileConfig, DaemonDefaults,
    ScaleSetDaemon, ScaleSetFileConfig, StartReport,
};
pub use demand::{
    classify_offer, grant_oldest, DemandState, DemandStore, OfferTrust, SubmitOutcome,
};
pub use errors::{ActionsApiException, RequestFailure, ScaleSetError, ScaleSetFault};
pub use fixtures::{
    verify_redaction, DemandSeed, DemandSeedRow, FixtureManifest, Fixtures, PollTranscript,
    TranscriptPoll, FIXTURE_HOST, REDACTED,
};
pub use intents::{
    jit_fingerprint, labels_hash, mint_batch_id, parse_permit_holder, permit_holder,
    provision_operation_id, provision_ownership_id, reconcile_returned_ids, runner_name,
    stable_i64, AcquireBatch, AcquireBatchStore, BatchState, ProvisionIntent, ProvisionIntentStore,
};
pub use key_material::{load_app_auth, load_pat, AppKeyConfig, KeySource, ScaleSetAuthConfig};
pub use lane::{
    AdoptReport, DaemonWorkerLane, LaneConfig, LaneError, ShutdownReport, WorkerRegistry,
};
pub use listener::{ClientSession, Listener, ListenerError, LoopConfig, LoopSession, SessionStore};
pub use listener::{SessionCursor, INITIAL_MESSAGE_ID};
pub use metrics::{MetricSnapshot, Metrics};
pub use reconcile::{
    idle_poll, startup, unknown_event, IdleReport, StartupReport, UNCERTAIN_REACQUIRE_AFTER,
};
pub use registration::{reconcile_registration, ReconciledSet, RegistrationPlan};
pub use scale::ScaleOutcome;
pub use scale::{
    Processor, ProcessorConfig, QueueSession, ScaleError, ScaleKind, MAX_ACQUIRE_BATCH,
};
pub use session::{parse_message_response, MessageSessionClient, ParsedMessage};
pub use shared_ledger::SharedLedger;
pub use upstream_pin::{require_pin, UPSTREAM_COMMIT, UPSTREAM_REPO};

use anyhow::Result;

/// Adapter configuration. Credentials live in the client (in-process refresh);
/// job payloads never carry App keys.
#[derive(Debug, Clone)]
pub struct Config {
    /// Enterprise, org, or repository URL, e.g. `https://github.com/octo-org`.
    pub github_config_url: String,
    /// Scale-set ID from `GetRunnerScaleSetByID` or the set name lookup.
    pub scale_set_id: i32,
    /// Session owner name (upstream passes the org/login here).
    pub owner: String,
    /// Auth material: PAT or GitHub App (+ optional custom JWT provider).
    pub auth: ActionsAuth,
    /// Identity block serialized into the `User-Agent` JSON.
    pub system_info: SystemInfo,
    /// Retry policy; defaults mirror upstream (`retryMax=4`, 30s wait cap).
    pub retry: RetryPolicy,
}

/// Build an authenticated admin-plane client from adapter config.
pub fn connect(config: &Config) -> Result<ScaleSetClient> {
    ScaleSetClient::new(
        &config.github_config_url,
        config.auth.clone(),
        config.system_info.clone(),
        config.retry.clone(),
    )
}

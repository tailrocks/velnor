//! Journal-backed logical consumer detachment and bounded action adoption.
//!
//! GitHub cancellation is a logical-run operation. This coordinator keeps
//! that operation separate from physical action lifetime: a detached action
//! is retained only within a bounded window, and every physical termination
//! goes through one durable claim and one executor hook.

use std::{collections::VecDeque, fmt, path::Path};

use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use velnor_action_model::{
    ActionKey, ActionState, CanonicalizationError, Clock, Digest, DigestError, LogicalInstant,
    ProducerLease,
};
use velnor_model::{
    InvalidTelemetry, TelemetryEnvelope, TelemetryEnvelopeInput, TelemetryEvent, TelemetryFields,
    TelemetryLane, Timestamp,
};

use crate::{ActionRecord, JournalError, LeaseManager};

/// Lower bound for speculative retention.
pub const MIN_RETENTION_MS: u64 = 30_000;
/// Upper bound for speculative retention.
pub const MAX_RETENTION_MS: u64 = 60_000;
/// Default speculative retention.
pub const DEFAULT_RETENTION_MS: u64 = 45_000;
const TERMINATION_RETRY_MS: u64 = 1_000;

/// Supersession feature configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersessionConfig {
    /// Feature switch. The task ships dark until the later default-on cutover.
    #[serde(default)]
    pub enabled: bool,
    /// Bounded retention window for detached adoptable actions.
    #[serde(default = "default_retention_ms")]
    pub retention_ms: u64,
}

const fn default_retention_ms() -> u64 {
    DEFAULT_RETENTION_MS
}

impl Default for SupersessionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_ms: DEFAULT_RETENTION_MS,
        }
    }
}

impl SupersessionConfig {
    /// Construct and validate a configuration.
    pub fn new(enabled: bool, retention_ms: u64) -> Result<Self, SupersessionError> {
        let config = Self {
            enabled,
            retention_ms,
        };
        config.validate()?;
        Ok(config)
    }

    /// Parse `[supersession]` from TOML. An absent table means defaults.
    pub fn parse_toml(text: &str) -> Result<Self, SupersessionError> {
        #[derive(Debug, Deserialize, Default)]
        struct File {
            supersession: Option<SupersessionConfig>,
        }

        let file: File = toml::from_str(text)
            .map_err(|error| SupersessionError::InvalidConfig(error.to_string()))?;
        let config = file.supersession.unwrap_or_default();
        config.validate()?;
        Ok(config)
    }

    /// Reject values outside the design's 30–60 second bound.
    pub fn validate(&self) -> Result<(), SupersessionError> {
        if !(MIN_RETENTION_MS..=MAX_RETENTION_MS).contains(&self.retention_ms) {
            return Err(SupersessionError::InvalidConfig(format!(
                "[supersession] retention_ms must be between {MIN_RETENTION_MS} and {MAX_RETENTION_MS}"
            )));
        }
        Ok(())
    }
}

/// Failure from the supersession coordinator.
#[derive(Debug, Error)]
pub enum SupersessionError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Canonical(#[from] CanonicalizationError),
    #[error(transparent)]
    Digest(#[from] DigestError),
    #[error("invalid supersession configuration: {0}")]
    InvalidConfig(String),
    #[error("consumer run ID must not be empty")]
    InvalidRunId,
    #[error("action termination is already in progress")]
    TerminationClaimed,
    #[error("physical action termination failed: {0}")]
    Terminator(String),
    #[error("action is blocked by trust revocation: {reason}")]
    TrustRevoked { reason: String },
}

/// Reason passed to the physical executor termination hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KillReason {
    NoConsumers,
    RetentionExpired,
    NonAdoptable,
    FailedAction,
    TrustRevoked,
}

impl fmt::Display for KillReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoConsumers => "no_consumers",
            Self::RetentionExpired => "retention_expired",
            Self::NonAdoptable => "non_adoptable",
            Self::FailedAction => "failed_action",
            Self::TrustRevoked => "trust_revoked",
        })
    }
}

/// The only physical termination seam used by the coordinator.
pub trait PhysicalActionTerminator {
    /// Kill and reap the physical action identified by its immutable key.
    ///
    /// The coordinator may call this more than once after a crash or an
    /// ambiguous error. Implementations must therefore make termination
    /// idempotent for the action key (and reason) they receive.
    fn terminate(&mut self, action: &ActionKey, reason: KillReason) -> Result<(), String>;
}

/// Result of admitting a logical run to an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// No reusable producer was available; the caller may execute using this lease.
    Started { lease: ProducerLease },
    /// The run joined an already-running physical action.
    AdoptedRunning { record: ActionRecord },
    /// The run adopted an immutable completed result.
    AdoptedComplete { record: ActionRecord },
    /// Another producer owns the action; wait for its lifecycle result.
    Waiting,
}

enum Candidate {
    AdoptedRunning { record: ActionRecord },
    AdoptedComplete { record: ActionRecord },
    NeedsProducer { consumer_inserted: bool },
}

/// Result of detaching one logical run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetachOutcome {
    /// The run was not attached; no physical state changed.
    NotAttached,
    /// Other logical consumers still need the action.
    Continued { live_consumers: u64 },
    /// The last consumer detached and bounded retention began.
    Retained { retained_until: LogicalInstant },
    /// A physical termination was requested immediately.
    Terminated { reason: KillReason },
    /// No physical action record exists for the detached consumer.
    NoPhysicalAction,
}

/// Result of processing due retention entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReapReport {
    pub reaped: u64,
    pub skipped_live: u64,
}

enum TerminationClaim {
    Claimed,
    AlreadyInFlight,
    /// Physical termination is known complete; only durable finalization
    /// remains. The executor must not be called again.
    FinalizationPending,
    BlockedByConsumers(u64),
}

enum RetentionClaim {
    Claimed { reason: KillReason },
    AlreadyInFlight,
    LiveConsumers(u64),
    SkippedTerminalState,
    Missing,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TerminationPhase {
    Unstarted = 0,
    InFlight = 2,
    Complete = 1,
}

#[derive(Clone, Copy)]
struct PendingTermination {
    reason: KillReason,
    /// `0` is unstarted, `2` means termination is in flight or ambiguous, and
    /// `1` means the executor completed. Phase 2 is retried through the
    /// idempotent immutable-key terminator seam.
    phase: TerminationPhase,
}

impl PendingTermination {
    fn bypasses_consumers(self) -> bool {
        self.reason == KillReason::TrustRevoked
    }
}

/// Supersession telemetry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupersessionEventKind {
    SupersessionAdopted,
    ConsumerDetached,
    RetainedThenReaped,
    RetentionKillSkipped,
    TrustRevoked,
}

impl SupersessionEventKind {
    fn telemetry_event(self) -> TelemetryEvent {
        match self {
            Self::SupersessionAdopted => TelemetryEvent::SupersessionAdopted,
            Self::ConsumerDetached => TelemetryEvent::ConsumerDetached,
            Self::RetainedThenReaped => TelemetryEvent::RetainedThenReaped,
            Self::RetentionKillSkipped => TelemetryEvent::RetentionKillSkipped,
            Self::TrustRevoked => TelemetryEvent::TrustRevoked,
        }
    }
}

/// Secret-safe observation emitted after a supersession transition commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersessionTelemetryEvent {
    pub action_key_digest: Digest,
    pub run_id: Option<String>,
    pub at: LogicalInstant,
    pub kind: SupersessionEventKind,
    pub live_consumers: u64,
    pub reason: Option<String>,
    pub retained_until: Option<LogicalInstant>,
}

impl SupersessionTelemetryEvent {
    /// Convert the observation into the shared secret-safe envelope.
    pub fn into_envelope(
        &self,
        repo: &str,
        trust_domain: &str,
        ts_logical: u64,
    ) -> Result<TelemetryEnvelope, InvalidTelemetry> {
        let mut fields = std::collections::BTreeMap::from([(
            "live_consumers".to_owned(),
            serde_json::json!(self.live_consumers),
        )]);
        if let Some(reason) = &self.reason {
            fields.insert("reason".to_owned(), serde_json::json!(reason));
        }
        if let Some(deadline) = self.retained_until {
            fields.insert(
                "retained_until_ms".to_owned(),
                serde_json::json!(deadline.as_millis()),
            );
        }
        TelemetryEnvelope::new(TelemetryEnvelopeInput {
            run_id: self.run_id.as_deref().unwrap_or("system"),
            action_key_digest: Some(self.action_key_digest.as_str()),
            lane: TelemetryLane::Velnor,
            repo,
            trust_domain,
            event: self.kind.telemetry_event(),
            ts_logical,
            ts_wall: Timestamp::now(),
            fields: TelemetryFields::new(fields)?,
        })
    }
}

type TelemetrySink = Box<dyn FnMut(&SupersessionTelemetryEvent) + Send>;

/// Controller-facing journal coordinator.
pub struct SupersessionCoordinator<C: Clock> {
    manager: LeaseManager<C>,
    config: SupersessionConfig,
    telemetry_sink: Option<TelemetrySink>,
    pending_telemetry: VecDeque<SupersessionTelemetryEvent>,
}

impl<C: Clock> SupersessionCoordinator<C> {
    /// Open a coordinator over one durable action journal.
    pub fn open(
        path: impl AsRef<Path>,
        clock: C,
        config: SupersessionConfig,
    ) -> Result<Self, SupersessionError> {
        config.validate()?;
        Self::from_manager(LeaseManager::open(path, clock)?, config)
    }

    /// Build a coordinator around an existing lease manager.
    pub fn from_manager(
        manager: LeaseManager<C>,
        config: SupersessionConfig,
    ) -> Result<Self, SupersessionError> {
        config.validate()?;
        let mut coordinator = Self {
            manager,
            config,
            telemetry_sink: None,
            pending_telemetry: VecDeque::new(),
        };
        coordinator.recover_incomplete_terminations()?;
        Ok(coordinator)
    }

    /// Borrow the immutable configuration.
    #[must_use]
    pub fn config(&self) -> &SupersessionConfig {
        &self.config
    }

    /// Borrow the underlying lease manager for executor integration.
    #[must_use]
    pub fn manager(&self) -> &LeaseManager<C> {
        &self.manager
    }

    /// Mutably borrow the underlying lease manager for producer lifecycle updates.
    #[must_use]
    pub fn manager_mut(&mut self) -> &mut LeaseManager<C> {
        &mut self.manager
    }

    /// Install a telemetry adapter. Events are retained until an adapter exists.
    pub fn set_telemetry_sink(
        &mut self,
        sink: impl FnMut(&SupersessionTelemetryEvent) + Send + 'static,
    ) {
        self.telemetry_sink = Some(Box::new(sink));
        let pending = self.pending_telemetry.drain(..).collect::<Vec<_>>();
        for event in pending {
            self.emit_telemetry(event);
        }
    }

    /// Drain events for polling adapters.
    pub fn drain_telemetry(&mut self) -> Vec<SupersessionTelemetryEvent> {
        self.pending_telemetry.drain(..).collect()
    }

    /// Attach a run, adopting only an identical running or complete action.
    fn attach_or_adopt(
        &mut self,
        action: &ActionKey,
        run_id: &str,
    ) -> Result<Candidate, SupersessionError> {
        if run_id.is_empty() {
            return Err(SupersessionError::InvalidRunId);
        }
        let digest = action.digest()?;
        let latest = self.manager.latest_action(action)?;
        if self.config.enabled && action.execution_policy.adoptable {
            if let Some(record) = latest.clone().filter(is_adoptable_state) {
                let adopted = match record.state {
                    ActionState::Running => {
                        self.attach_and_clear_retention(&digest, run_id, true)?
                    }
                    ActionState::Complete => {
                        self.attach_and_clear_retention(&digest, run_id, false)?
                    }
                    _ => unreachable!(),
                };
                if adopted {
                    self.emit_telemetry(SupersessionTelemetryEvent {
                        action_key_digest: digest,
                        run_id: Some(run_id.to_owned()),
                        at: self.manager.clock().now(),
                        kind: SupersessionEventKind::SupersessionAdopted,
                        live_consumers: self.manager.live_consumer_count(action)?,
                        reason: Some(match record.state {
                            ActionState::Running => "running".to_owned(),
                            ActionState::Complete => "complete".to_owned(),
                            _ => unreachable!(),
                        }),
                        retained_until: None,
                    });
                    return Ok(match record.state {
                        ActionState::Running => Candidate::AdoptedRunning { record },
                        ActionState::Complete => Candidate::AdoptedComplete { record },
                        _ => unreachable!(),
                    });
                }
            }
        }
        let consumer_inserted = self.attach_for_admission(&digest, run_id)?;
        Ok(Candidate::NeedsProducer { consumer_inserted })
    }

    /// Attach/adopt and elect one producer through the durable lease.
    pub fn admit(
        &mut self,
        action: &ActionKey,
        run_id: &str,
        owner: impl Into<String>,
        lease_duration_ms: u64,
        heartbeat_every_ms: u64,
    ) -> Result<Admission, SupersessionError> {
        let owner = owner.into();
        match self.attach_or_adopt(action, run_id)? {
            Candidate::AdoptedRunning { record } => Ok(Admission::AdoptedRunning { record }),
            Candidate::AdoptedComplete { record } => Ok(Admission::AdoptedComplete { record }),
            Candidate::NeedsProducer { consumer_inserted } => {
                match self
                    .manager
                    .acquire(action, owner, lease_duration_ms, heartbeat_every_ms)
                {
                    Ok(lease) => Ok(Admission::Started { lease }),
                    Err(JournalError::LeaseBusy { .. }) => {
                        let retry = (|| {
                            let latest = self.manager.latest_action(action)?;
                            if self.config.enabled
                                && action.execution_policy.adoptable
                                && latest.as_ref().is_some_and(is_adoptable_state)
                            {
                                return Ok(match self.attach_or_adopt(action, run_id)? {
                                    Candidate::AdoptedRunning { record } => {
                                        Admission::AdoptedRunning { record }
                                    }
                                    Candidate::AdoptedComplete { record } => {
                                        Admission::AdoptedComplete { record }
                                    }
                                    Candidate::NeedsProducer { .. } => Admission::Waiting,
                                });
                            }
                            Ok(Admission::Waiting)
                        })();
                        match retry {
                            Ok(admission) => Ok(admission),
                            Err(error) => self.cleanup_admission_error(
                                action,
                                run_id,
                                consumer_inserted,
                                error,
                            ),
                        }
                    }
                    Err(error) => self.cleanup_admission_error(
                        action,
                        run_id,
                        consumer_inserted,
                        error.into(),
                    ),
                }
            }
        }
    }

    /// Detach a logical consumer and either retain or terminate the action.
    pub fn detach<T: PhysicalActionTerminator>(
        &mut self,
        action: &ActionKey,
        run_id: &str,
        terminator: &mut T,
    ) -> Result<DetachOutcome, SupersessionError> {
        if run_id.is_empty() {
            return Err(SupersessionError::InvalidRunId);
        }
        let detached = self.manager.detach_consumer(action, run_id)?;
        if !detached {
            return Ok(DetachOutcome::NotAttached);
        }
        let digest = action.digest()?;
        let live_consumers = self.manager.live_consumer_count(action)?;
        self.emit_telemetry(SupersessionTelemetryEvent {
            action_key_digest: digest.clone(),
            run_id: Some(run_id.to_owned()),
            at: self.manager.clock().now(),
            kind: SupersessionEventKind::ConsumerDetached,
            live_consumers,
            reason: None,
            retained_until: None,
        });
        if live_consumers > 0 {
            return Ok(DetachOutcome::Continued { live_consumers });
        }
        let Some(record) = self.manager.latest_action(action)? else {
            return Ok(DetachOutcome::NoPhysicalAction);
        };
        if self.revocation_reason(&digest)?.is_some() {
            let _ = self.terminate(action, KillReason::TrustRevoked, terminator)?;
            return Ok(DetachOutcome::Terminated {
                reason: KillReason::TrustRevoked,
            });
        }
        if !self.config.enabled || !action.execution_policy.adoptable {
            let reason = if action.execution_policy.adoptable {
                KillReason::NoConsumers
            } else {
                KillReason::NonAdoptable
            };
            if !self.terminate(action, reason, terminator)? {
                let live_consumers = self.manager.live_consumer_count(action)?;
                if live_consumers > 0 {
                    return Ok(DetachOutcome::Continued { live_consumers });
                }
            }
            return Ok(DetachOutcome::Terminated { reason });
        }
        if matches!(record.state, ActionState::Failed | ActionState::Abandoned) {
            let _ = self.terminate(action, KillReason::FailedAction, terminator)?;
            return Ok(DetachOutcome::Terminated {
                reason: KillReason::FailedAction,
            });
        }
        if !matches!(record.state, ActionState::Running | ActionState::Complete) {
            return Ok(DetachOutcome::NoPhysicalAction);
        }
        let retained_until = self.schedule_retention(&digest)?;
        Ok(DetachOutcome::Retained { retained_until })
    }

    /// Reap all retention entries whose bounded deadline has elapsed.
    pub fn reap_due<T: PhysicalActionTerminator>(
        &mut self,
        terminator: &mut T,
    ) -> Result<ReapReport, SupersessionError> {
        let now = self.manager.clock().now();
        let now_ms = sqlite_integer(now.as_millis())?;
        let rows = {
            let mut statement = self.manager.journal.connection.prepare(
                "SELECT action_key_digest FROM action_retention WHERE retained_until_ms <= ?1",
            )?;
            let values = statement
                .query_map([now_ms], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            values
        };
        let mut report = ReapReport {
            reaped: 0,
            skipped_live: 0,
        };
        for raw_digest in rows {
            let digest = Digest::parse(raw_digest)?;
            let (action, latest_state) = match self.action_record_for_digest(&digest)? {
                Some(record) => (record.action_key, record.state),
                None => {
                    let action_key_json: Option<String> = self
                        .manager
                        .journal
                        .connection
                        .query_row(
                            "SELECT action_key_json FROM producer_leases
                             WHERE action_key_digest = ?1",
                            [digest.to_string()],
                            |row| row.get(0),
                        )
                        .optional()?;
                    let action_key_json = action_key_json.ok_or_else(|| {
                        SupersessionError::InvalidConfig(format!(
                            "producer lease action key is missing for {digest}"
                        ))
                    })?;
                    crate::verify_stored_action_key(&action_key_json, &digest)?;
                    (
                        serde_json::from_str(&action_key_json).map_err(JournalError::from)?,
                        ActionState::Leased,
                    )
                }
            };
            match self.claim_due_retention(&digest, latest_state, now_ms)? {
                RetentionClaim::LiveConsumers(live_consumers) => {
                    report.skipped_live += 1;
                    self.emit_telemetry(SupersessionTelemetryEvent {
                        action_key_digest: digest,
                        run_id: None,
                        at: now,
                        kind: SupersessionEventKind::RetentionKillSkipped,
                        live_consumers,
                        reason: Some("live_consumer".to_owned()),
                        retained_until: None,
                    });
                }
                RetentionClaim::SkippedTerminalState => {
                    self.emit_telemetry(SupersessionTelemetryEvent {
                        action_key_digest: digest,
                        run_id: None,
                        at: now,
                        kind: SupersessionEventKind::RetentionKillSkipped,
                        live_consumers: 0,
                        reason: Some("failed_or_abandoned".to_owned()),
                        retained_until: None,
                    });
                }
                RetentionClaim::Claimed { reason } => {
                    self.execute_claimed_termination_with_retry(&action, reason, terminator)?;
                    report.reaped += 1;
                    self.emit_telemetry(SupersessionTelemetryEvent {
                        action_key_digest: digest,
                        run_id: None,
                        at: now,
                        kind: SupersessionEventKind::RetainedThenReaped,
                        live_consumers: 0,
                        reason: Some(reason.to_string()),
                        retained_until: None,
                    });
                }
                RetentionClaim::AlreadyInFlight => {}
                RetentionClaim::Missing => {}
            }
        }
        Ok(report)
    }

    /// Return the earliest durable retention deadline.
    pub fn next_retention(&self) -> Result<Option<LogicalInstant>, SupersessionError> {
        let value: Option<i64> = self.manager.journal.connection.query_row(
            "SELECT MIN(retained_until_ms) FROM action_retention",
            [],
            |row| row.get(0),
        )?;
        value
            .map(|value| {
                u64::try_from(value)
                    .map(LogicalInstant::from_millis)
                    .map_err(|_| {
                        SupersessionError::InvalidConfig(
                            "retention deadline is negative".to_owned(),
                        )
                    })
            })
            .transpose()
    }

    /// Wait for the next persisted retention deadline, then reap due actions.
    pub async fn reap_next<T: PhysicalActionTerminator>(
        &mut self,
        terminator: &mut T,
    ) -> Result<ReapReport, SupersessionError> {
        loop {
            let deadline = self.next_retention()?.unwrap_or(LogicalInstant::MAX);
            self.manager.clock().sleep_until(deadline).await;
            let report = self.reap_due(terminator)?;
            if report.reaped > 0 || report.skipped_live > 0 {
                return Ok(report);
            }
        }
    }

    /// Revoke the trust context and terminate immediately, bypassing retention.
    pub fn revoke_trust<T: PhysicalActionTerminator>(
        &mut self,
        action: &ActionKey,
        reason: impl Into<String>,
        terminator: &mut T,
    ) -> Result<(), SupersessionError> {
        let reason = reason.into();
        let digest = action.digest()?;
        let now = self.manager.clock().now();
        self.manager.journal.connection.execute(
            "INSERT OR IGNORE INTO action_trust_revocations(action_key_digest, reason, revoked_at_ms)
             VALUES (?1, ?2, ?3)",
            params![digest.to_string(), reason, sqlite_integer(now.as_millis())?],
        )?;
        self.delete_retention(&digest)?;
        self.emit_telemetry(SupersessionTelemetryEvent {
            action_key_digest: digest.clone(),
            run_id: None,
            at: now,
            kind: SupersessionEventKind::TrustRevoked,
            live_consumers: self.manager.live_consumer_count(action)?,
            reason: Some(reason.clone()),
            retained_until: None,
        });
        let _ = self.terminate(action, KillReason::TrustRevoked, terminator)?;
        Ok(())
    }

    fn schedule_retention(&mut self, digest: &Digest) -> Result<LogicalInstant, SupersessionError> {
        let deadline = self
            .manager
            .clock()
            .now()
            .saturating_add(self.config.retention_ms);
        self.manager.journal.connection.execute(
            "INSERT OR IGNORE INTO action_retention(action_key_digest, retained_until_ms)
             VALUES (?1, ?2)",
            params![digest.to_string(), sqlite_integer(deadline.as_millis())?],
        )?;
        self.manager.clock().wake_expiry();
        let retained_until = self.manager.journal.connection.query_row(
            "SELECT retained_until_ms FROM action_retention WHERE action_key_digest = ?1",
            [digest.to_string()],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(LogicalInstant::from_millis(
            u64::try_from(retained_until).map_err(|_| {
                SupersessionError::InvalidConfig("retention deadline is negative".to_owned())
            })?,
        ))
    }

    fn recover_incomplete_terminations(&mut self) -> Result<(), SupersessionError> {
        let now = self.manager.clock().now();
        let transaction = self
            .manager
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let invalid: Option<i64> = transaction
            .query_row(
                "SELECT completed FROM action_termination_claims
                 WHERE completed NOT IN (0, 1, 2) LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(phase) = invalid {
            return Err(invalid_termination_phase(phase));
        }
        transaction.execute(
            "UPDATE action_termination_claims SET completed = 0
             WHERE completed = 2",
            [],
        )?;
        transaction.execute(
            "INSERT INTO action_retention(action_key_digest, retained_until_ms)
             SELECT action_key_digest, ?1
             FROM action_termination_claims
             WHERE completed IN (0, 1)
             ON CONFLICT(action_key_digest) DO NOTHING",
            [sqlite_integer(now.as_millis())?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn attach_and_clear_retention(
        &mut self,
        digest: &Digest,
        run_id: &str,
        require_active_lease: bool,
    ) -> Result<bool, SupersessionError> {
        let transaction = self
            .manager
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_admission_fence(&transaction, digest)?;
        if require_active_lease {
            let now_ms = sqlite_integer(self.manager.clock.now().as_millis())?;
            let active_lease: Option<i64> = transaction
                .query_row(
                    "SELECT 1 FROM producer_leases
                     WHERE action_key_digest = ?1
                       AND state = 'active' AND expires_at_ms > ?2",
                    params![digest.to_string(), now_ms],
                    |row| row.get(0),
                )
                .optional()?;
            if active_lease.is_none() {
                return Ok(false);
            }
        }
        transaction.execute(
            "INSERT OR IGNORE INTO action_consumers(action_key_digest, run_id)
             VALUES (?1, ?2)",
            params![digest.to_string(), run_id],
        )?;
        transaction.execute(
            "DELETE FROM action_retention WHERE action_key_digest = ?1",
            [digest.to_string()],
        )?;
        transaction.commit()?;
        self.manager.clock().wake_expiry();
        Ok(true)
    }

    fn cleanup_admission_error(
        &mut self,
        action: &ActionKey,
        run_id: &str,
        consumer_inserted: bool,
        error: SupersessionError,
    ) -> Result<Admission, SupersessionError> {
        let lease_abandonable = matches!(
            &error,
            SupersessionError::Journal(JournalError::LeaseAbandonable { .. })
        );
        if consumer_inserted {
            self.manager.detach_consumer(action, run_id)?;
        }
        if lease_abandonable {
            let digest = action.digest()?;
            let phase: Option<i64> = self
                .manager
                .journal
                .connection
                .query_row(
                    "SELECT completed FROM action_termination_claims
                     WHERE action_key_digest = ?1",
                    [digest.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(phase) = phase {
                validate_termination_phase(phase)?;
                return Err(SupersessionError::TerminationClaimed);
            }
        }
        Err(error)
    }

    /// Attach a producer-needed run behind the same durable admission fence
    /// used by adoption. A termination claim must not coexist with a new
    /// consumer that can elect a producer.
    fn attach_for_admission(
        &mut self,
        digest: &Digest,
        run_id: &str,
    ) -> Result<bool, SupersessionError> {
        let transaction = self
            .manager
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_admission_fence(&transaction, digest)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO action_consumers(action_key_digest, run_id)
             VALUES (?1, ?2)",
            params![digest.to_string(), run_id],
        )? == 1;
        transaction.commit()?;
        self.manager.clock().wake_expiry();
        Ok(inserted)
    }

    /// Read revocation and termination state while holding the immediate
    /// transaction that attaches the consumer and clears retention.
    fn check_admission_fence(
        transaction: &rusqlite::Transaction<'_>,
        digest: &Digest,
    ) -> Result<(), SupersessionError> {
        let revoked: Option<String> = transaction
            .query_row(
                "SELECT reason FROM action_trust_revocations
                 WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(reason) = revoked {
            return Err(SupersessionError::TrustRevoked { reason });
        }
        let terminating: Option<i64> = transaction
            .query_row(
                "SELECT completed FROM action_termination_claims
                 WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(phase) = terminating {
            validate_termination_phase(phase)?;
        }
        if terminating.is_some() {
            return Err(SupersessionError::TerminationClaimed);
        }
        Ok(())
    }

    fn delete_retention(&mut self, digest: &Digest) -> Result<(), SupersessionError> {
        self.manager.journal.connection.execute(
            "DELETE FROM action_retention WHERE action_key_digest = ?1",
            [digest.to_string()],
        )?;
        self.manager.clock().wake_expiry();
        Ok(())
    }

    fn restore_retention(
        &mut self,
        digest: &Digest,
        retained_until: LogicalInstant,
    ) -> Result<(), SupersessionError> {
        self.manager.journal.connection.execute(
            "INSERT INTO action_retention(action_key_digest, retained_until_ms)
             VALUES (?1, ?2)
             ON CONFLICT(action_key_digest) DO UPDATE SET retained_until_ms = excluded.retained_until_ms",
            params![digest.to_string(), sqlite_integer(retained_until.as_millis())?],
        )?;
        self.manager.clock().wake_expiry();
        Ok(())
    }

    fn action_record_for_digest(
        &self,
        digest: &Digest,
    ) -> Result<Option<ActionRecord>, SupersessionError> {
        Ok(self.manager.journal.latest()?.remove(digest))
    }

    fn revocation_reason(&self, digest: &Digest) -> Result<Option<String>, SupersessionError> {
        Ok(self
            .manager
            .journal
            .connection
            .query_row(
                "SELECT reason FROM action_trust_revocations WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .optional()?)
    }

    fn terminate<T: PhysicalActionTerminator>(
        &mut self,
        action: &ActionKey,
        reason: KillReason,
        terminator: &mut T,
    ) -> Result<bool, SupersessionError> {
        let digest = action.digest()?;
        let now = self.manager.clock().now();
        match self.claim_termination(&digest, reason, reason == KillReason::TrustRevoked, now)? {
            TerminationClaim::BlockedByConsumers(_) => return Ok(false),
            TerminationClaim::AlreadyInFlight => return Ok(false),
            TerminationClaim::Claimed | TerminationClaim::FinalizationPending => {}
        }
        self.execute_claimed_termination_with_retry(action, reason, terminator)?;
        Ok(true)
    }

    fn claim_termination(
        &mut self,
        digest: &Digest,
        reason: KillReason,
        bypass_consumer_check: bool,
        now: LogicalInstant,
    ) -> Result<TerminationClaim, SupersessionError> {
        let transaction = self
            .manager
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !bypass_consumer_check {
            let live: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM action_consumers WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )?;
            if live > 0 {
                transaction.commit()?;
                return Ok(TerminationClaim::BlockedByConsumers(
                    u64::try_from(live).map_err(|_| {
                        SupersessionError::InvalidConfig("consumer count is negative".to_owned())
                    })?,
                ));
            }
        }
        let claim = claim_termination_row(
            &transaction,
            digest,
            reason,
            sqlite_integer(now.as_millis())?,
        )?;
        transaction.commit()?;
        Ok(claim)
    }

    fn claim_due_retention(
        &mut self,
        digest: &Digest,
        latest_state: ActionState,
        now_ms: i64,
    ) -> Result<RetentionClaim, SupersessionError> {
        let transaction = self
            .manager
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(retained_until) = transaction
            .query_row(
                "SELECT retained_until_ms FROM action_retention WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        else {
            transaction.commit()?;
            return Ok(RetentionClaim::Missing);
        };
        if retained_until > now_ms {
            transaction.commit()?;
            return Ok(RetentionClaim::Missing);
        }
        let pending_termination: Option<PendingTermination> = transaction
            .query_row(
                "SELECT reason, completed FROM action_termination_claims
                 WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .map(|(reason, phase)| {
                validate_termination_phase(phase).and_then(|phase| {
                    parse_kill_reason(&reason).map(|reason| PendingTermination { reason, phase })
                })
            })
            .transpose()?;
        if (latest_state == ActionState::Abandoned
            && pending_termination
                .is_some_and(|pending| pending.phase == TerminationPhase::Complete))
            || (pending_termination.is_none() && latest_state == ActionState::Failed)
        {
            transaction.execute(
                "DELETE FROM action_retention WHERE action_key_digest = ?1",
                [digest.to_string()],
            )?;
            transaction.commit()?;
            return Ok(RetentionClaim::SkippedTerminalState);
        }
        let live: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM action_consumers WHERE action_key_digest = ?1",
            [digest.to_string()],
            |row| row.get(0),
        )?;
        if live > 0
            && pending_termination
                .is_none_or(|pending| pending.phase == TerminationPhase::Unstarted)
            && !pending_termination.is_some_and(PendingTermination::bypasses_consumers)
        {
            if pending_termination.is_some() {
                transaction.execute(
                    "UPDATE action_retention SET retained_until_ms = ?1
                     WHERE action_key_digest = ?2",
                    params![
                        now_ms.saturating_add(sqlite_integer(TERMINATION_RETRY_MS)?),
                        digest.to_string()
                    ],
                )?;
            } else {
                transaction.execute(
                    "DELETE FROM action_retention WHERE action_key_digest = ?1",
                    [digest.to_string()],
                )?;
            }
            transaction.commit()?;
            return Ok(RetentionClaim::LiveConsumers(u64::try_from(live).map_err(
                |_| SupersessionError::InvalidConfig("consumer count is negative".to_owned()),
            )?));
        }
        let reason =
            pending_termination.map_or(KillReason::RetentionExpired, |pending| pending.reason);
        let claim = if let Some(pending) = pending_termination {
            match pending.phase {
                TerminationPhase::Unstarted => TerminationClaim::Claimed,
                TerminationPhase::InFlight => TerminationClaim::AlreadyInFlight,
                TerminationPhase::Complete => TerminationClaim::FinalizationPending,
            }
        } else {
            claim_termination_row(&transaction, digest, reason, now_ms)?
        };
        if matches!(
            claim,
            TerminationClaim::Claimed | TerminationClaim::FinalizationPending
        ) {
            transaction.execute(
                "DELETE FROM action_retention WHERE action_key_digest = ?1",
                [digest.to_string()],
            )?;
        }
        transaction.commit()?;
        Ok(match claim {
            TerminationClaim::Claimed | TerminationClaim::FinalizationPending => {
                RetentionClaim::Claimed { reason }
            }
            TerminationClaim::AlreadyInFlight => RetentionClaim::AlreadyInFlight,
            TerminationClaim::BlockedByConsumers(count) => RetentionClaim::LiveConsumers(count),
        })
    }

    fn execute_claimed_termination<T: PhysicalActionTerminator>(
        &mut self,
        action: &ActionKey,
        reason: KillReason,
        terminator: &mut T,
    ) -> Result<(), SupersessionError> {
        let digest = action.digest()?;
        let start = self.mark_termination_started(&digest)?;
        match start {
            TerminationStart::AlreadyInFlight => return Ok(()),
            TerminationStart::Claimed => {
                if let Err(error) = terminator.terminate(action, reason) {
                    // Phase 2 remains durable. The call may have had an external
                    // side effect before reporting an error, so retry through the
                    // idempotent immutable-key seam instead of resetting to 0.
                    return Err(SupersessionError::Terminator(error));
                }
                self.mark_termination_completed(&digest)?;
            }
            TerminationStart::Complete => {}
        }
        self.delete_retention(&digest)?;
        if let Some(mut record) = self.manager.latest_action(action)? {
            if record.state != ActionState::Abandoned {
                record.state = ActionState::Abandoned;
                self.manager.append_action(&record)?;
            }
        }
        Ok(())
    }

    /// Record that the external termination seam is about to run.
    fn mark_termination_started(
        &mut self,
        digest: &Digest,
    ) -> Result<TerminationStart, SupersessionError> {
        let transaction = self
            .manager
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let phase: Option<i64> = transaction
            .query_row(
                "SELECT completed FROM action_termination_claims
                 WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(phase) = phase else {
            return Err(SupersessionError::InvalidConfig(
                "termination claim disappeared before execution".to_owned(),
            ));
        };
        let phase = validate_termination_phase(phase)?;
        let start = match phase {
            TerminationPhase::Unstarted => {
                transaction.execute(
                    "UPDATE action_termination_claims SET completed = 2
                     WHERE action_key_digest = ?1 AND completed = 0",
                    [digest.to_string()],
                )?;
                TerminationStart::Claimed
            }
            TerminationPhase::InFlight => TerminationStart::AlreadyInFlight,
            TerminationPhase::Complete => TerminationStart::Complete,
        };
        transaction.commit()?;
        Ok(start)
    }

    fn mark_termination_completed(&mut self, digest: &Digest) -> Result<(), SupersessionError> {
        let transaction = self
            .manager
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let phase: i64 = transaction.query_row(
            "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
            [digest.to_string()],
            |row| row.get(0),
        )?;
        match validate_termination_phase(phase)? {
            TerminationPhase::InFlight => {
                transaction.execute(
                    "UPDATE action_termination_claims SET completed = 1
                     WHERE action_key_digest = ?1 AND completed = 2",
                    [digest.to_string()],
                )?;
            }
            TerminationPhase::Complete => {}
            TerminationPhase::Unstarted => {
                return Err(SupersessionError::InvalidConfig(
                    "cannot complete an unstarted termination claim".to_owned(),
                ));
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn execute_claimed_termination_with_retry<T: PhysicalActionTerminator>(
        &mut self,
        action: &ActionKey,
        reason: KillReason,
        terminator: &mut T,
    ) -> Result<(), SupersessionError> {
        let digest = action.digest()?;
        if let Err(error) = self.execute_claimed_termination(action, reason, terminator) {
            // Keep the incomplete durable claim and make it visible to the
            // reaper after a short bounded retry delay.
            let retry_at = self
                .manager
                .clock()
                .now()
                .saturating_add(TERMINATION_RETRY_MS);
            self.restore_retention(&digest, retry_at)?;
            return Err(error);
        }
        Ok(())
    }

    fn emit_telemetry(&mut self, event: SupersessionTelemetryEvent) {
        if let Some(mut sink) = self.telemetry_sink.take() {
            let delivered =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(&event))).is_ok();
            self.telemetry_sink = Some(sink);
            if !delivered {
                self.pending_telemetry.push_back(event);
            }
        } else {
            self.pending_telemetry.push_back(event);
        }
    }
}

fn claim_termination_row(
    transaction: &rusqlite::Transaction<'_>,
    digest: &Digest,
    reason: KillReason,
    now_ms: i64,
) -> Result<TerminationClaim, SupersessionError> {
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
            [digest.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    match existing.map(validate_termination_phase).transpose()? {
        Some(TerminationPhase::Complete) => Ok(TerminationClaim::FinalizationPending),
        Some(TerminationPhase::InFlight) => Ok(TerminationClaim::AlreadyInFlight),
        Some(TerminationPhase::Unstarted) => {
            // A failed hook leaves an incomplete intent. Retrying is safe
            // because executor termination is idempotent for an action key.
            transaction.execute(
                "UPDATE action_termination_claims
                 SET reason = ?1, claimed_at_ms = ?2
                 WHERE action_key_digest = ?3 AND completed = 0",
                params![reason.to_string(), now_ms, digest.to_string()],
            )?;
            Ok(TerminationClaim::Claimed)
        }
        None => {
            transaction.execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, ?2, ?3, 0)",
                params![digest.to_string(), reason.to_string(), now_ms],
            )?;
            Ok(TerminationClaim::Claimed)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TerminationStart {
    Claimed,
    AlreadyInFlight,
    Complete,
}

fn validate_termination_phase(phase: i64) -> Result<TerminationPhase, SupersessionError> {
    match phase {
        0 => Ok(TerminationPhase::Unstarted),
        1 => Ok(TerminationPhase::Complete),
        2 => Ok(TerminationPhase::InFlight),
        _ => Err(invalid_termination_phase(phase)),
    }
}

fn invalid_termination_phase(phase: i64) -> SupersessionError {
    SupersessionError::InvalidConfig(format!(
        "invalid termination claim phase {phase}; expected 0, 1, or 2"
    ))
}

fn is_adoptable_state(record: &ActionRecord) -> bool {
    matches!(record.state, ActionState::Running | ActionState::Complete)
}

fn parse_kill_reason(value: &str) -> Result<KillReason, SupersessionError> {
    match value {
        "no_consumers" => Ok(KillReason::NoConsumers),
        "retention_expired" => Ok(KillReason::RetentionExpired),
        "non_adoptable" => Ok(KillReason::NonAdoptable),
        "failed_action" => Ok(KillReason::FailedAction),
        "trust_revoked" => Ok(KillReason::TrustRevoked),
        other => Err(SupersessionError::InvalidConfig(format!(
            "unknown termination claim reason: {other}"
        ))),
    }
}

fn sqlite_integer(value: u64) -> Result<i64, SupersessionError> {
    i64::try_from(value).map_err(|_| {
        SupersessionError::InvalidConfig("logical value exceeds SQLite integer range".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LeaseStatus;
    use std::{
        collections::{BTreeMap, BTreeSet},
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
    };
    use tempfile::TempDir;
    use velnor_action_model::{ActionTiming, ExecutionPolicy, PlatformIdentity, TrustClass};

    #[derive(Clone, Default)]
    struct TestClock {
        now_ms: Arc<Mutex<u64>>,
        wake: Arc<tokio::sync::Notify>,
    }

    impl TestClock {
        fn advance(&self, millis: u64) {
            *self.now_ms.lock().unwrap() += millis;
            self.wake.notify_one();
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> LogicalInstant {
            LogicalInstant::from_millis(*self.now_ms.lock().unwrap())
        }

        fn sleep_until(
            &self,
            deadline: LogicalInstant,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            let clock = self.clone();
            Box::pin(async move {
                if clock.now() < deadline {
                    clock.wake.notified().await;
                }
            })
        }

        fn wake_expiry(&self) {
            self.wake.notify_one();
        }
    }

    #[derive(Default)]
    struct Spy {
        calls: Vec<(Digest, KillReason)>,
    }

    impl PhysicalActionTerminator for Spy {
        fn terminate(&mut self, action: &ActionKey, reason: KillReason) -> Result<(), String> {
            self.calls
                .push((action.digest().map_err(|error| error.to_string())?, reason));
            Ok(())
        }
    }

    struct FailOnceSpy {
        calls: usize,
        failed: bool,
        reasons: Vec<KillReason>,
    }

    impl PhysicalActionTerminator for FailOnceSpy {
        fn terminate(&mut self, _action: &ActionKey, reason: KillReason) -> Result<(), String> {
            self.calls += 1;
            self.reasons.push(reason);
            if !self.failed {
                self.failed = true;
                Err("transient executor failure".to_owned())
            } else {
                Ok(())
            }
        }
    }

    fn digest(seed: u8) -> Digest {
        Digest::from_bytes(&[seed])
    }

    fn action(seed: u8) -> ActionKey {
        ActionKey {
            command_digest: digest(seed),
            input_root: digest(seed + 1),
            image_digest: digest(seed + 2),
            toolchain_digest: digest(seed + 3),
            platform: PlatformIdentity::new("linux", "x86_64", None),
            environment_digest: digest(seed + 4),
            dependency_outputs: vec![],
            execution_policy: ExecutionPolicy {
                trust_class: TrustClass::Trusted,
                ..Default::default()
            },
        }
    }

    fn record(action: ActionKey, state: ActionState) -> ActionRecord {
        ActionRecord {
            action_key: action,
            state,
            producer_lease_ref: None,
            consumer_run_ids: BTreeSet::new(),
            output_digests: BTreeMap::new(),
            timing: ActionTiming::default(),
            worker_id: Some("worker".into()),
            trust_class: TrustClass::Trusted,
        }
    }

    fn coordinator(clock: TestClock) -> SupersessionCoordinator<TestClock> {
        SupersessionCoordinator::open(
            ":memory:",
            clock,
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn config_enforces_bounded_retention() {
        assert!(SupersessionConfig::parse_toml(
            "[supersession]\nenabled = true\nretention_ms = 29999\n"
        )
        .is_err());
        assert!(SupersessionConfig::parse_toml(
            "[supersession]\nenabled = true\nretention_ms = 60001\n"
        )
        .is_err());
        assert_eq!(
            SupersessionConfig::parse_toml("[supersession]\nenabled = true\n")
                .unwrap()
                .retention_ms,
            DEFAULT_RETENTION_MS
        );
    }

    #[test]
    fn rapid_commit_detaches_then_adopts_without_restart() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock.clone());
        let action = action(1);
        coordinator
            .manager_mut()
            .acquire(&action, "worker-a", 1_000, 250)
            .unwrap();
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        let mut spy = Spy::default();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-a")
            .unwrap();
        assert!(matches!(
            coordinator.detach(&action, "run-a", &mut spy).unwrap(),
            DetachOutcome::Retained { .. }
        ));
        assert!(matches!(
            coordinator
                .admit(&action, "run-b", "worker-b", 1_000, 250)
                .unwrap(),
            Admission::AdoptedRunning { .. }
        ));
        assert!(spy.calls.is_empty());
        assert_eq!(
            coordinator.manager().live_consumer_count(&action).unwrap(),
            1
        );
    }

    #[test]
    fn identical_rerun_elects_one_producer_and_runs_zero_duplicate_commands() {
        let mut coordinator = coordinator(TestClock::default());
        let action = action(5);
        let mut physical_commands = 0;
        let first = coordinator
            .admit(&action, "run-a", "worker-a", 1_000, 250)
            .unwrap();
        if matches!(first, Admission::Started { .. }) {
            physical_commands += 1;
        }
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        assert_eq!(
            coordinator.manager().lease_status(&action).unwrap(),
            Some(LeaseStatus::Active)
        );
        let second = coordinator
            .admit(&action, "run-b", "worker-b", 1_000, 250)
            .unwrap();
        assert!(matches!(second, Admission::AdoptedRunning { .. }));
        if matches!(second, Admission::Started { .. }) {
            physical_commands += 1;
        }
        assert_eq!(physical_commands, 1);
    }

    #[test]
    fn expired_running_record_is_not_adopted_or_left_attached() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock.clone());
        let action = action(21);
        coordinator
            .manager_mut()
            .acquire(&action, "worker-a", 1_000, 250)
            .unwrap();
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        clock.advance(1_000);

        let result = coordinator.admit(&action, "run-b", "worker-b", 1_000, 250);
        assert!(matches!(result, Err(SupersessionError::TerminationClaimed)));
        assert_eq!(
            coordinator.manager().lease_status(&action).unwrap(),
            Some(LeaseStatus::Abandoned)
        );
        assert_eq!(
            coordinator.manager().live_consumer_count(&action).unwrap(),
            0
        );
        assert_eq!(
            coordinator
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Running
        );
    }

    #[test]
    fn abandoned_running_record_is_not_adopted_or_left_attached() {
        let mut coordinator = coordinator(TestClock::default());
        let action = action(22);
        let lease = coordinator
            .manager_mut()
            .acquire(&action, "worker-a", 1_000, 250)
            .unwrap();
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        coordinator.manager_mut().abandon(&lease).unwrap();

        let result = coordinator.admit(&action, "run-b", "worker-b", 1_000, 250);
        assert!(matches!(result, Err(SupersessionError::TerminationClaimed)));
        assert_eq!(
            coordinator.manager().lease_status(&action).unwrap(),
            Some(LeaseStatus::Abandoned)
        );
        assert_eq!(
            coordinator.manager().live_consumer_count(&action).unwrap(),
            0
        );
        assert_eq!(
            coordinator
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Running
        );
    }

    #[test]
    fn multi_consumer_partial_cancel_keeps_action_live() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock.clone());
        let action = action(2);
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-a")
            .unwrap();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-b")
            .unwrap();
        let mut spy = Spy::default();
        assert_eq!(
            coordinator.detach(&action, "run-a", &mut spy).unwrap(),
            DetachOutcome::Continued { live_consumers: 1 }
        );
        assert!(spy.calls.is_empty());
    }

    #[test]
    fn trust_revocation_bypasses_retention_and_termination_is_once() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock.clone());
        let action = action(3);
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-a")
            .unwrap();
        let mut spy = Spy::default();
        coordinator.detach(&action, "run-a", &mut spy).unwrap();
        coordinator
            .revoke_trust(&action, "credential revoked", &mut spy)
            .unwrap();
        assert_eq!(spy.calls.len(), 1);
        clock.advance(DEFAULT_RETENTION_MS);
        coordinator.reap_due(&mut spy).unwrap();
        assert_eq!(spy.calls.len(), 1);
    }

    #[test]
    fn completed_action_is_adopted_and_reaped_after_window() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock.clone());
        let action = action(4);
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Complete))
            .unwrap();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-a")
            .unwrap();
        let mut spy = Spy::default();
        coordinator.detach(&action, "run-a", &mut spy).unwrap();
        clock.advance(DEFAULT_RETENTION_MS - 1);
        assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
        clock.advance(1);
        assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 1);
        assert_eq!(spy.calls.len(), 1);
    }

    #[test]
    fn expired_lease_without_action_record_is_reaped_from_persisted_key() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock.clone());
        let action = action(23);
        coordinator
            .manager_mut()
            .acquire(&action, "worker-a", 1_000, 250)
            .unwrap();
        clock.advance(1_000);
        assert_eq!(coordinator.manager_mut().expire_due().unwrap(), 1);
        assert!(coordinator
            .manager()
            .latest_action(&action)
            .unwrap()
            .is_none());

        let digest = action.digest().unwrap();
        let mut spy = Spy::default();
        assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 1);
        assert_eq!(spy.calls, vec![(digest, KillReason::FailedAction)]);
        assert!(coordinator
            .manager()
            .latest_action(&action)
            .unwrap()
            .is_none());
    }

    #[test]
    fn due_retention_without_producer_lease_fails_closed() {
        let mut coordinator = coordinator(TestClock::default());
        let digest = action(24).digest().unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 0)",
                [digest.to_string()],
            )
            .unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'failed_action', 0, 0)",
                [digest.to_string()],
            )
            .unwrap();

        let mut spy = Spy::default();
        assert!(coordinator.reap_due(&mut spy).is_err());
        assert!(spy.calls.is_empty());
        assert_eq!(
            coordinator.next_retention().unwrap(),
            Some(LogicalInstant::from_millis(0))
        );
        let phase: i64 = coordinator
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, 0);
    }

    #[test]
    fn malformed_producer_lease_key_fails_closed() {
        let mut coordinator = coordinator(TestClock::default());
        let digest = action(25).digest().unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute(
                "INSERT INTO producer_leases(
                     action_key_digest, action_key_json, generation, owner,
                     expires_at_ms, heartbeat_every_ms, lease_duration_ms, state
                 ) VALUES (?1, '{', 1, 'worker-a', 0, 250, 1, 'abandoned')",
                [digest.to_string()],
            )
            .unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 0)",
                [digest.to_string()],
            )
            .unwrap();

        let mut spy = Spy::default();
        assert!(coordinator.reap_due(&mut spy).is_err());
        assert!(spy.calls.is_empty());
        assert_eq!(
            coordinator.next_retention().unwrap(),
            Some(LogicalInstant::from_millis(0))
        );
    }

    #[test]
    fn mismatched_producer_lease_key_fails_closed() {
        let mut coordinator = coordinator(TestClock::default());
        let expected = action(26);
        let digest = expected.digest().unwrap();
        let wrong_json = serde_json::to_string(&action(27)).unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute(
                "INSERT INTO producer_leases(
                     action_key_digest, action_key_json, generation, owner,
                     expires_at_ms, heartbeat_every_ms, lease_duration_ms, state
                 ) VALUES (?1, ?2, 1, 'worker-a', 0, 250, 1, 'abandoned')",
                params![digest.to_string(), wrong_json],
            )
            .unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 0)",
                [digest.to_string()],
            )
            .unwrap();

        let mut spy = Spy::default();
        assert!(coordinator.reap_due(&mut spy).is_err());
        assert!(spy.calls.is_empty());
        assert_eq!(
            coordinator.next_retention().unwrap(),
            Some(LogicalInstant::from_millis(0))
        );
    }

    #[test]
    fn incomplete_termination_claim_is_retryable() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("incomplete-termination-claim.sqlite");
        let clock = TestClock::default();
        let action = action(6);
        let digest = action.digest().unwrap();
        {
            let mut coordinator = SupersessionCoordinator::open(
                &path,
                clock.clone(),
                SupersessionConfig::new(false, DEFAULT_RETENTION_MS).unwrap(),
            )
            .unwrap();
            coordinator
                .manager_mut()
                .append_action(&record(action.clone(), ActionState::Running))
                .unwrap();
            coordinator
                .manager_mut()
                .attach_consumer(&action, "run-a")
                .unwrap();
            let mut spy = FailOnceSpy {
                calls: 0,
                failed: false,
                reasons: Vec::new(),
            };
            assert!(matches!(
                coordinator.detach(&action, "run-a", &mut spy),
                Err(SupersessionError::Terminator(_))
            ));
            coordinator
                .revoke_trust(&action, "retry termination", &mut spy)
                .unwrap();
            assert_eq!(spy.calls, 1);
            let phase: i64 = coordinator
                .manager
                .journal
                .connection
                .query_row(
                    "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                    [digest.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(phase, TerminationPhase::InFlight as i64);
            assert_eq!(
                coordinator
                    .manager()
                    .latest_action(&action)
                    .unwrap()
                    .unwrap()
                    .state,
                ActionState::Running
            );
        }

        let mut recovered = SupersessionCoordinator::open(
            &path,
            clock,
            SupersessionConfig::new(false, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let phase: i64 = recovered
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, TerminationPhase::Unstarted as i64);
        let mut retry_spy = Spy::default();
        assert_eq!(recovered.reap_due(&mut retry_spy).unwrap().reaped, 1);
        assert_eq!(retry_spy.calls, vec![(digest, KillReason::NoConsumers)]);
        assert_eq!(
            recovered
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
    }

    #[test]
    fn in_flight_claim_retries_physical_termination_after_restart_window() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("in-flight-termination-claim.sqlite");
        let clock = TestClock::default();
        let action = action(18);
        let digest = action.digest().unwrap();
        {
            let mut coordinator = SupersessionCoordinator::open(
                &path,
                clock.clone(),
                SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
            )
            .unwrap();
            coordinator
                .manager_mut()
                .append_action(&record(action.clone(), ActionState::Running))
                .unwrap();
            coordinator
                .manager
                .journal
                .connection
                .execute(
                    "INSERT INTO action_termination_claims(
                         action_key_digest, reason, claimed_at_ms, completed
                     ) VALUES (?1, 'no_consumers', 0, 2)",
                    [digest.to_string()],
                )
                .unwrap();
            coordinator
                .manager
                .journal
                .connection
                .execute(
                    "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                     VALUES (?1, 0)",
                    [digest.to_string()],
                )
                .unwrap();

            let mut spy = Spy::default();
            assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
            assert!(spy.calls.is_empty());
            assert_eq!(
                coordinator
                    .manager()
                    .latest_action(&action)
                    .unwrap()
                    .unwrap()
                    .state,
                ActionState::Running
            );
            let phase: i64 = coordinator
                .manager
                .journal
                .connection
                .query_row(
                    "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                    [digest.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(phase, TerminationPhase::InFlight as i64);
        }

        let mut recovered = SupersessionCoordinator::open(
            &path,
            clock,
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let phase: i64 = recovered
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, TerminationPhase::Unstarted as i64);
        let mut retry_spy = Spy::default();
        assert_eq!(recovered.reap_due(&mut retry_spy).unwrap().reaped, 1);
        assert_eq!(retry_spy.calls, vec![(digest, KillReason::NoConsumers)]);
        assert_eq!(
            recovered
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
    }

    #[test]
    fn already_in_flight_termination_does_not_finalize_without_hook_owner() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock);
        let action = action(70);
        let digest = action.digest().unwrap();
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute(
                "INSERT INTO action_termination_claims(
                     action_key_digest, reason, claimed_at_ms, completed
                 ) VALUES (?1, 'no_consumers', 0, 2)",
                [digest.to_string()],
            )
            .unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute(
                "INSERT INTO action_retention(action_key_digest, retained_until_ms)
                 VALUES (?1, 0)",
                [digest.to_string()],
            )
            .unwrap();

        let mut spy = Spy::default();
        coordinator
            .execute_claimed_termination(&action, KillReason::NoConsumers, &mut spy)
            .unwrap();

        assert_eq!(
            (
                spy.calls,
                coordinator.next_retention().unwrap(),
                coordinator
                    .manager()
                    .latest_action(&action)
                    .unwrap()
                    .unwrap()
                    .state,
            ),
            (
                Vec::new(),
                Some(LogicalInstant::from_millis(0)),
                ActionState::Running,
            )
        );
    }

    #[test]
    fn ambiguous_terminator_error_keeps_in_flight_phase_for_bounded_retry() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("ambiguous-terminator.sqlite");
        let clock = TestClock::default();
        let action = action(19);
        let digest = action.digest().unwrap();
        {
            let mut coordinator = SupersessionCoordinator::open(
                &path,
                clock.clone(),
                SupersessionConfig::new(false, DEFAULT_RETENTION_MS).unwrap(),
            )
            .unwrap();
            coordinator
                .manager_mut()
                .append_action(&record(action.clone(), ActionState::Running))
                .unwrap();
            coordinator
                .manager_mut()
                .attach_consumer(&action, "run-a")
                .unwrap();
            let mut spy = FailOnceSpy {
                calls: 0,
                failed: false,
                reasons: Vec::new(),
            };

            assert!(matches!(
                coordinator.detach(&action, "run-a", &mut spy),
                Err(SupersessionError::Terminator(_))
            ));
            let phase: i64 = coordinator
                .manager
                .journal
                .connection
                .query_row(
                    "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                    [digest.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(phase, TerminationPhase::InFlight as i64);
            clock.advance(TERMINATION_RETRY_MS);
            assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
            assert_eq!(spy.calls, 1);
            assert_eq!(
                coordinator
                    .manager()
                    .latest_action(&action)
                    .unwrap()
                    .unwrap()
                    .state,
                ActionState::Running
            );
        }

        let mut recovered = SupersessionCoordinator::open(
            &path,
            clock,
            SupersessionConfig::new(false, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let phase: i64 = recovered
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, TerminationPhase::Unstarted as i64);
        let mut retry_spy = Spy::default();
        assert_eq!(recovered.reap_due(&mut retry_spy).unwrap().reaped, 1);
        assert_eq!(retry_spy.calls, vec![(digest, KillReason::NoConsumers)]);
        assert_eq!(
            recovered
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
    }

    #[test]
    fn corrupt_termination_phase_fails_closed_during_startup_recovery() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("corrupt-termination-phase.sqlite");
        let clock = TestClock::default();
        let action = action(20);
        let digest = action.digest().unwrap();
        {
            let coordinator = SupersessionCoordinator::open(
                &path,
                clock.clone(),
                SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
            )
            .unwrap();
            coordinator
                .manager
                .journal
                .connection
                .execute(
                    "INSERT INTO action_termination_claims(
                         action_key_digest, reason, claimed_at_ms, completed
                     ) VALUES (?1, 'no_consumers', 0, 3)",
                    [digest.to_string()],
                )
                .unwrap();
        }

        let result = SupersessionCoordinator::open(
            &path,
            clock,
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        );
        assert!(matches!(
            result,
            Err(SupersessionError::InvalidConfig(message)) if message.contains("phase 3")
        ));
    }

    #[test]
    fn immediate_detach_failure_schedules_retry_for_reaper_once() {
        let clock = TestClock::default();
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("immediate-detach-retry.sqlite");
        let mut coordinator = SupersessionCoordinator::open(
            &path,
            clock.clone(),
            SupersessionConfig::new(false, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let action = action(8);
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-a")
            .unwrap();
        let mut spy = FailOnceSpy {
            calls: 0,
            failed: false,
            reasons: Vec::new(),
        };

        assert!(matches!(
            coordinator.detach(&action, "run-a", &mut spy),
            Err(SupersessionError::Terminator(_))
        ));
        assert_eq!(
            coordinator.next_retention().unwrap(),
            Some(LogicalInstant::from_millis(TERMINATION_RETRY_MS))
        );

        clock.advance(TERMINATION_RETRY_MS);
        assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
        assert_eq!(spy.calls, 1);
        assert_eq!(spy.reasons, [KillReason::NoConsumers]);
        assert_eq!(
            coordinator
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Running
        );

        drop(coordinator);
        let mut recovered = SupersessionCoordinator::open(
            &path,
            clock,
            SupersessionConfig::new(false, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        assert_eq!(recovered.reap_due(&mut spy).unwrap().reaped, 1);
        assert_eq!(spy.calls, 2);
        assert_eq!(
            spy.reasons,
            [KillReason::NoConsumers, KillReason::NoConsumers]
        );
        assert_eq!(
            recovered
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
        assert_eq!(recovered.reap_due(&mut spy).unwrap().reaped, 0);
        assert_eq!(spy.calls, 2);
    }

    #[test]
    fn trust_revocation_failure_schedules_urgent_retry_for_reaper_once() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("trust-revocation-retry.sqlite");
        let clock = TestClock::default();
        let mut coordinator = SupersessionCoordinator::open(
            &path,
            clock.clone(),
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let action = action(9);
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        let mut spy = FailOnceSpy {
            calls: 0,
            failed: false,
            reasons: Vec::new(),
        };

        assert!(matches!(
            coordinator.revoke_trust(&action, "credential revoked", &mut spy),
            Err(SupersessionError::Terminator(_))
        ));
        assert_eq!(
            coordinator.next_retention().unwrap(),
            Some(LogicalInstant::from_millis(TERMINATION_RETRY_MS))
        );

        clock.advance(TERMINATION_RETRY_MS);
        assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
        assert_eq!(spy.calls, 1);
        assert_eq!(spy.reasons, [KillReason::TrustRevoked]);
        assert_eq!(
            coordinator
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Running
        );
        let phase: i64 = coordinator
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                [action.digest().unwrap().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, 2);

        drop(coordinator);
        let mut recovered = SupersessionCoordinator::open(
            &path,
            clock,
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let recovered_phase: i64 = recovered
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                [action.digest().unwrap().to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(recovered_phase, 0);
        assert_eq!(recovered.reap_due(&mut spy).unwrap().reaped, 1);
        assert_eq!(spy.calls, 2);
        assert_eq!(
            spy.reasons,
            [KillReason::TrustRevoked, KillReason::TrustRevoked]
        );
        assert_eq!(
            recovered
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
        assert_eq!(recovered.reap_due(&mut spy).unwrap().reaped, 0);
        assert_eq!(spy.calls, 2);
    }

    #[test]
    fn completion_metadata_failure_repairs_without_reterminating_or_adopting() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("completion-metadata-repair.sqlite");
        let clock = TestClock::default();
        let action = action(15);
        let digest = action.digest().unwrap();
        {
            let mut coordinator = SupersessionCoordinator::open(
                &path,
                clock.clone(),
                SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
            )
            .unwrap();
            coordinator
                .manager_mut()
                .append_action(&record(action.clone(), ActionState::Running))
                .unwrap();
            coordinator
                .manager_mut()
                .attach_consumer(&action, "run-a")
                .unwrap();
            coordinator
                .manager
                .journal
                .connection
                .execute_batch(
                    "CREATE TRIGGER fail_termination_completion
                     BEFORE UPDATE OF completed ON action_termination_claims
                     WHEN NEW.completed = 1
                     BEGIN SELECT RAISE(ABORT, 'completion metadata unavailable'); END",
                )
                .unwrap();
            let mut spy = Spy::default();

            assert!(matches!(
                coordinator.detach(&action, "run-a", &mut spy).unwrap(),
                DetachOutcome::Retained { .. }
            ));
            clock.advance(DEFAULT_RETENTION_MS);
            assert!(coordinator.reap_due(&mut spy).is_err());
            assert_eq!(spy.calls.len(), 1);
            clock.advance(TERMINATION_RETRY_MS);
            assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
            assert_eq!(spy.calls.len(), 1);
            assert!(matches!(
                coordinator.admit(&action, "run-b", "worker-b", 1_000, 250),
                Err(SupersessionError::TerminationClaimed)
            ));
            let phase: i64 = coordinator
                .manager
                .journal
                .connection
                .query_row(
                    "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                    [digest.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(phase, TerminationPhase::InFlight as i64);
        }

        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("DROP TRIGGER fail_termination_completion")
            .unwrap();
        drop(connection);

        let mut recovered = SupersessionCoordinator::open(
            &path,
            clock,
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let phase: i64 = recovered
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, TerminationPhase::Unstarted as i64);
        let mut retry_spy = Spy::default();
        assert_eq!(recovered.reap_due(&mut retry_spy).unwrap().reaped, 1);
        assert_eq!(
            retry_spy.calls,
            vec![(digest.clone(), KillReason::RetentionExpired)]
        );
        let phase: i64 = recovered
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, TerminationPhase::Complete as i64);
        assert_eq!(
            recovered
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
        assert_eq!(recovered.reap_due(&mut retry_spy).unwrap().reaped, 0);
    }

    #[test]
    fn abandoned_append_failure_repairs_without_reterminating_or_adopting() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock.clone());
        let action = action(16);
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-a")
            .unwrap();
        coordinator
            .manager
            .journal
            .connection
            .execute_batch(
                "CREATE TRIGGER fail_abandoned_append
                 BEFORE INSERT ON action_events
                 WHEN NEW.state = 'abandoned'
                 BEGIN SELECT RAISE(ABORT, 'abandoned append unavailable'); END",
            )
            .unwrap();
        let mut spy = Spy::default();

        assert!(matches!(
            coordinator.detach(&action, "run-a", &mut spy).unwrap(),
            DetachOutcome::Retained { .. }
        ));
        clock.advance(DEFAULT_RETENTION_MS);
        assert!(coordinator.reap_due(&mut spy).is_err());
        assert_eq!(spy.calls.len(), 1);
        assert!(matches!(
            coordinator.admit(&action, "run-b", "worker-b", 1_000, 250),
            Err(SupersessionError::TerminationClaimed)
        ));
        let phase: i64 = coordinator
            .manager
            .journal
            .connection
            .query_row(
                "SELECT completed FROM action_termination_claims",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(phase, 1);

        coordinator
            .manager
            .journal
            .connection
            .execute_batch("DROP TRIGGER fail_abandoned_append")
            .unwrap();
        clock.advance(TERMINATION_RETRY_MS);
        assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 1);
        assert_eq!(spy.calls.len(), 1);
        assert_eq!(
            coordinator
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
        assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
    }

    #[test]
    fn finalization_repair_survives_restart_without_reterminating() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("finalization-repair.sqlite");
        let clock = TestClock::default();
        let action = action(17);
        {
            let mut coordinator = SupersessionCoordinator::open(
                &path,
                clock.clone(),
                SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
            )
            .unwrap();
            coordinator
                .manager_mut()
                .append_action(&record(action.clone(), ActionState::Running))
                .unwrap();
            coordinator
                .manager_mut()
                .attach_consumer(&action, "run-a")
                .unwrap();
            coordinator
                .manager
                .journal
                .connection
                .execute_batch(
                    "CREATE TRIGGER fail_restart_append
                     BEFORE INSERT ON action_events
                     WHEN NEW.state = 'abandoned'
                     BEGIN SELECT RAISE(ABORT, 'append unavailable'); END",
                )
                .unwrap();
            let mut spy = Spy::default();
            coordinator.detach(&action, "run-a", &mut spy).unwrap();
            clock.advance(DEFAULT_RETENTION_MS);
            assert!(coordinator.reap_due(&mut spy).is_err());
            assert_eq!(spy.calls.len(), 1);
        }

        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("DROP TRIGGER fail_restart_append")
            .unwrap();
        drop(connection);

        let mut restarted = SupersessionCoordinator::open(
            &path,
            clock.clone(),
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        clock.advance(TERMINATION_RETRY_MS);
        let mut spy = Spy::default();
        assert_eq!(restarted.reap_due(&mut spy).unwrap().reaped, 1);
        assert!(spy.calls.is_empty());
        assert_eq!(
            restarted
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
    }

    #[test]
    fn trust_revocation_retry_bypasses_live_consumers() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("trust-revocation-live-consumer.sqlite");
        let clock = TestClock::default();
        let mut coordinator = SupersessionCoordinator::open(
            &path,
            clock.clone(),
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let action = action(12);
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-a")
            .unwrap();
        let mut spy = FailOnceSpy {
            calls: 0,
            failed: false,
            reasons: Vec::new(),
        };

        assert!(matches!(
            coordinator.revoke_trust(&action, "credential revoked", &mut spy),
            Err(SupersessionError::Terminator(_))
        ));
        assert_eq!(
            coordinator.manager().live_consumer_count(&action).unwrap(),
            1
        );

        clock.advance(TERMINATION_RETRY_MS);
        assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
        assert_eq!(spy.calls, 1);
        assert_eq!(spy.reasons, [KillReason::TrustRevoked]);
        assert_eq!(
            coordinator.manager().live_consumer_count(&action).unwrap(),
            1
        );
        assert_eq!(
            coordinator
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Running
        );

        drop(coordinator);
        let mut recovered = SupersessionCoordinator::open(
            &path,
            clock,
            SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        assert_eq!(recovered.reap_due(&mut spy).unwrap().reaped, 1);
        assert_eq!(spy.calls, 2);
        assert_eq!(
            spy.reasons,
            [KillReason::TrustRevoked, KillReason::TrustRevoked]
        );
        assert_eq!(recovered.manager().live_consumer_count(&action).unwrap(), 1);
        assert_eq!(
            recovered
                .manager()
                .latest_action(&action)
                .unwrap()
                .unwrap()
                .state,
            ActionState::Abandoned
        );
        assert_eq!(recovered.reap_due(&mut spy).unwrap().reaped, 0);
        assert_eq!(spy.calls, 2);
    }

    #[test]
    fn failed_and_abandoned_termination_retries_are_reaped() {
        let clock = TestClock::default();
        let temp = TempDir::new().unwrap();

        for (seed, state) in [(13, ActionState::Failed), (14, ActionState::Abandoned)] {
            let path = temp
                .path()
                .join(format!("failed-or-abandoned-{seed}.sqlite"));
            let mut coordinator = SupersessionCoordinator::open(
                &path,
                clock.clone(),
                SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
            )
            .unwrap();
            let action = action(seed);
            coordinator
                .manager_mut()
                .append_action(&record(action.clone(), state))
                .unwrap();
            coordinator
                .manager_mut()
                .attach_consumer(&action, "run-a")
                .unwrap();
            let mut spy = FailOnceSpy {
                calls: 0,
                failed: false,
                reasons: Vec::new(),
            };

            assert!(matches!(
                coordinator.detach(&action, "run-a", &mut spy),
                Err(SupersessionError::Terminator(_))
            ));
            clock.advance(TERMINATION_RETRY_MS);
            assert_eq!(coordinator.reap_due(&mut spy).unwrap().reaped, 0);
            assert_eq!(spy.calls, 1);
            assert_eq!(spy.reasons, [KillReason::FailedAction]);
            assert_eq!(
                coordinator
                    .manager()
                    .latest_action(&action)
                    .unwrap()
                    .unwrap()
                    .state,
                state
            );

            drop(coordinator);
            let mut recovered = SupersessionCoordinator::open(
                &path,
                clock.clone(),
                SupersessionConfig::new(true, DEFAULT_RETENTION_MS).unwrap(),
            )
            .unwrap();
            assert_eq!(recovered.reap_due(&mut spy).unwrap().reaped, 1);
            assert_eq!(spy.calls, 2);
            assert_eq!(
                spy.reasons,
                [KillReason::FailedAction, KillReason::FailedAction]
            );
            assert_eq!(
                recovered
                    .manager()
                    .latest_action(&action)
                    .unwrap()
                    .unwrap()
                    .state,
                ActionState::Abandoned
            );
            assert_eq!(recovered.reap_due(&mut spy).unwrap().reaped, 0);
            assert_eq!(spy.calls, 2);
        }
    }

    #[cfg(feature = "virtual-time")]
    #[tokio::test(flavor = "current_thread")]
    async fn retention_reaper_waits_on_durable_deadline() {
        let clock = TestClock::default();
        let mut coordinator = coordinator(clock.clone());
        let action = action(7);
        coordinator
            .manager_mut()
            .append_action(&record(action.clone(), ActionState::Running))
            .unwrap();
        coordinator
            .manager_mut()
            .attach_consumer(&action, "run-a")
            .unwrap();
        let mut spy = Spy::default();
        coordinator.detach(&action, "run-a", &mut spy).unwrap();
        let task = tokio::spawn(async move { coordinator.reap_next(&mut spy).await });
        tokio::task::yield_now().await;
        clock.advance(DEFAULT_RETENTION_MS - 1);
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        clock.advance(1);
        assert_eq!(task.await.unwrap().unwrap().reaped, 1);
    }
}

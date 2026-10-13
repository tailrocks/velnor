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
    NeedsProducer,
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
    AlreadyClaimed,
    BlockedByConsumers(u64),
}

enum RetentionClaim {
    Claimed,
    AlreadyClaimed,
    LiveConsumers(u64),
    Missing,
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
        if let Some(reason) = self.revocation_reason(&digest)? {
            return Err(SupersessionError::TrustRevoked { reason });
        }
        let latest = self.manager.latest_action(action)?;
        if self.config.enabled
            && action.execution_policy.adoptable
            && let Some(record) = latest.clone().filter(is_adoptable_state)
        {
            self.attach_and_clear_retention(&digest, run_id)?;
            self.emit_telemetry(SupersessionTelemetryEvent {
                action_key_digest: digest,
                run_id: Some(run_id.to_owned()),
                at: self.manager.clock().now(),
                kind: match record.state {
                    ActionState::Running => SupersessionEventKind::SupersessionAdopted,
                    ActionState::Complete => SupersessionEventKind::SupersessionAdopted,
                    _ => unreachable!(),
                },
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
        self.manager.attach_consumer(action, run_id)?;
        Ok(Candidate::NeedsProducer)
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
            Candidate::NeedsProducer => {
                match self
                    .manager
                    .acquire(action, owner, lease_duration_ms, heartbeat_every_ms)
                {
                    Ok(lease) => Ok(Admission::Started { lease }),
                    Err(JournalError::LeaseBusy { .. }) => {
                        let latest = self.manager.latest_action(action)?;
                        if self.config.enabled
                            && action.execution_policy.adoptable
                            && latest.as_ref().is_some_and(is_adoptable_state)
                        {
                            return match self.attach_or_adopt(action, run_id)? {
                                Candidate::AdoptedRunning { record } => {
                                    Ok(Admission::AdoptedRunning { record })
                                }
                                Candidate::AdoptedComplete { record } => {
                                    Ok(Admission::AdoptedComplete { record })
                                }
                                Candidate::NeedsProducer => Ok(Admission::Waiting),
                            };
                        }
                        Ok(Admission::Waiting)
                    }
                    Err(error) => Err(error.into()),
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
            statement
                .query_map([now_ms], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut report = ReapReport {
            reaped: 0,
            skipped_live: 0,
        };
        for raw_digest in rows {
            let digest = Digest::parse(raw_digest)?;
            let Some(action) = self.action_for_digest(&digest)? else {
                self.delete_retention(&digest)?;
                continue;
            };
            if matches!(
                action_state(&self.manager, &action)?,
                Some(ActionState::Failed | ActionState::Abandoned)
            ) {
                self.delete_retention(&digest)?;
                self.emit_telemetry(SupersessionTelemetryEvent {
                    action_key_digest: digest,
                    run_id: None,
                    at: now,
                    kind: SupersessionEventKind::RetentionKillSkipped,
                    live_consumers: 0,
                    reason: Some("failed_or_abandoned".to_owned()),
                    retained_until: None,
                });
                continue;
            }
            match self.claim_due_retention(&digest, now_ms)? {
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
                RetentionClaim::Claimed => {
                    if let Err(error) = self.execute_claimed_termination(
                        &action,
                        KillReason::RetentionExpired,
                        terminator,
                    ) {
                        // The durable claim remains incomplete. Restore a
                        // short, bounded retry deadline so a transient hook
                        // failure cannot strand physical work forever.
                        self.restore_retention(&digest, now.saturating_add(TERMINATION_RETRY_MS))?;
                        return Err(error);
                    }
                    report.reaped += 1;
                    self.emit_telemetry(SupersessionTelemetryEvent {
                        action_key_digest: digest,
                        run_id: None,
                        at: now,
                        kind: SupersessionEventKind::RetainedThenReaped,
                        live_consumers: 0,
                        reason: Some(KillReason::RetentionExpired.to_string()),
                        retained_until: None,
                    });
                }
                RetentionClaim::AlreadyClaimed | RetentionClaim::Missing => {}
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
        self.manager.journal.connection.execute(
            "INSERT INTO action_retention(action_key_digest, retained_until_ms)
             SELECT action_key_digest, ?1
             FROM action_termination_claims
             WHERE completed = 0
             ON CONFLICT(action_key_digest) DO NOTHING",
            [sqlite_integer(now.as_millis())?],
        )?;
        Ok(())
    }

    fn attach_and_clear_retention(
        &mut self,
        digest: &Digest,
        run_id: &str,
    ) -> Result<(), SupersessionError> {
        let transaction = self
            .manager
            .journal
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let terminating: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM action_termination_claims WHERE action_key_digest = ?1",
                [digest.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if terminating.is_some() {
            return Err(SupersessionError::TerminationClaimed);
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

    fn action_for_digest(&self, digest: &Digest) -> Result<Option<ActionKey>, SupersessionError> {
        Ok(self
            .manager
            .journal
            .latest()?
            .remove(digest)
            .map(|record| record.action_key))
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
            TerminationClaim::BlockedByConsumers(_) | TerminationClaim::AlreadyClaimed => {
                return Ok(false)
            }
            TerminationClaim::Claimed => {}
        }
        self.execute_claimed_termination(action, reason, terminator)?;
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
        let live: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM action_consumers WHERE action_key_digest = ?1",
            [digest.to_string()],
            |row| row.get(0),
        )?;
        if live > 0 {
            transaction.execute(
                "DELETE FROM action_retention WHERE action_key_digest = ?1",
                [digest.to_string()],
            )?;
            transaction.commit()?;
            return Ok(RetentionClaim::LiveConsumers(u64::try_from(live).map_err(
                |_| SupersessionError::InvalidConfig("consumer count is negative".to_owned()),
            )?));
        }
        let claim =
            claim_termination_row(&transaction, digest, KillReason::RetentionExpired, now_ms)?;
        if matches!(
            claim,
            TerminationClaim::Claimed | TerminationClaim::AlreadyClaimed
        ) {
            transaction.execute(
                "DELETE FROM action_retention WHERE action_key_digest = ?1",
                [digest.to_string()],
            )?;
        }
        transaction.commit()?;
        Ok(match claim {
            TerminationClaim::Claimed => RetentionClaim::Claimed,
            TerminationClaim::AlreadyClaimed => RetentionClaim::AlreadyClaimed,
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
        terminator
            .terminate(action, reason)
            .map_err(SupersessionError::Terminator)?;
        self.manager.journal.connection.execute(
            "UPDATE action_termination_claims SET completed = 1 WHERE action_key_digest = ?1",
            [digest.to_string()],
        )?;
        self.delete_retention(&digest)?;
        if let Some(mut record) = self.manager.latest_action(action)?
            && record.state != ActionState::Abandoned
        {
            record.state = ActionState::Abandoned;
            self.manager.append_action(&record)?;
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

fn action_state<C: Clock>(
    manager: &LeaseManager<C>,
    action: &ActionKey,
) -> Result<Option<ActionState>, SupersessionError> {
    Ok(manager.latest_action(action)?.map(|record| record.state))
}

fn claim_termination_row(
    transaction: &rusqlite::Transaction<'_>,
    digest: &Digest,
    reason: KillReason,
    now_ms: i64,
) -> Result<TerminationClaim, rusqlite::Error> {
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT completed FROM action_termination_claims WHERE action_key_digest = ?1",
            [digest.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(completed) if completed != 0 => Ok(TerminationClaim::AlreadyClaimed),
        Some(_) => {
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

fn is_adoptable_state(record: &ActionRecord) -> bool {
    matches!(record.state, ActionState::Running | ActionState::Complete)
}

fn sqlite_integer(value: u64) -> Result<i64, SupersessionError> {
    i64::try_from(value).map_err(|_| {
        SupersessionError::InvalidConfig("logical value exceeds SQLite integer range".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::{BTreeMap, BTreeSet},
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
    };
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
    }

    impl PhysicalActionTerminator for FailOnceSpy {
        fn terminate(&mut self, _action: &ActionKey, _reason: KillReason) -> Result<(), String> {
            self.calls += 1;
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
    fn incomplete_termination_claim_is_retryable() {
        let clock = TestClock::default();
        let mut coordinator = SupersessionCoordinator::open(
            ":memory:",
            clock,
            SupersessionConfig::new(false, DEFAULT_RETENTION_MS).unwrap(),
        )
        .unwrap();
        let action = action(6);
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
        };
        assert!(matches!(
            coordinator.detach(&action, "run-a", &mut spy),
            Err(SupersessionError::Terminator(_))
        ));
        coordinator
            .revoke_trust(&action, "retry termination", &mut spy)
            .unwrap();
        assert_eq!(spy.calls, 2);
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

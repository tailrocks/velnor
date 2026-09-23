//! Poll→Scale→ACK loop (`listener.go` at [`UPSTREAM_COMMIT`][pin]).
//!
//! Mirrors upstream `Listener.Run`: feed the synthetic initial message
//! ([`INITIAL_MESSAGE_ID`], session statistics) to `Scale` first, then loop
//! `GetMessage(lastMessageID, maxCapacity)` → `Scale(msg)` (nil included)
//! → on successful `Scale`, ACK via `DeleteMessage`, then persist the cursor.
//! Failures before the ACK leave the message unacked; cursor-persistence
//! failures are retried with the durable cursor still behind the ACK.
//!
//! Daemon deviations from upstream (all deliberate, all documented):
//!
//! * Upstream keeps `lastMessageID` in memory and STOPS the listener on any
//!   error. The daemon persists the cursor (crash resume) and backs off
//!   through transient failures instead of exiting. Memory-only + stop is
//!   what makes upstream's advance-before-ACK safe; with a durable cursor
//!   the order flips to ACK-then-persist — a failed ACK leaves the old
//!   cursor, so the message is redelivered instead of skipped.
//! * Upstream passes the configured `maxRunners` total as capacity on every
//!   poll. The daemon advertises shared-grant-derived free headroom (see
//!   [`capacity`][crate::scaleset::capacity]) — never `N` per listener.
//! * Upstream loops immediately after a nil poll; the daemon waits a flat
//!   [`IdlePolicy::nil_delay`] floor (instant-202 hot-spin guard) and runs
//!   bounded idle reconcile on nils.
//!
//! [pin]: crate::scaleset::upstream_pin::UPSTREAM_COMMIT

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use velnor_model::{RunnerScaleSetMessage, RunnerScaleSetStatistic};

use crate::scaleset::backoff::{IdlePolicy, PollOutcomeClass, RetryPolicy};
use crate::scaleset::capacity::{advertise_free, CapacityLedger};
use crate::scaleset::converge::WorkerLane;
use crate::scaleset::metrics::Metrics;
use crate::scaleset::reconcile::{idle_poll, startup};
use crate::scaleset::scale::{Processor, QueueSession, ScaleError, ScaleKind, ScaleOutcome};
use crate::scaleset::session::MessageSessionClient;

/// Message ID of the synthetic first message (upstream `InitialMessageID`).
/// Carries session statistics only; never ACKed, never persisted as cursor.
pub const INITIAL_MESSAGE_ID: i32 = -1;

/// Validate the server-owned session identity before it becomes a durable
/// cursor fence. The current Scale Set API uses UUID session IDs; accepting a
/// malformed or nil value would make restart fencing and deletion ambiguous.
pub(crate) fn validate_session_id(session_id: &str) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(session_id).context("scale-set session id is not a UUID")?;
    if uuid.is_nil() {
        anyhow::bail!("scale-set session id is nil");
    }
    Ok(())
}

/// The queue surface the loop needs: acquire (for `Scale`) plus the poll
/// triplet (for the listener). [`ClientSession`] adapts the real client.
pub trait LoopSession: QueueSession + Clone {
    /// Current session statistics for the initial message and nil-poll
    /// stats observation.
    async fn session_statistics(&self) -> Option<RunnerScaleSetStatistic>;
    /// Session identity for the durable cursor row.
    async fn session_identity(&self) -> (String, String);
    /// Long poll: `GET {queue}?lastMessageId={n}` + capacity header.
    async fn get_message(
        &self,
        last_message_id: i32,
        max_capacity: i32,
    ) -> Result<Option<RunnerScaleSetMessage>, Self::Error>;
    /// ACK: `DELETE {queue}/{id}`.
    async fn delete_message(&self, message_id: i32) -> Result<(), Self::Error>;
}

/// [`LoopSession`] over the real message-session client.
#[derive(Debug, Clone)]
pub struct ClientSession {
    client: MessageSessionClient,
}

impl ClientSession {
    #[must_use]
    pub fn new(client: MessageSessionClient) -> Self {
        Self { client }
    }
}

impl QueueSession for ClientSession {
    type Error = crate::scaleset::errors::ScaleSetError;

    async fn acquire_jobs(&self, request_ids: &[i64]) -> Result<Vec<i64>, Self::Error> {
        self.client.acquire_jobs(request_ids).await
    }
}

impl LoopSession for ClientSession {
    async fn session_statistics(&self) -> Option<RunnerScaleSetStatistic> {
        self.client.session().await.statistics
    }

    async fn session_identity(&self) -> (String, String) {
        let session = self.client.session().await;
        (session.session_id, session.owner_name)
    }

    async fn get_message(
        &self,
        last_message_id: i32,
        max_capacity: i32,
    ) -> Result<Option<RunnerScaleSetMessage>, Self::Error> {
        self.client.get_message(last_message_id, max_capacity).await
    }

    async fn delete_message(&self, message_id: i32) -> Result<(), Self::Error> {
        self.client.delete_message(message_id).await
    }
}

/// One `scaleset_sessions` row: durable poll cursor + last stats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCursor {
    pub scale_set_id: i32,
    pub session_id: String,
    pub owner: String,
    pub last_message_id: i32,
    pub stats_json: Option<String>,
    pub generation: u64,
}

/// Durable session cursors over `scaleset_sessions`. Tokens and queue URLs
/// never touch this table (fingerprints only, per the secrets rule — and
/// in practice neither is stored at all: the live session client holds
/// them in process).
#[derive(Debug)]
pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    /// Open the cursor store at the state database path.
    pub fn open(path: &Path) -> Result<Self> {
        velnor_control::store::Store::open(path).context("migrate session cursor schema")?;
        let conn = Connection::open(path).context("open session cursor database")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("set session cursor store busy timeout")?;
        Ok(Self { conn })
    }

    fn now_rfc3339() -> String {
        velnor_model::Timestamp::now()
            .to_rfc3339()
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
    }

    /// Fetch one set's cursor, if the adapter ever polled it.
    pub fn get(&self, scale_set_id: i32) -> Result<Option<SessionCursor>> {
        self.conn
            .query_row(
                "SELECT scale_set_id, session_id, owner, last_message_id, stats_json, generation
                 FROM scaleset_sessions WHERE scale_set_id = ?1",
                params![scale_set_id],
                |row| {
                    let generation_raw: i64 = row.get(5)?;
                    Ok(SessionCursor {
                        scale_set_id: row.get(0)?,
                        session_id: row.get(1)?,
                        owner: row.get(2)?,
                        last_message_id: row.get(3)?,
                        stats_json: row.get(4)?,
                        generation: generation_raw.max(0) as u64,
                    })
                },
            )
            .optional()
            .context("fetch session cursor")
    }

    /// Record session identity after (re)connect.
    ///
    /// The row is the ownership fence. A newer generation may replace it and
    /// starts its session at cursor zero. An equal-generation write is valid
    /// only for the same session. Older generations and same-generation
    /// foreign sessions lose the race and fail without changing the row.
    pub fn save_session(
        &mut self,
        scale_set_id: i32,
        session_id: &str,
        owner: &str,
        generation: u64,
    ) -> Result<()> {
        let generation = i64::try_from(generation).unwrap_or(i64::MAX);
        let changed = self
            .conn
            .execute(
                "INSERT INTO scaleset_sessions
                 (scale_set_id, session_id, owner, last_message_id, stats_json, generation, updated_at)
                 VALUES (?1, ?2, ?3, 0, NULL, ?4, ?5)
                 ON CONFLICT (scale_set_id) DO UPDATE SET
                   session_id = excluded.session_id,
                   owner = excluded.owner,
                   last_message_id = CASE
                       WHEN scaleset_sessions.session_id = excluded.session_id
                       THEN scaleset_sessions.last_message_id
                       ELSE 0
                   END,
                   generation = excluded.generation,
                   updated_at = excluded.updated_at
                 WHERE scaleset_sessions.generation < excluded.generation
                    OR (scaleset_sessions.generation = excluded.generation
                        AND scaleset_sessions.session_id = excluded.session_id)",
                params![scale_set_id, session_id, owner, generation, Self::now_rfc3339()],
            )
            .context("compare-and-set session cursor")?;
        if changed == 0 {
            anyhow::bail!(
                "scale-set session cursor fenced: scale_set_id={scale_set_id}, session_id={session_id:?}, generation={generation}"
            );
        }
        Ok(())
    }

    /// Advance the cursor past an ACKed message. Called AFTER the ACK lands.
    /// The session and generation predicates make a late old listener unable
    /// to overwrite the current owner; the message predicate makes the
    /// cursor monotonic within one owner.
    pub fn save_cursor(
        &mut self,
        scale_set_id: i32,
        session_id: &str,
        last_message_id: i32,
        generation: u64,
    ) -> Result<()> {
        let generation = i64::try_from(generation).unwrap_or(i64::MAX);
        let changed = self
            .conn
            .execute(
                "UPDATE scaleset_sessions
                 SET last_message_id = ?1, updated_at = ?2
                 WHERE scale_set_id = ?3
                   AND session_id = ?4
                   AND generation = ?5
                   AND last_message_id <= ?1",
                params![
                    last_message_id,
                    Self::now_rfc3339(),
                    scale_set_id,
                    session_id,
                    generation,
                ],
            )
            .context("compare-and-set session cursor")?;
        if changed == 0 {
            anyhow::bail!(
                "scale-set cursor fenced or regressed: scale_set_id={scale_set_id}, session_id={session_id:?}, generation={generation}, message_id={last_message_id}"
            );
        }
        Ok(())
    }

    /// Persist the authoritative statistics from a poll (every poll,
    /// including nils via the session snapshot).
    pub fn save_stats(
        &mut self,
        scale_set_id: i32,
        session_id: &str,
        stats: &RunnerScaleSetStatistic,
        generation: u64,
    ) -> Result<()> {
        let stats_json = serde_json::to_string(stats).context("encode session statistics")?;
        let generation = i64::try_from(generation).unwrap_or(i64::MAX);
        let changed = self
            .conn
            .execute(
                "UPDATE scaleset_sessions
                 SET stats_json = ?1, updated_at = ?2
                 WHERE scale_set_id = ?3
                   AND session_id = ?4
                   AND generation = ?5",
                params![
                    stats_json,
                    Self::now_rfc3339(),
                    scale_set_id,
                    session_id,
                    generation,
                ],
            )
            .context("compare-and-set session statistics")?;
        if changed == 0 {
            anyhow::bail!(
                "scale-set statistics fenced: scale_set_id={scale_set_id}, session_id={session_id:?}, generation={generation}"
            );
        }
        Ok(())
    }
}

/// Static loop configuration.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub scale_set_id: i32,
    pub retry: RetryPolicy,
    pub idle: IdlePolicy,
}

/// Loop failures. Every variant skips the ACK (redelivery) and is
/// retryable; the daemon backs off and re-polls rather than exiting.
#[derive(Debug)]
pub enum ListenerError<SE, WE> {
    Poll(SE),
    Ack(SE),
    Scale(ScaleError<SE, WE>),
    Store(anyhow::Error),
    Ledger(anyhow::Error),
    Reconcile(anyhow::Error),
    MissingSessionStats,
    /// The in-flight long poll was cancelled because the daemon is draining.
    ShuttingDown,
}

impl<SE: std::fmt::Display, WE: std::fmt::Display> std::fmt::Display for ListenerError<SE, WE> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Poll(error) => write!(f, "scale-set poll: {error}"),
            Self::Ack(error) => write!(f, "scale-set ACK: {error}"),
            Self::Scale(error) => write!(f, "scale: {error}"),
            Self::Store(error) => write!(f, "scale-set cursor store: {error}"),
            Self::Ledger(error) => write!(f, "scale-set ledger: {error}"),
            Self::Reconcile(error) => write!(f, "scale-set reconcile: {error}"),
            Self::MissingSessionStats => write!(f, "session carries no statistics"),
            Self::ShuttingDown => write!(f, "scale-set listener is shutting down"),
        }
    }
}

impl<
        SE: std::error::Error + Send + Sync + 'static,
        WE: std::error::Error + Send + Sync + 'static,
    > std::error::Error for ListenerError<SE, WE>
{
}

/// The poll→Scale→ACK loop.
pub struct Listener<S, L, W> {
    session: S,
    processor: Processor<S, L, W>,
    cursors: SessionStore,
    session_id: Option<String>,
    metrics: Metrics,
    config: LoopConfig,
    started: bool,
    reconciled_generation: Option<u64>,
    consecutive_errors: u32,
}

impl<S, L, W> Listener<S, L, W> {
    pub fn new(
        session: S,
        processor: Processor<S, L, W>,
        cursors: SessionStore,
        metrics: Metrics,
        config: LoopConfig,
    ) -> Self {
        Self {
            session,
            processor,
            cursors,
            session_id: None,
            metrics,
            config,
            started: false,
            reconciled_generation: None,
            consecutive_errors: 0,
        }
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn processor(&self) -> &Processor<S, L, W> {
        &self.processor
    }

    /// Mutable processor access for daemon lifecycle passes (shutdown
    /// triage runs through the lane; the loop owns every other call).
    pub fn processor_mut(&mut self) -> &mut Processor<S, L, W> {
        &mut self.processor
    }
}

impl<S: LoopSession, L: CapacityLedger, W: WorkerLane> Listener<S, L, W> {
    /// One poll iteration: initial handshake once, reconcile-before-
    /// advertise on every new epoch, poll, Scale, ACK-after-success.
    pub async fn run_once(&mut self) -> Result<ScaleOutcome, ListenerError<S::Error, W::Error>> {
        self.run_once_with_shutdown(None).await
    }

    async fn run_once_with_shutdown(
        &mut self,
        shutdown: Option<&AtomicBool>,
    ) -> Result<ScaleOutcome, ListenerError<S::Error, W::Error>> {
        self.metrics.inc_polls();
        let outcome = self.run_once_inner(shutdown).await;
        match &outcome {
            Ok(_) => self.consecutive_errors = 0,
            Err(_) => self.consecutive_errors = self.consecutive_errors.saturating_add(1),
        }
        outcome
    }

    async fn run_once_inner(
        &mut self,
        shutdown: Option<&AtomicBool>,
    ) -> Result<ScaleOutcome, ListenerError<S::Error, W::Error>> {
        let set = self.config.scale_set_id;
        if !self.started {
            self.handshake().await?;
            self.started = true;
        }
        // Reconcile-before-advertise on every new epoch (restart included:
        // `reconciled_generation` starts `None`, so the first poll always
        // reconciles before the first advertisement).
        let generation = self
            .processor
            .ledger_generation()
            .map_err(ListenerError::Ledger)?;
        if self.reconciled_generation != Some(generation) {
            let (ledger, demand, batches, _lane) = self.processor.parts_mut();
            startup(ledger, demand, batches, set, &self.metrics)
                .map_err(ListenerError::Reconcile)?;
            self.reconciled_generation = Some(generation);
        }
        let capacity = advertise_free(self.processor.ledger_ref());
        self.metrics.set_advertised_capacity(capacity);
        let last_id = self
            .cursors
            .get(set)
            .map_err(ListenerError::Store)?
            .map_or(0, |cursor| cursor.last_message_id);
        let capacity_i32 = i32::try_from(capacity).unwrap_or(i32::MAX);
        let polled = if let Some(shutdown) = shutdown {
            tokio::select! {
                biased;
                () = wait_for_shutdown(shutdown) => return Err(ListenerError::ShuttingDown),
                result = self.session.get_message(last_id, capacity_i32) => {
                    result.map_err(ListenerError::Poll)?
                }
            }
        } else {
            self.session
                .get_message(last_id, capacity_i32)
                .await
                .map_err(ListenerError::Poll)?
        };
        // Close the race where the poll and drain become ready together.
        // Leave a returned message unacked so the next process can replay it.
        if shutdown.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(ListenerError::ShuttingDown);
        }
        match polled {
            None => self.scale_nil(generation).await,
            Some(message) => self.scale_message(message, generation).await,
        }
    }

    /// Initial handshake: session identity recorded, synthetic initial
    /// message scaled (stats only — never ACKed, never a cursor).
    async fn handshake(&mut self) -> Result<(), ListenerError<S::Error, W::Error>> {
        let Some(stats) = self.session.session_statistics().await else {
            return Err(ListenerError::MissingSessionStats);
        };
        let (session_id, owner) = self.session.session_identity().await;
        validate_session_id(&session_id).map_err(ListenerError::Store)?;
        let generation = self
            .processor
            .ledger_generation()
            .map_err(ListenerError::Ledger)?;
        self.cursors
            .save_session(self.config.scale_set_id, &session_id, &owner, generation)
            .map_err(ListenerError::Store)?;
        self.cursors
            .save_stats(self.config.scale_set_id, &session_id, &stats, generation)
            .map_err(ListenerError::Store)?;
        self.session_id = Some(session_id);
        let initial = RunnerScaleSetMessage {
            message_id: INITIAL_MESSAGE_ID,
            statistics: Some(stats),
            ..RunnerScaleSetMessage::default()
        };
        self.processor
            .scale(Some(&initial))
            .await
            .map_err(ListenerError::Scale)?;
        tracing::info!(
            total_assigned_jobs = stats.total_assigned_jobs,
            "handling initial session statistics"
        );
        Ok(())
    }

    /// Nil poll: converge on cached stats, observe session stats, run
    /// bounded idle reconcile.
    async fn scale_nil(
        &mut self,
        generation: u64,
    ) -> Result<ScaleOutcome, ListenerError<S::Error, W::Error>> {
        self.metrics.inc_nil_polls();
        self.idle_reconcile(generation).await?;
        // Uncertain resolution can promote rows to Acquired; the same nil
        // pass must provision those rows and drain any older Granted work.
        let outcome = self
            .processor
            .scale(None)
            .await
            .map_err(ListenerError::Scale)?;
        debug_assert!(matches!(outcome.kind, ScaleKind::Nil));
        Ok(outcome)
    }

    /// Run recovery work independently of whether the long poll returned an
    /// empty response. A continuous message stream must not starve overdue
    /// uncertain acquire batches or lane reconciliation.
    async fn idle_reconcile(
        &mut self,
        generation: u64,
    ) -> Result<(), ListenerError<S::Error, W::Error>> {
        self.processor
            .lane_mut()
            .idle_tick()
            .map_err(|error| ListenerError::Scale(ScaleError::Lane(error)))?;
        let (ledger, demand, batches, lane) = self.processor.parts_mut();
        idle_poll(
            &self.session,
            ledger,
            demand,
            batches,
            lane,
            self.config.scale_set_id,
            generation,
            &self.metrics,
        )
        .await
        .map_err(ListenerError::Reconcile)
        .map(|_| ())
    }

    /// Real message: persist stats, Scale, ACK-then-persist-cursor.
    async fn scale_message(
        &mut self,
        message: RunnerScaleSetMessage,
        generation: u64,
    ) -> Result<ScaleOutcome, ListenerError<S::Error, W::Error>> {
        self.metrics.inc_messages();
        let session_id = self.session_id.clone().ok_or_else(|| {
            ListenerError::Store(anyhow::anyhow!(
                "scale-set session identity missing before message persistence"
            ))
        })?;
        if let Some(stats) = message.statistics {
            self.cursors
                .save_stats(self.config.scale_set_id, &session_id, &stats, generation)
                .map_err(ListenerError::Store)?;
        }
        self.idle_reconcile(generation).await?;
        let outcome = self
            .processor
            .scale(Some(&message))
            .await
            .map_err(|error| {
                self.metrics.inc_scale_errors();
                ListenerError::Scale(error)
            })?;
        // ACK-then-persist: a failed ACK keeps the old cursor, so the
        // message is redelivered instead of skipped.
        self.session
            .delete_message(message.message_id)
            .await
            .map_err(ListenerError::Ack)?;
        self.cursors
            .save_cursor(
                self.config.scale_set_id,
                &session_id,
                message.message_id,
                generation,
            )
            .map_err(ListenerError::Store)?;
        self.metrics.inc_acks();
        self.metrics.set_last_message_id(message.message_id);
        Ok(outcome)
    }

    /// Run until `shutdown` is set. Transient failures back off and re-poll;
    /// only the shutdown flag stops the loop.
    pub async fn run(&mut self, shutdown: &AtomicBool) {
        loop {
            if shutdown.load(Ordering::Relaxed) {
                return;
            }
            let class = match self.run_once_with_shutdown(Some(shutdown)).await {
                Ok(outcome) => match outcome.kind {
                    ScaleKind::Message { .. } | ScaleKind::Initial => PollOutcomeClass::Message,
                    ScaleKind::Nil => PollOutcomeClass::Nil,
                },
                Err(ListenerError::ShuttingDown) => return,
                Err(error) => {
                    tracing::warn!(
                        error = error.to_string(),
                        "scale-set poll failed; backing off"
                    );
                    PollOutcomeClass::Error
                }
            };
            let delay =
                self.config
                    .retry
                    .poll_delay(class, self.consecutive_errors, &self.config.idle);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
    }
}

async fn wait_for_shutdown(shutdown: &AtomicBool) {
    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
    use crate::scaleset::capacity::MemLedger;
    use crate::scaleset::converge::ProvisionImages;
    use crate::scaleset::demand::{DemandStore, OfferAdmission};
    use crate::scaleset::intents::{AcquireBatchStore, ProvisionIntentStore};
    use crate::scaleset::scale::{ProcessorConfig, QueueSession};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use velnor_model::{ScaleSetJobAvailable, ScaleSetJobMessage, ScaleSetJobMessageType};

    const SESSION_ID: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
    const SESSION_OLD_ID: &str = "11111111-1111-4111-8111-111111111111";
    const SESSION_NEW_ID: &str = "22222222-2222-4222-8222-222222222222";
    const SESSION_NEXT_ID: &str = "33333333-3333-4333-8333-333333333333";

    #[derive(Debug)]
    struct SessionError(String);

    impl std::fmt::Display for SessionError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "session: {}", self.0)
        }
    }

    impl std::error::Error for SessionError {}

    #[derive(Debug, Default)]
    struct ScriptedState {
        polls: Mutex<VecDeque<Option<RunnerScaleSetMessage>>>,
        acks: Mutex<Vec<i32>>,
        acquired: Mutex<Vec<Vec<i64>>>,
        seen_capacity: Mutex<Vec<i32>>,
        seen_last_id: Mutex<Vec<i32>>,
        cursor_path_after_ack: Mutex<Option<std::path::PathBuf>>,
        stats: RunnerScaleSetStatistic,
    }

    // Shared handle: the listener clones the session once for its
    // processor, and both clones record into the same script.
    #[derive(Debug, Clone, Default)]
    struct ScriptedSession {
        state: std::sync::Arc<ScriptedState>,
    }

    impl QueueSession for ScriptedSession {
        type Error = SessionError;

        async fn acquire_jobs(&self, request_ids: &[i64]) -> Result<Vec<i64>, Self::Error> {
            self.state
                .acquired
                .lock()
                .unwrap()
                .push(request_ids.to_vec());
            Ok(request_ids.to_vec())
        }
    }

    impl LoopSession for ScriptedSession {
        async fn session_statistics(&self) -> Option<RunnerScaleSetStatistic> {
            Some(self.state.stats)
        }

        async fn session_identity(&self) -> (String, String) {
            (SESSION_ID.to_owned(), "octo-org".to_owned())
        }

        async fn get_message(
            &self,
            last_message_id: i32,
            max_capacity: i32,
        ) -> Result<Option<RunnerScaleSetMessage>, Self::Error> {
            self.state
                .seen_last_id
                .lock()
                .unwrap()
                .push(last_message_id);
            self.state.seen_capacity.lock().unwrap().push(max_capacity);
            Ok(self.state.polls.lock().unwrap().pop_front().flatten())
        }

        async fn delete_message(&self, message_id: i32) -> Result<(), Self::Error> {
            self.state.acks.lock().unwrap().push(message_id);
            if let Some(path) = self.state.cursor_path_after_ack.lock().unwrap().clone() {
                let mut cursors = SessionStore::open(&path).map_err(|error| {
                    SessionError(format!("open interleaved cursor store: {error:#}"))
                })?;
                cursors
                    .save_cursor(7, SESSION_ID, 100, 0)
                    .map_err(|error| {
                        SessionError(format!("advance interleaved cursor: {error:#}"))
                    })?;
            }
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct StubLane;

    #[derive(Debug)]
    struct LaneError(String);

    impl std::fmt::Display for LaneError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "lane: {}", self.0)
        }
    }

    impl std::error::Error for LaneError {}

    impl WorkerLane for StubLane {
        type Error = LaneError;

        async fn provision(
            &mut self,
            _intent: &crate::scaleset::intents::ProvisionIntent,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn note_assigned(
            &mut self,
            _assigned: &velnor_model::ScaleSetJobAssigned,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn note_started(
            &mut self,
            _started: &velnor_model::ScaleSetJobStarted,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn note_terminal(
            &mut self,
            _completed: &velnor_model::ScaleSetJobCompleted,
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-scaleset-listener-{name}-{}-{}",
            std::process::id(),
            velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.db")
    }

    #[test]
    fn session_store_rejects_stale_owner_and_generation() {
        let path = temp_path("session-fence");
        let mut current = SessionStore::open(&path).unwrap();
        current
            .save_session(7, SESSION_NEW_ID, "octo-org", 2)
            .unwrap();
        current.save_cursor(7, SESSION_NEW_ID, 41, 2).unwrap();
        drop(current);

        let mut stale = SessionStore::open(&path).unwrap();
        let error = stale
            .save_session(7, SESSION_OLD_ID, "octo-org", 1)
            .unwrap_err();
        assert!(error.to_string().contains("fenced"));
        let error = stale.save_cursor(7, SESSION_OLD_ID, 99, 1).unwrap_err();
        assert!(error.to_string().contains("fenced"));

        let cursor = stale.get(7).unwrap().unwrap();
        assert_eq!(cursor.session_id, SESSION_NEW_ID);
        assert_eq!(cursor.generation, 2);
        assert_eq!(cursor.last_message_id, 41);
    }

    #[test]
    fn session_store_keeps_same_session_cursor_monotonic() {
        let path = temp_path("cursor-monotonic");
        let mut store = SessionStore::open(&path).unwrap();
        store.save_session(7, SESSION_ID, "octo-org", 3).unwrap();
        store.save_cursor(7, SESSION_ID, 41, 3).unwrap();

        let error = store.save_cursor(7, SESSION_ID, 40, 3).unwrap_err();
        assert!(error.to_string().contains("regressed"));
        store.save_cursor(7, SESSION_ID, 42, 3).unwrap();
        store.save_session(7, SESSION_ID, "octo-org", 4).unwrap();
        assert_eq!(store.get(7).unwrap().unwrap().last_message_id, 42);
    }

    #[test]
    fn concurrent_session_writers_keep_the_highest_generation() {
        let path = temp_path("session-concurrent");
        let mut seed = SessionStore::open(&path).unwrap();
        seed.save_session(7, SESSION_ID, "octo-org", 1).unwrap();
        drop(seed);

        let mut older = SessionStore::open(&path).unwrap();
        let mut newer = SessionStore::open(&path).unwrap();
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let older_start = start.clone();
        let older_thread = std::thread::spawn(move || {
            older_start.wait();
            older.save_session(7, SESSION_NEW_ID, "octo-org", 2)
        });
        let newer_start = start.clone();
        let newer_thread = std::thread::spawn(move || {
            newer_start.wait();
            newer.save_session(7, SESSION_NEXT_ID, "octo-org", 3)
        });
        start.wait();

        let older_result = older_thread.join().unwrap();
        let newer_result = newer_thread.join().unwrap();
        assert!(newer_result.is_ok());
        if let Err(error) = older_result {
            assert!(error.to_string().contains("fenced"));
        }

        let cursor = SessionStore::open(&path).unwrap().get(7).unwrap().unwrap();
        assert_eq!(cursor.session_id, SESSION_NEXT_ID);
        assert_eq!(cursor.generation, 3);
        assert_eq!(cursor.last_message_id, 0);
    }

    fn stats() -> RunnerScaleSetStatistic {
        RunnerScaleSetStatistic {
            total_assigned_jobs: 2,
            ..RunnerScaleSetStatistic::default()
        }
    }

    fn admission() -> OfferAdmission {
        OfferAdmission::exact(
            "tailrocks",
            "tailrocks/velnor",
            "main",
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "push",
        )
    }

    fn offer(id: i64) -> ScaleSetJobAvailable {
        ScaleSetJobAvailable {
            acquire_job_url: String::new(),
            base: ScaleSetJobMessage {
                message_type: ScaleSetJobMessageType::JobAvailable,
                runner_request_id: id,
                repository_name: "velnor".to_owned(),
                owner_name: "tailrocks".to_owned(),
                job_id: format!("job-{id}"),
                job_workflow_ref: "tailrocks/velnor/.github/workflows/ci.yml@main".to_owned(),
                job_display_name: String::new(),
                workflow_run_id: 0,
                event_name: "push".to_owned(),
                request_labels: vec!["velnor".to_owned()],
                queue_time: String::new(),
                scale_set_assign_time: String::new(),
                runner_assign_time: String::new(),
                finish_time: String::new(),
            },
        }
    }

    fn harness(
        path: &std::path::Path,
        polls: Vec<Option<RunnerScaleSetMessage>>,
    ) -> (
        Listener<ScriptedSession, MemLedger, StubLane>,
        ScriptedSession,
    ) {
        let session = ScriptedSession {
            state: std::sync::Arc::new(ScriptedState {
                polls: Mutex::new(polls.into()),
                stats: stats(),
                ..ScriptedState::default()
            }),
        };
        let metrics = Metrics::new();
        let ledger = MemLedger::new();
        ledger.set_max_jobs(2);
        let processor = Processor::new(
            session.clone(),
            ledger,
            StubLane,
            DemandStore::open_with_admission(path, admission()).unwrap(),
            AcquireBatchStore::open(path).unwrap(),
            ProvisionIntentStore::open(path).unwrap(),
            metrics.clone(),
            ProcessorConfig {
                scale_set_id: 7,
                images: ProvisionImages {
                    runner_digest: "sha256:runner".to_owned(),
                    dind_digest: "sha256:dind".to_owned(),
                },
                max_acquire_batch: crate::scaleset::scale::MAX_ACQUIRE_BATCH,
            },
        );
        let listener = Listener::new(
            session.clone(),
            processor,
            SessionStore::open(path).unwrap(),
            metrics,
            LoopConfig {
                scale_set_id: 7,
                retry: RetryPolicy::default(),
                idle: IdlePolicy::default(),
            },
        );
        (listener, session)
    }

    #[tokio::test]
    async fn initial_then_message_then_nil_advances_cursor_and_acks() {
        let path = temp_path("cursor");
        let batch = RunnerScaleSetMessage {
            message_id: 41,
            statistics: Some(stats()),
            job_available_messages: vec![offer(901)],
            ..RunnerScaleSetMessage::default()
        };
        let (mut listener, session) = harness(&path, vec![Some(batch), None]);
        let first = listener.run_once().await.unwrap();
        assert!(matches!(first.kind, ScaleKind::Message { message_id: 41 }));
        assert_eq!(first.acquired, vec![901]);
        let second = listener.run_once().await.unwrap();
        assert_eq!(second.kind, ScaleKind::Nil);
        // Wire order: poll from 0, ACK 41, next poll resumes at 41.
        assert_eq!(
            session.state.seen_last_id.lock().unwrap().as_slice(),
            &[0, 41]
        );
        assert_eq!(session.state.acks.lock().unwrap().as_slice(), &[41]);

        let cursors = SessionStore::open(&path).unwrap();
        let cursor = cursors.get(7).unwrap().unwrap();
        assert_eq!(cursor.last_message_id, 41);
        assert_eq!(cursor.session_id, SESSION_ID);
        let snapshot = listener.metrics().snapshot();
        assert_eq!(snapshot.polls, 2);
        assert_eq!(snapshot.messages, 1);
        assert_eq!(snapshot.nil_polls, 1);
        assert_eq!(snapshot.acks, 1);
        assert_eq!(snapshot.last_message_id, 41);
    }

    #[tokio::test]
    async fn cursor_fence_failure_after_ack_is_reported_and_cursor_stays_monotonic() {
        let path = temp_path("ack-cursor-order");
        let batch = RunnerScaleSetMessage {
            message_id: 41,
            ..RunnerScaleSetMessage::default()
        };
        let (mut listener, session) = harness(&path, vec![Some(batch)]);
        *session.state.cursor_path_after_ack.lock().unwrap() = Some(path.clone());

        let result = listener.run_once().await;
        assert!(matches!(result, Err(ListenerError::Store(_))));
        assert_eq!(session.state.acks.lock().unwrap().as_slice(), &[41]);
        assert_eq!(
            SessionStore::open(&path)
                .unwrap()
                .get(7)
                .unwrap()
                .unwrap()
                .last_message_id,
            100
        );
    }

    #[tokio::test]
    async fn reconcile_runs_before_first_advertisement() {
        let path = temp_path("readvertise");
        let (mut listener, _session) = harness(&path, vec![None]);
        listener.run_once().await.unwrap();
        // First poll reconciled (runs: startup + idle) and advertised the
        // full reconciled N=2 — never before reconciling.
        let snapshot = listener.metrics().snapshot();
        assert_eq!(snapshot.reconcile_runs, 2);
        assert_eq!(snapshot.last_advertised_capacity, 2);
    }

    #[tokio::test]
    async fn run_stops_on_shutdown() {
        let path = temp_path("shutdown");
        let (mut listener, _session) = harness(&path, vec![None, None, None]);
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            flag.store(true, Ordering::Relaxed);
        });
        listener.config.idle.nil_delay = std::time::Duration::from_millis(5);
        listener.run(&shutdown).await;
        assert!(listener.metrics().snapshot().polls >= 1);
    }
}

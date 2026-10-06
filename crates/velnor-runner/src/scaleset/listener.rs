//! Poll→Scale→ACK loop (`listener.go` at [`UPSTREAM_COMMIT`][pin]).
//!
//! Mirrors upstream `Listener.Run`: feed the synthetic initial message
//! ([`INITIAL_MESSAGE_ID`], session statistics) to `Scale` first, then loop
//! `GetMessage(lastMessageID, maxCapacity)` → `Scale(msg)` (nil included)
//! → on success advance the cursor and ACK via `DeleteMessage`. Any step
//! failure skips the ACK so the message is redelivered.
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

    fn upsert(&mut self, cursor: &SessionCursor) -> Result<()> {
        let now = Self::now_rfc3339();
        self.conn
            .execute(
                "INSERT INTO scaleset_sessions
                 (scale_set_id, session_id, owner, last_message_id, stats_json, generation, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (scale_set_id) DO UPDATE SET
                   session_id = excluded.session_id, owner = excluded.owner,
                   last_message_id = excluded.last_message_id, stats_json = excluded.stats_json,
                   generation = excluded.generation, updated_at = excluded.updated_at",
                params![
                    cursor.scale_set_id,
                    cursor.session_id,
                    cursor.owner,
                    cursor.last_message_id,
                    cursor.stats_json,
                    i64::try_from(cursor.generation).unwrap_or(i64::MAX),
                    now,
                ],
            )
            .context("upsert session cursor")?;
        Ok(())
    }

    fn load_or_default(&self, scale_set_id: i32) -> Result<SessionCursor> {
        Ok(self.get(scale_set_id)?.unwrap_or(SessionCursor {
            scale_set_id,
            session_id: String::new(),
            owner: String::new(),
            last_message_id: 0,
            stats_json: None,
            generation: 0,
        }))
    }

    /// Record session identity after (re)connect. Never moves the cursor within
    /// an existing session, but resets last_message_id to 0 on session rollover.
    pub fn save_session(
        &mut self,
        scale_set_id: i32,
        session_id: &str,
        owner: &str,
        generation: u64,
    ) -> Result<()> {
        let mut cursor = self.load_or_default(scale_set_id)?;
        if cursor.session_id != session_id {
            cursor.last_message_id = 0;
        }
        cursor.session_id = session_id.to_owned();
        cursor.owner = owner.to_owned();
        cursor.generation = generation;
        self.upsert(&cursor)
    }

    /// Advance the cursor past an ACKed message. Called AFTER the ACK lands.
    pub fn save_cursor(
        &mut self,
        scale_set_id: i32,
        last_message_id: i32,
        generation: u64,
    ) -> Result<()> {
        let mut cursor = self.load_or_default(scale_set_id)?;
        cursor.last_message_id = last_message_id;
        cursor.generation = generation;
        self.upsert(&cursor)
    }

    /// Persist the authoritative statistics from a poll (every poll,
    /// including nils via the session snapshot).
    pub fn save_stats(
        &mut self,
        scale_set_id: i32,
        stats: &RunnerScaleSetStatistic,
        generation: u64,
    ) -> Result<()> {
        let mut cursor = self.load_or_default(scale_set_id)?;
        cursor.stats_json =
            Some(serde_json::to_string(stats).context("encode session statistics")?);
        cursor.generation = generation;
        self.upsert(&cursor)
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
        let generation = self
            .processor
            .ledger_generation()
            .map_err(ListenerError::Ledger)?;
        self.cursors
            .save_session(self.config.scale_set_id, &session_id, &owner, generation)
            .map_err(ListenerError::Store)?;
        self.cursors
            .save_stats(self.config.scale_set_id, &stats, generation)
            .map_err(ListenerError::Store)?;
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
        if let Some(stats) = self.session.session_statistics().await {
            self.cursors
                .save_stats(self.config.scale_set_id, &stats, generation)
                .map_err(ListenerError::Store)?;
            self.processor.set_cached_stats(stats);
        }
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
        .map_err(ListenerError::Reconcile)?;
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

    /// Real message: persist stats, Scale, ACK-then-persist-cursor.
    async fn scale_message(
        &mut self,
        message: RunnerScaleSetMessage,
        generation: u64,
    ) -> Result<ScaleOutcome, ListenerError<S::Error, W::Error>> {
        self.metrics.inc_messages();
        if let Some(stats) = message.statistics {
            self.cursors
                .save_stats(self.config.scale_set_id, &stats, generation)
                .map_err(ListenerError::Store)?;
        }
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
            .save_cursor(self.config.scale_set_id, message.message_id, generation)
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
    use crate::scaleset::demand::DemandStore;
    use crate::scaleset::intents::{AcquireBatchStore, ProvisionIntentStore};
    use crate::scaleset::scale::{ProcessorConfig, QueueSession};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use velnor_model::{ScaleSetJobAvailable, ScaleSetJobMessage, ScaleSetJobMessageType};

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
            ("session-1".to_owned(), "octo-org".to_owned())
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

    fn stats() -> RunnerScaleSetStatistic {
        RunnerScaleSetStatistic {
            total_assigned_jobs: 2,
            ..RunnerScaleSetStatistic::default()
        }
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
                job_workflow_ref: String::new(),
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
            DemandStore::open(path).unwrap(),
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
        assert_eq!(cursor.session_id, "session-1");
        let snapshot = listener.metrics().snapshot();
        assert_eq!(snapshot.polls, 2);
        assert_eq!(snapshot.messages, 1);
        assert_eq!(snapshot.nil_polls, 1);
        assert_eq!(snapshot.acks, 1);
        assert_eq!(snapshot.last_message_id, 41);
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

//! The `Scale` step function (§5.1 steps 1–5 + 7).
//!
//! Mirrors the upstream `Scaler` contract (`listener.go`): one call per
//! poll, `None` on long-poll timeout, the synthetic initial message first,
//! never concurrent. `Ok` means every effect is durable and the listener
//! may ACK; `Err` means no ACK and the message is redelivered, so every
//! step is idempotent.
//!
//! Step order per poll (steps 6/8/9 live in [`listener`][crate::scaleset::listener]):
//!
//! 1. [`Processor::scale`] folds `JobAssigned/Started/Completed` into
//!    demand rows + ledger transitions idempotently (terminal replays are
//!    no-ops; untracked IDs are not ours and create no rows).
//! 2. `JobAvailable` offers submit to the durable queue (redelivery keeps
//!    age) and the trust gate grants the oldest grantable offers.
//! 3. Granted offers reserve ledger permits (oldest first, bounded by
//!    convergence headroom and free capacity) and the acquire intent
//!    persists BEFORE the HTTP call.
//! 4. `acquirejobs` answers reconcile as sets: acquired members move on,
//!    missing members release + re-queue with age kept, transport failure
//!    marks the batch uncertain (still counted, still ACKable).
//! 5. Acquired-without-intent rows persist provision intents and run the
//!    worker lane (query-based, so redelivery retries failed provisions).
//! 7. Population converges on `TotalAssignedJobs` (cached for nil polls).
//!
//! Offer-validity guard: `Ok` proves every `JobAvailable` in the batch
//! reached a durable row. Every demand state is terminal-for-this-message:
//! `observed` rows durably park trust-unknown offers, `eligible`/`granted`
//! rows re-queue with age kept, and the rest record grant/acquire/provision
//! progress. A missing row vetoes the ACK.

use anyhow::{Context, Result};
use velnor_model::{
    RunnerScaleSetMessage, RunnerScaleSetStatistic, ScaleSetJobAssigned, ScaleSetJobCompleted,
    ScaleSetJobStarted,
};

use crate::protocol::TaskResult;
use crate::scaleset::capacity::{
    reserve_for_offer_with_token, CapacityLedger, LedgerLane, LedgerPermitState, ReserveOutcome,
};
use crate::scaleset::converge::{
    ensure_provision_intent, local_population, reconcile_population, PopulationDecision,
    ProvisionImages, WorkerLane,
};
use crate::scaleset::demand::{grant_oldest, Demand, DemandState, DemandStore, SubmitOutcome};
use crate::scaleset::intents::{
    mint_batch_id, permit_holder, reconcile_returned_ids, AcquireBatchStore, ProvisionIntentStore,
};
use crate::scaleset::metrics::Metrics;
use crate::scaleset::reconcile::unknown_event;

/// The queue calls the loop needs. [`SessionQueue`] adapts the real
/// session client; tests script this trait.
pub trait QueueSession {
    type Error: std::error::Error + Send + Sync + 'static;

    /// `POST .../acquirejobs` with the raw request-ID array; returns the
    /// acquired subset (never assumed to echo the request).
    async fn acquire_jobs(&self, request_ids: &[i64]) -> Result<Vec<i64>, Self::Error>;
}

/// Server batch cap mirrored locally: one acquire round never exceeds it.
pub const MAX_ACQUIRE_BATCH: usize = 50;

/// Static processor configuration.
#[derive(Debug, Clone)]
pub struct ProcessorConfig {
    pub scale_set_id: i32,
    pub images: ProvisionImages,
    pub max_acquire_batch: usize,
}

/// What one [`Processor::scale`] call did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleOutcome {
    pub kind: ScaleKind,
    pub assigned: usize,
    pub started: usize,
    pub completed: usize,
    pub offers_seen: usize,
    pub offers_submitted: usize,
    pub granted: usize,
    pub acquired: Vec<i64>,
    pub missing: Vec<i64>,
    pub uncertain: Vec<i64>,
    pub provisioned: Vec<i64>,
    pub decision: Option<PopulationDecision>,
}

/// Which shape of message was scaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleKind {
    /// Synthetic initial message (`message_id == -1`): stats only.
    Initial,
    /// Long-poll timeout (`None`): converge on cached stats.
    Nil,
    /// A real queued batch.
    Message { message_id: i32 },
}

/// The §5.1 step function. Owns its stores, ledger handle, worker lane,
/// and queue handle; `scale` takes `&mut self` (never concurrent, mirroring
/// upstream).
pub struct Processor<Q, L, W> {
    queue: Q,
    ledger: L,
    lane: W,
    demand: DemandStore,
    batches: AcquireBatchStore,
    provision: ProvisionIntentStore,
    metrics: Metrics,
    config: ProcessorConfig,
    cached_stats: Option<RunnerScaleSetStatistic>,
}

impl<Q, L, W> Processor<Q, L, W> {
    #[allow(
        clippy::too_many_arguments,
        reason = "single processor construction site"
    )]
    pub fn new(
        queue: Q,
        ledger: L,
        lane: W,
        demand: DemandStore,
        batches: AcquireBatchStore,
        provision: ProvisionIntentStore,
        metrics: Metrics,
        config: ProcessorConfig,
    ) -> Self {
        Self {
            queue,
            ledger,
            lane,
            demand,
            batches,
            provision,
            metrics,
            config,
            cached_stats: None,
        }
    }

    #[must_use]
    pub fn cached_stats(&self) -> Option<RunnerScaleSetStatistic> {
        self.cached_stats
    }

    /// Refresh the authoritative snapshot observed outside a real message
    /// (the message session exposes statistics on 202/None polls too).
    pub fn set_cached_stats(&mut self, stats: RunnerScaleSetStatistic) {
        self.cached_stats = Some(stats);
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn ledger_mut(&mut self) -> &mut L {
        &mut self.ledger
    }

    pub fn demand_mut(&mut self) -> &mut DemandStore {
        &mut self.demand
    }

    pub fn batches_mut(&mut self) -> &mut AcquireBatchStore {
        &mut self.batches
    }

    pub fn lane_mut(&mut self) -> &mut W {
        &mut self.lane
    }

    /// Disjoint mutable access to the reconcile inputs (one method so the
    /// borrow checker sees the disjoint fields). The lane answers worker
    /// ownership for the idle orphan release.
    pub fn parts_mut(&mut self) -> (&mut L, &mut DemandStore, &mut AcquireBatchStore, &mut W) {
        (
            &mut self.ledger,
            &mut self.demand,
            &mut self.batches,
            &mut self.lane,
        )
    }

    pub fn ledger_ref(&self) -> &L {
        &self.ledger
    }

    /// Current ledger generation as [`anyhow::Error`] for loop plumbing.
    pub fn ledger_generation(&self) -> Result<u64>
    where
        L: CapacityLedger,
    {
        self.ledger
            .generation()
            .map_err(|error| anyhow::anyhow!("read ledger generation: {error}"))
    }
}

impl<Q: QueueSession, L: CapacityLedger, W: WorkerLane> Processor<Q, L, W> {
    /// Run steps 1–5 + 7 over one poll result. `Ok` licenses the ACK.
    pub async fn scale(
        &mut self,
        message: Option<&RunnerScaleSetMessage>,
    ) -> Result<ScaleOutcome, ScaleError<Q::Error, W::Error>> {
        let Some(message) = message else {
            return self.scale_idle().await;
        };
        if message.message_id == crate::scaleset::listener::INITIAL_MESSAGE_ID {
            return self.scale_initial(message);
        }
        self.scale_message(message).await
    }

    /// Resume durable work on an empty poll. Offers and partial acquire
    /// batches can outlive the message that created them, so grants,
    /// acquires, and provisions must not depend on another queue message.
    async fn scale_idle(&mut self) -> Result<ScaleOutcome, ScaleError<Q::Error, W::Error>> {
        self.recover_demand_release_stages()?;
        self.require_no_pending_worker_releases()?;
        let generation = self.generation()?;
        let eligible = self
            .demand
            .oldest_eligible(self.config.scale_set_id, 512)
            .map_err(ScaleError::Store)?;
        for row in &eligible {
            self.observe_global_demand(row)?;
        }
        let granted = grant_oldest(
            &mut self.demand,
            self.config.scale_set_id,
            generation,
            &self.metrics,
        )
        .map_err(ScaleError::Store)?;
        for row in &granted {
            self.observe_global_demand(row)?;
        }
        let (acquired, missing, uncertain) = self.acquire_pass(self.cached_stats).await?;
        let canceled = self.drain_canceled_acquisitions()?;
        let provisioned = self.provision_pass().await?;
        let decision = self
            .cached_stats
            .map(|stats| {
                let local = self.local_count()?;
                Ok(reconcile_population(&stats, local))
            })
            .transpose()?;
        Ok(ScaleOutcome {
            kind: ScaleKind::Nil,
            granted: granted.len(),
            acquired,
            missing,
            uncertain,
            completed: canceled,
            provisioned,
            decision,
            ..ScaleOutcome::empty()
        })
    }

    fn scale_initial(
        &mut self,
        message: &RunnerScaleSetMessage,
    ) -> Result<ScaleOutcome, ScaleError<Q::Error, W::Error>> {
        let Some(stats) = message.statistics else {
            return Err(ScaleError::MissingInitialStats);
        };
        self.cached_stats = Some(stats);
        let local = self.local_count()?;
        Ok(ScaleOutcome {
            kind: ScaleKind::Initial,
            decision: Some(reconcile_population(&stats, local)),
            ..ScaleOutcome::empty()
        })
    }

    async fn scale_message(
        &mut self,
        message: &RunnerScaleSetMessage,
    ) -> Result<ScaleOutcome, ScaleError<Q::Error, W::Error>> {
        if let Some(stats) = message.statistics {
            self.cached_stats = Some(stats);
        }
        unknown_event(&self.metrics, &message.unknown_message_types);

        // Step 1: idempotent observations.
        let mut outcome = ScaleOutcome::empty();
        outcome.kind = ScaleKind::Message {
            message_id: message.message_id,
        };
        let offered_request_ids: std::collections::HashSet<i64> = message
            .job_available_messages
            .iter()
            .map(|offer| crate::scaleset::demand::resolve_job_request_id(&offer.base))
            .collect();
        // Parser buckets event types, losing wire order. If this batch has
        // both JobAvailable and JobAssigned for one request, acquirejobs
        // must establish token ownership before the assignment observation.
        for assigned in &message.job_assigned_messages {
            let request_id = crate::scaleset::demand::resolve_job_request_id(&assigned.base);
            if offered_request_ids.contains(&request_id) {
                continue;
            }
            if self.observe_assigned(assigned)? {
                outcome.assigned += 1;
            }
        }
        for started in &message.job_started_messages {
            if self.observe_started(started)? {
                outcome.started += 1;
            }
        }
        for completed in &message.job_completed_messages {
            if self.observe_completed(completed)? {
                outcome.completed += 1;
            }
        }
        self.recover_demand_release_stages()?;
        self.require_no_pending_worker_releases()?;

        // Step 2: queue offers, then grant the oldest grantable ones.
        let generation = self.generation()?;
        outcome.offers_seen = message.job_available_messages.len();
        for offer in &message.job_available_messages {
            let request_id = crate::scaleset::demand::resolve_job_request_id(&offer.base);
            match self
                .demand
                .submit_offer(self.config.scale_set_id, offer, generation)
                .map_err(ScaleError::Store)?
            {
                SubmitOutcome::Inserted { .. } => outcome.offers_submitted += 1,
                SubmitOutcome::Redelivered { .. } | SubmitOutcome::ReofferedTerminal => {}
            }
            if let Some(row) = self.demand.get(request_id).map_err(ScaleError::Store)? {
                self.sync_global_demand(&row)?;
            }
        }
        self.metrics.add_offers_observed(outcome.offers_seen as u64);
        let granted = grant_oldest(
            &mut self.demand,
            self.config.scale_set_id,
            generation,
            &self.metrics,
        )
        .map_err(ScaleError::Store)?;
        for row in &granted {
            self.observe_global_demand(row)?;
        }
        outcome.granted = granted.len();

        // Steps 3–4: reserve + acquire, bounded by convergence headroom.
        let (acquired, missing, uncertain) = self
            .acquire_pass(message.statistics.or(self.cached_stats))
            .await?;
        outcome.acquired = acquired;
        outcome.missing = missing;
        outcome.uncertain = uncertain;

        // Process assignments for same-batch offers only after acquirejobs
        // has either persisted their exact token or left them unacquired.
        for assigned in &message.job_assigned_messages {
            let request_id = crate::scaleset::demand::resolve_job_request_id(&assigned.base);
            if offered_request_ids.contains(&request_id) && self.observe_assigned(assigned)? {
                outcome.assigned += 1;
            }
        }

        outcome.completed += self.drain_canceled_acquisitions()?;

        // Step 5: provision every acquired row still missing its intent.
        outcome.provisioned = self.provision_pass().await?;

        // Offer-validity guard: every offer in this batch must have a
        // durable row before `Ok` licenses the ACK.
        for offer in &message.job_available_messages {
            let request_id = crate::scaleset::demand::resolve_job_request_id(&offer.base);
            let present = self
                .demand
                .get(request_id)
                .map_err(ScaleError::Store)?
                .is_some();
            if !present {
                return Err(ScaleError::OfferWithoutRow { request_id });
            }
        }

        // Step 7: converge on the authoritative statistics.
        let stats = message.statistics.or(self.cached_stats);
        outcome.decision = stats
            .map(|stats| {
                let local = self.local_count()?;
                Ok(reconcile_population(&stats, local))
            })
            .transpose()?;

        Ok(outcome)
    }

    fn generation(&self) -> Result<u64, ScaleError<Q::Error, W::Error>> {
        self.ledger
            .generation()
            .map_err(|error| ScaleError::Ledger(ledger_error(error)))
    }

    fn release_with_projection(
        &mut self,
        request_id: i64,
        attempt_token: &str,
        kind: OwnedReleaseKind,
        next_state: DemandState,
    ) -> Result<bool, ScaleError<Q::Error, W::Error>> {
        self.release_with_projection_batch(request_id, attempt_token, kind, next_state, None)
    }

    fn release_with_projection_batch(
        &mut self,
        request_id: i64,
        attempt_token: &str,
        kind: OwnedReleaseKind,
        next_state: DemandState,
        batch_id: Option<&str>,
    ) -> Result<bool, ScaleError<Q::Error, W::Error>> {
        let holder = permit_holder(self.config.scale_set_id, request_id);
        let ledger_state = match kind {
            OwnedReleaseKind::Eligible => velnor_control::permit_ledger::DemandState::Eligible,
            OwnedReleaseKind::Canceled => velnor_control::permit_ledger::DemandState::Cancelled,
            OwnedReleaseKind::Terminal => velnor_control::permit_ledger::DemandState::Terminal,
        };
        match batch_id {
            Some(batch_id) => self.demand.stage_attempt_release_for_batch(
                &holder,
                self.config.scale_set_id,
                request_id,
                attempt_token,
                ledger_state,
                Some(next_state),
                batch_id,
            ),
            None => self.demand.stage_attempt_release(
                &holder,
                self.config.scale_set_id,
                request_id,
                attempt_token,
                ledger_state,
                Some(next_state),
                None,
            ),
        }
        .map_err(ScaleError::Store)?;
        let released = release_attempt(&mut self.ledger, &holder, attempt_token, kind)
            .map_err(ScaleError::Ledger)?;
        self.demand
            .finish_attempt_release(&holder, self.generation()?)
            .map_err(ScaleError::Store)?;
        Ok(released)
    }

    fn recover_demand_release_stages(&mut self) -> Result<(), ScaleError<Q::Error, W::Error>> {
        let stages = self
            .demand
            .pending_attempt_releases()
            .map_err(ScaleError::Store)?;
        for stage in stages {
            if stage.worker_ownership_id.is_some() {
                continue;
            }
            let kind = match stage.ledger_demand_state {
                velnor_control::permit_ledger::DemandState::Eligible => OwnedReleaseKind::Eligible,
                velnor_control::permit_ledger::DemandState::Cancelled => OwnedReleaseKind::Canceled,
                velnor_control::permit_ledger::DemandState::Terminal => OwnedReleaseKind::Terminal,
                _ => {
                    return Err(ScaleError::Store(anyhow::anyhow!(
                        "staged release for {} has invalid ledger demand state",
                        stage.holder
                    )));
                }
            };
            release_attempt(&mut self.ledger, &stage.holder, &stage.attempt_token, kind)
                .map_err(ScaleError::Ledger)?;
            self.demand
                .finish_attempt_release(&stage.holder, self.generation()?)
                .map_err(ScaleError::Store)?;
        }
        Ok(())
    }

    fn require_no_pending_worker_releases(&self) -> Result<(), ScaleError<Q::Error, W::Error>> {
        if self
            .demand
            .pending_attempt_releases()
            .map_err(ScaleError::Store)?
            .into_iter()
            .any(|stage| stage.worker_ownership_id.is_some())
        {
            return Err(ScaleError::Store(anyhow::anyhow!(
                "Scale Set worker release awaits lane recovery; refusing new work"
            )));
        }
        Ok(())
    }

    fn local_count(&self) -> Result<u32, ScaleError<Q::Error, W::Error>> {
        local_population(&self.demand, self.config.scale_set_id).map_err(ScaleError::Store)
    }

    fn observe_global_demand(
        &mut self,
        row: &Demand,
    ) -> Result<(), ScaleError<Q::Error, W::Error>> {
        let parsed = velnor_model::Timestamp::parse(&row.first_seen_at)
            .context("parse durable Scale Set first-seen time")
            .map_err(ScaleError::Store)?;
        let first_seen_unix = u64::try_from(parsed.as_offset_datetime().unix_timestamp())
            .context("Scale Set first-seen time predates Unix epoch")
            .map_err(ScaleError::Store)?;
        let holder = permit_holder(row.scale_set_id, row.request_id);
        let scope = format!("scaleset/{}", row.scale_set_id);
        self.ledger
            .observe_demand(
                &holder,
                LedgerLane::ScaleSet,
                &scope,
                first_seen_unix,
                velnor_control::permit_ledger::unix_now(),
            )
            .map_err(|error| ScaleError::Ledger(ledger_error(error)))
    }

    fn sync_global_demand(&mut self, row: &Demand) -> Result<(), ScaleError<Q::Error, W::Error>> {
        match row.state {
            DemandState::Observed | DemandState::Declined => {
                let holder = permit_holder(row.scale_set_id, row.request_id);
                if self
                    .ledger
                    .holder_state(&holder)
                    .map_err(|error| ScaleError::Ledger(ledger_error(error)))?
                    .is_some()
                {
                    return Err(ScaleError::Store(anyhow::anyhow!(
                        "non-permit demand {holder:?} still has a held permit"
                    )));
                }
                self.ledger
                    .cancel_demand(&holder)
                    .map_err(|error| ScaleError::Ledger(ledger_error(error)))?;
            }
            DemandState::Terminal => {}
            DemandState::CanceledPending | DemandState::CanceledAcquired => {}
            DemandState::CanceledDone => {
                let holder = permit_holder(row.scale_set_id, row.request_id);
                if let Some(attempt_token) = row.permit_attempt_token.as_deref() {
                    self.release_with_projection(
                        row.request_id,
                        attempt_token,
                        OwnedReleaseKind::Canceled,
                        DemandState::CanceledDone,
                    )?;
                } else {
                    if self
                        .ledger
                        .holder_state(&holder)
                        .map_err(|error| ScaleError::Ledger(ledger_error(error)))?
                        .is_some()
                    {
                        return Err(ScaleError::Store(anyhow::anyhow!(
                            "canceled demand {holder:?} has a permit but no attempt token"
                        )));
                    }
                    self.ledger
                        .cancel_demand(&holder)
                        .map_err(|error| ScaleError::Ledger(ledger_error(error)))?;
                }
            }
            DemandState::Eligible
            | DemandState::Granted
            | DemandState::AcquireIntent
            | DemandState::Acquired
            | DemandState::Uncertain
            | DemandState::ProvisionIntent => self.observe_global_demand(row)?,
        }
        Ok(())
    }

    /// Require the token captured by reserve or serialized startup recovery.
    /// Runtime events never create ownership from a holder-only row.
    fn ensure_attempt_token(&self, row: &Demand) -> Result<String, ScaleError<Q::Error, W::Error>> {
        let holder = permit_holder(row.scale_set_id, row.request_id);
        let Some(attempt_token) = row
            .permit_attempt_token
            .as_deref()
            .filter(|token| !token.is_empty())
        else {
            return Err(ScaleError::Store(anyhow::anyhow!(
                "active demand {holder:?} has no captured attempt token; refusing holder-only ownership"
            )));
        };
        if !self
            .ledger
            .is_current_attempt(&holder, attempt_token)
            .map_err(|error| ScaleError::Ledger(ledger_error(error)))?
        {
            return Err(ScaleError::Store(anyhow::anyhow!(
                "persisted permit token for {holder:?} is stale"
            )));
        }
        Ok(attempt_token.to_owned())
    }

    fn drain_canceled_acquisitions(&mut self) -> Result<usize, ScaleError<Q::Error, W::Error>> {
        let canceled = self
            .demand
            .list_in_states(
                self.config.scale_set_id,
                &[DemandState::CanceledAcquired, DemandState::CanceledDone],
            )
            .map_err(ScaleError::Store)?;
        let mut completed = 0;
        for (request_id, state) in canceled {
            let row = self
                .demand
                .get(request_id)
                .map_err(ScaleError::Store)?
                .ok_or_else(|| {
                    ScaleError::Store(anyhow::anyhow!("canceled demand row {request_id} vanished"))
                })?;
            let attempt_token = required_attempt_token(&row).map_err(ScaleError::Store)?;
            if state == DemandState::CanceledAcquired {
                self.lane
                    .note_canceled(request_id, attempt_token)
                    .map_err(ScaleError::Lane)?;
            }
            self.release_with_projection(
                request_id,
                attempt_token,
                OwnedReleaseKind::Canceled,
                DemandState::Terminal,
            )?;
            completed += 1;
        }
        Ok(completed)
    }

    fn close_canceled_unacquired(
        &mut self,
        request_id: i64,
    ) -> Result<(), ScaleError<Q::Error, W::Error>> {
        let generation = self.generation()?;
        let row = self
            .demand
            .get(request_id)
            .map_err(ScaleError::Store)?
            .ok_or_else(|| {
                ScaleError::Store(anyhow::anyhow!("canceled demand row {request_id} vanished"))
            })?;
        let holder = permit_holder(self.config.scale_set_id, request_id);
        let held = self
            .ledger
            .holder_state(&holder)
            .map_err(|error| ScaleError::Ledger(ledger_error(error)))?
            .is_some();
        let attempt_token = row.permit_attempt_token.as_deref();
        if held {
            let attempt_token = required_attempt_token(&row).map_err(ScaleError::Store)?;
            self.release_with_projection(
                request_id,
                attempt_token,
                OwnedReleaseKind::Canceled,
                DemandState::Terminal,
            )?;
        } else {
            if let Some(attempt_token) = attempt_token {
                self.demand
                    .set_state_owned(
                        request_id,
                        DemandState::CanceledDone,
                        None,
                        generation,
                        attempt_token,
                    )
                    .map_err(ScaleError::Store)?;
            } else {
                self.demand
                    .set_state(request_id, DemandState::CanceledDone, None, generation)
                    .map_err(ScaleError::Store)?;
            }
            self.ledger
                .cancel_demand(&holder)
                .map_err(|error| ScaleError::Ledger(ledger_error(error)))?;
        }
        if let Some(attempt_token) = attempt_token {
            self.demand.set_state_owned(
                request_id,
                DemandState::Terminal,
                None,
                generation,
                attempt_token,
            )
        } else {
            self.demand
                .set_state(request_id, DemandState::Terminal, None, generation)
        }
        .map_err(ScaleError::Store)
    }

    /// Step-1 `JobAssigned`: only rows already backed by an acquire token
    /// reach the lane. A direct assignment without prior offer/acquire proof
    /// is recorded as unconfirmed and fails closed.
    /// Untracked IDs and stake-less rows (another adapter won the race)
    /// are not ours: counted, never claimed. Returns whether the
    /// observation matched tracked work.
    fn observe_assigned(
        &mut self,
        assigned: &ScaleSetJobAssigned,
    ) -> Result<bool, ScaleError<Q::Error, W::Error>> {
        let generation = self.generation()?;
        let (request_id, state) = self
            .demand
            .submit_assigned(self.config.scale_set_id, assigned, generation)
            .map_err(ScaleError::Store)?;
        if state == DemandState::Terminal || state == DemandState::Declined {
            return Ok(true);
        }
        if !matches!(
            state,
            DemandState::Acquired | DemandState::ProvisionIntent | DemandState::CanceledAcquired
        ) {
            return Ok(true);
        }
        let row = self
            .demand
            .get(request_id)
            .map_err(ScaleError::Store)?
            .ok_or_else(|| {
                ScaleError::Store(anyhow::anyhow!("assigned demand row {request_id} vanished"))
            })?;
        let attempt_token = self.ensure_attempt_token(&row)?;
        self.observe_global_demand(&row)?;
        let holder = permit_holder(self.config.scale_set_id, request_id);
        fenced_transition(
            &mut self.ledger,
            &holder,
            LedgerPermitState::Acquiring,
            generation,
            &attempt_token,
        )
        .map_err(ScaleError::Store)?;
        self.lane
            .note_assigned(assigned, &attempt_token)
            .map_err(ScaleError::Lane)?;
        Ok(true)
    }

    /// Step-1 `JobStarted`: staked rows forward to the lane (permits
    /// adopted when a started-only stream skipped the assignment);
    /// stake-less rows are another adapter's running job. Untracked IDs
    /// are not ours.
    fn observe_started(
        &mut self,
        started: &ScaleSetJobStarted,
    ) -> Result<bool, ScaleError<Q::Error, W::Error>> {
        let request_id = crate::scaleset::demand::resolve_job_request_id(&started.base);
        let Some(request_id) = crate::scaleset::intents::request_id_for_runner(
            self.config.scale_set_id,
            &started.runner_name,
            request_id,
        ) else {
            return Ok(false);
        };
        let Some(row) = self.demand.get(request_id).map_err(ScaleError::Store)? else {
            return Ok(false);
        };
        if row.state == DemandState::Terminal || row.state == DemandState::Declined {
            return Ok(true);
        }
        let holder = permit_holder(self.config.scale_set_id, request_id);
        if row.state == DemandState::CanceledPending {
            let generation = self.generation()?;
            let attempt_token = required_attempt_token(&row).map_err(ScaleError::Store)?;
            self.demand
                .set_state_owned(
                    request_id,
                    DemandState::CanceledAcquired,
                    None,
                    generation,
                    attempt_token,
                )
                .map_err(ScaleError::Store)?;
            fenced_transition(
                &mut self.ledger,
                &holder,
                LedgerPermitState::Acquiring,
                generation,
                &attempt_token,
            )
            .map_err(ScaleError::Store)?;
            self.lane
                .note_started(started, &attempt_token)
                .map_err(ScaleError::Lane)?;
            return Ok(true);
        }
        if row.state == DemandState::CanceledAcquired {
            let attempt_token = required_attempt_token(&row).map_err(ScaleError::Store)?;
            self.lane
                .note_started(started, &attempt_token)
                .map_err(ScaleError::Lane)?;
            return Ok(true);
        }
        if !row.state.holds_permit() {
            return Ok(true);
        }
        let generation = self.generation()?;
        self.observe_global_demand(&row)?;
        let attempt_token = self.ensure_attempt_token(&row)?;
        if matches!(
            row.state,
            DemandState::Granted | DemandState::AcquireIntent | DemandState::Uncertain
        ) {
            self.demand
                .set_state_owned(
                    request_id,
                    DemandState::ProvisionIntent,
                    None,
                    generation,
                    &attempt_token,
                )
                .map_err(ScaleError::Store)?;
        }
        fenced_transition(
            &mut self.ledger,
            &holder,
            LedgerPermitState::Acquiring,
            generation,
            &attempt_token,
        )
        .map_err(ScaleError::Store)?;
        self.lane
            .note_started(started, &attempt_token)
            .map_err(ScaleError::Lane)?;
        Ok(true)
    }

    /// Step-1 `JobCompleted`: tracked rows go `terminal` (foreign queued
    /// rows too — the job is done, so offering ends); permits move to
    /// `cleaning` while held, and staked completions drive the lane's
    /// export + cleanup. Fully-released replays are no-ops; held replays
    /// retry the lane call.
    fn observe_completed(
        &mut self,
        completed: &ScaleSetJobCompleted,
    ) -> Result<bool, ScaleError<Q::Error, W::Error>> {
        let request_id = crate::scaleset::demand::resolve_job_request_id(&completed.base);
        let Some(request_id) = crate::scaleset::intents::request_id_for_runner(
            self.config.scale_set_id,
            &completed.runner_name,
            request_id,
        ) else {
            return Ok(false);
        };
        let Some(row) = self.demand.get(request_id).map_err(ScaleError::Store)? else {
            return Ok(false);
        };
        let holder = permit_holder(self.config.scale_set_id, request_id);
        let held = self
            .ledger
            .holder_state(&holder)
            .map_err(|error| ScaleError::Ledger(ledger_error(error)))?
            .is_some();
        let attempt_token = row.permit_attempt_token.as_deref();
        // The wire carries both `actions/runner` PascalCase and Velnor
        // lowercase spellings; parse via the protocol result type so a
        // `Canceled` completion takes the cancel path too.
        let canceled = matches!(
            TaskResult::parse_wire(&completed.result),
            Some(TaskResult::Canceled)
        );
        if row.state == DemandState::Terminal && !held {
            if canceled {
                if let Some(attempt_token) = attempt_token {
                    self.release_with_projection(
                        request_id,
                        attempt_token,
                        OwnedReleaseKind::Canceled,
                        DemandState::Terminal,
                    )?;
                } else {
                    if self
                        .ledger
                        .holder_state(&holder)
                        .map_err(|error| ScaleError::Ledger(ledger_error(error)))?
                        .is_some()
                    {
                        return Err(ScaleError::Store(anyhow::anyhow!(
                            "canceled terminal demand {holder:?} has a permit but no attempt token"
                        )));
                    }
                    self.ledger
                        .cancel_demand(&holder)
                        .map_err(|error| ScaleError::Ledger(ledger_error(error)))?;
                }
            }
            return Ok(true);
        }

        if canceled {
            match row.state {
                DemandState::Observed | DemandState::Declined => {
                    self.close_canceled_unacquired(request_id)?;
                    return Ok(true);
                }
                DemandState::Eligible | DemandState::Granted => {
                    self.observe_global_demand(&row)?;
                    self.close_canceled_unacquired(request_id)?;
                    return Ok(true);
                }
                DemandState::AcquireIntent
                | DemandState::Uncertain
                | DemandState::CanceledPending => {
                    // An unbatched intent is before the network boundary, so
                    // this old canceled attempt can close immediately. For a
                    // batched intent, keep occupancy until acquirejobs proves
                    // whether the runner took ownership. Upstream emits a
                    // new request for a requeued retry.
                    if row.state == DemandState::AcquireIntent
                        && !self
                            .batches
                            .contains_request(self.config.scale_set_id, request_id)
                            .map_err(ScaleError::Store)?
                    {
                        self.observe_global_demand(&row)?;
                        self.close_canceled_unacquired(request_id)?;
                        return Ok(true);
                    }
                    let generation = self.generation()?;
                    let attempt_token = attempt_token.ok_or_else(|| {
                        ScaleError::Store(anyhow::anyhow!(
                            "canceled held request {request_id} has no permit attempt token"
                        ))
                    })?;
                    fenced_transition(
                        &mut self.ledger,
                        &holder,
                        LedgerPermitState::Uncertain,
                        generation,
                        attempt_token,
                    )
                    .map_err(ScaleError::Ledger)?;
                    self.demand
                        .set_state_owned(
                            request_id,
                            DemandState::CanceledPending,
                            None,
                            generation,
                            attempt_token,
                        )
                        .map_err(ScaleError::Store)?;
                    return Ok(true);
                }
                DemandState::CanceledAcquired
                | DemandState::Acquired
                | DemandState::ProvisionIntent => {
                    // Durable acquire response or worker state proves this
                    // is an owned cancellation. Drive cleanup before ACK.
                }
                DemandState::CanceledDone => {
                    self.close_canceled_unacquired(request_id)?;
                    return Ok(true);
                }
                DemandState::Terminal => {}
            }
        }
        let staked = row.state.holds_permit() || held;
        let generation = self.generation()?;
        if row.state != DemandState::Terminal {
            if let Some(attempt_token) = attempt_token {
                self.demand
                    .set_state_owned(
                        request_id,
                        DemandState::Terminal,
                        None,
                        generation,
                        attempt_token,
                    )
                    .map_err(ScaleError::Store)?;
            } else {
                self.demand
                    .set_state(request_id, DemandState::Terminal, None, generation)
                    .map_err(ScaleError::Store)?;
            }
        }
        if held {
            let attempt_token = attempt_token.ok_or_else(|| {
                ScaleError::Store(anyhow::anyhow!(
                    "held request {request_id} has no permit attempt token"
                ))
            })?;
            fenced_transition(
                &mut self.ledger,
                &holder,
                LedgerPermitState::Cleaning,
                generation,
                attempt_token,
            )
            .map_err(ScaleError::Ledger)?;
        }
        if staked {
            let attempt_token = attempt_token.ok_or_else(|| {
                ScaleError::Store(anyhow::anyhow!(
                    "owned request {request_id} has no permit attempt token"
                ))
            })?;
            self.lane
                .note_terminal(completed, attempt_token)
                .map_err(ScaleError::Lane)?;
        } else if canceled {
            self.ledger
                .cancel_demand(&holder)
                .map_err(|error| ScaleError::Ledger(ledger_error(error)))?;
        }
        Ok(true)
    }

    /// Steps 3–4: reserve permits for granted offers (oldest first, bounded
    /// by convergence headroom, batch cap, and free capacity), persist the
    /// acquire intent, call `acquirejobs`, and set-reconcile the answer.
    async fn acquire_pass(
        &mut self,
        stats: Option<RunnerScaleSetStatistic>,
    ) -> Result<(Vec<i64>, Vec<i64>, Vec<i64>), ScaleError<Q::Error, W::Error>> {
        let Some(stats) = stats else {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        };
        let headroom = match reconcile_population(&stats, self.local_count()?) {
            PopulationDecision::AcquireMore { headroom, .. } => headroom,
            PopulationDecision::Hold { .. } => 0,
        };
        if headroom == 0 {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }
        let take = usize::try_from(headroom)
            .unwrap_or(usize::MAX)
            .min(self.config.max_acquire_batch);
        let candidates = self
            .demand
            .oldest_granted(self.config.scale_set_id, take)
            .map_err(ScaleError::Store)?;
        if candidates.is_empty() {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }
        for candidate in &candidates {
            self.observe_global_demand(candidate)?;
        }

        // Step 3: reserve oldest-first; the first exhausted permit stops the
        // take and the rest stay granted for the next poll.
        let mut generation = self.generation()?;
        let mut taken: Vec<(i64, String, String)> = Vec::new();
        let mut staged_acquires = Vec::new();
        for candidate in &candidates {
            let holder = permit_holder(self.config.scale_set_id, candidate.request_id);
            let attempt_token = uuid::Uuid::new_v4().to_string();
            let stage = self
                .demand
                .stage_attempt_acquire(
                    &holder,
                    self.config.scale_set_id,
                    candidate.request_id,
                    candidate.permit_attempt_token.as_deref(),
                    &attempt_token,
                )
                .map_err(ScaleError::Store)?;
            let reserved =
                reserve_for_offer_with_token(&mut self.ledger, &holder, generation, &attempt_token)
                    .map_err(|error| {
                        ScaleError::Ledger(anyhow::anyhow!("reserve permit: {error}"))
                    })?;
            match reserved {
                (
                    ReserveOutcome::Reserved {
                        attempt_token: acquired_token,
                    },
                    landed,
                ) => {
                    if acquired_token != attempt_token {
                        return Err(ScaleError::Store(anyhow::anyhow!(
                            "ledger returned a different token for staged acquire {holder:?}"
                        )));
                    }
                    self.demand
                        .finish_attempt_acquire(&stage, true)
                        .map_err(ScaleError::Store)?;
                    generation = landed;
                    taken.push((candidate.request_id, holder, acquired_token));
                    staged_acquires.push(stage);
                }
                (ReserveOutcome::AlreadyHeld, _) => {
                    return Err(ScaleError::Store(anyhow::anyhow!(
                        "ledger already holds {holder:?} without transferring ownership; startup recovery must prove prior attempt dead"
                    )));
                }
                (ReserveOutcome::CapacityExhausted | ReserveOutcome::NotConfigured, landed) => {
                    self.demand
                        .finish_attempt_acquire(&stage, false)
                        .map_err(ScaleError::Store)?;
                    generation = landed;
                    break;
                }
            }
        }
        if taken.is_empty() {
            return Ok((Vec::new(), Vec::new(), Vec::new()));
        }
        let request_ids: Vec<i64> = taken.iter().map(|(id, _, _)| *id).collect();
        let holders: Vec<String> = taken.iter().map(|(_, holder, _)| holder.clone()).collect();
        let attempt_tokens: Vec<String> = taken
            .iter()
            .map(|(_, _, attempt_token)| attempt_token.clone())
            .collect();
        let batch_id = mint_batch_id(self.config.scale_set_id);
        self.batches
            .record_intended(
                &batch_id,
                self.config.scale_set_id,
                &request_ids,
                &holders,
                &attempt_tokens,
                generation,
            )
            .map_err(ScaleError::Store)?;
        for stage in &staged_acquires {
            self.demand
                .finish_attempt_acquire_batch(stage)
                .map_err(ScaleError::Store)?;
        }
        // The batch record is the durable boundary before the network call.
        // If a crash lands while these row writes are partial, startup can
        // recover every member from the batch as uncertain. An unbatched
        // AcquireIntent therefore proves the request was never sent.
        for request_id in &request_ids {
            let attempt_token = taken
                .iter()
                .find(|(id, _, _)| id == request_id)
                .map(|(_, _, token)| token.as_str())
                .ok_or_else(|| {
                    ScaleError::Store(anyhow::anyhow!(
                        "reserved request {request_id} has no captured permit token"
                    ))
                })?;
            self.demand
                .set_state_owned(
                    *request_id,
                    DemandState::AcquireIntent,
                    None,
                    generation,
                    attempt_token,
                )
                .map_err(ScaleError::Store)?;
        }
        self.metrics.inc_acquire_batches();

        // Step 4: the call, then set-reconcile. Transport failure after send
        // is `uncertain` — durable, counted, and ACKable — never a guess.
        let returned = match self.queue.acquire_jobs(&request_ids).await {
            Ok(ids) => ids,
            Err(error) => {
                for request_id in &request_ids {
                    let attempt_token = taken
                        .iter()
                        .find(|(id, _, _)| id == request_id)
                        .map(|(_, _, token)| token.as_str())
                        .ok_or_else(|| {
                            ScaleError::Store(anyhow::anyhow!(
                                "uncertain request {request_id} has no captured permit token"
                            ))
                        })?;
                    self.demand
                        .set_state_owned(
                            *request_id,
                            DemandState::Uncertain,
                            None,
                            generation,
                            attempt_token,
                        )
                        .map_err(ScaleError::Store)?;
                }
                self.batches
                    .resolve(&batch_id, true)
                    .map_err(ScaleError::Store)?;
                self.metrics.inc_uncertain_batches();
                tracing::warn!(
                    batch = batch_id.as_str(),
                    error = error.to_string(),
                    "acquirejobs failed after intent; batch uncertain"
                );
                return Ok((Vec::new(), Vec::new(), request_ids));
            }
        };
        let (acquired, missing) = reconcile_returned_ids(&request_ids, &returned);
        for request_id in &acquired {
            let attempt_token = taken
                .iter()
                .find(|(id, _, _)| id == request_id)
                .map(|(_, _, token)| token.as_str())
                .ok_or_else(|| {
                    ScaleError::Store(anyhow::anyhow!(
                        "acquired request {request_id} has no captured permit token"
                    ))
                })?;
            self.demand
                .set_state_owned(
                    *request_id,
                    DemandState::Acquired,
                    None,
                    generation,
                    attempt_token,
                )
                .map_err(ScaleError::Store)?;
            fenced_transition(
                &mut self.ledger,
                &permit_holder(self.config.scale_set_id, *request_id),
                LedgerPermitState::Acquiring,
                generation,
                taken
                    .iter()
                    .find(|(id, _, _)| id == request_id)
                    .map(|(_, _, token)| token.as_str())
                    .ok_or_else(|| {
                        ScaleError::Store(anyhow::anyhow!(
                            "acquired request {request_id} has no captured permit token"
                        ))
                    })?,
            )
            .map_err(ScaleError::Ledger)?;
        }
        // Journal every missing member before the first ledger delete. The
        // batch stays open until the final release transaction also resolves
        // it, so a crash cannot erase the only per-member outcome evidence.
        let mut missing_releases = Vec::with_capacity(missing.len());
        for request_id in &missing {
            let attempt_token = taken
                .iter()
                .find(|(id, _, _)| id == request_id)
                .map(|(_, _, token)| token.as_str())
                .ok_or_else(|| {
                    ScaleError::Store(anyhow::anyhow!(
                        "missing request {request_id} has no captured permit token"
                    ))
                })?;
            missing_releases.push((*request_id, attempt_token.to_owned()));
        }
        self.demand
            .stage_attempt_releases_for_batch(
                self.config.scale_set_id,
                &batch_id,
                &missing_releases,
            )
            .map_err(ScaleError::Store)?;
        for request_id in &missing {
            let attempt_token = taken
                .iter()
                .find(|(id, _, _)| id == request_id)
                .map(|(_, _, token)| token.as_str())
                .ok_or_else(|| {
                    ScaleError::Store(anyhow::anyhow!(
                        "missing request {request_id} has no captured permit token"
                    ))
                })?;
            self.release_with_projection_batch(
                *request_id,
                attempt_token,
                OwnedReleaseKind::Eligible,
                DemandState::Eligible,
                Some(&batch_id),
            )?;
        }
        self.batches
            .resolve(&batch_id, false)
            .map_err(ScaleError::Store)?;
        self.metrics.add_acquired_ids(acquired.len() as u64);
        self.metrics.add_missing_ids(missing.len() as u64);
        Ok((acquired, missing, Vec::new()))
    }

    /// Step 5: provision every `acquired` row — fresh grants and
    /// redelivered retries alike. There is deliberately no
    /// intent-exists skip: [`ensure_provision_intent`] is idempotent on
    /// the stable operation id, and the lane call must re-run after a
    /// failed provision or a crash between intent and provision (the
    /// demand row stays `acquired` in both windows, so the skip would
    /// strand the worker forever with its permit held). Success moves
    /// the row to `provision_intent`, which is what stops the retries.
    async fn provision_pass(&mut self) -> Result<Vec<i64>, ScaleError<Q::Error, W::Error>> {
        let acquired = self
            .demand
            .list_in_states(self.config.scale_set_id, &[DemandState::Acquired])
            .map_err(ScaleError::Store)?;
        let mut provisioned = Vec::new();
        for (request_id, _) in acquired.into_iter().take(self.config.max_acquire_batch) {
            let generation = self.generation()?;
            let row = self
                .demand
                .get(request_id)
                .map_err(ScaleError::Store)?
                .ok_or_else(|| {
                    ScaleError::Store(anyhow::anyhow!("acquired demand row {request_id} vanished"))
                })?;
            let attempt_token = required_attempt_token(&row).map_err(ScaleError::Store)?;
            ensure_provision_intent(
                &mut self.provision,
                &mut self.lane,
                self.config.scale_set_id,
                request_id,
                0,
                &self.config.images,
                generation,
                attempt_token,
                &self.metrics,
            )
            .await
            .map_err(ScaleError::Store)?;
            self.demand
                .set_state_owned(
                    request_id,
                    DemandState::ProvisionIntent,
                    None,
                    generation,
                    attempt_token,
                )
                .map_err(ScaleError::Store)?;
            provisioned.push(request_id);
        }
        Ok(provisioned)
    }
}

impl ScaleOutcome {
    fn empty() -> Self {
        Self {
            kind: ScaleKind::Nil,
            assigned: 0,
            started: 0,
            completed: 0,
            offers_seen: 0,
            offers_submitted: 0,
            granted: 0,
            acquired: Vec::new(),
            missing: Vec::new(),
            uncertain: Vec::new(),
            provisioned: Vec::new(),
            decision: None,
        }
    }
}

/// Scale failures. Any variant vetoes the ACK: the message is redelivered
/// and every step replays idempotently.
#[derive(Debug)]
pub enum ScaleError<Q, W> {
    Store(anyhow::Error),
    Ledger(anyhow::Error),
    Queue(Q),
    Lane(W),
    MissingInitialStats,
    OfferWithoutRow { request_id: i64 },
}

impl<Q: std::fmt::Display, W: std::fmt::Display> std::fmt::Display for ScaleError<Q, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(f, "scale store: {error}"),
            Self::Ledger(error) => write!(f, "scale ledger: {error}"),
            Self::Queue(error) => write!(f, "scale queue: {error}"),
            Self::Lane(error) => write!(f, "scale worker lane: {error}"),
            Self::MissingInitialStats => write!(f, "initial message carries no statistics"),
            Self::OfferWithoutRow { request_id } => write!(
                f,
                "offer {request_id} reached no durable row; refusing the ACK"
            ),
        }
    }
}

impl<
        Q: std::error::Error + Send + Sync + 'static,
        W: std::error::Error + Send + Sync + 'static,
    > std::error::Error for ScaleError<Q, W>
{
}

fn ledger_error(error: impl std::error::Error + Send + Sync + 'static) -> anyhow::Error {
    anyhow::Error::new(error)
}

/// Fenced ledger transition: stale generations re-read and retry once.
fn required_attempt_token(row: &Demand) -> Result<&str> {
    row.permit_attempt_token
        .as_deref()
        .filter(|attempt_token| !attempt_token.is_empty())
        .with_context(|| {
            format!(
                "held request {} has no permit attempt token",
                row.request_id
            )
        })
}

#[derive(Debug, Clone, Copy)]
enum OwnedReleaseKind {
    Eligible,
    Canceled,
    Terminal,
}

fn release_attempt<L: CapacityLedger>(
    ledger: &mut L,
    holder: &str,
    attempt_token: &str,
    kind: OwnedReleaseKind,
) -> Result<bool> {
    if attempt_token.is_empty() {
        anyhow::bail!("refusing tokenless permit release for {holder:?}");
    }
    let target = match kind {
        OwnedReleaseKind::Eligible => velnor_control::permit_ledger::DemandState::Eligible,
        OwnedReleaseKind::Canceled => velnor_control::permit_ledger::DemandState::Cancelled,
        OwnedReleaseKind::Terminal => velnor_control::permit_ledger::DemandState::Terminal,
    };
    match ledger
        .release_staged(holder, attempt_token, target)
        .map_err(|error| anyhow::anyhow!("release permit {holder:?}: {error}"))?
    {
        velnor_control::permit_ledger::OwnedReleaseOutcome::Released => Ok(true),
        velnor_control::permit_ledger::OwnedReleaseOutcome::AlreadyAbsent => Ok(false),
        velnor_control::permit_ledger::OwnedReleaseOutcome::StaleAttempt => {
            anyhow::bail!("permit {holder:?} is held by a different attempt token")
        }
    }
}

/// Fenced ledger transition: stale generations re-read and retry once.
fn fenced_transition<L: CapacityLedger>(
    ledger: &mut L,
    holder: &str,
    state: LedgerPermitState,
    generation: u64,
    attempt_token: &str,
) -> Result<u64> {
    match ledger.transition(holder, state, generation, attempt_token) {
        Ok(()) => Ok(generation),
        Err(error) if L::is_stale_generation(&error) => {
            let fresh = ledger
                .generation()
                .map_err(|error| anyhow::anyhow!("re-read ledger generation: {error}"))?;
            ledger
                .transition(holder, state, fresh, attempt_token)
                .map_err(|error| anyhow::anyhow!("transition ledger holder: {error}"))?;
            Ok(fresh)
        }
        Err(error) => Err(anyhow::anyhow!("transition ledger holder: {error}")),
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
    use crate::scaleset::capacity::{
        AcquireOutcome, CapacityLedger, LedgerHolder, LedgerLane, LedgerPermitState,
        MemLedger as CapacityMemLedger, ReconcileReport,
    };
    use crate::scaleset::demand::DemandStore;
    use velnor_model::{
        ScaleSetJobAvailable, ScaleSetJobMessage, ScaleSetJobMessageType, ScaleSetJobStarted,
    };

    #[derive(Debug)]
    struct QueueError(String);

    impl std::fmt::Display for QueueError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "queue: {}", self.0)
        }
    }

    impl std::error::Error for QueueError {}

    #[derive(Debug, Default)]
    struct ScriptedQueue {
        answer: std::sync::Mutex<Option<Result<Vec<i64>, String>>>,
    }

    impl QueueSession for ScriptedQueue {
        type Error = QueueError;

        async fn acquire_jobs(&self, request_ids: &[i64]) -> Result<Vec<i64>, Self::Error> {
            self.answer
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Ok(request_ids.to_vec()))
                .map_err(QueueError)
        }
    }

    /// Test ledger that mirrors the durable retain semantics: a Cleaning
    /// claim stays active with its demand untouched; other exact attempts
    /// become Uncertain and terminalize served demand.
    #[derive(Debug, Default)]
    struct MemLedger {
        inner: CapacityMemLedger,
        demand_states:
            std::collections::HashMap<String, velnor_control::permit_ledger::DemandState>,
    }

    impl MemLedger {
        fn new() -> Self {
            Self::default()
        }

        fn set_max_jobs(&self, max_jobs: u32) {
            self.inner.set_max_jobs(max_jobs);
        }

        fn demand_state(&self, holder: &str) -> Option<velnor_control::permit_ledger::DemandState> {
            self.demand_states.get(holder).copied()
        }
    }

    impl CapacityLedger for MemLedger {
        type Error = <CapacityMemLedger as CapacityLedger>::Error;

        fn generation(&self) -> Result<u64, Self::Error> {
            self.inner.generation()
        }

        fn advertised_free(&self) -> Result<Option<u32>, Self::Error> {
            self.inner.advertised_free()
        }

        fn occupied(&self) -> Result<u32, Self::Error> {
            self.inner.occupied()
        }

        fn holders(&self) -> Result<Vec<LedgerHolder>, Self::Error> {
            self.inner.holders()
        }

        fn holder_state(&self, holder: &str) -> Result<Option<LedgerPermitState>, Self::Error> {
            self.inner.holder_state(holder)
        }

        fn is_current_attempt(
            &self,
            holder: &str,
            attempt_token: &str,
        ) -> Result<bool, Self::Error> {
            self.inner.is_current_attempt(holder, attempt_token)
        }

        fn acquire(
            &mut self,
            holder: &str,
            lane: LedgerLane,
            state: LedgerPermitState,
            generation: u64,
        ) -> Result<AcquireOutcome, Self::Error> {
            let outcome = self.inner.acquire(holder, lane, state, generation)?;
            if lane == LedgerLane::ScaleSet && matches!(&outcome, AcquireOutcome::Acquired { .. }) {
                self.demand_states
                    .entry(holder.to_owned())
                    .or_insert(velnor_control::permit_ledger::DemandState::Granted);
            }
            Ok(outcome)
        }

        fn acquire_with_attempt_token(
            &mut self,
            holder: &str,
            state: LedgerPermitState,
            generation: u64,
            attempt_token: &str,
        ) -> Result<AcquireOutcome, Self::Error> {
            let outcome =
                self.inner
                    .acquire_with_attempt_token(holder, state, generation, attempt_token)?;
            if matches!(&outcome, AcquireOutcome::Acquired { .. }) {
                self.demand_states
                    .entry(holder.to_owned())
                    .or_insert(velnor_control::permit_ledger::DemandState::Granted);
            }
            Ok(outcome)
        }

        fn transition(
            &mut self,
            holder: &str,
            state: LedgerPermitState,
            generation: u64,
            attempt_token: &str,
        ) -> Result<(), Self::Error> {
            self.inner
                .transition(holder, state, generation, attempt_token)
        }

        fn release_staged(
            &mut self,
            holder: &str,
            attempt_token: &str,
            target: velnor_control::permit_ledger::DemandState,
        ) -> Result<velnor_control::permit_ledger::OwnedReleaseOutcome, Self::Error> {
            use velnor_control::permit_ledger::{DemandState, OwnedReleaseOutcome};

            if target == DemandState::Eligible
                && !matches!(
                    self.demand_states.get(holder),
                    Some(DemandState::Eligible | DemandState::Granted)
                )
            {
                return Ok(OwnedReleaseOutcome::StaleAttempt);
            }
            let outcome = self.inner.release_staged(holder, attempt_token, target)?;
            if matches!(
                outcome,
                OwnedReleaseOutcome::Released | OwnedReleaseOutcome::AlreadyAbsent
            ) {
                self.demand_states.insert(holder.to_owned(), target);
            }
            Ok(outcome)
        }

        fn retain_uncertain(
            &mut self,
            holder: &str,
            generation: u64,
            attempt_token: &str,
        ) -> Result<(), Self::Error> {
            use velnor_control::permit_ledger::DemandState;

            let state = self.inner.holder_state(holder)?;
            self.inner
                .retain_uncertain(holder, generation, attempt_token)?;
            if state != Some(LedgerPermitState::Cleaning)
                && matches!(
                    self.demand_states.get(holder),
                    Some(DemandState::Eligible | DemandState::Granted)
                )
            {
                self.demand_states
                    .insert(holder.to_owned(), DemandState::Terminal);
            }
            Ok(())
        }

        fn reconcile_attempts(
            &mut self,
            alive: &[(&str, LedgerLane, &str)],
        ) -> Result<ReconcileReport, Self::Error> {
            self.inner.reconcile_attempts(alive)
        }

        fn is_stale_generation(error: &Self::Error) -> bool {
            <CapacityMemLedger as CapacityLedger>::is_stale_generation(error)
        }
    }

    #[derive(Debug)]
    struct StubLane {
        provisioned: Vec<String>,
        terminals: Vec<i64>,
        assigned: Vec<i64>,
        started: Vec<i64>,
        canceled: Vec<i64>,
        terminal_permit_states: Vec<Option<LedgerPermitState>>,
        terminal_demand_states: Vec<Option<velnor_control::permit_ledger::DemandState>>,
        terminal_attempt_tokens: Vec<String>,
        ledger: SharedMemLedger,
        scale_set_id: i32,
        fail_terminal_cleanup: bool,
    }

    impl StubLane {
        fn new(ledger: SharedMemLedger, scale_set_id: i32) -> Self {
            Self {
                provisioned: Vec::new(),
                terminals: Vec::new(),
                assigned: Vec::new(),
                started: Vec::new(),
                canceled: Vec::new(),
                terminal_permit_states: Vec::new(),
                terminal_demand_states: Vec::new(),
                terminal_attempt_tokens: Vec::new(),
                ledger,
                scale_set_id,
                fail_terminal_cleanup: false,
            }
        }
    }

    /// The processor and its lane must see the same in-memory permit state,
    /// matching production where both adapters address the shared ledger.
    #[derive(Debug, Clone)]
    struct SharedMemLedger(std::sync::Arc<std::sync::Mutex<MemLedger>>);

    impl SharedMemLedger {
        fn lock(&self) -> std::sync::MutexGuard<'_, MemLedger> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn demand_state(&self, holder: &str) -> Option<velnor_control::permit_ledger::DemandState> {
            self.lock().demand_state(holder)
        }
    }

    impl CapacityLedger for SharedMemLedger {
        type Error = <MemLedger as CapacityLedger>::Error;

        fn generation(&self) -> Result<u64, Self::Error> {
            self.lock().generation()
        }

        fn advertised_free(&self) -> Result<Option<u32>, Self::Error> {
            self.lock().advertised_free()
        }

        fn occupied(&self) -> Result<u32, Self::Error> {
            self.lock().occupied()
        }

        fn holders(&self) -> Result<Vec<LedgerHolder>, Self::Error> {
            self.lock().holders()
        }

        fn holder_state(&self, holder: &str) -> Result<Option<LedgerPermitState>, Self::Error> {
            self.lock().holder_state(holder)
        }

        fn is_current_attempt(
            &self,
            holder: &str,
            attempt_token: &str,
        ) -> Result<bool, Self::Error> {
            self.lock().is_current_attempt(holder, attempt_token)
        }

        fn acquire(
            &mut self,
            holder: &str,
            lane: LedgerLane,
            state: LedgerPermitState,
            generation: u64,
        ) -> Result<AcquireOutcome, Self::Error> {
            self.lock().acquire(holder, lane, state, generation)
        }

        fn acquire_with_attempt_token(
            &mut self,
            holder: &str,
            state: LedgerPermitState,
            generation: u64,
            attempt_token: &str,
        ) -> Result<AcquireOutcome, Self::Error> {
            self.lock()
                .acquire_with_attempt_token(holder, state, generation, attempt_token)
        }

        fn transition(
            &mut self,
            holder: &str,
            state: LedgerPermitState,
            generation: u64,
            attempt_token: &str,
        ) -> Result<(), Self::Error> {
            self.lock()
                .transition(holder, state, generation, attempt_token)
        }

        fn release_staged(
            &mut self,
            holder: &str,
            attempt_token: &str,
            target: velnor_control::permit_ledger::DemandState,
        ) -> Result<velnor_control::permit_ledger::OwnedReleaseOutcome, Self::Error> {
            self.lock().release_staged(holder, attempt_token, target)
        }

        fn retain_uncertain(
            &mut self,
            holder: &str,
            generation: u64,
            attempt_token: &str,
        ) -> Result<(), Self::Error> {
            self.lock()
                .retain_uncertain(holder, generation, attempt_token)
        }

        fn reconcile_attempts(
            &mut self,
            alive: &[(&str, LedgerLane, &str)],
        ) -> Result<ReconcileReport, Self::Error> {
            self.lock().reconcile_attempts(alive)
        }

        fn is_stale_generation(error: &Self::Error) -> bool {
            <MemLedger as CapacityLedger>::is_stale_generation(error)
        }
    }

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
            intent: &crate::scaleset::intents::ProvisionIntent,
        ) -> Result<(), Self::Error> {
            self.provisioned.push(intent.operation_id.clone());
            Ok(())
        }

        fn note_assigned(
            &mut self,
            assigned: &ScaleSetJobAssigned,
            _attempt_token: &str,
        ) -> Result<(), Self::Error> {
            self.assigned.push(assigned.base.runner_request_id);
            Ok(())
        }

        fn note_started(
            &mut self,
            started: &ScaleSetJobStarted,
            _attempt_token: &str,
        ) -> Result<(), Self::Error> {
            self.started.push(started.base.runner_request_id);
            Ok(())
        }

        fn note_terminal(
            &mut self,
            completed: &ScaleSetJobCompleted,
            attempt_token: &str,
        ) -> Result<(), Self::Error> {
            self.terminals.push(completed.base.runner_request_id);
            self.terminal_attempt_tokens.push(attempt_token.to_owned());
            let request_id = crate::scaleset::intents::request_id_for_runner(
                self.scale_set_id,
                &completed.runner_name,
                completed.base.runner_request_id,
            )
            .ok_or_else(|| LaneError("terminal runner belongs to another scale set".to_owned()))?;
            let holder = permit_holder(self.scale_set_id, request_id);
            self.terminal_permit_states.push(
                self.ledger
                    .holder_state(&holder)
                    .map_err(|error| LaneError(format!("read terminal permit state: {error}")))?,
            );
            self.terminal_demand_states
                .push(self.ledger.demand_state(&holder));
            if self.fail_terminal_cleanup {
                let generation = self
                    .ledger
                    .generation()
                    .map_err(|error| LaneError(format!("read ledger generation: {error}")))?;
                self.ledger
                    .retain_uncertain(&holder, generation, attempt_token)
                    .map_err(|error| {
                        LaneError(format!("retain permit after cleanup failure: {error}"))
                    })?;
                return Err(LaneError("terminal cleanup failed".to_owned()));
            }
            match self
                .ledger
                .release_staged(
                    &holder,
                    attempt_token,
                    velnor_control::permit_ledger::DemandState::Terminal,
                )
                .map_err(|error| LaneError(format!("release terminal permit: {error}")))?
            {
                velnor_control::permit_ledger::OwnedReleaseOutcome::Released
                | velnor_control::permit_ledger::OwnedReleaseOutcome::AlreadyAbsent => Ok(()),
                velnor_control::permit_ledger::OwnedReleaseOutcome::StaleAttempt => Err(LaneError(
                    format!("terminal permit attempt for {holder:?} is stale"),
                )),
            }
        }

        fn note_canceled(
            &mut self,
            request_id: i64,
            _attempt_token: &str,
        ) -> Result<(), Self::Error> {
            self.canceled.push(request_id);
            Ok(())
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-scaleset-scale-{name}-{}-{}",
            std::process::id(),
            velnor_model::Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.db")
    }

    fn images() -> ProvisionImages {
        ProvisionImages {
            runner_digest: "sha256:runner".to_owned(),
            dind_digest: "sha256:dind".to_owned(),
        }
    }

    fn push_offer(id: i64) -> ScaleSetJobAvailable {
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

    fn base(id: i64, message_type: ScaleSetJobMessageType) -> ScaleSetJobMessage {
        ScaleSetJobMessage {
            message_type,
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
        }
    }

    fn completed(id: i64, result: &str) -> ScaleSetJobCompleted {
        ScaleSetJobCompleted {
            result: result.to_owned(),
            runner_id: 1,
            runner_name: format!("velnor-7-{id}"),
            base: base(id, ScaleSetJobMessageType::JobCompleted),
        }
    }

    fn completion_message(message_id: i32, id: i64) -> RunnerScaleSetMessage {
        RunnerScaleSetMessage {
            message_id,
            statistics: Some(stats(4)),
            job_completed_messages: vec![completed(id, "canceled")],
            ..RunnerScaleSetMessage::default()
        }
    }

    fn age_batch(path: &std::path::Path, batch_id: &str) {
        let old = velnor_model::Timestamp::now()
            .minus(std::time::Duration::from_secs(3600))
            .to_rfc3339()
            .unwrap();
        rusqlite::Connection::open(path)
            .unwrap()
            .execute(
                "UPDATE scaleset_acquire_batches SET created_at = ?1 WHERE batch_id = ?2",
                rusqlite::params![old, batch_id],
            )
            .unwrap();
    }

    fn stats(assigned: i32) -> RunnerScaleSetStatistic {
        RunnerScaleSetStatistic {
            total_assigned_jobs: assigned,
            ..RunnerScaleSetStatistic::default()
        }
    }

    fn message(id: i32, offers: Vec<ScaleSetJobAvailable>) -> RunnerScaleSetMessage {
        RunnerScaleSetMessage {
            message_id: id,
            statistics: Some(stats(4)),
            job_available_messages: offers,
            ..RunnerScaleSetMessage::default()
        }
    }

    fn processor(
        path: &std::path::Path,
        queue: ScriptedQueue,
    ) -> Processor<ScriptedQueue, SharedMemLedger, StubLane> {
        let ledger = std::sync::Arc::new(std::sync::Mutex::new(MemLedger::new()));
        {
            let mut inner = ledger.lock().unwrap();
            inner.set_max_jobs(4);
            inner.reconcile_attempts(&[]).unwrap();
        }
        let shared_ledger = SharedMemLedger(std::sync::Arc::clone(&ledger));
        Processor::new(
            queue,
            shared_ledger.clone(),
            StubLane::new(shared_ledger, 7),
            DemandStore::open(path).unwrap(),
            AcquireBatchStore::open(path).unwrap(),
            ProvisionIntentStore::open(path).unwrap(),
            Metrics::new(),
            ProcessorConfig {
                scale_set_id: 7,
                images: images(),
                max_acquire_batch: MAX_ACQUIRE_BATCH,
            },
        )
    }

    #[test]
    fn non_cleaning_retain_marks_exact_attempt_uncertain_and_terminal() {
        let mut ledger = MemLedger::new();
        ledger.set_max_jobs(1);
        ledger.reconcile_attempts(&[]).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "scaleset/7/non-cleaning-retain";
        let AcquireOutcome::Acquired { attempt_token } = ledger
            .acquire(
                holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Running,
                generation,
            )
            .unwrap()
        else {
            panic!("permit acquisition failed");
        };

        ledger
            .retain_uncertain(holder, generation, &attempt_token)
            .unwrap();

        assert_eq!(
            ledger.holder_state(holder).unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
        assert_eq!(
            ledger.demand_state(holder),
            Some(velnor_control::permit_ledger::DemandState::Terminal)
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
    }

    #[tokio::test]
    async fn full_pass_grants_acquires_and_provisions() {
        let path = temp_path("full");
        let mut processor = processor(&path, ScriptedQueue::default());
        let outcome = processor
            .scale(Some(&message(10, vec![push_offer(501), push_offer(502)])))
            .await
            .unwrap();
        assert_eq!(outcome.offers_seen, 2);
        assert_eq!(outcome.offers_submitted, 2);
        assert_eq!(outcome.granted, 2);
        assert_eq!(outcome.acquired, vec![501, 502]);
        assert!(outcome.missing.is_empty());
        assert!(outcome.uncertain.is_empty());
        assert_eq!(outcome.provisioned, vec![501, 502]);
        assert_eq!(
            outcome.decision,
            Some(PopulationDecision::AcquireMore {
                desired: 4,
                local: 2,
                headroom: 2,
            })
        );
    }

    #[tokio::test]
    async fn same_batch_assignment_runs_after_offer_acquire_proof() {
        let path = temp_path("assigned-with-offer-same-batch");
        let mut processor = processor(&path, ScriptedQueue::default());
        let mut observed = message(11, vec![push_offer(505)]);
        observed.job_assigned_messages = vec![ScaleSetJobAssigned {
            base: base(505, ScaleSetJobMessageType::JobAssigned),
        }];

        let outcome = processor.scale(Some(&observed)).await.unwrap();
        assert_eq!(outcome.acquired, vec![505]);
        assert_eq!(outcome.assigned, 1);
        assert_eq!(outcome.provisioned, vec![505]);
        let demand = processor.demand_mut().get(505).unwrap().unwrap();
        let attempt_token = demand.permit_attempt_token.unwrap();
        assert!(!attempt_token.is_empty());
        assert!(processor
            .ledger_mut()
            .is_current_attempt("scaleset/7/505", &attempt_token)
            .unwrap());
    }

    #[tokio::test]
    async fn multiple_null_request_id_offers_keep_distinct_job_identity() {
        let path = temp_path("null-request-identity");
        let mut processor = processor(&path, ScriptedQueue::default());
        let batch = serde_json::to_string(&vec![
            serde_json::json!({
                "messageType": "JobAvailable",
                "runnerRequestId": null,
                "jobId": "job-null-a",
                "workflowRunId": 0,
                "repositoryName": "velnor",
                "ownerName": "tailrocks",
                "eventName": "push",
                "requestLabels": ["velnor"],
                "acquireJobUrl": "https://scaleset-fixture.invalid/acquire"
            }),
            serde_json::json!({
                "messageType": "JobAvailable",
                "runnerRequestId": null,
                "jobId": "job-null-b",
                "workflowRunId": 0,
                "repositoryName": "velnor",
                "ownerName": "tailrocks",
                "eventName": "push",
                "requestLabels": ["velnor"],
                "acquireJobUrl": "https://scaleset-fixture.invalid/acquire"
            }),
        ])
        .unwrap();
        let envelope = serde_json::json!({
            "messageId": 10,
            "messageType": crate::scaleset::session::JOB_MESSAGES_ENVELOPE,
            "body": batch,
            "statistics": {"totalAssignedJobs": 4}
        });
        let parsed = crate::scaleset::session::parse_message_response(
            &serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
        assert_eq!(parsed.job_available_messages.len(), 2);
        assert!(parsed
            .job_available_messages
            .iter()
            .all(|offer| offer.base.runner_request_id == 0));
        let first_id =
            crate::scaleset::demand::resolve_job_request_id(&parsed.job_available_messages[0].base);
        let second_id =
            crate::scaleset::demand::resolve_job_request_id(&parsed.job_available_messages[1].base);
        assert_ne!(first_id, second_id);

        let outcome = processor.scale(Some(&parsed)).await.unwrap();

        assert_eq!(outcome.offers_submitted, 2);
        assert_eq!(outcome.acquired.len(), 2);
        assert!(outcome.acquired.contains(&first_id));
        assert!(outcome.acquired.contains(&second_id));
        assert!(processor.demand_mut().get(first_id).unwrap().is_some());
        assert!(processor.demand_mut().get(second_id).unwrap().is_some());
        assert!(processor.demand_mut().get(0).unwrap().is_none());
    }

    #[tokio::test]
    async fn partial_acquire_requeues_missing_with_age_kept() {
        let path = temp_path("partial");
        let queue = ScriptedQueue {
            answer: std::sync::Mutex::new(Some(Ok(vec![601]))),
        };
        let mut processor = processor(&path, queue);
        let outcome = processor
            .scale(Some(&message(11, vec![push_offer(601), push_offer(602)])))
            .await
            .unwrap();
        assert_eq!(outcome.acquired, vec![601]);
        assert_eq!(outcome.missing, vec![602]);
        let before = processor.demand_mut().get(602).unwrap().unwrap();
        assert_eq!(before.state, DemandState::Eligible);
        // Redelivered offer keeps its age; the acquired one provisions.
        assert_eq!(outcome.provisioned, vec![601]);
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 1);
    }

    #[tokio::test]
    async fn transport_failure_marks_uncertain_and_stays_ackable() {
        let path = temp_path("uncertain");
        let queue = ScriptedQueue {
            answer: std::sync::Mutex::new(Some(Err("connection reset".to_owned()))),
        };
        let mut processor = processor(&path, queue);
        // `Ok` licenses the ACK: uncertain is terminal-for-this-message.
        let outcome = processor
            .scale(Some(&message(12, vec![push_offer(701)])))
            .await
            .unwrap();
        assert_eq!(outcome.uncertain, vec![701]);
        assert!(outcome.provisioned.is_empty());
        assert_eq!(
            processor.demand_mut().get(701).unwrap().unwrap().state,
            DemandState::Uncertain
        );
        // Still counted: no double-spend on the next poll.
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 1);
    }

    #[tokio::test]
    async fn observations_fold_idempotently_and_ignore_untracked() {
        let path = temp_path("observations");
        let mut processor = processor(&path, ScriptedQueue::default());
        let outcome = processor
            .scale(Some(&message(13, vec![push_offer(801)])))
            .await
            .unwrap();
        assert_eq!(outcome.provisioned, vec![801]);

        let completed = ScaleSetJobCompleted {
            result: "succeeded".to_owned(),
            runner_id: 1,
            runner_name: "velnor-7-801".to_owned(),
            base: ScaleSetJobMessage {
                message_type: ScaleSetJobMessageType::JobCompleted,
                runner_request_id: 801,
                repository_name: "velnor".to_owned(),
                owner_name: "tailrocks".to_owned(),
                job_id: "job-801".to_owned(),
                job_workflow_ref: String::new(),
                job_display_name: String::new(),
                workflow_run_id: 0,
                event_name: "push".to_owned(),
                request_labels: Vec::new(),
                queue_time: String::new(),
                scale_set_assign_time: String::new(),
                runner_assign_time: String::new(),
                finish_time: String::new(),
            },
        };
        // Plus an untracked completion (another listener's job): ignored.
        let mut foreign = completed.clone();
        foreign.base.runner_request_id = 999_999;
        foreign.runner_name = "velnor-7-999999".to_owned();
        let observed = RunnerScaleSetMessage {
            message_id: 14,
            statistics: Some(stats(4)),
            job_completed_messages: vec![completed, foreign],
            ..RunnerScaleSetMessage::default()
        };
        let outcome = processor.scale(Some(&observed)).await.unwrap();
        assert_eq!(outcome.completed, 1);
        assert_eq!(
            processor.demand_mut().get(801).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert!(processor.demand_mut().get(999_999).unwrap().is_none());
        // Replay is a no-op once the lane released the permit.
        let attempt_token = processor
            .demand_mut()
            .get(801)
            .unwrap()
            .unwrap()
            .permit_attempt_token
            .unwrap();
        assert_eq!(
            processor
                .ledger_mut()
                .release_staged(
                    &permit_holder(7, 801),
                    &attempt_token,
                    velnor_control::permit_ledger::DemandState::Terminal,
                )
                .unwrap(),
            velnor_control::permit_ledger::OwnedReleaseOutcome::AlreadyAbsent
        );
        let replay = processor.scale(Some(&observed)).await.unwrap();
        assert_eq!(replay.completed, 1);
    }

    #[tokio::test]
    async fn assigned_then_canceled_before_acquire_closes_old_attempt() {
        let path = temp_path("assigned-canceled-before-acquire");
        let mut processor = processor(&path, ScriptedQueue::default());
        let generation = processor.ledger_mut().generation().unwrap();
        processor
            .demand_mut()
            .submit_offer(7, &push_offer(901), generation)
            .unwrap();
        processor
            .demand_mut()
            .set_state(901, DemandState::Granted, None, generation)
            .unwrap();

        assert!(processor
            .observe_assigned(&ScaleSetJobAssigned {
                base: base(901, ScaleSetJobMessageType::JobAssigned),
            })
            .unwrap());
        assert_eq!(
            processor.demand_mut().get(901).unwrap().unwrap().state,
            DemandState::Granted,
            "JobAssigned alone must not assert acquire success"
        );
        assert!(processor
            .observe_completed(&completed(901, "canceled"))
            .unwrap());
        assert_eq!(
            processor.demand_mut().get(901).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);
        assert!(processor.lane_mut().assigned.is_empty());
        assert!(processor.lane_mut().terminals.is_empty());
        assert!(processor.lane_mut().canceled.is_empty());
    }

    #[tokio::test]
    async fn direct_assignment_without_acquire_proof_fails_closed() {
        let path = temp_path("direct-assignment-unconfirmed");
        let mut processor = processor(&path, ScriptedQueue::default());
        let assigned = ScaleSetJobAssigned {
            base: base(902, ScaleSetJobMessageType::JobAssigned),
        };

        assert!(processor.observe_assigned(&assigned).is_err());
        let first = processor.demand_mut().get(902).unwrap().unwrap();
        assert_eq!(first.state, DemandState::Acquired);
        assert!(first.permit_attempt_token.is_none());
        assert!(processor
            .demand_mut()
            .pending_attempt_acquires()
            .unwrap()
            .is_empty());
        assert_eq!(
            processor
                .ledger_mut()
                .holder_state("scaleset/7/902")
                .unwrap(),
            None
        );
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);

        // Redelivery remains unconfirmed; JobAssigned does not mint a
        // permit token or transfer a holder-only ledger row.
        assert!(processor.observe_assigned(&assigned).is_err());
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);
    }

    #[tokio::test]
    async fn completed_result_casings_both_take_cancel_path() {
        // The wire carries both `actions/runner` PascalCase and Velnor
        // lowercase `result` spellings; both must take the cancel path
        // (close without driving the lane terminal).
        for (index, result) in ["canceled", "Canceled"].iter().enumerate() {
            let id = 910 + index as i64;
            let path = temp_path(&format!("completed-result-casing-{result}"));
            let mut processor = processor(&path, ScriptedQueue::default());
            let generation = processor.ledger_mut().generation().unwrap();
            processor
                .demand_mut()
                .submit_offer(7, &push_offer(id), generation)
                .unwrap();
            processor
                .demand_mut()
                .set_state(id, DemandState::Granted, None, generation)
                .unwrap();
            assert!(processor.observe_completed(&completed(id, result)).unwrap());
            assert_eq!(
                processor.demand_mut().get(id).unwrap().unwrap().state,
                DemandState::Terminal,
                "result {result} must close the attempt"
            );
            assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);
            assert!(
                processor.lane_mut().terminals.is_empty(),
                "result {result} must take the cancel path, not the lane terminal"
            );
        }
    }

    #[tokio::test]
    async fn acquired_cancellation_cleans_lane_before_releasing_permit() {
        let path = temp_path("acquired-canceled");
        let queue = ScriptedQueue {
            answer: std::sync::Mutex::new(Some(Ok(vec![902]))),
        };
        let mut processor = processor(&path, queue);
        let initial = processor
            .scale(Some(&message(21, vec![push_offer(902)])))
            .await
            .unwrap();
        assert_eq!(initial.acquired, vec![902]);
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 1);

        let outcome = processor
            .scale(Some(&completion_message(22, 902)))
            .await
            .unwrap();
        assert_eq!(outcome.completed, 1);
        assert_eq!(
            processor.demand_mut().get(902).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert_eq!(processor.lane_mut().terminals, vec![902]);
        assert!(processor.lane_mut().canceled.is_empty());
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);
    }

    #[tokio::test]
    async fn terminal_cleanup_failure_retains_occupancy_until_lane_releases_attempt() {
        let path = temp_path("terminal-cleanup-release");
        let mut processor = processor(&path, ScriptedQueue::default());
        let acquired = processor
            .scale(Some(&message(27, vec![push_offer(905)])))
            .await
            .unwrap();
        assert_eq!(acquired.acquired, vec![905]);
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 1);
        let holder = "scaleset/7/905";
        let attempt_token = processor
            .demand_mut()
            .get(905)
            .unwrap()
            .unwrap()
            .permit_attempt_token
            .unwrap();
        assert_eq!(
            processor.ledger_ref().demand_state(holder),
            Some(velnor_control::permit_ledger::DemandState::Granted)
        );

        let terminal = RunnerScaleSetMessage {
            message_id: 28,
            statistics: Some(stats(4)),
            job_completed_messages: vec![completed(905, "succeeded")],
            ..RunnerScaleSetMessage::default()
        };
        processor.lane_mut().fail_terminal_cleanup = true;
        assert!(processor.scale(Some(&terminal)).await.is_err());
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 1);
        assert_eq!(
            processor.ledger_mut().holder_state(holder).unwrap(),
            Some(LedgerPermitState::Cleaning)
        );
        assert_eq!(
            processor.ledger_ref().demand_state(holder),
            Some(velnor_control::permit_ledger::DemandState::Granted)
        );
        assert!(processor
            .ledger_mut()
            .is_current_attempt(holder, &attempt_token)
            .unwrap());
        assert_eq!(
            processor.demand_mut().get(905).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert_eq!(
            processor.lane_mut().terminal_attempt_tokens,
            vec![attempt_token.clone()]
        );

        processor.lane_mut().fail_terminal_cleanup = false;
        let retried = processor.scale(Some(&terminal)).await.unwrap();
        assert_eq!(retried.completed, 1);
        assert_eq!(
            processor.lane_mut().terminal_permit_states,
            vec![
                Some(LedgerPermitState::Cleaning),
                Some(LedgerPermitState::Cleaning)
            ]
        );
        assert_eq!(
            processor.lane_mut().terminal_attempt_tokens,
            vec![attempt_token.clone(), attempt_token]
        );
        assert_eq!(
            processor.lane_mut().terminal_demand_states,
            vec![
                Some(velnor_control::permit_ledger::DemandState::Granted),
                Some(velnor_control::permit_ledger::DemandState::Granted)
            ]
        );
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);
        assert_eq!(processor.ledger_mut().holder_state(holder).unwrap(), None);
        assert_eq!(
            processor.ledger_ref().demand_state(holder),
            Some(velnor_control::permit_ledger::DemandState::Terminal)
        );
        assert_eq!(
            processor.demand_mut().get(905).unwrap().unwrap().state,
            DemandState::Terminal
        );
    }

    #[tokio::test]
    async fn unresolved_cancellation_holds_until_reacquire_proves_absence() {
        let path = temp_path("canceled-uncertain-missing");
        let queue = ScriptedQueue {
            answer: std::sync::Mutex::new(Some(Err("connection reset".to_owned()))),
        };
        let mut processor = processor(&path, queue);
        processor
            .scale(Some(&message(23, vec![push_offer(903)])))
            .await
            .unwrap();
        processor
            .scale(Some(&completion_message(24, 903)))
            .await
            .unwrap();
        assert_eq!(
            processor.demand_mut().get(903).unwrap().unwrap().state,
            DemandState::CanceledPending
        );
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 1);
        assert!(processor.lane_mut().terminals.is_empty());

        let batch = processor
            .batches_mut()
            .open_batches(7, usize::MAX)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        age_batch(&path, &batch.batch_id);
        *processor.queue.answer.lock().unwrap() = Some(Ok(Vec::new()));
        let generation = processor.ledger_mut().generation().unwrap();
        let report = {
            let Processor {
                queue,
                ledger,
                demand,
                batches,
                lane,
                metrics,
                ..
            } = &mut processor;
            super::super::reconcile::idle_poll(
                queue, ledger, demand, batches, lane, 7, generation, metrics,
            )
            .await
            .unwrap()
        };
        assert_eq!(report.reacquired_batches, 1);
        assert_eq!(
            processor.demand_mut().get(903).unwrap().unwrap().state,
            DemandState::CanceledDone
        );
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);

        let drained = processor.scale(None).await.unwrap();
        assert_eq!(drained.completed, 1);
        assert_eq!(
            processor.demand_mut().get(903).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert!(processor.lane_mut().terminals.is_empty());
        assert!(processor.lane_mut().canceled.is_empty());
    }

    #[tokio::test]
    async fn started_after_pending_cancel_binds_lane_and_finishes_cleanup() {
        let path = temp_path("canceled-pending-started");
        let queue = ScriptedQueue {
            answer: std::sync::Mutex::new(Some(Err("connection reset".to_owned()))),
        };
        let mut processor = processor(&path, queue);
        processor
            .scale(Some(&message(25, vec![push_offer(904)])))
            .await
            .unwrap();
        processor
            .scale(Some(&completion_message(26, 904)))
            .await
            .unwrap();
        assert_eq!(
            processor.demand_mut().get(904).unwrap().unwrap().state,
            DemandState::CanceledPending
        );

        let started = ScaleSetJobStarted {
            runner_id: 1,
            runner_name: "velnor-7-904".to_owned(),
            base: base(904, ScaleSetJobMessageType::JobStarted),
        };
        assert!(processor.observe_started(&started).unwrap());
        assert_eq!(
            processor.demand_mut().get(904).unwrap().unwrap().state,
            DemandState::CanceledAcquired
        );
        assert_eq!(processor.lane_mut().started, vec![904]);
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 1);

        processor.scale(None).await.unwrap();
        assert_eq!(
            processor.demand_mut().get(904).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert_eq!(processor.lane_mut().canceled, vec![904]);
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);
    }

    #[tokio::test]
    async fn foreign_observations_are_counted_never_claimed() {
        let path = temp_path("foreign");
        let mut processor = processor(&path, ScriptedQueue::default());
        // A trust-unknown offer: observed, stake-less, no permit.
        let queued = processor
            .scale(Some(&message(
                15,
                vec![{
                    let mut pr = push_offer(802);
                    pr.base.event_name = "pull_request".to_owned();
                    pr
                }],
            )))
            .await
            .unwrap();
        assert_eq!(queued.granted, 0);
        assert_eq!(
            processor.demand_mut().get(802).unwrap().unwrap().state,
            DemandState::Observed
        );

        let base = |message_type| ScaleSetJobMessage {
            message_type,
            runner_request_id: 802,
            repository_name: "velnor".to_owned(),
            owner_name: "tailrocks".to_owned(),
            job_id: "job-802".to_owned(),
            job_workflow_ref: String::new(),
            job_display_name: String::new(),
            workflow_run_id: 0,
            event_name: "pull_request".to_owned(),
            request_labels: Vec::new(),
            queue_time: String::new(),
            scale_set_assign_time: String::new(),
            runner_assign_time: String::new(),
            finish_time: String::new(),
        };
        let foreign = RunnerScaleSetMessage {
            message_id: 16,
            statistics: Some(stats(4)),
            job_assigned_messages: vec![ScaleSetJobAssigned {
                base: base(ScaleSetJobMessageType::JobAssigned),
            }],
            job_started_messages: vec![ScaleSetJobStarted {
                runner_id: 1,
                runner_name: "velnor-7-802".to_owned(),
                base: base(ScaleSetJobMessageType::JobStarted),
            }],
            ..RunnerScaleSetMessage::default()
        };
        let outcome = processor.scale(Some(&foreign)).await.unwrap();
        assert_eq!(outcome.assigned, 1);
        assert_eq!(outcome.started, 1);
        // Counted but never claimed: still observed, no permit, no lane.
        assert_eq!(
            processor.demand_mut().get(802).unwrap().unwrap().state,
            DemandState::Observed
        );
        assert_eq!(processor.ledger_mut().occupied().unwrap(), 0);

        let done = RunnerScaleSetMessage {
            message_id: 17,
            statistics: Some(stats(4)),
            job_completed_messages: vec![ScaleSetJobCompleted {
                result: "succeeded".to_owned(),
                runner_id: 1,
                runner_name: "velnor-7-802".to_owned(),
                base: base(ScaleSetJobMessageType::JobCompleted),
            }],
            ..RunnerScaleSetMessage::default()
        };
        let outcome = processor.scale(Some(&done)).await.unwrap();
        assert_eq!(outcome.completed, 1);
        // Foreign completion ends offering without touching the lane.
        assert_eq!(
            processor.demand_mut().get(802).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert!(processor.lane_mut().terminals.is_empty());
        assert!(processor.lane_mut().provisioned.is_empty());
    }

    #[tokio::test]
    async fn runner_name_routes_by_set_and_request_and_rejects_foreign_events() {
        let path = temp_path("runner-name-routing");
        let mut processor = processor(&path, ScriptedQueue::default());
        processor
            .scale(Some(&message(18, vec![push_offer(801)])))
            .await
            .unwrap();

        let foreign_started = ScaleSetJobStarted {
            runner_id: 1,
            runner_name: "velnor-8-801".to_owned(),
            base: base(801, ScaleSetJobMessageType::JobStarted),
        };
        assert!(!processor.observe_started(&foreign_started).unwrap());
        assert!(processor.lane_mut().started.is_empty());
        assert_eq!(
            processor.demand_mut().get(801).unwrap().unwrap().state,
            DemandState::ProvisionIntent
        );

        let foreign_completed = ScaleSetJobCompleted {
            result: "succeeded".to_owned(),
            runner_id: 1,
            runner_name: "velnor-8-801".to_owned(),
            base: base(801, ScaleSetJobMessageType::JobCompleted),
        };
        assert!(!processor.observe_completed(&foreign_completed).unwrap());
        assert_eq!(
            processor.demand_mut().get(801).unwrap().unwrap().state,
            DemandState::ProvisionIntent
        );

        // The runner identity is authoritative even when the wire request
        // field is stale: this event must finish request 801, not request 999.
        let routed_started = ScaleSetJobStarted {
            runner_id: 1,
            runner_name: "velnor-7-801".to_owned(),
            base: base(999, ScaleSetJobMessageType::JobStarted),
        };
        assert!(processor.observe_started(&routed_started).unwrap());
        assert_eq!(processor.lane_mut().started, vec![999]);

        let routed_completed = ScaleSetJobCompleted {
            result: "succeeded".to_owned(),
            runner_id: 1,
            runner_name: "velnor-7-801".to_owned(),
            base: base(999, ScaleSetJobMessageType::JobCompleted),
        };
        assert!(processor.observe_completed(&routed_completed).unwrap());
        assert_eq!(processor.lane_mut().terminals, vec![999]);
        assert_eq!(
            processor.demand_mut().get(801).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert!(processor.demand_mut().get(999).unwrap().is_none());
    }

    #[tokio::test]
    async fn nil_scales_on_cached_stats() {
        let path = temp_path("nil");
        let mut processor = processor(&path, ScriptedQueue::default());
        let initial = RunnerScaleSetMessage {
            message_id: crate::scaleset::listener::INITIAL_MESSAGE_ID,
            statistics: Some(stats(2)),
            ..RunnerScaleSetMessage::default()
        };
        let first = processor.scale(Some(&initial)).await.unwrap();
        assert_eq!(first.kind, ScaleKind::Initial);
        let nil = processor.scale(None).await.unwrap();
        assert_eq!(nil.kind, ScaleKind::Nil);
        assert_eq!(
            nil.decision,
            Some(PopulationDecision::AcquireMore {
                desired: 2,
                local: 0,
                headroom: 2,
            })
        );
    }
}

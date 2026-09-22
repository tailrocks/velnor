//! Provision intents (§5.1 step 5) + population convergence (step 7).
//!
//! Step 5 persists the [`ProvisionIntent`][pi] BEFORE the worker lane runs:
//! retries adopt by `(operation_id, ownership_id)` instead of creating a
//! second runner. The worker lane itself (`worker/`, d1b) implements
//! [`WorkerLane`]; this module only defines the seam and the intent write.
//!
//! Step 7 converges on `statistics.TotalAssignedJobs` — the authoritative
//! desired count — never on batch sizes. Local `>` desired drains by
//! completion; the adapter never kills running work to chase the number
//! down.
//!
//! [pi]: crate::scaleset::intents::ProvisionIntent

use anyhow::Result;
use velnor_model::{RunnerScaleSetStatistic, ScaleSetJobCompleted};

use crate::scaleset::demand::{DemandState, DemandStore};
use crate::scaleset::intents::{
    provision_operation_id, provision_ownership_id, runner_name, ProvisionIntent,
    ProvisionIntentStore,
};

/// The worker-lane seam (d1b implements; loop tests stub).
///
/// All calls happen AFTER the durable intent write, so every method must
/// tolerate redelivery: `provision` adopts by `(operation_id,
/// ownership_id)`, `note_terminal` is a terminal-state no-op on replay.
pub trait WorkerLane {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Fetch the JIT config and create (or adopt) the runner + DinD pair
    /// for a persisted intent. On success the lane records the JIT
    /// fingerprint via
    /// [`ProvisionIntentStore::record_jit_fingerprint`].
    ///
    /// Async by construction: the JIT fetch is network I/O, and driving
    /// it with `block_on` from a worker self-deadlocks the runtime's
    /// driver whenever no other thread keeps driving I/O.
    async fn provision(&mut self, intent: &ProvisionIntent) -> Result<(), Self::Error>;

    /// Observe `JobAssigned` for tracked work (idempotent replay).
    fn note_assigned(
        &mut self,
        assigned: &velnor_model::ScaleSetJobAssigned,
    ) -> Result<(), Self::Error>;

    /// Observe `JobStarted` for tracked work (idempotent replay).
    fn note_started(
        &mut self,
        started: &velnor_model::ScaleSetJobStarted,
    ) -> Result<(), Self::Error>;

    /// Drive the worker for `request_id` toward terminal (diagnostic
    /// export + owned cleanup scheduled). The permit stays with the
    /// worker until cleanup confirms; the lane releases it via
    /// [`release_after_cleanup`][rac] afterwards, or retains it
    /// `uncertain` when cleanup fails.
    ///
    /// [rac]: crate::scaleset::capacity::release_after_cleanup
    fn note_terminal(&mut self, completed: &ScaleSetJobCompleted) -> Result<(), Self::Error>;

    /// Finish cancellation after an uncertain acquire was later confirmed
    /// acquired. No second wire message is required because the completion
    /// was already durably recorded as `canceled_pending`.
    fn note_canceled(&mut self, _request_id: i64) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Reconcile live workers during an idle poll. Implementations may
    /// throttle this call; it is the progress clock when GitHub sends no
    /// messages. The default keeps lightweight/test lanes stateless.
    fn idle_tick(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Whether the lane still owns terminal cleanup for `request_id` — a
    /// worker row exists that the terminal path visits. The idle path
    /// releases a `Terminal`+held orphan's permit only when the lane
    /// disowns it (no worker row: nothing to clean, and a completed job
    /// emits no further messages, so the occupancy would leak forever).
    /// Defaults to owned: lanes that track no workers keep the
    /// terminal-handler-owns-cleanup design.
    fn owns_terminal_cleanup(&self, _request_id: i64) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Image digests pinned for one provision. d1b owns the pin values; the
/// loop only threads them into the intent row.
#[derive(Debug, Clone)]
pub struct ProvisionImages {
    pub runner_digest: String,
    pub dind_digest: String,
}

/// Persist the provision intent (idempotent on `operation_id`) and then run
/// the worker lane. Returns the durable intent row.
#[allow(clippy::too_many_arguments, reason = "single step-5 call site")]
pub async fn ensure_provision_intent<W: WorkerLane>(
    intents: &mut ProvisionIntentStore,
    lane: &mut W,
    scale_set_id: i32,
    request_id: i64,
    attempt: u32,
    images: &ProvisionImages,
    generation: u64,
    metrics: &crate::scaleset::metrics::Metrics,
) -> Result<ProvisionIntent> {
    let name = runner_name(scale_set_id, request_id);
    let intent = intents.record_intent(
        &provision_operation_id(scale_set_id, request_id, attempt),
        &provision_ownership_id(scale_set_id, &name),
        scale_set_id,
        request_id,
        &name,
        &images.runner_digest,
        &images.dind_digest,
        generation,
    )?;
    metrics.inc_provision_intents();
    lane.provision(&intent)
        .await
        .map_err(|error| anyhow::anyhow!("worker provision for request {request_id}: {error}"))?;
    Ok(intent)
}

/// Step-7 decision from authoritative statistics vs local population.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopulationDecision {
    /// Local `count` is below desired: the next acquire pass may take up to
    /// `headroom` more offers (still bounded by free ledger capacity).
    AcquireMore {
        desired: u32,
        local: u32,
        headroom: u32,
    },
    /// At or above desired: no new acquires; excess drains by completion.
    /// The adapter never kills running work to chase the number down.
    Hold { desired: u32, local: u32 },
}

/// Converge step: compare `TotalAssignedJobs` against the local population
/// (alive + acquiring + provisioning + uncertain for this set). Pure over
/// its inputs; the caller supplies the durable counts.
#[must_use]
pub fn reconcile_population(
    statistics: &RunnerScaleSetStatistic,
    local: u32,
) -> PopulationDecision {
    let desired = statistics.desired_runners();
    if local < desired {
        PopulationDecision::AcquireMore {
            desired,
            local,
            headroom: desired - local,
        }
    } else {
        PopulationDecision::Hold { desired, local }
    }
}

/// Local population for one set: `acquire_intent | acquired | uncertain
/// | provision_intent` — workers alive, acquiring, provisioning, or
/// uncertain, per the step-7 contract. `granted` rows are deliberately
/// EXCLUDED: they are the acquire pass's input, and counting them would
/// hold a granted backlog at `local == desired` forever (liveness). Queued
/// (`eligible`/`observed`) and finished (`declined`/`terminal`) rows are
/// not population either.
pub fn local_population(store: &DemandStore, scale_set_id: i32) -> Result<u32> {
    let count = store.count_in_states(
        scale_set_id,
        &[
            DemandState::AcquireIntent,
            DemandState::Acquired,
            DemandState::Uncertain,
            DemandState::CanceledPending,
            DemandState::CanceledAcquired,
            DemandState::ProvisionIntent,
        ],
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
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

    #[derive(Debug, Default)]
    struct StubLane {
        provisioned: Vec<String>,
        terminals: Vec<i64>,
    }

    #[derive(Debug)]
    struct StubError(String);

    impl std::fmt::Display for StubError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "stub lane: {}", self.0)
        }
    }

    impl std::error::Error for StubError {}

    impl WorkerLane for StubLane {
        type Error = StubError;

        async fn provision(&mut self, intent: &ProvisionIntent) -> Result<(), Self::Error> {
            self.provisioned.push(intent.operation_id.clone());
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

        fn note_terminal(&mut self, completed: &ScaleSetJobCompleted) -> Result<(), Self::Error> {
            self.terminals.push(completed.base.runner_request_id);
            Ok(())
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-scaleset-converge-{name}-{}",
            std::process::id()
        ));
        // Drop stale state from pid-reusing earlier runs: every test starts
        // from an empty database, deterministically.
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

    #[tokio::test]
    async fn provision_intent_persists_before_lane_runs() {
        let path = temp_path("intent");
        let mut intents = ProvisionIntentStore::open(&path).unwrap();
        let mut lane = StubLane::default();
        let metrics = crate::scaleset::metrics::Metrics::new();
        let intent =
            ensure_provision_intent(&mut intents, &mut lane, 7, 4242, 0, &images(), 2, &metrics)
                .await
                .unwrap();
        assert_eq!(intent.runner_name, "velnor-7-4242");
        assert_eq!(lane.provisioned, vec!["prov-op-7-4242-0".to_owned()]);
        assert_eq!(metrics.snapshot().provision_intents, 1);
        // Retry adopts the same row.
        let again =
            ensure_provision_intent(&mut intents, &mut lane, 7, 4242, 0, &images(), 2, &metrics)
                .await
                .unwrap();
        assert_eq!(again, intent);
    }

    #[test]
    fn desired_comes_from_statistics_not_batches() {
        let stats = RunnerScaleSetStatistic {
            total_assigned_jobs: 3,
            ..RunnerScaleSetStatistic::default()
        };
        assert_eq!(
            reconcile_population(&stats, 1),
            PopulationDecision::AcquireMore {
                desired: 3,
                local: 1,
                headroom: 2,
            }
        );
        assert_eq!(
            reconcile_population(&stats, 3),
            PopulationDecision::Hold {
                desired: 3,
                local: 3,
            }
        );
        assert_eq!(
            reconcile_population(&stats, 9),
            PopulationDecision::Hold {
                desired: 3,
                local: 9,
            }
        );
        let negative = RunnerScaleSetStatistic {
            total_assigned_jobs: -5,
            ..RunnerScaleSetStatistic::default()
        };
        assert_eq!(
            reconcile_population(&negative, 0),
            PopulationDecision::Hold {
                desired: 0,
                local: 0,
            }
        );
    }

    #[test]
    fn local_population_counts_acquiring_and_beyond_not_granted() {
        let path = temp_path("population");
        let mut demand = DemandStore::open(&path).unwrap();
        let push = |id: i64| velnor_model::ScaleSetJobAvailable {
            acquire_job_url: String::new(),
            base: velnor_model::ScaleSetJobMessage {
                message_type: velnor_model::ScaleSetJobMessageType::JobAvailable,
                runner_request_id: id,
                repository_name: "velnor".to_owned(),
                owner_name: "tailrocks".to_owned(),
                job_id: format!("job-{id}"),
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
        demand.submit_offer(7, &push(1), 1).unwrap();
        demand.submit_offer(7, &push(2), 1).unwrap();
        demand.submit_offer(7, &push(3), 1).unwrap();
        demand
            .set_state(7, 1, DemandState::Acquired, None, 1)
            .unwrap();
        demand
            .set_state(7, 3, DemandState::Granted, None, 1)
            .unwrap();
        // eligible(2) is queued demand and granted(3) is acquire input:
        // neither is population.
        assert_eq!(local_population(&demand, 7).unwrap(), 1);
    }
}

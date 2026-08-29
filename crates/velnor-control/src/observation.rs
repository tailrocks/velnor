//! Ordered event, metric, and wait primitives.
//!
//! The stream is bounded and cursor-based. Consumers that fall behind receive
//! a conflict and must resnapshot through the query port; no unbounded polling
//! or detached observer task is created here.

use std::sync::{Arc, RwLock};

use velnor_model::{ConditionStatus, Event, Timestamp};

use crate::ports::{PortError, WatchItem, WatchPort, WatchRequest};
use crate::store::records::validate_event_contract;
use crate::store::{EventRow, Store, StoreResult};

const MAX_BUFFER: usize = 4_096;

/// One bounded event stream with a monotonic version.
#[derive(Clone)]
pub struct EventStream {
    state: Arc<RwLock<EventState>>,
    durable: Option<DurableProjection>,
}

#[derive(Clone)]
struct DurableProjection {
    store: Arc<Store>,
    instance_slug: String,
}

struct EventState {
    generation: u64,
    next_version: u64,
    events: std::collections::VecDeque<WatchItem>,
}

impl Default for EventStream {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(EventState {
                generation: 1,
                next_version: 1,
                events: std::collections::VecDeque::new(),
            })),
            durable: None,
        }
    }
}

impl EventStream {
    /// Create an empty stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reopen an event stream from the daemon's durable state.
    pub fn with_store(store: Arc<Store>, instance_slug: impl Into<String>) -> StoreResult<Self> {
        let instance_slug = instance_slug.into();
        Ok(Self {
            state: Arc::new(RwLock::new(EventState {
                generation: 1,
                next_version: 1,
                events: std::collections::VecDeque::new(),
            })),
            durable: Some(DurableProjection {
                store,
                instance_slug,
            }),
        })
    }

    /// Append one sanitized event and return its stream version.
    pub fn publish(&self, event: Event) -> Result<u64, PortError> {
        let validation_instance = self
            .durable
            .as_ref()
            .map_or(event.meta.name.as_str(), |durable| {
                durable.instance_slug.as_str()
            });
        validate_event(&event, validation_instance)?;
        if let Some(durable) = &self.durable {
            return durable
                .store
                .append_event(&EventRow {
                    instance_slug: durable.instance_slug.clone(),
                    event_kind: event.event_kind.clone(),
                    subject: event.subject.clone(),
                    correlation_id: Some(event.meta.name.clone()),
                    occurred_at: event.occurred_at,
                    detail: event.detail.clone(),
                })
                .map_err(|_| durable_error("event persistence"));
        }
        let mut state = self.state.write().map_err(|_| unavailable())?;
        let version = state.next_version;
        state.next_version = state.next_version.saturating_add(1);
        state.events.push_back(WatchItem { version, event });
        if state.events.len() > MAX_BUFFER {
            state.events.pop_front();
            state.generation = state.generation.saturating_add(1);
        }
        Ok(version)
    }

    /// Current stream generation.
    pub fn generation(&self) -> Result<u64, PortError> {
        self.state
            .read()
            .map(|state| state.generation)
            .map_err(|_| unavailable())
    }
}

fn durable_error(operation: &str) -> PortError {
    PortError::Operation {
        operation: operation.to_owned(),
    }
}

fn validate_event(event: &Event, instance_slug: &str) -> Result<(), PortError> {
    validate_event_contract(
        instance_slug,
        &event.event_kind,
        &event.subject,
        Some(&event.meta.name),
        event.detail.as_deref(),
    )
    .map_err(|error| PortError::Invalid {
        field: "event".to_owned(),
        message: error.envelope.reason,
    })
}

impl WatchPort for EventStream {
    fn watch(&self, request: WatchRequest) -> Result<Vec<WatchItem>, PortError> {
        if request.limit == 0 || request.limit as usize > MAX_BUFFER {
            return Err(PortError::Invalid {
                field: "limit".to_owned(),
                message: format!("must be between 1 and {MAX_BUFFER}"),
            });
        }
        let after = request.after_version.unwrap_or(0);
        if let Some(durable) = &self.durable {
            let window = durable
                .store
                .event_window(
                    &durable.instance_slug,
                    after,
                    request.resource_kind.as_deref(),
                    request.limit,
                )
                .map_err(|_| durable_error("event read"))?;
            if let Some(first) = window.first_retained_id {
                if after != 0 && after.saturating_add(1) < first {
                    return Err(PortError::Conflict {
                        operation: "event cursor expired; resnapshot required".to_owned(),
                    });
                }
            }
            if window.high_water_id.is_some_and(|last| after > last) {
                return Err(PortError::Invalid {
                    field: "after_version".to_owned(),
                    message: "event cursor is ahead of the stream".to_owned(),
                });
            }
            return Ok(window
                .events
                .into_iter()
                .map(|stored| {
                    let row = stored.row;
                    // `correlation_id` is an opaque transition/run token. It
                    // is never an authority for the event resource identity.
                    // The subject is the only identity that may be exposed as
                    // ResourceMeta.name, preventing cross-instance/resource
                    // claims during durable rehydration.
                    let event = Event {
                        meta: velnor_model::ResourceMeta::new(
                            &row.subject,
                            velnor_model::Source::Local,
                            row.occurred_at,
                        ),
                        sequence: stored.id,
                        occurred_at: row.occurred_at,
                        event_kind: row.event_kind,
                        subject: row.subject,
                        detail: row.detail,
                    };
                    WatchItem {
                        version: stored.id,
                        event,
                    }
                })
                .collect());
        }
        let state = self.state.read().map_err(|_| unavailable())?;
        if let Some(first) = state.events.front() {
            if after != 0 && after.saturating_add(1) < first.version {
                return Err(PortError::Conflict {
                    operation: "event cursor expired; resnapshot required".to_owned(),
                });
            }
        }
        Ok(state
            .events
            .iter()
            .filter(|item| item.version > after)
            .filter(|item| {
                request.resource_kind.as_deref().is_none_or(|kind| {
                    item.event.event_kind.eq_ignore_ascii_case(kind)
                        || item.event.subject.eq_ignore_ascii_case(kind)
                })
            })
            .take(request.limit as usize)
            .cloned()
            .collect())
    }
}

/// One bounded live metric observation.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricSample {
    /// Resource identity.
    pub resource: String,
    /// Metric name.
    pub metric: String,
    /// Numeric value, when observed.
    pub value: Option<f64>,
    /// Unit such as `bytes`, `seconds`, or `percent`.
    pub unit: String,
    /// Gauge/counter classification.
    pub kind: MetricKind,
    /// Source observation time.
    pub observed_at: Timestamp,
    /// Monotonic local observation timestamp in nanoseconds.
    pub monotonic_ns: u128,
}

/// Metric semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// Current value.
    Gauge,
    /// Accumulated value.
    Counter,
}

/// Terminal outcome of a condition wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitOutcome {
    /// Predicate became true.
    Satisfied,
    /// Predicate became false with a stable reason.
    Failed { reason: String },
    /// Observation deadline elapsed.
    TimedOut,
    /// Caller canceled observation.
    Canceled,
}

/// Convert a condition status into a typed wait outcome.
#[must_use]
pub fn wait_outcome(status: ConditionStatus, reason: Option<&str>) -> WaitOutcome {
    match status {
        ConditionStatus::True => WaitOutcome::Satisfied,
        ConditionStatus::False => WaitOutcome::Failed {
            reason: reason.unwrap_or("condition false").to_owned(),
        },
        ConditionStatus::Unknown => WaitOutcome::TimedOut,
    }
}

fn unavailable() -> PortError {
    PortError::Unavailable {
        resource: "event stream".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use super::*;
    use crate::store::{RetentionBudget, Store};
    use velnor_model::{ResourceMeta, Source};

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let nonce = Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
                .unsigned_abs();
            let dir = std::env::temp_dir().join(format!(
                "velnor-observation-cursor-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("temporary directory");
            let path = dir.join("state.db");
            Self { dir, path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn event(kind: &str) -> Event {
        Event {
            meta: ResourceMeta::new("instance/primary", Source::Local, Timestamp::UNIX_EPOCH),
            sequence: 0,
            occurred_at: Timestamp::UNIX_EPOCH,
            event_kind: kind.to_owned(),
            subject: "instance/primary".to_owned(),
            detail: None,
        }
    }

    #[test]
    fn stream_resumes_after_monotonic_cursor() {
        let stream = EventStream::new();
        let first = stream.publish(event("ready")).expect("publish");
        stream.publish(event("degraded")).expect("publish");
        let items = stream
            .watch(WatchRequest {
                resource_kind: None,
                after_version: Some(first),
                limit: 10,
            })
            .expect("watch");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].event.event_kind, "degraded");
    }

    #[test]
    fn stream_resource_kind_filter_matches_subject() {
        let stream = EventStream::new();
        let mut candidate = event("ready");
        candidate.subject = "job/build".to_owned();
        stream.publish(candidate).expect("publish");

        let items = stream
            .watch(WatchRequest {
                resource_kind: Some("job/build".to_owned()),
                after_version: None,
                limit: 10,
            })
            .expect("watch");

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].event.subject, "job/build");
    }

    #[test]
    fn stream_enforces_the_durable_event_contract() {
        let stream = EventStream::new();

        let mut candidate = event("ready");
        candidate.detail = Some("x".repeat(4 * 1024));
        assert!(stream.publish(candidate).is_ok());

        let mut candidate = event("ready");
        candidate.detail = Some("x".repeat(4 * 1024 + 1));
        assert!(stream.publish(candidate).is_err());

        let mut candidate = event("ready");
        candidate.detail = Some("safe\nlog".to_owned());
        assert!(stream.publish(candidate).is_err());

        let mut candidate = event("ready");
        candidate.detail = Some("token=[REDACTED]LEAK".to_owned());
        assert!(stream.publish(candidate).is_err());

        let mut candidate = event("ready");
        candidate.detail = Some("token=[REDACTED]".to_owned());
        assert!(stream.publish(candidate).is_ok());

        let candidate = event("ready\n");
        assert!(stream.publish(candidate).is_err());

        let mut candidate = event("ready");
        candidate.subject = "job name".to_owned();
        assert!(stream.publish(candidate).is_err());
    }

    #[test]
    fn durable_correlation_is_opaque_and_subject_owns_event_identity() {
        let temp = TempDb::new();
        let store = Arc::new(Store::open(&temp.path).expect("open store"));
        let stream = EventStream::with_store(Arc::clone(&store), "instance/primary")
            .expect("create durable stream");

        let mut candidate = event("job.started");
        candidate.meta.name = "instance/other".to_owned();
        candidate.subject = "job/build".to_owned();
        stream.publish(candidate).expect("publish event");

        let window = store
            .event_window("instance/primary", 0, None, 10)
            .expect("read event row");
        assert_eq!(
            window.events[0].row.correlation_id.as_deref(),
            Some("instance/other")
        );

        let items = stream
            .watch(WatchRequest {
                resource_kind: None,
                after_version: None,
                limit: 10,
            })
            .expect("watch durable event");
        assert_eq!(items[0].event.subject, "job/build");
        assert_eq!(items[0].event.meta.name, "job/build");
    }

    #[test]
    fn durable_stream_reopens_and_preserves_pruned_cursor_bounds() {
        let temp = TempDb::new();
        let store = Arc::new(Store::open(&temp.path).expect("open store"));
        let stream = EventStream::with_store(Arc::clone(&store), "instance/primary")
            .expect("create durable stream");
        let first = stream.publish(event("started")).expect("publish first");
        stream.publish(event("waiting")).expect("publish second");
        let third = stream.publish(event("completed")).expect("publish third");
        drop(stream);
        drop(store);

        let store = Arc::new(Store::open(&temp.path).expect("reopen store"));
        let stream = EventStream::with_store(Arc::clone(&store), "instance/primary")
            .expect("reopen durable stream");
        let resumed = stream
            .watch(WatchRequest {
                resource_kind: None,
                after_version: Some(first),
                limit: 10,
            })
            .expect("resume after reopen");
        assert_eq!(resumed.len(), 2);
        assert_eq!(resumed[1].version, third);

        let report = store
            .prune_history(&RetentionBudget {
                max_event_age: Some(Duration::from_secs(1)),
                max_event_rows: 0,
                max_terminal_job_age: None,
                max_terminal_job_rows: 0,
                max_database_bytes: u64::MAX,
                batch_size: 10,
            })
            .expect("prune all expired events");
        assert_eq!(report.deleted_events, 3);
        drop(stream);
        drop(store);

        let store = Arc::new(Store::open(&temp.path).expect("reopen pruned store"));
        let stream = EventStream::with_store(Arc::clone(&store), "instance/primary")
            .expect("create stream over pruned store");
        let current = stream
            .watch(WatchRequest {
                resource_kind: None,
                after_version: Some(third),
                limit: 10,
            })
            .expect("high-water cursor remains valid");
        assert!(current.is_empty());

        assert!(matches!(
            stream.watch(WatchRequest {
                resource_kind: None,
                after_version: Some(first),
                limit: 10,
            }),
            Err(PortError::Conflict { .. })
        ));
        assert!(matches!(
            stream.watch(WatchRequest {
                resource_kind: None,
                after_version: Some(third + 1),
                limit: 10,
            }),
            Err(PortError::Invalid { field, .. }) if field == "after_version"
        ));
    }

    #[test]
    fn unknown_condition_does_not_report_success() {
        assert_eq!(
            wait_outcome(ConditionStatus::Unknown, None),
            WaitOutcome::TimedOut
        );
    }
}

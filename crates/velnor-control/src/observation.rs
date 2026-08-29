//! Ordered event, metric, and wait primitives.
//!
//! The stream is bounded and cursor-based. Consumers that fall behind receive
//! a conflict and must resnapshot through the query port; no unbounded polling
//! or detached observer task is created here.

use std::sync::{Arc, RwLock};

use velnor_model::{ConditionStatus, Event, Timestamp};

use crate::ports::{PortError, WatchItem, WatchPort, WatchRequest};

const MAX_BUFFER: usize = 4_096;

/// One bounded event stream with a monotonic version.
#[derive(Clone)]
pub struct EventStream {
    state: Arc<RwLock<EventState>>,
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
        }
    }
}

impl EventStream {
    /// Create an empty stream.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one sanitized event and return its stream version.
    pub fn publish(&self, event: Event) -> Result<u64, PortError> {
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

impl WatchPort for EventStream {
    fn watch(&self, request: WatchRequest) -> Result<Vec<WatchItem>, PortError> {
        if request.limit == 0 || request.limit as usize > MAX_BUFFER {
            return Err(PortError::Invalid {
                field: "limit".to_owned(),
                message: format!("must be between 1 and {MAX_BUFFER}"),
            });
        }
        let state = self.state.read().map_err(|_| unavailable())?;
        let after = request.after_version.unwrap_or(0);
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
                        || item.event.meta.name.eq_ignore_ascii_case(kind)
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
    use super::*;
    use velnor_model::{ResourceMeta, Source};

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
    fn unknown_condition_does_not_report_success() {
        assert_eq!(
            wait_outcome(ConditionStatus::Unknown, None),
            WaitOutcome::TimedOut
        );
    }
}

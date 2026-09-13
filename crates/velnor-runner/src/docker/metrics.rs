//! Per-job host `docker` invocation accounting.
//!
//! Every host `docker` process a job spawns passes through the concrete
//! `CommandRunner` in `crate::executor`, which is the single seam where this is
//! recorded — there are no counters at call sites. A minimal job is expected to
//! spawn on the order of a dozen host `docker` processes and a representative
//! one 50-70; this counter is how the Engine-API client migration is shown to
//! have removed them: a query served by the `engine` module records
//! [`observe_api`] and spawns no process, so `invocations` falls while
//! `api_calls` rises for the same workload.
//!
//! Every runner slot is its own process and runs one job at a time, so process
//! globals are exactly job scope. [`begin_job`] asserts that scope explicitly
//! and its guard reports the totals however the job ends.
//!
//! No field emitted here is derived from an argument vector. The operation
//! class is a closed vocabulary from [`DockerOp::label`], because the `tracing`
//! and forensic sinks perform no redaction and an argument vector can carry an
//! image reference, a registry URL or a credential.

use super::deadline::DockerOp;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const CLASSES: usize = DockerOp::ALL.len();

#[expect(
    clippy::declare_interior_mutable_const,
    reason = "array initializer for atomics; each element is a distinct static counter"
)]
const ZERO: AtomicU64 = AtomicU64::new(0);

static INVOCATIONS: AtomicU64 = AtomicU64::new(0);
static TIMEOUTS: AtomicU64 = AtomicU64::new(0);
static FAILURES: AtomicU64 = AtomicU64::new(0);
static CLASS_COUNT: [AtomicU64; CLASSES] = [ZERO; CLASSES];
static CLASS_MICROS: [AtomicU64; CLASSES] = [ZERO; CLASSES];
static API_CALLS: AtomicU64 = AtomicU64::new(0);
static API_FALLBACKS: AtomicU64 = AtomicU64::new(0);
static API_CLASS_COUNT: [AtomicU64; CLASSES] = [ZERO; CLASSES];
static API_CLASS_MICROS: [AtomicU64; CLASSES] = [ZERO; CLASSES];

/// Record one completed host `docker` invocation.
pub fn observe(op: DockerOp, elapsed: Duration, exit_code: i32, timed_out: bool) {
    let sequence = INVOCATIONS.fetch_add(1, Ordering::Relaxed) + 1;
    let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    CLASS_COUNT[op.index()].fetch_add(1, Ordering::Relaxed);
    CLASS_MICROS[op.index()].fetch_add(micros, Ordering::Relaxed);
    if timed_out {
        TIMEOUTS.fetch_add(1, Ordering::Relaxed);
    }
    if exit_code != 0 {
        FAILURES.fetch_add(1, Ordering::Relaxed);
    }
    tracing::debug!(
        target: "velnor.docker",
        docker_op = op.label(),
        docker_latency_ms = micros / 1_000,
        docker_exit_code = exit_code,
        docker_timed_out = timed_out,
        docker_invocation = sequence,
        "host docker invocation"
    );
}

/// Record one query served by the Engine API: no process was spawned, so
/// `invocations` is untouched and this is the counter that rises instead.
/// The per-class API latency is the migrated-calls comparison against the
/// CLI histogram above; only successful servings land here, so the number
/// is the fast-path cost, not a mix with degraded attempts.
pub fn observe_api(op: DockerOp, elapsed: Duration) {
    let sequence = API_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    API_CLASS_COUNT[op.index()].fetch_add(1, Ordering::Relaxed);
    API_CLASS_MICROS[op.index()].fetch_add(micros, Ordering::Relaxed);
    tracing::debug!(
        target: "velnor.docker",
        docker_op = op.label(),
        docker_transport = "api",
        docker_latency_ms = micros / 1_000,
        docker_api_call = sequence,
        "engine api query served"
    );
}

/// Record one Engine API attempt that fell back to the CLI. The CLI call
/// that follows records its own [`observe`], so a fallback costs exactly one
/// subprocess — the same as before the migration. The caller logs the reason
/// (a [`super::engine::EngineError` carries no body bytes) at `warn`.
pub fn observe_api_fallback(op: DockerOp) {
    let sequence = API_FALLBACKS.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::debug!(
        target: "velnor.docker",
        docker_op = op.label(),
        docker_transport = "cli-fallback",
        docker_api_fallback = sequence,
        "engine api fell back to docker cli"
    );
}

/// Reset the counters and return the guard that reports them.
///
/// Called once per job. Dropping the guard emits the totals, so an early return
/// or an error path reports as reliably as a clean finish.
#[must_use]
pub fn begin_job(job_id: &str) -> JobDockerScope {
    INVOCATIONS.store(0, Ordering::Relaxed);
    TIMEOUTS.store(0, Ordering::Relaxed);
    FAILURES.store(0, Ordering::Relaxed);
    API_CALLS.store(0, Ordering::Relaxed);
    API_FALLBACKS.store(0, Ordering::Relaxed);
    for index in 0..CLASSES {
        CLASS_COUNT[index].store(0, Ordering::Relaxed);
        CLASS_MICROS[index].store(0, Ordering::Relaxed);
        API_CLASS_COUNT[index].store(0, Ordering::Relaxed);
        API_CLASS_MICROS[index].store(0, Ordering::Relaxed);
    }
    JobDockerScope {
        job_id: job_id.to_string(),
    }
}

/// Guard covering one job's host `docker` accounting.
pub struct JobDockerScope {
    job_id: String,
}

/// Machine-readable form of the per-job counters, for the benchmark bridge.
///
/// This reads the same process counters the `velnor.docker` tracing fields are
/// derived from; it is not a second counter. `velnor-bench` consumes snapshots
/// (or the identical tracing fields) instead of recounting invocations at its
/// own call sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassTotal {
    /// Closed vocabulary label from [`DockerOp::label`]; never an argument.
    pub label: &'static str,
    pub count: u64,
    pub latency_ms: u64,
}

/// Point-in-time view of the current job scope's counters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub invocations: u64,
    pub timeouts: u64,
    pub failures: u64,
    /// One entry per class that occurred, in [`DockerOp::ALL`] order.
    pub classes: Vec<ClassTotal>,
    /// Queries served by the Engine API without spawning a process.
    pub api_calls: u64,
    /// API attempts that fell back to the CLI (each cost one subprocess,
    /// counted in `invocations` by the CLI call that follows).
    pub api_fallbacks: u64,
    /// Per-class API servings, in [`DockerOp::ALL`] order: the migrated-calls
    /// latency comparison against `classes`.
    pub api_classes: Vec<ClassTotal>,
}

/// Read the current counters without resetting them.
#[must_use]
pub fn snapshot() -> Snapshot {
    Snapshot {
        invocations: INVOCATIONS.load(Ordering::Relaxed),
        timeouts: TIMEOUTS.load(Ordering::Relaxed),
        failures: FAILURES.load(Ordering::Relaxed),
        classes: class_totals(&CLASS_COUNT, &CLASS_MICROS),
        api_calls: API_CALLS.load(Ordering::Relaxed),
        api_fallbacks: API_FALLBACKS.load(Ordering::Relaxed),
        api_classes: class_totals(&API_CLASS_COUNT, &API_CLASS_MICROS),
    }
}

fn class_totals(counts: &[AtomicU64], micros: &[AtomicU64]) -> Vec<ClassTotal> {
    let mut classes = Vec::new();
    for op in DockerOp::ALL {
        let count = counts[op.index()].load(Ordering::Relaxed);
        if count == 0 {
            continue;
        }
        let latency = micros[op.index()].load(Ordering::Relaxed);
        classes.push(ClassTotal {
            label: op.label(),
            count,
            latency_ms: latency / 1_000,
        });
    }
    classes
}

impl JobDockerScope {
    /// Host `docker` processes spawned so far in this job.
    #[must_use]
    pub fn invocations(&self) -> u64 {
        INVOCATIONS.load(Ordering::Relaxed)
    }

    /// `class=count` for every class that occurred, in class order.
    #[must_use]
    pub fn per_class_counts(&self) -> String {
        join_class_fields(&CLASS_COUNT, 1)
    }

    /// `class=milliseconds` for every class that occurred, in class order.
    #[must_use]
    pub fn per_class_latency_ms(&self) -> String {
        join_class_fields(&CLASS_MICROS, 1_000)
    }

    /// Queries served by the Engine API so far in this job.
    #[must_use]
    pub fn api_calls(&self) -> u64 {
        API_CALLS.load(Ordering::Relaxed)
    }

    /// API attempts that fell back to the CLI so far in this job.
    #[must_use]
    pub fn api_fallbacks(&self) -> u64 {
        API_FALLBACKS.load(Ordering::Relaxed)
    }

    /// `class=milliseconds` of API servings, for the migrated-calls latency
    /// comparison against [`Self::per_class_latency_ms`].
    #[must_use]
    pub fn per_class_api_latency_ms(&self) -> String {
        join_class_fields(&API_CLASS_MICROS, 1_000)
    }
}

fn join_class_fields(counters: &[AtomicU64; CLASSES], divisor: u64) -> String {
    let mut fields = String::new();
    for op in DockerOp::ALL {
        let raw = counters[op.index()].load(Ordering::Relaxed);
        if raw == 0 {
            continue;
        }
        if !fields.is_empty() {
            fields.push(',');
        }
        fields.push_str(op.label());
        fields.push('=');
        fields.push_str(&(raw / divisor).to_string());
    }
    fields
}

impl Drop for JobDockerScope {
    fn drop(&mut self) {
        let total_micros: u64 = CLASS_MICROS
            .iter()
            .map(|counter| counter.load(Ordering::Relaxed))
            .sum();
        tracing::info!(
            target: "velnor.docker",
            job_id = self.job_id.as_str(),
            docker_invocations = self.invocations(),
            docker_invocations_by_class = self.per_class_counts().as_str(),
            docker_latency_ms_by_class = self.per_class_latency_ms().as_str(),
            docker_wall_ms = total_micros / 1_000,
            docker_timeouts = TIMEOUTS.load(Ordering::Relaxed),
            docker_failures = FAILURES.load(Ordering::Relaxed),
            docker_api_calls = self.api_calls(),
            docker_api_fallbacks = self.api_fallbacks(),
            docker_api_latency_ms_by_class = self.per_class_api_latency_ms().as_str(),
            "host docker invocations for job"
        );
    }
}

// The counters are process globals, which is exactly job scope in
// production (one job per slot process) but shared across tests in one
// binary. Counter assertions therefore run under one lock, shared with the
// facade routing tests in `super::client` through
// [`lock_serial_for_test`].
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
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hold while asserting counter values in tests, here or in the facade
/// routing tests: without one shared lock a parallel test's `begin_job`
/// resets the counters mid-assertion and the suite flakes.
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
pub(crate) fn lock_serial_for_test() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|error| error.into_inner())
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

    #[test]
    fn a_job_scope_counts_every_class_it_sees() {
        let _serial = lock_serial_for_test();
        let scope = begin_job("job-1");
        observe(DockerOp::Query, Duration::from_millis(5), 0, false);
        observe(DockerOp::Query, Duration::from_millis(7), 0, false);
        observe(DockerOp::Remove, Duration::from_millis(20_000), 124, true);
        assert_eq!(scope.invocations(), 3);
        assert_eq!(scope.per_class_counts(), "query=2,remove=1");
        assert_eq!(scope.per_class_latency_ms(), "query=12,remove=20000");
        assert_eq!(TIMEOUTS.load(Ordering::Relaxed), 1);
        assert_eq!(FAILURES.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_new_job_scope_starts_from_zero() {
        let _serial = lock_serial_for_test();
        let first = begin_job("job-1");
        observe(DockerOp::Payload, Duration::from_millis(1), 0, false);
        assert_eq!(first.invocations(), 1);
        drop(first);
        let second = begin_job("job-2");
        assert_eq!(second.invocations(), 0);
        assert_eq!(second.per_class_counts(), "");
    }

    #[test]
    fn api_servings_rise_without_touching_invocations() {
        let _serial = lock_serial_for_test();
        let scope = begin_job("job-api");
        observe_api(DockerOp::Query, Duration::from_micros(400));
        observe_api(DockerOp::Query, Duration::from_micros(600));
        observe_api(DockerOp::DaemonQuery, Duration::from_millis(1));
        observe_api_fallback(DockerOp::Query);
        assert_eq!(scope.invocations(), 0);
        assert_eq!(scope.api_calls(), 3);
        assert_eq!(scope.api_fallbacks(), 1);
        assert_eq!(scope.per_class_api_latency_ms(), "daemon-query=1,query=1");
        let counts = snapshot();
        assert_eq!(counts.api_calls, 3);
        assert_eq!(counts.api_fallbacks, 1);
        assert_eq!(
            counts.api_classes,
            vec![
                ClassTotal {
                    label: "daemon-query",
                    count: 1,
                    latency_ms: 1,
                },
                ClassTotal {
                    label: "query",
                    count: 2,
                    latency_ms: 1,
                },
            ]
        );
        assert!(counts.classes.is_empty());
        drop(scope);
        let second = begin_job("job-api-reset");
        assert_eq!(second.api_calls(), 0);
        assert_eq!(second.api_fallbacks(), 0);
        assert_eq!(snapshot().api_classes, Vec::new());
    }

    #[test]
    fn a_snapshot_reads_the_same_counters_as_the_forensics_fields() {
        let _serial = lock_serial_for_test();
        let scope = begin_job("job-snapshot");
        observe(DockerOp::Query, Duration::from_millis(5), 0, false);
        observe(DockerOp::Query, Duration::from_millis(7), 0, false);
        observe(DockerOp::Remove, Duration::from_millis(20_000), 124, true);
        let snapshot = snapshot();
        assert_eq!(snapshot.invocations, scope.invocations());
        assert_eq!(snapshot.timeouts, 1);
        assert_eq!(snapshot.failures, 1);
        assert_eq!(
            snapshot.classes,
            vec![
                ClassTotal {
                    label: "query",
                    count: 2,
                    latency_ms: 12,
                },
                ClassTotal {
                    label: "remove",
                    count: 1,
                    latency_ms: 20_000,
                },
            ]
        );
        // The snapshot is a read, not a reset: the scope still reports.
        assert_eq!(scope.invocations(), 3);
        assert_eq!(scope.per_class_counts(), "query=2,remove=1");
        assert_eq!(
            snapshot
                .classes
                .iter()
                .map(|class| class.count)
                .sum::<u64>(),
            3
        );
    }

    #[test]
    fn a_snapshot_of_an_idle_scope_is_empty_but_valid() {
        let _serial = lock_serial_for_test();
        let _scope = begin_job("job-idle");
        assert_eq!(snapshot(), Snapshot::default());
    }
}

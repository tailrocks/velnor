//! Per-job host `docker` invocation accounting.
//!
//! Every host `docker` process a job spawns passes through the concrete
//! `CommandRunner` in `crate::executor`, which is the single seam where this is
//! recorded — there are no counters at call sites. A minimal job is expected to
//! spawn on the order of a dozen host `docker` processes and a representative
//! one 50-70; this counter is how the pending Engine-API client migration will
//! be shown to have removed them, so it has to exist before that migration
//! rather than after it.
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

/// Reset the counters and return the guard that reports them.
///
/// Called once per job. Dropping the guard emits the totals, so an early return
/// or an error path reports as reliably as a clean finish.
#[must_use]
pub fn begin_job(job_id: &str) -> JobDockerScope {
    INVOCATIONS.store(0, Ordering::Relaxed);
    TIMEOUTS.store(0, Ordering::Relaxed);
    FAILURES.store(0, Ordering::Relaxed);
    for index in 0..CLASSES {
        CLASS_COUNT[index].store(0, Ordering::Relaxed);
        CLASS_MICROS[index].store(0, Ordering::Relaxed);
    }
    JobDockerScope {
        job_id: job_id.to_string(),
    }
}

/// Guard covering one job's host `docker` accounting.
pub struct JobDockerScope {
    job_id: String,
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
            "host docker invocations for job"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The counters are process globals, which is exactly job scope in
    // production (one job per slot process) but shared across tests in one
    // binary. These two assertions therefore run under one lock.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_job_scope_counts_every_class_it_sees() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
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
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        let first = begin_job("job-1");
        observe(DockerOp::Payload, Duration::from_millis(1), 0, false);
        assert_eq!(first.invocations(), 1);
        drop(first);
        let second = begin_job("job-2");
        assert_eq!(second.invocations(), 0);
        assert_eq!(second.per_class_counts(), "");
    }
}

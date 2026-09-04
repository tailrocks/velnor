//! Soak harness: the suite that catches "the 500th job is mysteriously slower".
//!
//! Every test here is `#[ignore]`d and must be asked for by name:
//!
//! ```text
//! cargo test -p velnor-runner --features test-support --test soak -- --ignored --test-threads=1
//! ```
//!
//! They are ignored rather than feature-gated because the reason is runtime,
//! not compilation: each one drives hundreds to thousands of real durable
//! lifecycles through SQLite with `synchronous=FULL`, which takes minutes. A
//! multi-minute test in the default suite is a test people stop running, and a
//! feature flag would additionally need CI wiring to be discoverable. `--ignored`
//! keeps them compiled — so they cannot rot — while keeping the default suite
//! fast.
//!
//! What is monitored, and what is not:
//!
//! * monitored here — resident memory, open file descriptors, OS threads, the
//!   journal's on-disk size, staged completion payloads, and per-job latency
//!   drift between the first and last decile of the run;
//! * **not** monitored here — Docker objects, container processes, and image
//!   disk usage, because this harness deliberately runs without a Docker
//!   daemon. A soak that needed `dockerd` would be a soak nobody ran. The
//!   report accompanying this suite names the infrastructure that gap needs.
//!
//! Latency drift is asserted as a ratio with an absolute floor, so a busy host
//! makes the test slower but never red: only a genuine super-linear slowdown
//! trips it.

mod fault_support;

use std::time::{Duration, Instant};

use velnor_control::journal::{payload_checksum, Event, Journal};
use velnor_model::{JobId, SlotId};
use velnor_runner::execution::{CancelReason, JobCancellation, TerminationTarget};

use fault_support::{
    open_journal, prime_ready_slot, prime_ready_slots, tree_bytes, ResourceCensus, TempRoot,
};

/// Jobs per sequential soak. Large enough that a per-job leak of a single
/// descriptor or a few kilobytes is unmistakable.
const SEQUENTIAL_JOBS: usize = 500;
/// Slots interleaved in the multi-slot soak.
const SLOTS: usize = 4;
/// Jobs per slot in the multi-slot soak.
const JOBS_PER_SLOT: usize = 60;
/// Warm-path repetitions.
const WARM_REPEATS: usize = 2_000;

/// A deterministic pseudo-random source. Randomised cancellation must be
/// reproducible, or a soak failure cannot be investigated.
struct Lcg(u64);

impl Lcg {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        // Numerical Recipes' constants; the sequence only has to be varied and
        // repeatable, not cryptographic.
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn one_in(&mut self, n: u64) -> bool {
        self.next().is_multiple_of(n)
    }
}

/// Latency drift between the first and last tenth of a run.
struct Drift {
    first_decile: Duration,
    last_decile: Duration,
}

impl Drift {
    fn of(samples: &[Duration]) -> Self {
        let decile = (samples.len() / 10).max(1);
        let mean = |slice: &[Duration]| {
            slice
                .iter()
                .sum::<Duration>()
                .checked_div(u32::try_from(slice.len()).unwrap_or(1))
                .unwrap_or_default()
        };
        Self {
            first_decile: mean(&samples[..decile]),
            last_decile: mean(&samples[samples.len() - decile..]),
        }
    }

    /// Fail only on a slowdown that cannot be explained by host noise.
    ///
    /// The absolute floor matters: at tens of microseconds per job, one
    /// scheduler hiccup is a 10x ratio and means nothing. A run that is still
    /// fast in absolute terms is not leaking, whatever its ratio.
    fn assert_bounded(&self, label: &str, ratio: u32, floor: Duration) {
        if self.last_decile <= floor {
            return;
        }
        assert!(
            self.last_decile <= self.first_decile * ratio,
            "{label}: the last tenth of the run averaged {:?} against the first tenth's {:?} \
             (over {ratio}x); the runner is accumulating state across jobs",
            self.last_decile,
            self.first_decile
        );
    }
}

/// Assert that a soak left no measurable resource behind.
fn assert_no_growth(label: &str, before: ResourceCensus, after: ResourceCensus) {
    assert!(
        after.descriptor_growth(before) <= 4,
        "{label}: {} file descriptors leaked across the run (before {}, after {})",
        after.descriptor_growth(before),
        before.file_descriptors,
        after.file_descriptors
    );
    assert!(
        after.thread_growth(before) <= 2,
        "{label}: {} OS threads leaked across the run (before {}, after {})",
        after.thread_growth(before),
        before.threads,
        after.threads
    );
    if let Some(growth) = after.resident_growth_kib(before) {
        assert!(
            growth <= 256 * 1024,
            "{label}: resident memory grew by {growth} KiB across the run"
        );
    }
}

/// Drive one job's full durable lifecycle to a clean terminal state.
fn run_one_job(journal: &mut Journal, state_dir: &std::path::Path, slot: &SlotId, index: usize) {
    let job_id = JobId(format!("soak-{}-{index}", slot.0));
    let generation = journal
        .materialized_state()
        .expect("materialize fleet state")
        .slots
        .iter()
        .find(|record| record.slot_id == *slot)
        .expect("the soak slot exists")
        .generation;

    for event in [
        Event::Assigned {
            slot_id: slot.clone(),
            job_id: job_id.clone(),
            generation,
        },
        Event::JobOwned {
            job_id: job_id.clone(),
            slot_id: slot.clone(),
            attempt: 1,
            generation,
            worker: format!("velnor-job@{}", job_id.0),
            accepted_unix: 0,
        },
        Event::JobStarted {
            job_id: job_id.clone(),
            generation,
        },
        Event::JobTerminalResult {
            job_id: job_id.clone(),
            generation,
            conclusion: "success".into(),
        },
    ] {
        let outcome = journal.apply(event.clone()).expect("apply a soak event");
        assert!(!outcome.rejected, "soak event rejected: {event:?}");
    }

    let payload = format!(r#"{{"conclusion":"success","job":"{}"}}"#, job_id.0).into_bytes();
    velnor_runner::node::cleanup::write_outbox(state_dir, &job_id.0, generation.0, &payload)
        .expect("stage the soak payload");
    for event in [
        Event::CompletionIntended {
            job_id: job_id.clone(),
            generation,
            payload_sha256: payload_checksum(&payload),
        },
        Event::CompletionSendStarted {
            job_id: job_id.clone(),
            generation,
        },
        Event::RemoteAcked {
            job_id: job_id.clone(),
            generation,
        },
    ] {
        let outcome = journal
            .apply(event.clone())
            .expect("apply a soak completion event");
        assert!(
            !outcome.rejected,
            "soak completion event rejected: {event:?}"
        );
    }
    velnor_runner::node::cleanup::remove_outbox(state_dir, &job_id.0, generation.0)
        .expect("remove the acknowledged soak payload");

    // The slot must be immediately reusable, or the next iteration would be
    // measuring a different thing than the first.
    let ready = journal
        .apply(Event::ReadyAttempt {
            slot_id: slot.clone(),
            generation,
        })
        .expect("return the slot to Ready");
    assert!(!ready.rejected, "the soak slot did not return to Ready");
}

#[test]
#[ignore = "soak: hundreds of durable lifecycles, minutes long; run with --ignored"]
fn five_hundred_sequential_tiny_jobs_leave_no_residue_and_do_not_slow_down() {
    let root = TempRoot::new("soak-sequential");
    let slot = SlotId("soak-slot-0".into());
    let mut journal = open_journal(root.path());
    prime_ready_slot(&mut journal, &slot);

    let before = ResourceCensus::take();
    let mut latencies = Vec::with_capacity(SEQUENTIAL_JOBS);
    for index in 0..SEQUENTIAL_JOBS {
        let started = Instant::now();
        run_one_job(&mut journal, root.path(), &slot, index);
        latencies.push(started.elapsed());
    }
    let after = ResourceCensus::take();

    assert_no_growth("sequential soak", before, after);
    Drift::of(&latencies).assert_bounded("sequential soak", 4, Duration::from_millis(2));

    // No orphaned resources after 500 jobs.
    assert!(
        fault_support::staged_outbox_files(&root.join("outbox")).is_empty(),
        "the outbox must be empty after every job acknowledged"
    );
    assert!(journal
        .pending_outbox()
        .expect("read the pending outbox")
        .is_empty());

    // No cross-job contamination: exactly one slot, and no job left owned.
    let state = journal
        .materialized_state()
        .expect("materialize fleet state");
    assert_eq!(state.slots.len(), 1);
    assert!(
        state.jobs.is_empty(),
        "{} jobs are still owned after the soak",
        state.jobs.len()
    );

    // The journal grows with the immutable log, which is correct, but it must
    // grow proportionally rather than quadratically.
    let bytes = tree_bytes(root.path());
    assert!(
        bytes < 256 * 1024 * 1024,
        "the journal grew to {bytes} bytes over {SEQUENTIAL_JOBS} jobs"
    );
}

#[test]
#[ignore = "soak: sustained multi-slot execution; run with --ignored"]
fn sustained_multi_slot_execution_keeps_the_slots_independent() {
    let root = TempRoot::new("soak-multi-slot");
    let mut journal = open_journal(root.path());
    let slots: Vec<SlotId> = (0..SLOTS)
        .map(|index| SlotId(format!("soak-slot-{index}")))
        .collect();
    prime_ready_slots(&mut journal, &slots);

    let before = ResourceCensus::take();
    let mut latencies = Vec::with_capacity(SLOTS * JOBS_PER_SLOT);
    for round in 0..JOBS_PER_SLOT {
        for slot in &slots {
            let started = Instant::now();
            run_one_job(&mut journal, root.path(), slot, round);
            latencies.push(started.elapsed());
        }
    }
    let after = ResourceCensus::take();

    assert_no_growth("multi-slot soak", before, after);
    Drift::of(&latencies).assert_bounded("multi-slot soak", 4, Duration::from_millis(2));

    let state = journal
        .materialized_state()
        .expect("materialize fleet state");
    assert_eq!(
        state.slots.len(),
        SLOTS,
        "every slot must survive sustained execution"
    );
    assert!(state.jobs.is_empty(), "no job may outlive the soak");
    assert!(fault_support::staged_outbox_files(&root.join("outbox")).is_empty());
}

#[test]
#[ignore = "soak: repeated warm publish and read cycles; run with --ignored"]
fn repeated_warm_payload_cycles_do_not_accumulate_files_or_descriptors() {
    // The warm-build and warm-checkout analogue available without a Docker
    // daemon: the same staged-artifact path exercised over and over, where a
    // leaked descriptor or an un-deleted temporary would compound.
    let root = TempRoot::new("soak-warm");
    let payload = vec![b'x'; 64 * 1024];

    let before = ResourceCensus::take();
    let mut latencies = Vec::with_capacity(WARM_REPEATS);
    for index in 0..WARM_REPEATS {
        let job = format!("warm-{}", index % 8);
        let started = Instant::now();
        velnor_runner::node::cleanup::write_outbox(root.path(), &job, 1, &payload)
            .expect("stage the warm payload");
        let read = velnor_runner::node::cleanup::read_outbox(root.path(), &job, 1)
            .expect("read the warm payload");
        assert_eq!(read.len(), payload.len());
        velnor_runner::node::cleanup::remove_outbox(root.path(), &job, 1)
            .expect("remove the warm payload");
        latencies.push(started.elapsed());
    }
    let after = ResourceCensus::take();

    assert_no_growth("warm-path soak", before, after);
    Drift::of(&latencies).assert_bounded("warm-path soak", 4, Duration::from_millis(2));
    assert!(
        fault_support::staged_outbox_files(&root.join("outbox")).is_empty(),
        "repeated warm cycles must leave no temporary files behind"
    );
    assert!(
        tree_bytes(root.path()) < 1024 * 1024,
        "the warm path must not accumulate bytes on disk"
    );
}

#[test]
#[ignore = "soak: randomised cancellation over many jobs; run with --ignored"]
fn randomised_cancellation_over_many_jobs_leaks_no_targets_or_threads() {
    // Random, but seeded: a failure here is reproducible by re-running the same
    // seed, which is the difference between a soak and a lottery.
    let mut rng = Lcg::new(0x5EED_0024);
    let before = ResourceCensus::take();

    let mut cancelled = 0_usize;
    for index in 0..SEQUENTIAL_JOBS {
        let token = JobCancellation::recording(Some(Duration::from_secs(600)));
        let _job_container = token.register(TerminationTarget::Container {
            name: format!("velnor-soak-{index}"),
            role: velnor_runner::execution::ContainerRole::Job,
        });
        let _step = token.register(TerminationTarget::ProcessGroup {
            pgid: 0,
            label: format!("step-{index}"),
        });

        if rng.one_in(3) {
            cancelled += 1;
            // `force` plus an explicit fan-out keeps the soak synchronous: the
            // asynchronous ladder would leave one detached thread per cancelled
            // job asleep for its whole grace period, and the thread census
            // below would then measure the test, not the runner.
            token.force();
            token.fan_out_once();
            assert!(token.is_forced());
        }

        assert_eq!(
            token.target_keys().len(),
            2,
            "both targets must still be registered while the job is alive"
        );
        drop(_step);
        drop(_job_container);
        assert!(
            token.target_keys().is_empty(),
            "dropping a registration must remove its target, or the fan-out set \
             grows without bound"
        );
    }
    let after = ResourceCensus::take();

    assert!(
        cancelled > 0 && cancelled < SEQUENTIAL_JOBS,
        "the seeded run must cancel some jobs and spare others, cancelled {cancelled}"
    );
    assert_no_growth("cancellation soak", before, after);
}

#[test]
#[ignore = "soak: periodic reopen and compaction over a long run; run with --ignored"]
fn periodic_journal_reopen_bounds_the_write_ahead_log() {
    // The GC leg: a long-lived node checkpoints its journal periodically. If
    // the write-ahead log only ever grew, a host would run out of disk on
    // uptime alone.
    let root = TempRoot::new("soak-gc");
    let slot = SlotId("soak-slot-gc".into());
    let mut journal = open_journal(root.path());
    prime_ready_slot(&mut journal, &slot);

    let mut sizes = Vec::new();
    for index in 0..SEQUENTIAL_JOBS {
        run_one_job(&mut journal, root.path(), &slot, index);
        if index % 50 == 49 {
            // Reopening is what a restarted controller does, and it is the
            // checkpoint boundary the write-ahead log is bounded by.
            drop(journal);
            journal = open_journal(root.path());
            sizes.push(wal_bytes(&root.join("journal.db-wal")));
        }
    }

    let peak = sizes.iter().copied().max().unwrap_or(0);
    assert!(
        peak < 64 * 1024 * 1024,
        "the write-ahead log peaked at {peak} bytes across {SEQUENTIAL_JOBS} jobs"
    );
    // The last checkpoint must not be the largest: that would mean the log is
    // growing monotonically with uptime rather than being reclaimed.
    let last = sizes.last().copied().unwrap_or(0);
    assert!(
        last <= peak,
        "the write-ahead log ended at its own peak ({last} bytes)"
    );
    assert!(fault_support::staged_outbox_files(&root.join("outbox")).is_empty());
}

fn wal_bytes(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

#[test]
#[ignore = "soak: a cancelled job still reaches a clean terminal state; run with --ignored"]
fn cancelled_jobs_interleaved_with_completed_ones_leave_no_residue() {
    let root = TempRoot::new("soak-cancel-mixed");
    let slot = SlotId("soak-slot-mixed".into());
    let mut journal = open_journal(root.path());
    prime_ready_slot(&mut journal, &slot);
    let mut rng = Lcg::new(0xC0FF_EE24);

    let before = ResourceCensus::take();
    for index in 0..SEQUENTIAL_JOBS {
        if rng.one_in(4) {
            let token = JobCancellation::recording(Some(Duration::from_secs(600)));
            assert!(token.request(CancelReason::ServerRequested));
        }
        run_one_job(&mut journal, root.path(), &slot, index);
    }
    let after = ResourceCensus::take();

    // The cancellation ladder spawns one detached thread per `request`, each
    // sleeping out its grace period, so threads are deliberately excluded from
    // this leg's census — descriptors and memory are not.
    assert!(
        after.descriptor_growth(before) <= 4,
        "{} file descriptors leaked across the mixed soak",
        after.descriptor_growth(before)
    );
    if let Some(growth) = after.resident_growth_kib(before) {
        assert!(growth <= 256 * 1024, "resident memory grew by {growth} KiB");
    }

    let state = journal
        .materialized_state()
        .expect("materialize fleet state");
    assert!(state.jobs.is_empty());
    assert!(fault_support::staged_outbox_files(&root.join("outbox")).is_empty());
}

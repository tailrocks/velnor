//! Shared fixtures for the fault-injection and soak suites.
//!
//! Every helper here is deterministic by construction: no wall-clock waits, no
//! sleeps whose length decides the assertion, and no dependence on a real
//! Docker daemon, network, or GitHub. A fault test that races is worse than no
//! test, because a suite people learn to re-run teaches them to ignore red.
//!
//! The module is `include`d by several integration binaries, so unused-item
//! warnings are expected per binary and are silenced here rather than by
//! splitting the fixtures into one module per consumer.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use velnor_control::journal::{Event, Journal, OutboxRecord};
use velnor_model::{Generation, JobId, SlotId};

/// A temporary directory that removes itself, so a failing fault test cannot
/// leave the very orphaned state the suite exists to detect.
pub struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    #[must_use]
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "velnor-fault-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create fault-test temp root");
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The slot every fixture job is assigned to.
#[must_use]
pub fn fixture_slot() -> SlotId {
    SlotId("scope-1".into())
}

/// Open (or reopen) the fleet journal under `dir`.
///
/// Reopening is exactly what a restarted process sees, because every boundary
/// the fault suite asserts on is a durable commit.
///
/// # Panics
/// When the journal cannot be opened.
#[must_use]
pub fn open_journal(dir: &Path) -> Journal {
    Journal::open(dir.join("journal.db")).expect("open fleet journal")
}

/// Bring one slot to Ready. Returns its generation.
///
/// # Panics
/// When any bootstrap event is rejected, which would mean the fixture no
/// longer matches the reducer and every test built on it is meaningless.
pub fn prime_ready_slot(journal: &mut Journal, slot: &SlotId) -> Generation {
    prime_ready_slots(journal, std::slice::from_ref(slot))
}

/// Bring several slots to Ready under one advertised capacity.
///
/// The advertised capacity has to match the number of slots in one write: the
/// reducer refuses a fleet whose capacity and slot set disagree, which is
/// exactly the invariant a multi-slot soak would otherwise violate on its
/// second slot.
///
/// # Panics
/// When any bootstrap event is rejected.
pub fn prime_ready_slots(journal: &mut Journal, slots: &[SlotId]) -> Generation {
    let generation = Generation::INITIAL;
    let mut events = vec![
        Event::ControlLive,
        Event::JournalWritable,
        Event::Dependency {
            github_reachable: true,
        },
        Event::Routing {
            valid: true,
            group_valid: true,
        },
        Event::DesiredCapacity {
            ready: u32::try_from(slots.len()).expect("soak slot counts are small"),
        },
    ];
    for slot in slots {
        events.extend([
            Event::PermitReserved {
                slot_id: slot.clone(),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot.clone(),
                generation,
            },
            Event::SessionLive {
                slot_id: slot.clone(),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot.clone(),
                generation,
            },
            Event::Registered {
                slot_id: slot.clone(),
                generation,
            },
            Event::ReadyAttempt {
                slot_id: slot.clone(),
                generation,
            },
        ]);
    }
    for event in events {
        let outcome = journal.apply(event.clone()).expect("apply bootstrap event");
        assert!(
            !outcome.rejected,
            "fixture bootstrap event was rejected: {event:?}"
        );
    }
    generation
}

/// Bring one slot to Ready and give it an owned job. Returns its generation.
///
/// # Panics
/// When any fixture event is rejected.
pub fn prime_owned_job(journal: &mut Journal, slot: &SlotId, job_id: &JobId) -> Generation {
    let generation = prime_ready_slot(journal, slot);
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
    ] {
        let outcome = journal.apply(event.clone()).expect("apply fixture event");
        assert!(!outcome.rejected, "fixture event was rejected: {event:?}");
    }
    generation
}

/// The outbox row for `job_id`, if the journal has one.
#[must_use]
pub fn outbox_row(journal: &Journal, job_id: &JobId) -> Option<OutboxRecord> {
    journal
        .materialized_state()
        .expect("materialize fleet state")
        .outbox
        .into_iter()
        .find(|row| row.job_id == *job_id)
}

/// The phase recorded for `job_id`, if the journal still tracks it.
#[must_use]
pub fn job_phase(journal: &Journal, job_id: &JobId) -> Option<velnor_model::ActorPhase> {
    journal
        .materialized_state()
        .expect("materialize fleet state")
        .jobs
        .into_iter()
        .find(|job| job.job_id == *job_id)
        .map(|job| job.phase)
}

/// Every completion payload staged under `outbox_dir` (`<state dir>/outbox`).
///
/// An orphaned resource in the completion path is a payload file with no
/// journal row behind it, so the suite needs to enumerate both sides. Partial
/// writes (`..name.tmp-…`) are counted too: a leaked temporary is an orphan.
#[must_use]
pub fn staged_outbox_files(outbox_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_files(outbox_dir, &mut found);
    found.sort();
    found
}

fn collect_files(dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, into);
        } else {
            into.push(path);
        }
    }
}

/// Bytes occupied by everything under `dir`.
#[must_use]
pub fn tree_bytes(dir: &Path) -> u64 {
    let mut files = Vec::new();
    collect_files(dir, &mut files);
    files
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// A point-in-time census of the resources a leaking runner grows.
///
/// The soak suite compares two of these rather than asserting an absolute
/// number: absolute limits differ per host and per CI runner, while *growth*
/// across hundreds of identical jobs is the actual defect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceCensus {
    /// Open file descriptors held by this process.
    pub file_descriptors: usize,
    /// OS threads in this process.
    pub threads: usize,
    /// Resident set size in kibibytes, when the platform can report it.
    pub resident_kib: Option<u64>,
}

impl ResourceCensus {
    /// Take a census of the current process.
    #[must_use]
    pub fn take() -> Self {
        Self {
            file_descriptors: count_open_descriptors(),
            threads: count_threads(),
            resident_kib: resident_kib(),
        }
    }

    /// Descriptor growth since `earlier`, saturating at zero.
    #[must_use]
    pub fn descriptor_growth(self, earlier: Self) -> usize {
        self.file_descriptors
            .saturating_sub(earlier.file_descriptors)
    }

    /// Thread growth since `earlier`, saturating at zero.
    #[must_use]
    pub fn thread_growth(self, earlier: Self) -> usize {
        self.threads.saturating_sub(earlier.threads)
    }

    /// Resident-memory growth in kibibytes, when both censuses could read it.
    #[must_use]
    pub fn resident_growth_kib(self, earlier: Self) -> Option<u64> {
        match (self.resident_kib, earlier.resident_kib) {
            (Some(now), Some(before)) => Some(now.saturating_sub(before)),
            _ => None,
        }
    }
}

fn count_open_descriptors() -> usize {
    for candidate in ["/proc/self/fd", "/dev/fd"] {
        if let Ok(entries) = std::fs::read_dir(candidate) {
            // The read_dir handle itself is one of the entries; subtracting it
            // keeps two censuses comparable rather than exact.
            return entries.count().saturating_sub(1);
        }
    }
    0
}

fn count_threads() -> usize {
    if let Ok(entries) = std::fs::read_dir("/proc/self/task") {
        return entries.count();
    }
    let output = std::process::Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output();
    match output {
        // One header line plus one line per thread.
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .count()
            .saturating_sub(1),
        _ => 0,
    }
}

fn resident_kib() -> Option<u64> {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        return status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse().ok());
    }
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Assert that `message` reads like something an operator can act on: it names
/// the subject, and it is not an opaque debug dump.
///
/// A fault whose only symptom is `Error: None` costs an on-call engineer an
/// hour, so "there is a diagnostic" is itself an assertion.
///
/// # Panics
/// When the diagnostic is empty, or does not mention every required token.
pub fn assert_actionable_diagnostic(message: &str, must_mention: &[&str]) {
    assert!(
        message.len() > 16,
        "diagnostic is too short to act on: {message:?}"
    );
    for token in must_mention {
        assert!(
            message.to_ascii_lowercase().contains(&token.to_ascii_lowercase()),
            "diagnostic does not name {token:?}, so an operator cannot locate the subject: {message:?}"
        );
    }
}

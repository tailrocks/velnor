//! Hosted result watchdog per spec §8.
//!
//! One generated hosted authority reports the required result for each
//! run/attempt. It excludes itself and telemetry from the workload set, starts
//! after planning without `needs` on local jobs, validates authenticated
//! fresh outbound health (missing means unavailable), correlates runner/job
//! evidence, enumerates every API page, distinguishes attempts, never lets a
//! later report overwrite a failure with success, and revalidates reruns.
//! Outage findings carry measured detection latencies against the
//! 180s/120s/5m/10m operating targets.

#![allow(
    dead_code,
    reason = "D2 remainder API; schema-2 emission caller lands in d2a-rest"
)]

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::s2::provider::{ProviderId, ProviderSet};
use crate::s2::GeneratorError;

/// The one generated hosted watchdog job.
pub(crate) const WATCHDOG_JOB_ID: &str = "hosted-watchdog";
/// The telemetry writer: least privilege, never a workload job.
pub(crate) const TELEMETRY_JOB_ID: &str = "hosted-telemetry";

/// Whether a job id names workload the watchdog judges. The watchdog excludes
/// itself and telemetry.
#[must_use]
pub(crate) fn is_workload_job(job_id: &str) -> bool {
    job_id != WATCHDOG_JOB_ID && job_id != TELEMETRY_JOB_ID
}

/// Watchdog ordering: after planning, never waiting on local jobs — queued
/// local work must not prevent outage detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WatchdogOrdering {
    pub(crate) after_planning: bool,
    pub(crate) needs_local_jobs: bool,
}

impl WatchdogOrdering {
    /// The only valid ordering.
    #[must_use]
    pub(crate) fn spec() -> Self {
        Self {
            after_planning: true,
            needs_local_jobs: false,
        }
    }

    /// Reject any ordering that waits on local jobs or skips planning.
    ///
    /// # Errors
    /// Returns a usage error for a non-spec ordering.
    pub(crate) fn require_spec(ordering: Self) -> Result<(), GeneratorError> {
        if !ordering.after_planning {
            return Err(GeneratorError::usage(
                "the hosted watchdog must start after planning",
            ));
        }
        if ordering.needs_local_jobs {
            return Err(GeneratorError::usage(
                "the hosted watchdog must not wait in `needs` for local jobs; queued work would prevent outage detection",
            ));
        }
        Ok(())
    }
}

/// The watchdog authority: one job, always on the control plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WatchdogAuthority {
    pub(crate) job_id: String,
    pub(crate) provider: ProviderId,
}

impl WatchdogAuthority {
    /// The single authority for one universe: the control-plane provider.
    /// Any other provider is a hard error at construction time, not a
    /// runtime fallback.
    #[must_use]
    pub(crate) fn for_universe(universe: &ProviderSet) -> Self {
        Self {
            job_id: WATCHDOG_JOB_ID.to_owned(),
            provider: crate::s2::provider::control_plane_provider(universe),
        }
    }
}

/// Spec §8 operating targets in seconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_field_names,
    reason = "the _secs suffix on every field is the units contract, not stutter"
)]
pub(crate) struct WatchdogDeadlines {
    /// Reserve-to-connected: 180s.
    pub(crate) reserve_to_connected_secs: u64,
    /// Owned cleanup: 120s.
    pub(crate) owned_cleanup_secs: u64,
    /// Free-capacity provisioning stall diagnosed: 5m.
    pub(crate) stall_diagnosis_secs: u64,
    /// Full local outage reflected as failed/incomplete: 10m.
    pub(crate) outage_reflection_secs: u64,
}

impl WatchdogDeadlines {
    /// The spec §8 targets.
    #[must_use]
    pub(crate) fn spec() -> Self {
        Self {
            reserve_to_connected_secs: 180,
            owned_cleanup_secs: 120,
            stall_diagnosis_secs: 300,
            outage_reflection_secs: 600,
        }
    }
}

/// A health record is stale past this age: missing data means unavailable, not
/// backlog. One cleanup deadline keeps staleness well inside outage reflection.
pub(crate) const HEALTH_FRESHNESS_SECS: u64 = 120;
/// Future timestamps past this skew are rejected, so a far-future replay
/// cannot pass as fresh.
pub(crate) const HEALTH_CLOCK_SKEW_SECS: u64 = 60;

/// Authenticated outbound Velnor health, bound to repository/source/run/
/// attempt/provider/sequence/freshness/permits/progress.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HealthRecord {
    pub(crate) repository_id: String,
    pub(crate) source_sha: String,
    pub(crate) run_id: String,
    pub(crate) run_attempt: String,
    pub(crate) provider: ProviderId,
    pub(crate) sequence: u64,
    pub(crate) observed_at_secs: u64,
    pub(crate) occupied_permits: u32,
    pub(crate) provisioning_progress: String,
    /// HMAC-SHA256 over the canonical bytes, keyed by the telemetry writer
    /// key. The watchdog verifies; it never mints.
    pub(crate) tag: Vec<u8>,
}

impl HealthRecord {
    /// The canonical bytes the tag covers: every bound field, length-framed.
    #[must_use]
    pub(crate) fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for field in [
            self.repository_id.as_str(),
            self.source_sha.as_str(),
            self.run_id.as_str(),
            self.run_attempt.as_str(),
            self.provider.as_str(),
            self.provisioning_progress.as_str(),
        ] {
            bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
            bytes.extend_from_slice(field.as_bytes());
        }
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.extend_from_slice(&self.observed_at_secs.to_be_bytes());
        bytes.extend_from_slice(&self.occupied_permits.to_be_bytes());
        // The tag itself is never covered: it is empty at signing time and
        // populated at verification time, so covering it would break every
        // verification.
        bytes
    }
}

/// HMAC-SHA256 over `sha2`: the telemetry writer key never leaves the writer
/// and the watchdog's verification key.
#[must_use]
pub(crate) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0_u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        key_block[..32].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36_u8; BLOCK];
    let mut opad = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        ipad[index] ^= key_block[index];
        opad[index] ^= key_block[index];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    outer.finalize().into()
}

fn tag_matches(expected: &[u8; 32], presented: &[u8]) -> bool {
    if presented.len() != expected.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (left, right) in expected.iter().zip(presented.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

/// Why a health record was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HealthRejection {
    BadSignature,
    Stale { age_secs: u64 },
    FutureDated { ahead_secs: u64 },
    IdentityMismatch { reason: String },
}

/// The identity the watchdog binds a record to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HealthIdentity {
    pub(crate) repository_id: String,
    pub(crate) source_sha: String,
    pub(crate) run_id: String,
    pub(crate) run_attempt: String,
}

/// Validate identity, authenticity, and freshness. Any rejection means the
/// provider is unavailable for this run/attempt — never ordinary backlog.
pub(crate) fn validate_health(
    record: &HealthRecord,
    expected: &HealthIdentity,
    verification_key: &[u8],
    now_secs: u64,
) -> Result<(), HealthRejection> {
    if record.repository_id != expected.repository_id
        || record.source_sha != expected.source_sha
        || record.run_id != expected.run_id
        || record.run_attempt != expected.run_attempt
    {
        return Err(HealthRejection::IdentityMismatch {
            reason: "repository, sha, run, or attempt does not match this run".to_owned(),
        });
    }
    let expected_tag = hmac_sha256(verification_key, &record.canonical_bytes());
    if !tag_matches(&expected_tag, &record.tag) {
        return Err(HealthRejection::BadSignature);
    }
    if record.observed_at_secs > now_secs.saturating_add(HEALTH_CLOCK_SKEW_SECS) {
        return Err(HealthRejection::FutureDated {
            ahead_secs: record.observed_at_secs.saturating_sub(now_secs),
        });
    }
    let age = now_secs.saturating_sub(record.observed_at_secs);
    if age > HEALTH_FRESHNESS_SECS {
        return Err(HealthRejection::Stale { age_secs: age });
    }
    Ok(())
}

/// Which engine executed the work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EngineKind {
    OfficialRunner,
    Native,
}

/// Engine identity: kind, versions, and image digests — never a hostname.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EngineIdentity {
    pub(crate) kind: EngineKind,
    pub(crate) version: String,
    pub(crate) runner_digest: String,
    pub(crate) dind_digest: Option<String>,
}

/// Selected tests and `JUnit` counts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TestReport {
    pub(crate) selected: u64,
    pub(crate) passed: u64,
    pub(crate) failed: u64,
    pub(crate) junit_artifact: String,
}

/// Cache and timing evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CacheTimingReport {
    pub(crate) cache_hit: bool,
    pub(crate) restore_secs: u64,
    pub(crate) test_secs: u64,
    pub(crate) cleanup_secs: u64,
}

/// Cleanup evidence: owned resources removed, permit released exactly once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CleanupReceipt {
    pub(crate) owned_removed: u64,
    pub(crate) permit_released: bool,
}

/// Where correlation artifacts live. There is deliberately no container-local
/// variant: artifacts are preserved outside disposable containers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactLocation {
    ActionsArtifact { name: String },
    BlobStore { uri: String },
}

/// Full correlation: runner/job metadata × provisioning IDs × engine versions
/// × digests × tests/JUnit × cache/timing × cleanup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CorrelationRecord {
    pub(crate) github_job_id: String,
    pub(crate) provisioning_id: String,
    pub(crate) engine: EngineIdentity,
    pub(crate) image_digest: String,
    pub(crate) tests: TestReport,
    pub(crate) cache_timing: CacheTimingReport,
    pub(crate) cleanup: CleanupReceipt,
    pub(crate) artifacts: Vec<ArtifactLocation>,
}

/// Require every correlation piece: names each missing one.
///
/// # Errors
/// Returns a usage error naming the missing correlation pieces.
pub(crate) fn require_complete_correlation(
    record: &CorrelationRecord,
) -> Result<(), GeneratorError> {
    let mut missing = Vec::new();
    if record.github_job_id.is_empty() {
        missing.push("github job id");
    }
    if record.provisioning_id.is_empty() {
        missing.push("provisioning id");
    }
    if record.engine.version.is_empty() {
        missing.push("engine version");
    }
    if record.engine.runner_digest.is_empty() {
        missing.push("runner digest");
    }
    if record.image_digest.is_empty() {
        missing.push("image digest");
    }
    if record.tests.junit_artifact.is_empty() {
        missing.push("junit artifact");
    }
    if !record.cleanup.permit_released {
        missing.push("permit release");
    }
    if record.artifacts.is_empty() {
        missing.push("durable artifacts");
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(GeneratorError::usage(format!(
        "correlation is missing: {}; partial evidence never certifies a lane",
        missing.join(", ")
    )))
}

/// Collect every API page through `fetch`. `fetch(cursor)` returns the page
/// items and the next cursor; `None` ends the walk. A repeated cursor is a
/// fail-closed error, never an infinite walk.
///
/// # Errors
/// Returns a usage error when the API repeats a cursor.
pub(crate) fn collect_all_pages<T>(
    mut fetch: impl FnMut(Option<&str>) -> (Vec<T>, Option<String>),
) -> Result<Vec<T>, GeneratorError> {
    let mut items = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cursor: Option<String> = None;
    loop {
        let (page, next) = fetch(cursor.as_deref());
        items.extend(page);
        let Some(next_cursor) = next else {
            return Ok(items);
        };
        if !seen.insert(next_cursor.clone()) {
            return Err(GeneratorError::usage(format!(
                "paginated API repeated cursor `{next_cursor}`; refusing to loop forever"
            )));
        }
        cursor = Some(next_cursor);
    }
}

/// The required outcome for one run/attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RequiredOutcome {
    Pending,
    Success,
    Failed { reason: String },
}

/// The required-result aggregate, keyed by (run, attempt): attempts are
/// distinguished, and a later reporter or cancellation must not overwrite an
/// already failed required result with success.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RequiredAggregate {
    outcomes: BTreeMap<(String, String), RequiredOutcome>,
}

impl RequiredAggregate {
    /// Report an outcome. A `Failed` outcome sticks: later `Success` or
    /// `Pending` reports for the same run/attempt are ignored. A later
    /// `Failed` replaces the reason but never clears the failure.
    pub(crate) fn report(&mut self, run_id: &str, attempt: &str, outcome: RequiredOutcome) {
        let key = (run_id.to_owned(), attempt.to_owned());
        match (self.outcomes.get(&key), &outcome) {
            (Some(RequiredOutcome::Failed { .. }), RequiredOutcome::Success)
            | (Some(RequiredOutcome::Failed { .. }), RequiredOutcome::Pending) => {}
            _ => {
                self.outcomes.insert(key, outcome);
            }
        }
    }

    #[must_use]
    pub(crate) fn outcome_for(&self, run_id: &str, attempt: &str) -> Option<&RequiredOutcome> {
        self.outcomes.get(&(run_id.to_owned(), attempt.to_owned()))
    }
}

/// The identity a rerun must revalidate: same repository, source, run, and
/// plan; only the attempt advances.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RerunIdentity {
    pub(crate) repository_id: String,
    pub(crate) source_sha: String,
    pub(crate) run_id: String,
    pub(crate) plan_digest: String,
    pub(crate) attempt: String,
}

/// Revalidate a rerun against the previous attempt's identity and provenance.
///
/// # Errors
/// Returns a usage error when anything but the attempt changed, or when the
/// attempt did not advance (a duplicate report, not a rerun).
pub(crate) fn revalidate_rerun(
    previous: &RerunIdentity,
    candidate: &RerunIdentity,
) -> Result<(), GeneratorError> {
    if previous.repository_id != candidate.repository_id
        || previous.source_sha != candidate.source_sha
        || previous.run_id != candidate.run_id
        || previous.plan_digest != candidate.plan_digest
    {
        return Err(GeneratorError::usage(
            "rerun identity does not match the previous attempt; reruns revalidate exact identity and outcome provenance",
        ));
    }
    if previous.attempt == candidate.attempt {
        return Err(GeneratorError::usage(
            "rerun attempt did not advance; a duplicate report is not a rerun",
        ));
    }
    Ok(())
}

/// One outage finding with its measured detection latency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WatchdogFinding {
    ReserveBreach {
        reservation: String,
        waited_secs: u64,
    },
    CleanupBreach {
        job: String,
        waited_secs: u64,
    },
    ProvisioningStall {
        provider: ProviderId,
        idle_secs: u64,
    },
    ProviderOutage {
        provider: ProviderId,
        silent_secs: u64,
    },
}

impl WatchdogFinding {
    /// Seconds from the fault's start to this detection.
    #[must_use]
    pub(crate) fn detection_latency_secs(&self) -> u64 {
        match self {
            Self::ReserveBreach { waited_secs, .. }
            | Self::CleanupBreach { waited_secs, .. }
            | Self::ProvisioningStall {
                idle_secs: waited_secs,
                ..
            }
            | Self::ProviderOutage {
                silent_secs: waited_secs,
                ..
            } => *waited_secs,
        }
    }
}

/// Outage detector over an injected clock: observations in, deadline findings
/// out. All times are unix seconds supplied by the caller, so tests measure
/// exact detection latencies.
#[derive(Clone, Debug)]
pub(crate) struct OutageDetector {
    deadlines: WatchdogDeadlines,
    reservations: BTreeMap<String, u64>,
    cleanups: BTreeMap<String, u64>,
    last_progress: BTreeMap<ProviderId, u64>,
    last_health: BTreeMap<ProviderId, u64>,
    pending: BTreeSet<ProviderId>,
}

impl OutageDetector {
    /// A detector with the spec §8 deadlines.
    #[must_use]
    pub(crate) fn spec() -> Self {
        Self {
            deadlines: WatchdogDeadlines::spec(),
            reservations: BTreeMap::new(),
            cleanups: BTreeMap::new(),
            last_progress: BTreeMap::new(),
            last_health: BTreeMap::new(),
            pending: BTreeSet::new(),
        }
    }

    /// Capacity was committed for `reservation` at `at_secs`.
    pub(crate) fn observe_reservation(&mut self, reservation: &str, at_secs: u64) {
        self.reservations.insert(reservation.to_owned(), at_secs);
    }

    /// `reservation` connected; it leaves the reserve watch.
    pub(crate) fn observe_connected(&mut self, reservation: &str) {
        self.reservations.remove(reservation);
    }

    /// `job` reached terminal state at `at_secs`; owned cleanup is now due.
    pub(crate) fn observe_terminal(&mut self, job: &str, at_secs: u64) {
        self.cleanups.insert(job.to_owned(), at_secs);
    }

    /// `job`'s owned cleanup completed with its receipt.
    pub(crate) fn observe_cleaned(&mut self, job: &str) {
        self.cleanups.remove(job);
    }

    /// Provisioning progress on `provider` at `at_secs`.
    pub(crate) fn observe_progress(&mut self, provider: ProviderId, at_secs: u64) {
        self.last_progress.insert(provider, at_secs);
    }

    /// Fresh validated health from `provider` at `at_secs`.
    pub(crate) fn observe_health(&mut self, provider: ProviderId, at_secs: u64) {
        self.last_health.insert(provider, at_secs);
    }

    /// Whether `provider` holds admitted, unblocked demand. Legitimate FIFO
    /// backlog and unmet workflow dependencies are not pending demand, so
    /// they never read as a provisioning stall.
    pub(crate) fn set_pending(&mut self, provider: ProviderId, pending: bool) {
        if pending {
            self.pending.insert(provider);
        } else {
            self.pending.remove(&provider);
        }
    }

    /// Check every watch at `now_secs`. Findings are sorted for stable output.
    #[must_use]
    pub(crate) fn check(&self, now_secs: u64) -> Vec<WatchdogFinding> {
        let mut findings = Vec::new();
        for (reservation, reserved_at) in &self.reservations {
            let waited = now_secs.saturating_sub(*reserved_at);
            if waited > self.deadlines.reserve_to_connected_secs {
                findings.push(WatchdogFinding::ReserveBreach {
                    reservation: reservation.clone(),
                    waited_secs: waited,
                });
            }
        }
        for (job, terminal_at) in &self.cleanups {
            let waited = now_secs.saturating_sub(*terminal_at);
            if waited > self.deadlines.owned_cleanup_secs {
                findings.push(WatchdogFinding::CleanupBreach {
                    job: job.clone(),
                    waited_secs: waited,
                });
            }
        }
        for provider in &self.pending {
            let idle = self
                .last_progress
                .get(provider)
                .map_or(u64::MAX, |at| now_secs.saturating_sub(*at));
            if idle > self.deadlines.stall_diagnosis_secs {
                findings.push(WatchdogFinding::ProvisioningStall {
                    provider: *provider,
                    idle_secs: idle,
                });
            }
        }
        for provider in ProviderId::LOCAL {
            let silent = self
                .last_health
                .get(&provider)
                .map_or(u64::MAX, |at| now_secs.saturating_sub(*at));
            if silent > self.deadlines.outage_reflection_secs {
                findings.push(WatchdogFinding::ProviderOutage {
                    provider,
                    silent_secs: silent,
                });
            }
        }
        findings.sort_by_key(WatchdogFinding::detection_latency_secs);
        findings
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "a test whose setup fails should panic loudly"
    )]
    #![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

    use super::*;

    const KEY: &[u8] = b"telemetry-writer-key";
    const WRONG_KEY: &[u8] = b"some-other-key";

    fn healthy_record() -> HealthRecord {
        let mut record = HealthRecord {
            repository_id: "123".to_owned(),
            source_sha: "abc".to_owned(),
            run_id: "42".to_owned(),
            run_attempt: "1".to_owned(),
            provider: ProviderId::Velnor,
            sequence: 7,
            observed_at_secs: 1_000,
            occupied_permits: 3,
            provisioning_progress: "2 acquiring, 1 running".to_owned(),
            tag: Vec::new(),
        };
        record.tag = hmac_sha256(KEY, &record.canonical_bytes()).to_vec();
        record
    }

    fn identity() -> HealthIdentity {
        HealthIdentity {
            repository_id: "123".to_owned(),
            source_sha: "abc".to_owned(),
            run_id: "42".to_owned(),
            run_attempt: "1".to_owned(),
        }
    }

    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn watchdog_excludes_itself_and_telemetry_only() {
        assert!(!is_workload_job(WATCHDOG_JOB_ID));
        assert!(!is_workload_job(TELEMETRY_JOB_ID));
        assert!(is_workload_job("velnor-rust-widget"));
        assert!(is_workload_job("github-hosted-rust-widget"));
    }

    #[test]
    fn watchdog_authority_is_the_one_control_plane_job() {
        let hosted = ProviderSet::from([ProviderId::GithubHosted]);
        let authority = WatchdogAuthority::for_universe(&hosted);
        assert_eq!(authority.job_id, WATCHDOG_JOB_ID);
        assert_eq!(authority.provider, ProviderId::GithubHosted);
        let local = ProviderSet::from([ProviderId::Velnor]);
        let authority = WatchdogAuthority::for_universe(&local);
        assert_eq!(authority.job_id, WATCHDOG_JOB_ID);
        assert_eq!(authority.provider, ProviderId::Velnor);
    }

    #[test]
    fn watchdog_ordering_rejects_needs_on_local() {
        WatchdogOrdering::require_spec(WatchdogOrdering::spec()).unwrap();
        let error = must_fail(
            WatchdogOrdering::require_spec(WatchdogOrdering {
                after_planning: true,
                needs_local_jobs: true,
            }),
            "needs on local jobs",
        );
        assert!(error.contains("must not wait"), "{error}");
        let error = must_fail(
            WatchdogOrdering::require_spec(WatchdogOrdering {
                after_planning: false,
                needs_local_jobs: false,
            }),
            "watchdog before planning",
        );
        assert!(error.contains("after planning"), "{error}");
    }

    #[test]
    fn fresh_authenticated_health_validates() {
        let record = healthy_record();
        assert_eq!(validate_health(&record, &identity(), KEY, 1_000), Ok(()));
        assert_eq!(
            validate_health(&record, &identity(), KEY, 1_000 + HEALTH_FRESHNESS_SECS),
            Ok(())
        );
    }

    #[test]
    fn forged_health_is_rejected() {
        let record = healthy_record();
        assert_eq!(
            validate_health(&record, &identity(), WRONG_KEY, 1_000),
            Err(HealthRejection::BadSignature)
        );
        let mut tampered = healthy_record();
        tampered.occupied_permits = 0;
        assert_eq!(
            validate_health(&tampered, &identity(), KEY, 1_000),
            Err(HealthRejection::BadSignature)
        );
    }

    #[test]
    fn stale_health_means_unavailable() {
        let record = healthy_record();
        assert_eq!(
            validate_health(&record, &identity(), KEY, 1_000 + HEALTH_FRESHNESS_SECS + 1),
            Err(HealthRejection::Stale {
                age_secs: HEALTH_FRESHNESS_SECS + 1
            })
        );
    }

    #[test]
    fn future_dated_health_is_rejected() {
        let record = healthy_record();
        assert_eq!(
            validate_health(
                &record,
                &identity(),
                KEY,
                1_000 - HEALTH_CLOCK_SKEW_SECS - 1
            ),
            Err(HealthRejection::FutureDated {
                ahead_secs: HEALTH_CLOCK_SKEW_SECS + 1
            })
        );
    }

    #[test]
    fn wrong_run_identity_is_rejected() {
        let mut record = healthy_record();
        record.run_attempt = "2".to_owned();
        record.tag = hmac_sha256(KEY, &record.canonical_bytes()).to_vec();
        assert!(matches!(
            validate_health(&record, &identity(), KEY, 1_000),
            Err(HealthRejection::IdentityMismatch { .. })
        ));
    }

    fn correlated() -> CorrelationRecord {
        CorrelationRecord {
            github_job_id: "987".to_owned(),
            provisioning_id: "prov-1".to_owned(),
            engine: EngineIdentity {
                kind: EngineKind::Native,
                version: "velnor-runner 0.1.0".to_owned(),
                runner_digest: "sha256:aaa".to_owned(),
                dind_digest: None,
            },
            image_digest: "sha256:bbb".to_owned(),
            tests: TestReport {
                selected: 120,
                passed: 120,
                failed: 0,
                junit_artifact: "junit-rust-a.xml".to_owned(),
            },
            cache_timing: CacheTimingReport {
                cache_hit: true,
                restore_secs: 4,
                test_secs: 90,
                cleanup_secs: 8,
            },
            cleanup: CleanupReceipt {
                owned_removed: 14,
                permit_released: true,
            },
            artifacts: vec![ArtifactLocation::ActionsArtifact {
                name: "evidence-rust-a".to_owned(),
            }],
        }
    }

    #[test]
    fn complete_correlation_passes_and_each_gap_fails() {
        require_complete_correlation(&correlated()).unwrap();
        let mut missing_job = correlated();
        missing_job.github_job_id.clear();
        let error = must_fail(
            require_complete_correlation(&missing_job),
            "missing github job id",
        );
        assert!(error.contains("github job id"), "{error}");
        let mut missing_permit = correlated();
        missing_permit.cleanup.permit_released = false;
        let error = must_fail(
            require_complete_correlation(&missing_permit),
            "missing permit release",
        );
        assert!(error.contains("permit release"), "{error}");
        let mut missing_artifacts = correlated();
        missing_artifacts.artifacts.clear();
        let error = must_fail(
            require_complete_correlation(&missing_artifacts),
            "missing artifacts",
        );
        assert!(error.contains("durable artifacts"), "{error}");
    }

    #[test]
    fn pagination_collects_every_page() {
        let pages: Vec<Vec<u32>> = vec![vec![1, 2], vec![3], vec![]];
        let mut calls = 0_usize;
        let items = collect_all_pages(|cursor| {
            calls += 1;
            let index = cursor.map_or(0, |value| value.parse::<usize>().unwrap());
            let next = if index + 1 < pages.len() {
                Some((index + 1).to_string())
            } else {
                None
            };
            (pages[index].clone(), next)
        })
        .unwrap();
        assert_eq!(items, vec![1, 2, 3]);
        assert_eq!(calls, 3);
    }

    #[test]
    fn repeated_cursor_fails_closed() {
        let error = must_fail(
            collect_all_pages(|cursor: Option<&str>| {
                let next = cursor.map_or_else(|| "loop".to_owned(), str::to_owned);
                (vec![1_u32], Some(next))
            }),
            "repeated cursor",
        );
        assert!(error.contains("repeated cursor"), "{error}");
    }

    #[test]
    fn failure_sticks_and_attempts_stay_distinct() {
        let mut aggregate = RequiredAggregate::default();
        aggregate.report("42", "1", RequiredOutcome::Pending);
        aggregate.report(
            "42",
            "1",
            RequiredOutcome::Failed {
                reason: "velnor lane missing".to_owned(),
            },
        );
        // A later success or cancellation must not overwrite the failure.
        aggregate.report("42", "1", RequiredOutcome::Success);
        aggregate.report("42", "1", RequiredOutcome::Pending);
        assert!(matches!(
            aggregate.outcome_for("42", "1"),
            Some(RequiredOutcome::Failed { .. })
        ));
        // Attempt 2 is a distinct outcome, not an overwrite of attempt 1.
        aggregate.report("42", "2", RequiredOutcome::Success);
        assert_eq!(
            aggregate.outcome_for("42", "2"),
            Some(&RequiredOutcome::Success)
        );
        assert!(matches!(
            aggregate.outcome_for("42", "1"),
            Some(RequiredOutcome::Failed { .. })
        ));
    }

    #[test]
    fn reruns_revalidate_identity_and_advance_the_attempt() {
        let previous = RerunIdentity {
            repository_id: "123".to_owned(),
            source_sha: "abc".to_owned(),
            run_id: "42".to_owned(),
            plan_digest: "plan".to_owned(),
            attempt: "1".to_owned(),
        };
        let rerun = RerunIdentity {
            attempt: "2".to_owned(),
            ..previous.clone()
        };
        revalidate_rerun(&previous, &rerun).unwrap();
        let error = must_fail(
            revalidate_rerun(&previous, &previous),
            "duplicate report as rerun",
        );
        assert!(error.contains("did not advance"), "{error}");
        let drifted = RerunIdentity {
            source_sha: "def".to_owned(),
            attempt: "2".to_owned(),
            ..previous.clone()
        };
        let error = must_fail(
            revalidate_rerun(&previous, &drifted),
            "rerun with drifted source",
        );
        assert!(error.contains("does not match"), "{error}");
    }

    fn quiet_detector(at_secs: u64) -> OutageDetector {
        let mut detector = OutageDetector::spec();
        // Both local providers are freshly healthy and idle: no findings.
        detector.observe_health(ProviderId::GithubSelfHosted, at_secs);
        detector.observe_health(ProviderId::Velnor, at_secs);
        detector
    }

    #[test]
    fn healthy_fleet_has_no_findings() {
        let detector = quiet_detector(1_000);
        assert!(detector.check(1_000).is_empty());
    }

    #[test]
    fn reserve_breach_detects_against_180s_with_measured_latency() {
        let mut detector = quiet_detector(1_000);
        detector.observe_reservation("res-1", 1_000);
        assert!(detector.check(1_000 + 180).is_empty());
        let findings = detector.check(1_000 + 181);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            WatchdogFinding::ReserveBreach {
                reservation: "res-1".to_owned(),
                waited_secs: 181,
            }
        );
        assert!(findings[0].detection_latency_secs() <= 180 + 1);
        detector.observe_connected("res-1");
        let findings = detector.check(1_000 + 182);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn cleanup_breach_detects_against_120s_with_measured_latency() {
        let mut detector = quiet_detector(2_000);
        detector.observe_terminal("job-1", 2_000);
        assert!(detector.check(2_000 + 120).is_empty());
        let findings = detector.check(2_000 + 121);
        assert_eq!(
            findings,
            vec![WatchdogFinding::CleanupBreach {
                job: "job-1".to_owned(),
                waited_secs: 121,
            }]
        );
        assert!(findings[0].detection_latency_secs() <= 120 + 1);
    }

    #[test]
    fn stall_diagnoses_against_5m_for_pending_demand_only() {
        let mut detector = quiet_detector(3_000);
        detector.set_pending(ProviderId::Velnor, true);
        detector.observe_progress(ProviderId::Velnor, 3_000);
        assert!(detector.check(3_000 + 300).is_empty());
        let findings = detector.check(3_000 + 301);
        assert_eq!(
            findings,
            vec![WatchdogFinding::ProvisioningStall {
                provider: ProviderId::Velnor,
                idle_secs: 301,
            }]
        );
        assert!(findings[0].detection_latency_secs() <= 300 + 1);
        // Idle without pending demand is backlog, not a stall.
        let idle = quiet_detector(3_000);
        assert!(idle.check(3_000 + 301).is_empty());
    }

    #[test]
    fn full_outage_reflects_within_10m_with_measured_silence() {
        let mut detector = quiet_detector(4_000);
        detector.observe_health(ProviderId::GithubSelfHosted, 4_000 + 601);
        let findings = detector.check(4_000 + 601);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0],
            WatchdogFinding::ProviderOutage {
                provider: ProviderId::Velnor,
                silent_secs: 601,
            }
        );
        assert!(findings[0].detection_latency_secs() <= 600 + 1);
        // At exactly the deadline the provider is still merely quiet.
        let detector = quiet_detector(4_000);
        assert!(detector.check(4_000 + 600).is_empty());
    }
}

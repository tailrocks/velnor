//! Acceleration policy: the runtime half of execution.toml's `[acceleration]`
//! table plus the degradation ledger (workstreams 3 + 9 foundation).
//!
//! The policy is the single operator dial for Velnor's performance surface and
//! its default is maximum. The only runtime overrides are the
//! `VELNOR_ACCELERATION_*` env vars below — explicit diagnostic/emergency
//! levers, never configuration — and every override that changes the effective
//! policy emits a [`DegradationRecord`] so a slower fleet is always
//! explainable, never silent (AGENTS.md modern-first gate).

use serde::Serialize;
use std::path::Path;
use velnor_model::{
    AccelerationMode, AccelerationSection, ExecutionConfigError, ExecutionFile,
    NativeActionsChoice, ResultCacheChoice, TargetPersistenceChoice,
};

use crate::compiler_cache::CompilerCacheBackend;

/// Emergency override for `[acceleration] target_persistence`
/// (`auto | on | off`). Any value that changes the policy is recorded as a
/// [`DegradationRecord`]; an invalid value keeps the configured policy and is
/// recorded, never silently ignored.
pub const TARGET_PERSISTENCE_OVERRIDE_ENV: &str = "VELNOR_ACCELERATION_TARGET_PERSISTENCE";

/// Emergency override for `[acceleration] compiler_cache`
/// (`auto | kache | sccache | off`), same contract as
/// [`TARGET_PERSISTENCE_OVERRIDE_ENV`].
pub const COMPILER_CACHE_OVERRIDE_ENV: &str = "VELNOR_ACCELERATION_COMPILER_CACHE";

/// Effective acceleration policy for one job: the parsed `[acceleration]`
/// section plus env overrides, with one [`DegradationRecord`] per override
/// that deviated from the configured value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccelerationPolicy {
    pub mode: AccelerationMode,
    pub target_persistence: TargetPersistenceChoice,
    /// [`CompilerCacheBackend::Auto`] defers to per-job resolution; it is a
    /// policy value and never a resolved backend.
    pub compiler_cache: CompilerCacheBackend,
    pub typed_stores: bool,
    pub singleflight: bool,
    pub prefetch: bool,
    pub cache_aware_scheduling: bool,
    pub native_actions: NativeActionsChoice,
    pub buildkit_local: bool,
    pub result_cache: ResultCacheChoice,
    /// Env-override records; they travel with every backend resolution so the
    /// job acceleration report can explain the whole gap from maximum.
    pub degradations: Vec<DegradationRecord>,
}

impl Default for AccelerationPolicy {
    fn default() -> Self {
        Self::maximum()
    }
}

impl AccelerationPolicy {
    /// The maximum policy — identical to an execution.toml without an
    /// `[acceleration]` section. Also the audit stance of
    /// `velnorctl capabilities check`.
    #[must_use]
    pub fn maximum() -> Self {
        Self {
            mode: AccelerationMode::Maximum,
            target_persistence: TargetPersistenceChoice::Auto,
            compiler_cache: CompilerCacheBackend::Auto,
            typed_stores: true,
            singleflight: true,
            prefetch: true,
            cache_aware_scheduling: true,
            native_actions: NativeActionsChoice::Prefer,
            buildkit_local: true,
            result_cache: ResultCacheChoice::HermeticOnly,
            degradations: Vec::new(),
        }
    }

    /// Policy from a parsed `[acceleration]` section, no env overrides.
    #[must_use]
    pub fn from_section(section: &AccelerationSection) -> Self {
        Self {
            mode: section.mode,
            target_persistence: section.target_persistence,
            compiler_cache: match section.compiler_cache {
                velnor_model::CompilerCacheChoice::Auto => CompilerCacheBackend::Auto,
                velnor_model::CompilerCacheChoice::Kache => CompilerCacheBackend::Kache,
                velnor_model::CompilerCacheChoice::Sccache => CompilerCacheBackend::Sccache,
                velnor_model::CompilerCacheChoice::Off => CompilerCacheBackend::Off,
            },
            typed_stores: section.typed_stores,
            singleflight: section.singleflight,
            prefetch: section.prefetch,
            cache_aware_scheduling: section.cache_aware_scheduling,
            native_actions: section.native_actions,
            buildkit_local: section.buildkit_local,
            result_cache: section.result_cache,
            degradations: Vec::new(),
        }
    }

    /// Policy for one job: the execution file's `[acceleration]` section with
    /// env overrides applied. Missing file still fails closed through
    /// [`crate::execution::load_execution_file`].
    #[must_use]
    pub fn from_file(file: &ExecutionFile) -> Self {
        let mut policy = Self::from_section(file.acceleration());
        policy.apply_overrides(
            std::env::var(TARGET_PERSISTENCE_OVERRIDE_ENV).ok(),
            std::env::var(COMPILER_CACHE_OVERRIDE_ENV).ok(),
        );
        policy
    }

    /// Load the policy from a daemon config dir (`execution.toml`).
    ///
    /// # Errors
    /// [`ExecutionConfigError`] when execution.toml is missing or invalid.
    pub fn load(config_dir: &Path) -> Result<Self, ExecutionConfigError> {
        Ok(Self::from_file(&crate::execution::load_execution_file(
            config_dir, None,
        )?))
    }

    /// Apply emergency env overrides. A value that changes the policy is
    /// recorded as a degradation; the same value as configured is a no-op; an
    /// unparseable value keeps the configured policy and is recorded as an
    /// ignored override — an emergency lever must never fail silently in
    /// either direction.
    pub(crate) fn apply_overrides(
        &mut self,
        target_persistence: Option<String>,
        compiler_cache: Option<String>,
    ) {
        if let Some(raw) = target_persistence {
            let requested = raw.trim().to_ascii_lowercase();
            let parsed = match requested.as_str() {
                "auto" => Some(TargetPersistenceChoice::Auto),
                "on" => Some(TargetPersistenceChoice::On),
                "off" => Some(TargetPersistenceChoice::Off),
                _ => None,
            };
            match parsed {
                Some(choice) if choice != self.target_persistence => {
                    self.degradations.push(DegradationRecord::env_override(
                        "acceleration.target_persistence",
                        TARGET_PERSISTENCE_OVERRIDE_ENV,
                        &format!("{:?}", self.target_persistence).to_ascii_lowercase(),
                        &requested,
                    ));
                    self.target_persistence = choice;
                }
                Some(_) => {}
                None => self.degradations.push(DegradationRecord::ignored_override(
                    "acceleration.target_persistence",
                    TARGET_PERSISTENCE_OVERRIDE_ENV,
                    &requested,
                    "auto | on | off",
                )),
            }
        }
        if let Some(raw) = compiler_cache {
            let requested = raw.trim().to_ascii_lowercase();
            let parsed = CompilerCacheBackend::parse_choice(&requested);
            match parsed {
                Some(choice) if choice != self.compiler_cache => {
                    self.degradations.push(DegradationRecord::env_override(
                        "acceleration.compiler_cache",
                        COMPILER_CACHE_OVERRIDE_ENV,
                        self.compiler_cache.as_str(),
                        &requested,
                    ));
                    self.compiler_cache = choice;
                }
                Some(_) => {}
                None => self.degradations.push(DegradationRecord::ignored_override(
                    "acceleration.compiler_cache",
                    COMPILER_CACHE_OVERRIDE_ENV,
                    &requested,
                    "auto | kache | sccache | off",
                )),
            }
        }
    }
}

/// One acceleration feature running below its configured maximum, with enough
/// context to explain the slowdown and restore it. The job acceleration
/// report (later workstream) serializes these verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DegradationRecord {
    /// Policy feature running below maximum, e.g. `acceleration.compiler_cache`.
    pub feature: String,
    /// What is degraded, e.g. `compiler cache disabled`.
    pub reason: String,
    /// The exact trigger, e.g. the env var and its value.
    pub cause: String,
    /// What the slowdown looks like in job timings.
    pub expected_impact: String,
    /// The operator action that restores maximum.
    pub restore_hint: String,
}

impl DegradationRecord {
    /// An emergency env override that changed the configured policy.
    #[must_use]
    pub fn env_override(feature: &str, variable: &str, from: &str, to: &str) -> Self {
        Self {
            feature: feature.to_string(),
            reason: format!("{feature} restricted below the configured policy"),
            cause: format!("{variable}={to} (configured: {from})"),
            expected_impact: "jobs run without the restricted acceleration feature; ".to_string()
                + "compile-heavy work slows toward cold-build times",
            restore_hint: format!("unset {variable} or set it to the configured value {from}"),
        }
    }

    /// An emergency env override whose value is not an accepted choice: the
    /// configured policy stands and the ignored lever stays visible.
    #[must_use]
    pub fn ignored_override(feature: &str, variable: &str, value: &str, accepted: &str) -> Self {
        Self {
            feature: feature.to_string(),
            reason: format!("{feature} override ignored: value not accepted"),
            cause: format!("{variable}={value} (accepted: {accepted})"),
            expected_impact: "none: the configured policy is unchanged".to_string(),
            restore_hint: format!("fix or unset {variable}"),
        }
    }

    /// The compiler cache turned off while the maximum policy expects one.
    #[must_use]
    pub fn compiler_cache_disabled(cause: &str) -> Self {
        Self {
            feature: "acceleration.compiler_cache".to_string(),
            reason: "compiler cache disabled: every Rust job recompiles from cold objects"
                .to_string(),
            cause: cause.to_string(),
            expected_impact: "compile-heavy jobs lose warm-object reuse; full builds ".to_string()
                + "replace incremental hits",
            restore_hint: "restore [acceleration] compiler_cache = \"auto\" (or remove the "
                .to_string()
                + "override) so Rust jobs resolve the kache backend",
        }
    }

    /// Target snapshots turned off while the maximum policy expects them: the
    /// job runs with an ephemeral target directory and rebuilds from scratch.
    #[must_use]
    pub fn target_persistence_disabled(cause: &str) -> Self {
        Self {
            feature: "acceleration.target_persistence".to_string(),
            reason: "target snapshots disabled: the job keeps an ephemeral workspace target "
                .to_string()
                + "and cannot restore or publish warm generations",
            cause: cause.to_string(),
            expected_impact: "Rust jobs rebuild every dependency from source; warm-restore "
                .to_string()
                + "and cross-job reuse of compiled artifacts are lost",
            restore_hint: "restore [acceleration] target_persistence = \"auto\" (or remove the "
                .to_string()
                + "override) so Rust jobs restore compatible snapshots",
        }
    }

    /// The target snapshot store exists by policy but a lifecycle phase could
    /// not use it (`materialize` or `publish`). The job continues cold rather
    /// than failing — a broken cache must cost speed, never correctness — and
    /// the degradation stays visible so the store gets fixed.
    #[must_use]
    pub fn target_store_unusable(phase: &str, detail: &str) -> Self {
        Self {
            feature: "acceleration.target_persistence".to_string(),
            reason: format!("target snapshot store unusable during {phase}: job ran cold"),
            cause: detail.to_string(),
            expected_impact: "the job rebuilt from source and could not publish a warm "
                .to_string()
                + "generation for the next job",
            restore_hint: "inspect the target store permissions, disk space, and locks named "
                .to_string()
                + "in the cause; the next healthy job restores automatically",
        }
    }

    /// Defensive record for a state admission already rejects (e.g. mixed
    /// cache wrappers): resolution degrades rather than guessing.
    #[must_use]
    pub fn unresolved(feature: &str, reason: &str, cause: &str) -> Self {
        Self {
            feature: feature.to_string(),
            reason: reason.to_string(),
            cause: cause.to_string(),
            expected_impact: "no compiler cache engages for this job".to_string(),
            restore_hint: "fix the rejected job shape; admission describes the violation"
                .to_string(),
        }
    }
}

/// Collected [`DegradationRecord`]s for one job. The job execution state
/// carries this so the acceleration report can be emitted from job state
/// without re-deriving decisions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DegradationLog {
    records: Vec<DegradationRecord>,
}

impl DegradationLog {
    /// Record one degradation. Order is append-only set order.
    pub fn record(&mut self, record: DegradationRecord) {
        self.records.push(record);
    }

    /// Merge records from another source (e.g. the policy's env-override
    /// records) preserving their order.
    pub fn extend(&mut self, records: impl IntoIterator<Item = DegradationRecord>) {
        self.records.extend(records);
    }

    /// True when nothing is degraded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The records in append order.
    #[must_use]
    pub fn records(&self) -> &[DegradationRecord] {
        &self.records
    }

    /// JSON body for the job acceleration report. Infallible: the record
    /// shape is plain strings.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "[]".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximum_policy_matches_absent_acceleration_section() {
        let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
        assert_eq!(
            AccelerationPolicy::from_section(file.acceleration()),
            AccelerationPolicy::maximum()
        );
    }

    #[test]
    fn explicit_section_flows_into_the_policy() {
        let file = ExecutionFile::parse_toml(
            "[execution]\nbackend = \"docker\"\n\n[acceleration]\ncompiler_cache = \"sccache\"\ntarget_persistence = \"off\"\n",
        )
        .unwrap();
        let policy = AccelerationPolicy::from_section(file.acceleration());
        assert_eq!(policy.compiler_cache, CompilerCacheBackend::Sccache);
        assert_eq!(policy.target_persistence, TargetPersistenceChoice::Off);
        assert!(policy.prefetch);
        assert!(policy.degradations.is_empty());
    }

    #[test]
    fn env_override_deviating_from_configured_policy_degrades() {
        let mut policy = AccelerationPolicy::maximum();
        policy.apply_overrides(Some("on".to_string()), Some("off".to_string()));
        assert_eq!(policy.target_persistence, TargetPersistenceChoice::On);
        assert_eq!(policy.compiler_cache, CompilerCacheBackend::Off);
        let records = policy.degradations.clone();
        assert_eq!(records.len(), 2, "{records:?}");
        assert!(records[0].cause.contains(TARGET_PERSISTENCE_OVERRIDE_ENV));
        assert!(records[0].cause.contains("auto"));
        assert!(records[0].cause.contains("on"));
        assert!(records[1].cause.contains(COMPILER_CACHE_OVERRIDE_ENV));
        assert!(records[1]
            .restore_hint
            .contains(COMPILER_CACHE_OVERRIDE_ENV));

        let log = DegradationLog {
            records: policy.degradations.clone(),
        };
        assert!(!log.is_empty());
        let json = log.to_json();
        assert!(json.contains("acceleration.compiler_cache"), "{json}");
        assert!(json.contains("expected_impact"), "{json}");
    }

    #[test]
    fn env_override_equal_to_configured_policy_is_noop() {
        let mut policy = AccelerationPolicy::maximum();
        policy.apply_overrides(Some("auto".into()), Some("auto".into()));
        assert!(policy.degradations.is_empty());
    }

    #[test]
    fn invalid_env_override_keeps_policy_and_records() {
        let mut policy = AccelerationPolicy::maximum();
        policy.apply_overrides(Some("sometimes".into()), Some("gha".into()));
        assert_eq!(policy.target_persistence, TargetPersistenceChoice::Auto);
        assert_eq!(policy.compiler_cache, CompilerCacheBackend::Auto);
        let records = &policy.degradations;
        assert_eq!(records.len(), 2, "{records:?}");
        assert!(records[0]
            .reason
            .contains("override ignored: value not accepted"));
        assert!(records[1]
            .cause
            .contains("accepted: auto | kache | sccache | off"));
        assert!(records[1].expected_impact.contains("unchanged"));
    }

    #[test]
    fn compiler_cache_disabled_record_names_the_lever() {
        let record =
            DegradationRecord::compiler_cache_disabled("VELNOR_ACCELERATION_COMPILER_CACHE=off");
        assert!(record.reason.contains("cold"));
        assert!(record.restore_hint.contains("compiler_cache"));
    }

    #[test]
    fn target_persistence_records_name_their_phase() {
        let off = DegradationRecord::target_persistence_disabled(
            "policy [acceleration] target_persistence = \"off\"",
        );
        assert_eq!(off.feature, "acceleration.target_persistence");
        assert!(off.reason.contains("ephemeral"));
        assert!(off.restore_hint.contains("auto"));

        let unusable =
            DegradationRecord::target_store_unusable("materialize", "read-only store dir");
        assert!(unusable.reason.contains("materialize"));
        assert!(unusable.cause.contains("read-only"));
        assert!(unusable.reason.contains("cold"));
    }
}

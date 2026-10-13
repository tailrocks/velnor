//! Lease-safe compiler-cache service.
//!
//! The runner previously exposed only a backend enum. This crate owns the
//! cache boundary: one policy decision selects one namespace, the action
//! journal fences producers, and the CAS stores immutable result envelopes.
//! A partially written or untrusted entry is never returned as a hit.

#![allow(async_fn_in_trait)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use velnor_action_journal::{ActionRecord, JournalError, LeaseManager, LeaseStatus};
use velnor_action_model::{
    canonical_json_bytes, ActionKey, ActionResult, ActionState, Clock, Digest, ProducerLease,
    TrustClass,
};
use velnor_cas::{CasError, CasStore};

/// Compiler-cache metadata schema written beside each backend namespace.
pub const COMPILER_CACHE_SCHEMA_VERSION: u32 = 1;
pub const KACHE_VERSION: &str = "0.14.2";
pub const SCCACHE_VERSION: &str = "0.16.0";
const DEFAULT_LEASE_DURATION_MS: u64 = 30_000;
const DEFAULT_HEARTBEAT_MS: u64 = 10_000;
const DEFAULT_CACHE_QUOTA_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const MAX_OUTPUT_CAPTURE_BYTES: u64 = 5 * 1024 * 1024 * 1024 + 16 * 1024 * 1024;
const QUOTA_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;
const OUTPUT_ROOT_DIGEST: &str = "output_root";
const RESULT_DIGEST: &str = "compiler_cache_result";
const ACCOUNTING_DIGEST: &str = "compiler_cache_physical_byte_accounting";

/// One mutually exclusive compiler-cache implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompilerCacheBackend {
    Sccache,
    Kache,
    Off,
}

impl CompilerCacheBackend {
    fn namespace(self) -> &'static str {
        match self {
            Self::Sccache => "sccache",
            Self::Kache => "kache",
            Self::Off => "off",
        }
    }
}

/// Operator policy. `Auto` selects Kache unless a workflow explicitly asks
/// for sccache as its comparison backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompilerCachePolicy {
    #[default]
    Auto,
    Kache,
    Sccache,
    Off,
}

/// Workflow-declared wrapper facts used at admission time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WrapperDeclaration {
    pub sccache: bool,
    pub kache: bool,
}

/// Resolve one backend without permitting wrapper nesting or silent disable.
pub fn resolve_backend(
    policy: CompilerCachePolicy,
    declaration: &WrapperDeclaration,
) -> Result<CompilerCacheBackend, CacheAdmissionError> {
    if declaration.sccache && declaration.kache {
        return Err(CacheAdmissionError::ConflictingWrappers);
    }
    let policy_backend = match policy {
        CompilerCachePolicy::Auto => None,
        CompilerCachePolicy::Kache => Some(CompilerCacheBackend::Kache),
        CompilerCachePolicy::Sccache => Some(CompilerCacheBackend::Sccache),
        CompilerCachePolicy::Off => Some(CompilerCacheBackend::Off),
    };
    let declared_backend = match (declaration.sccache, declaration.kache) {
        (true, false) => Some(CompilerCacheBackend::Sccache),
        (false, true) => Some(CompilerCacheBackend::Kache),
        (false, false) => None,
        (true, true) => return Err(CacheAdmissionError::ConflictingWrappers),
    };
    let backend = policy_backend
        .or(declared_backend)
        .unwrap_or(CompilerCacheBackend::Kache);
    if let Some(declared) = declared_backend
        && backend != declared
    {
        return Err(CacheAdmissionError::PolicyConflict { policy, declared });
    }
    Ok(backend)
}

/// Exact cache admission failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CacheAdmissionError {
    #[error(
        "compiler-cache backend conflict: sccache and kache wrappers cannot be enabled together"
    )]
    ConflictingWrappers,
    #[error("compiler-cache backend conflict: policy {policy:?} cannot honor declared wrapper {declared:?}")]
    PolicyConflict {
        policy: CompilerCachePolicy,
        declared: CompilerCacheBackend,
    },
    #[error(
        "compiler-cache backend {declared:?} is unsupported for MicroVM: guest transport is not admitted"
    )]
    MicroVmTransportUnavailable { declared: CompilerCacheBackend },
    #[error(
        "compiler-cache environment '{name}' is unsupported for MicroVM: guest transport is not admitted"
    )]
    MicroVmEnvironmentUnsupported { name: String },
}

/// Service configuration. The root must be a daemon-owned persistent path.
#[derive(Debug, Clone)]
pub struct CompilerCacheConfig {
    pub root: PathBuf,
    pub owner: String,
    pub trust_class: TrustClass,
    pub policy: CompilerCachePolicy,
    pub lease_duration_ms: u64,
    pub heartbeat_every_ms: u64,
    /// Durable logical-byte ceiling for the service-owned CAS and metadata.
    pub max_storage_bytes: u64,
}

impl CompilerCacheConfig {
    /// Build a default-policy configuration for one daemon owner.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, owner: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            owner: owner.into(),
            trust_class: TrustClass::Untrusted,
            policy: CompilerCachePolicy::Auto,
            lease_duration_ms: DEFAULT_LEASE_DURATION_MS,
            heartbeat_every_ms: DEFAULT_HEARTBEAT_MS,
            max_storage_bytes: DEFAULT_CACHE_QUOTA_BYTES,
        }
    }

    fn validate(&self) -> Result<(), CacheError> {
        if self.owner.trim().is_empty() {
            return Err(CacheError::InvalidConfig(
                "compiler-cache owner must not be empty".into(),
            ));
        }
        if self.lease_duration_ms == 0
            || self.heartbeat_every_ms == 0
            || self.heartbeat_every_ms >= self.lease_duration_ms
        {
            return Err(CacheError::InvalidConfig(
                "compiler-cache lease duration and heartbeat must be positive and heartbeat must be strictly less than lease duration".into(),
            ));
        }
        if self.max_storage_bytes == 0 {
            return Err(CacheError::InvalidConfig(
                "compiler-cache storage quota must be positive".into(),
            ));
        }
        Ok(())
    }
}

/// Public key alias retained for the compiler-specific service contract.
pub type CompilerActionKey = ActionKey;

/// Public result alias retained for the compiler-specific service contract.
pub type CompilerResult = ActionResult;

pub use velnor_cas::PhysicalByteAccounting;

/// A validated compiler-cache result paired with durable physical-byte
/// evidence for its result-envelope CAS object publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerCacheEntry {
    result: CompilerResult,
    publication_accounting: PhysicalByteAccounting,
}

impl CompilerCacheEntry {
    /// The immutable compiler result.
    #[must_use]
    pub fn result(&self) -> &CompilerResult {
        &self.result
    }

    /// Physical-byte evidence for the result-envelope CAS object publication.
    #[must_use]
    pub const fn publication_accounting(&self) -> PhysicalByteAccounting {
        self.publication_accounting
    }
}

/// Backend-neutral compiler-cache contract.
pub trait CompilerCache: Send + Sync {
    /// Return a fully validated immutable result, or `None` for a clean miss.
    async fn lookup(&self, key: &CompilerActionKey) -> Result<Option<CompilerResult>, CacheError>;

    /// Fence one producer for a cache key.
    async fn begin(&self, key: &CompilerActionKey) -> Result<ProducerLease, CacheError>;

    /// Renew one producer lease before its persisted deadline.
    async fn renew(&self, lease: &mut ProducerLease) -> Result<(), CacheError>;

    /// Abandon one producer lease so it cannot publish a result.
    async fn abandon(&self, lease: &ProducerLease) -> Result<(), CacheError>;

    /// Publish one result under its still-valid producer lease.
    async fn publish(&self, lease: ProducerLease, result: CompilerResult)
        -> Result<(), CacheError>;
}

/// Environment owned by the selected backend. Callers must apply this whole
/// map for one invocation; the service never emits two wrappers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerCacheEnvironment {
    pub variables: BTreeMap<String, String>,
}

/// Runtime mount and environment contract for one compiler-cache backend.
///
/// The runner may choose where the host-side store lives, but it cannot invent
/// a second wrapper/environment mapping. Keeping this descriptor in the
/// service crate makes the mount and compiler-visible variables one atomic
/// backend contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerCacheRuntime {
    backend: CompilerCacheBackend,
    host_path: Option<PathBuf>,
    container_path: Option<&'static str>,
    environment: CompilerCacheEnvironment,
}

impl CompilerCacheRuntime {
    /// Construct the daemon-owned runtime contract for an enabled host-side
    /// store. Disabled caching uses [`Self::off`], so an enabled backend can
    /// never carry wrapper variables without a mount path.
    #[must_use]
    pub fn new(backend: CompilerCacheBackend, host_path: PathBuf) -> Self {
        if backend == CompilerCacheBackend::Off {
            return Self::off();
        }
        let (container_path, variables) = match backend {
            CompilerCacheBackend::Kache => (
                Some("/var/cache/kache"),
                BTreeMap::from([
                    ("RUSTC_WRAPPER".into(), "kache".into()),
                    ("KACHE_CACHE_DIR".into(), "/var/cache/kache".into()),
                    ("KACHE_MAX_SIZE".into(), "20GiB".into()),
                    ("KACHE_LOCAL_ONLY".into(), "true".into()),
                    ("KACHE_PREFETCH_ENABLED".into(), "false".into()),
                ]),
            ),
            CompilerCacheBackend::Sccache => (
                Some("/var/cache/sccache"),
                BTreeMap::from([
                    ("RUSTC_WRAPPER".into(), "sccache".into()),
                    ("SCCACHE_DIR".into(), "/var/cache/sccache".into()),
                    ("SCCACHE_CACHE_SIZE".into(), "20G".into()),
                    ("SCCACHE_GHA_ENABLED".into(), "false".into()),
                ]),
            ),
            CompilerCacheBackend::Off => unreachable!("off runtime returned above"),
        };
        Self {
            backend,
            host_path: Some(host_path),
            container_path,
            environment: CompilerCacheEnvironment { variables },
        }
    }

    /// Construct the explicit passthrough runtime.
    #[must_use]
    pub fn off() -> Self {
        Self {
            backend: CompilerCacheBackend::Off,
            host_path: None,
            container_path: None,
            environment: CompilerCacheEnvironment {
                variables: BTreeMap::new(),
            },
        }
    }

    #[must_use]
    pub fn backend(&self) -> CompilerCacheBackend {
        self.backend
    }

    #[must_use]
    pub fn host_path(&self) -> Option<&Path> {
        self.host_path.as_deref()
    }

    #[must_use]
    pub fn container_path(&self) -> Option<&'static str> {
        self.container_path
    }

    #[must_use]
    pub fn environment(&self) -> &CompilerCacheEnvironment {
        &self.environment
    }
}

/// Result of the daemon restore-path check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePathProbe {
    pub path: PathBuf,
    pub writable: bool,
    pub regular_round_trip: bool,
}

/// Snapshot of compiler-cache lookup outcomes for one service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompilerCacheTelemetry {
    /// Number of validated cache hits.
    pub hits: u64,
    /// Number of clean cache misses.
    pub misses: u64,
    /// Number of lookups passed through because caching is disabled.
    pub passthroughs: u64,
}

/// Durable compiler-cache service backed by CAS and the action journal.
pub struct CompilerCacheService<C: Clock> {
    backend: CompilerCacheBackend,
    storage_root: PathBuf,
    owner: String,
    trust_class: TrustClass,
    lease_duration_ms: u64,
    heartbeat_every_ms: u64,
    max_storage_bytes: u64,
    cas: CasStore,
    metadata: Mutex<Connection>,
    journal: Mutex<LeaseManager<C>>,
    telemetry: Mutex<CompilerCacheTelemetry>,
    #[cfg(test)]
    failure_boundary: Mutex<Option<FailureBoundary>>,
}

/// Production service type used by the synchronous Docker executor.
pub type ProductionCompilerCache = CompilerCacheService<velnor_action_journal::TokioClock>;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureBoundary {
    ResultCas,
    AccountingCas,
    JournalComplete,
    MetadataFinalization,
}

impl<C: Clock> CompilerCacheService<C> {
    /// Open the selected backend namespace and initialize durable metadata.
    pub fn open(
        config: CompilerCacheConfig,
        declaration: WrapperDeclaration,
        clock: C,
    ) -> Result<Self, CacheError> {
        config.validate()?;
        let backend = resolve_backend(config.policy, &declaration)?;
        let storage_root = config
            .root
            .join(trust_namespace(config.trust_class))
            .join("compiler")
            .join(backend.namespace());
        fs::create_dir_all(&storage_root)?;
        let cas = CasStore::new(storage_root.join("cas"))?;
        let mut metadata = Connection::open(storage_root.join("metadata.sqlite"))?;
        metadata.busy_timeout(std::time::Duration::from_secs(5))?;
        initialize_metadata(&mut metadata)?;
        let journal = LeaseManager::open(storage_root.join("journal.sqlite"), clock)?;
        Ok(Self {
            backend,
            storage_root,
            owner: config.owner,
            trust_class: config.trust_class,
            lease_duration_ms: config.lease_duration_ms,
            heartbeat_every_ms: config.heartbeat_every_ms,
            max_storage_bytes: config.max_storage_bytes,
            cas,
            metadata: Mutex::new(metadata),
            journal: Mutex::new(journal),
            telemetry: Mutex::new(CompilerCacheTelemetry::default()),
            #[cfg(test)]
            failure_boundary: Mutex::new(None),
        })
    }

    /// Return the selected backend after admission.
    #[must_use]
    pub fn backend(&self) -> CompilerCacheBackend {
        self.backend
    }

    /// Return the backend's private persistent namespace.
    #[must_use]
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// Return a snapshot of this service's compiler-cache lookup outcomes.
    #[must_use]
    pub fn telemetry(&self) -> CompilerCacheTelemetry {
        *self.lock_telemetry()
    }

    /// Look up a result and retain its durable publication physical-byte
    /// evidence. This does not attribute publication allocation to the lookup
    /// operation itself.
    pub async fn lookup_with_publication_accounting(
        &self,
        key: &CompilerActionKey,
    ) -> Result<Option<CompilerCacheEntry>, CacheError> {
        self.lookup_with_publication_accounting_blocking(key)
    }

    /// Synchronous adapter for the runner's blocking Docker executor.
    ///
    /// The cache state machine is already synchronous at its I/O boundary; the
    /// async trait method above exists for service callers. Keeping this
    /// adapter on the same implementation avoids nested runtimes and a second
    /// ownership path.
    pub fn lookup_with_publication_accounting_blocking(
        &self,
        key: &CompilerActionKey,
    ) -> Result<Option<CompilerCacheEntry>, CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            self.record_passthrough();
            return Ok(None);
        }
        self.ensure_trust_scope(key)?;
        let expected_key_json = canonical_key_json(key)?;
        let record = {
            let journal = self.lock_journal()?;
            let Some(record) = journal.latest_action(key)? else {
                self.record_miss();
                return Ok(None);
            };
            if record.state != ActionState::Complete {
                self.record_miss();
                return Ok(None);
            }
            if record.trust_class != key.execution_policy.trust_class {
                return Err(CacheError::TrustMismatch);
            }
            record
        };
        let key_digest = key.digest()?.to_string();
        let Some(result_digest) = record.output_digests.get(RESULT_DIGEST) else {
            self.record_miss();
            return Ok(None);
        };
        let result_digest = result_digest.clone();
        let output_digest = record_output_digest(&record)?;
        let metadata_entry = {
            let metadata = self.lock_metadata()?;
            read_metadata(&metadata, &key_digest)?
        };
        let bytes = self.cas.get(&result_digest).map_err(CacheError::from)?;
        let result: CompilerResult = serde_json::from_slice(&bytes)
            .map_err(|error| CacheError::CorruptEntry(error.to_string()))?;
        if result.action_key != *key
            || result.action_key.execution_policy.trust_class != key.execution_policy.trust_class
            || result.output_root != *output_digest
        {
            return Err(CacheError::CorruptEntry(
                "published compiler result does not match its journal record".into(),
            ));
        }
        let accounting = match record_accounting(&self.cas, &record)? {
            Some(accounting) => accounting,
            None => metadata_entry
                .as_ref()
                .and_then(|entry| entry.accounting)
                .unwrap_or_else(PhysicalByteAccounting::unknown),
        };
        let entry = match metadata_entry {
            Some(entry) => entry,
            None => {
                let mut metadata = self.lock_metadata()?;
                finalize_metadata(&mut metadata, key, &record, &result_digest, &accounting)?;
                read_metadata(&metadata, &key_digest)?.ok_or_else(|| {
                    CacheError::InvalidMetadata(
                        "metadata finalization committed no cache entry".into(),
                    )
                })?
            }
        };
        verify_metadata(
            &entry,
            key,
            &expected_key_json,
            &record,
            &result_digest,
            &accounting,
        )?;
        if result.exit_code != 0 {
            return Err(CacheError::CorruptEntry(
                "failed compiler result cannot be a cache hit".into(),
            ));
        }
        if result.action_key != *key
            || result.action_key.execution_policy.trust_class != key.execution_policy.trust_class
            || result.output_root.to_string() != entry.output_digest
        {
            return Err(CacheError::CorruptEntry(
                "published compiler result does not match its metadata".into(),
            ));
        }
        self.validate_output_tree(&result.output_root)?;
        self.record_hit();
        Ok(Some(CompilerCacheEntry {
            result,
            publication_accounting: accounting,
        }))
    }

    /// Look up a validated result without acquiring a producer lease.
    pub async fn lookup(
        &self,
        key: &CompilerActionKey,
    ) -> Result<Option<CompilerResult>, CacheError> {
        Ok(self
            .lookup_with_publication_accounting(key)
            .await?
            .map(|entry| entry.result))
    }

    /// Publish a result and return physical-byte evidence for its durable
    /// result-envelope CAS object publication. The separate accounting sidecar
    /// is durable and recoverable but is not folded into these components.
    pub async fn publish_with_accounting(
        &self,
        lease: ProducerLease,
        result: CompilerResult,
    ) -> Result<PhysicalByteAccounting, CacheError> {
        self.publish_with_accounting_blocking(lease, result)
    }

    /// Synchronous adapter for publication from the blocking Docker executor.
    pub fn publish_with_accounting_blocking(
        &self,
        lease: ProducerLease,
        result: CompilerResult,
    ) -> Result<PhysicalByteAccounting, CacheError> {
        let outcome = self.publish_with_accounting_inner(&lease, result);
        match outcome {
            Ok(accounting) => Ok(accounting),
            Err(error) => match self.abandon_blocking(&lease) {
                Ok(()) | Err(CacheError::Journal(JournalError::LeaseFenced)) => Err(error),
                Err(cleanup) => Err(CacheError::PublicationCleanup {
                    publication: error.to_string(),
                    cleanup: cleanup.to_string(),
                }),
            },
        }
    }

    fn publish_with_accounting_inner(
        &self,
        lease: &ProducerLease,
        result: CompilerResult,
    ) -> Result<PhysicalByteAccounting, CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(&lease.action)?;
        if lease.action != result.action_key {
            return Err(CacheError::LeaseKeyMismatch);
        }
        if result.action_key.execution_policy.trust_class
            != lease.action.execution_policy.trust_class
        {
            return Err(CacheError::TrustMismatch);
        }
        self.ensure_active_lease(lease)?;
        if result.exit_code != 0 {
            return Err(CacheError::FailedResult {
                exit_code: result.exit_code,
            });
        }
        self.validate_output_tree(&result.output_root)?;
        let result_bytes = serde_json::to_vec(&result)?;
        let (result_digest, accounting, accounting_digest) = {
            let mut metadata = self.lock_metadata()?;
            let transaction = metadata.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let accounting_probe = serde_json::to_vec(&PhysicalByteAccounting::unknown())?;
            let requested = u64::try_from(result_bytes.len())
                .map_err(|_| CacheError::InvalidMetadata("result is too large".into()))?
                .checked_add(
                    u64::try_from(accounting_probe.len()).map_err(|_| {
                        CacheError::InvalidMetadata("accounting is too large".into())
                    })?,
                )
                .ok_or_else(|| CacheError::InvalidMetadata("quota request overflowed".into()))?;
            self.ensure_quota(requested)?;
            let (result_digest, accounting) = self.cas.put_with_accounting(&result_bytes)?;
            #[cfg(test)]
            self.fail_if_injected(FailureBoundary::ResultCas)?;
            let accounting_bytes = serde_json::to_vec(&accounting)?;
            let accounting_digest = self.cas.put(&accounting_bytes)?;
            #[cfg(test)]
            self.fail_if_injected(FailureBoundary::AccountingCas)?;
            self.ensure_storage_within_quota()?;
            transaction.commit()?;
            (result_digest, accounting, accounting_digest)
        };
        let complete = ActionRecord {
            action_key: lease.action.clone(),
            state: ActionState::Complete,
            producer_lease_ref: Some(lease_reference(lease)),
            consumer_run_ids: BTreeSet::new(),
            output_digests: BTreeMap::from([
                (OUTPUT_ROOT_DIGEST.into(), result.output_root.clone()),
                (RESULT_DIGEST.into(), result_digest.clone()),
                (ACCOUNTING_DIGEST.into(), accounting_digest),
            ]),
            timing: result.timing,
            worker_id: Some(lease.owner.clone()),
            trust_class: lease.action.execution_policy.trust_class,
        };

        // Journal completion remains the fencing point. The accounting digest
        // is part of that durable record, so recovery never needs to infer
        // whether the primary result object was new or deduplicated.
        let mut journal = self.lock_journal()?;
        journal.append_action_and_release(lease, &complete)?;
        drop(journal);
        #[cfg(test)]
        self.fail_if_injected(FailureBoundary::JournalComplete)?;

        #[cfg(test)]
        self.fail_if_injected(FailureBoundary::MetadataFinalization)?;
        let mut metadata = self.lock_metadata()?;
        finalize_metadata(
            &mut metadata,
            &lease.action,
            &complete,
            &result_digest,
            &accounting,
        )?;
        Ok(accounting)
    }

    /// Acquire the single producer lease for an action miss.
    pub async fn begin(&self, key: &CompilerActionKey) -> Result<ProducerLease, CacheError> {
        self.begin_blocking(key)
    }

    /// Synchronous producer-lease adapter for the blocking Docker executor.
    pub fn begin_blocking(&self, key: &CompilerActionKey) -> Result<ProducerLease, CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(key)?;
        self.ensure_quota(0)?;
        let mut journal = self.lock_journal()?;
        let lease = journal.acquire(
            key,
            self.owner.clone(),
            self.lease_duration_ms,
            self.heartbeat_every_ms,
        )?;
        let record = ActionRecord {
            action_key: key.clone(),
            state: ActionState::Leased,
            producer_lease_ref: Some(lease_reference(&lease)),
            consumer_run_ids: BTreeSet::new(),
            output_digests: BTreeMap::new(),
            timing: Default::default(),
            worker_id: Some(self.owner.clone()),
            trust_class: key.execution_policy.trust_class,
        };
        if let Err(error) = journal.append_action(&record) {
            let _ = journal.abandon(&lease);
            return Err(error.into());
        }
        Ok(lease)
    }

    /// Renew a producer lease before its persisted deadline.
    pub async fn renew(&self, lease: &mut ProducerLease) -> Result<(), CacheError> {
        self.renew_blocking(lease)
    }

    /// Synchronous lease-renewal adapter for the heartbeat thread.
    pub fn renew_blocking(&self, lease: &mut ProducerLease) -> Result<(), CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(&lease.action)?;
        let mut journal = self.lock_journal()?;
        journal.renew(lease)?;
        Ok(())
    }

    /// Explicitly abandon a failed, cancelled, or timed-out producer lease.
    ///
    /// The lease is fenced immediately. Callers must invoke this operation on
    /// every path that does not publish successfully; cleanup is deliberately
    /// not hidden in a destructor because the durable transition can fail.
    pub async fn abandon(&self, lease: &ProducerLease) -> Result<(), CacheError> {
        self.abandon_blocking(lease)
    }

    /// Synchronous explicit cleanup adapter for the blocking Docker executor.
    pub fn abandon_blocking(&self, lease: &ProducerLease) -> Result<(), CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(&lease.action)?;
        let mut journal = self.lock_journal()?;
        journal.abandon(lease)?;
        Ok(())
    }

    /// Publish one successful result under its still-valid producer lease.
    pub async fn publish(
        &self,
        lease: ProducerLease,
        result: CompilerResult,
    ) -> Result<(), CacheError> {
        self.publish_with_accounting(lease, result)
            .await
            .map(|_| ())
    }

    /// Snapshot a compiler output directory into this service's immutable CAS.
    pub fn store_output_tree(&self, root: &Path) -> Result<Digest, CacheError> {
        let mut metadata = self.lock_metadata()?;
        let transaction = metadata.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.ensure_quota(MAX_OUTPUT_CAPTURE_BYTES)?;
        let digest = self
            .cas
            .put_directory_tree(root)
            .map_err(CacheError::from)?;
        self.ensure_storage_within_quota()?;
        transaction.commit()?;
        Ok(digest)
    }

    /// Validate an output tree and ensure every file belongs to the compiler
    /// runtime subset. Publication and lookup both use this fence.
    pub fn validate_output_tree(&self, root: &Digest) -> Result<(), CacheError> {
        let manifest = self.cas.validate_tree(root)?;
        if manifest.entries.iter().any(|entry| {
            !matches!(
                entry.class,
                velnor_cas::FileClass::Executable | velnor_cas::FileClass::Runtime
            )
        }) {
            return Err(CacheError::InvalidOutput(
                "compiler output tree contains a non-runtime entry".into(),
            ));
        }
        Ok(())
    }

    /// Restore an immutable output tree through a private staging directory.
    pub fn materialize_output_tree(
        &self,
        root: &Digest,
        destination: &Path,
    ) -> Result<(), CacheError> {
        self.validate_output_tree(root)?;
        let parent = destination.parent().ok_or_else(|| {
            CacheError::InvalidOutput(format!(
                "compiler output destination has no parent: {}",
                destination.display()
            ))
        })?;
        fs::create_dir_all(parent)?;
        if let Ok(metadata) = fs::symlink_metadata(destination)
            && (metadata.file_type().is_symlink() || !metadata.is_dir())
        {
            return Err(CacheError::InvalidOutput(format!(
                "compiler output destination is not a regular directory: {}",
                destination.display()
            )));
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let staging = parent.join(format!(".compiler-restore-{nonce}"));
        let backup = parent.join(format!(".compiler-backup-{nonce}"));
        let result = (|| {
            fs::create_dir(&staging)?;
            self.cas.materialize_subset(
                root,
                velnor_cas::SubsetSelector::RuntimeFiles,
                &staging,
            )?;
            let had_destination = fs::symlink_metadata(destination).is_ok();
            if had_destination {
                fs::rename(destination, &backup)?;
            }
            if let Err(error) = fs::rename(&staging, destination) {
                if had_destination {
                    let _ = fs::rename(&backup, destination);
                }
                return Err(CacheError::Io(error));
            }
            if had_destination {
                fs::remove_dir_all(&backup)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Return daemon-owned wrapper variables for one job.
    #[must_use]
    pub fn environment(&self) -> CompilerCacheEnvironment {
        let mut variables = BTreeMap::new();
        match self.backend {
            CompilerCacheBackend::Kache => {
                variables.insert("RUSTC_WRAPPER".into(), "kache".into());
                variables.insert(
                    "KACHE_CACHE_DIR".into(),
                    self.storage_root.join("wrapper").display().to_string(),
                );
                variables.insert("KACHE_MAX_SIZE".into(), "20GiB".into());
                variables.insert("KACHE_LOCAL_ONLY".into(), "true".into());
                variables.insert("KACHE_PREFETCH_ENABLED".into(), "false".into());
            }
            CompilerCacheBackend::Sccache => {
                variables.insert("RUSTC_WRAPPER".into(), "sccache".into());
                variables.insert(
                    "SCCACHE_DIR".into(),
                    self.storage_root.join("wrapper").display().to_string(),
                );
                variables.insert("SCCACHE_CACHE_SIZE".into(), "20G".into());
                variables.insert("SCCACHE_GHA_ENABLED".into(), "false".into());
            }
            CompilerCacheBackend::Off => {}
        }
        CompilerCacheEnvironment { variables }
    }

    /// Probe a selected restore path without modifying existing files.
    pub fn probe_restore_path(&self) -> Result<RestorePathProbe, CacheError> {
        fs::create_dir_all(&self.storage_root)?;
        let probe_path = self.storage_root.join(".restore-probe");
        let writable = fs::write(&probe_path, b"velnor-compiler-cache-probe").is_ok();
        let regular_round_trip = writable
            && fs::metadata(&probe_path)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
            && fs::read(&probe_path).is_ok_and(|bytes| bytes == b"velnor-compiler-cache-probe");
        if writable {
            fs::remove_file(&probe_path)?;
        }
        Ok(RestorePathProbe {
            path: self.storage_root.clone(),
            writable,
            regular_round_trip,
        })
    }
}

impl CompilerCacheService<velnor_action_journal::TokioClock> {
    /// Open a production service with the journal's wall-clock lease source.
    pub fn open_production(
        config: CompilerCacheConfig,
        declaration: WrapperDeclaration,
    ) -> Result<Self, CacheError> {
        Self::open(
            config,
            declaration,
            velnor_action_journal::TokioClock::default(),
        )
    }
}

impl<C: Clock> CompilerCache for CompilerCacheService<C> {
    async fn lookup(&self, key: &CompilerActionKey) -> Result<Option<CompilerResult>, CacheError> {
        CompilerCacheService::lookup(self, key).await
    }

    async fn begin(&self, key: &CompilerActionKey) -> Result<ProducerLease, CacheError> {
        CompilerCacheService::begin(self, key).await
    }

    async fn renew(&self, lease: &mut ProducerLease) -> Result<(), CacheError> {
        CompilerCacheService::renew(self, lease).await
    }

    async fn abandon(&self, lease: &ProducerLease) -> Result<(), CacheError> {
        CompilerCacheService::abandon(self, lease).await
    }

    async fn publish(
        &self,
        lease: ProducerLease,
        result: CompilerResult,
    ) -> Result<(), CacheError> {
        CompilerCacheService::publish(self, lease, result).await
    }
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error(transparent)]
    Admission(#[from] CacheAdmissionError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Canonical(#[from] velnor_action_model::CanonicalizationError),
    #[error(transparent)]
    Digest(#[from] velnor_action_model::DigestError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("compiler-cache is disabled")]
    Disabled,
    #[error("compiler-cache producer lease does not match the published key")]
    LeaseKeyMismatch,
    #[error("compiler-cache entry is corrupt: {0}")]
    CorruptEntry(String),
    #[error("compiler-cache entry trust class does not match the requested action")]
    TrustMismatch,
    #[error("compiler-cache action trust class is outside this service namespace")]
    TrustScopeMismatch,
    #[error("failed compiler result cannot be published: exit code {exit_code}")]
    FailedResult { exit_code: i32 },
    #[error("invalid compiler-cache metadata: {0}")]
    InvalidMetadata(String),
    #[error("invalid compiler-cache output: {0}")]
    InvalidOutput(String),
    #[error("invalid compiler-cache configuration: {0}")]
    InvalidConfig(String),
    #[error("compiler-cache mutex is poisoned")]
    LockPoisoned,
    #[error(
        "compiler-cache publication failed ({publication}); lease cleanup also failed ({cleanup})"
    )]
    PublicationCleanup {
        publication: String,
        cleanup: String,
    },
    #[error(
        "compiler-cache storage quota exceeded: limit={limit} current={current} requested={requested}"
    )]
    QuotaExceeded {
        limit: u64,
        current: u64,
        requested: u64,
    },
    #[cfg(test)]
    #[error("injected compiler-cache failure")]
    InjectedFailure,
}

impl CacheError {
    /// Whether a producer could not start because another generation owns or
    /// has just abandoned this action. The executor may safely bypass
    /// publication in these cases; all other admission failures remain fatal.
    #[must_use]
    pub fn is_lease_contention(&self) -> bool {
        matches!(
            self,
            Self::Journal(JournalError::LeaseBusy { .. } | JournalError::LeaseExpired)
        )
    }
}

#[derive(Debug)]
struct MetadataEntry {
    key_json: String,
    result_digest: String,
    output_digest: String,
    trust_class: String,
    schema_version: u32,
    lease_generation: u64,
    accounting_json: Option<String>,
    accounting: Option<PhysicalByteAccounting>,
}

fn initialize_metadata(connection: &mut Connection) -> Result<(), CacheError> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;",
    )?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "CREATE TABLE IF NOT EXISTS compiler_cache_entries (
             action_key_digest TEXT PRIMARY KEY NOT NULL,
             action_key_json TEXT NOT NULL,
             result_digest TEXT NOT NULL,
             output_digest TEXT NOT NULL,
             trust_class TEXT NOT NULL,
             schema_version INTEGER NOT NULL,
             lease_generation INTEGER NOT NULL,
             physical_byte_accounting_json TEXT
         )",
        [],
    )?;
    let has_accounting_column = transaction
        .query_row(
            "SELECT 1 FROM pragma_table_info('compiler_cache_entries')
             WHERE name = 'physical_byte_accounting_json'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if !has_accounting_column {
        transaction.execute(
            "ALTER TABLE compiler_cache_entries
             ADD COLUMN physical_byte_accounting_json TEXT",
            [],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn canonical_key_json(key: &ActionKey) -> Result<String, CacheError> {
    String::from_utf8(canonical_json_bytes(key)?).map_err(|error| {
        CacheError::InvalidMetadata(format!("canonical action key is not UTF-8: {error}"))
    })
}

fn record_result_digest(record: &ActionRecord) -> Result<&Digest, CacheError> {
    record.output_digests.get(RESULT_DIGEST).ok_or_else(|| {
        CacheError::InvalidMetadata("journal record is missing result digest".into())
    })
}

fn record_output_digest(record: &ActionRecord) -> Result<&Digest, CacheError> {
    record
        .output_digests
        .get(OUTPUT_ROOT_DIGEST)
        .ok_or_else(|| {
            CacheError::InvalidMetadata("journal record is missing output root digest".into())
        })
}

fn record_lease_generation(record: &ActionRecord) -> Result<u64, CacheError> {
    let lease_reference = record.producer_lease_ref.as_deref().ok_or_else(|| {
        CacheError::InvalidMetadata("journal record is missing lease reference".into())
    })?;
    let generation = lease_reference
        .strip_prefix("compiler-cache/")
        .ok_or_else(|| {
            CacheError::InvalidMetadata("journal record has invalid lease reference".into())
        })?;
    generation.parse().map_err(|error| {
        CacheError::InvalidMetadata(format!(
            "journal record has invalid lease generation: {error}"
        ))
    })
}

fn record_accounting(
    cas: &CasStore,
    record: &ActionRecord,
) -> Result<Option<PhysicalByteAccounting>, CacheError> {
    let Some(accounting_digest) = record.output_digests.get(ACCOUNTING_DIGEST) else {
        return Ok(None);
    };
    let bytes = cas.get(accounting_digest)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| CacheError::CorruptEntry(error.to_string()))
}

fn read_metadata(
    connection: &Connection,
    action_key_digest: &str,
) -> Result<Option<MetadataEntry>, CacheError> {
    let entry = connection
        .query_row(
            "SELECT action_key_json, result_digest, output_digest, trust_class,
                    schema_version, lease_generation, physical_byte_accounting_json
             FROM compiler_cache_entries WHERE action_key_digest = ?1",
            [action_key_digest],
            |row| {
                let schema_version = row.get::<_, i64>(4)?;
                let lease_generation = row.get::<_, i64>(5)?;
                Ok(MetadataEntry {
                    key_json: row.get(0)?,
                    result_digest: row.get(1)?,
                    output_digest: row.get(2)?,
                    trust_class: row.get(3)?,
                    schema_version: u32::try_from(schema_version)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, schema_version))?,
                    lease_generation: u64::try_from(lease_generation).map_err(|_| {
                        rusqlite::Error::IntegralValueOutOfRange(5, lease_generation)
                    })?,
                    accounting_json: row.get(6)?,
                    accounting: None,
                })
            },
        )
        .optional()
        .map_err(CacheError::from)?;
    entry
        .map(|entry| {
            let accounting = entry
                .accounting_json
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(|error| CacheError::CorruptEntry(error.to_string()))?;
            Ok(MetadataEntry {
                key_json: entry.key_json,
                result_digest: entry.result_digest,
                output_digest: entry.output_digest,
                trust_class: entry.trust_class,
                schema_version: entry.schema_version,
                lease_generation: entry.lease_generation,
                accounting_json: None,
                accounting,
            })
        })
        .transpose()
}

fn finalize_metadata(
    connection: &mut Connection,
    key: &ActionKey,
    record: &ActionRecord,
    result_digest: &Digest,
    accounting: &PhysicalByteAccounting,
) -> Result<(), CacheError> {
    if record.action_key != *key || record.state != ActionState::Complete {
        return Err(CacheError::InvalidMetadata(
            "journal record cannot finalize compiler-cache metadata".into(),
        ));
    }
    if record_result_digest(record)? != result_digest {
        return Err(CacheError::InvalidMetadata(
            "journal result digest does not match completion record".into(),
        ));
    }
    let key_digest = key.digest()?.to_string();
    let key_json = canonical_key_json(key)?;
    let trust_json = serde_json::to_string(&key.execution_policy.trust_class)?;
    let output_digest = record_output_digest(record)?.to_string();
    let lease_generation = record_lease_generation(record)?;
    let accounting_json = serde_json::to_string(accounting)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO compiler_cache_entries(
             action_key_digest, action_key_json, result_digest, output_digest,
             trust_class, schema_version, lease_generation, physical_byte_accounting_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(action_key_digest) DO NOTHING",
        params![
            key_digest,
            key_json,
            result_digest.to_string(),
            output_digest,
            trust_json,
            i64::from(COMPILER_CACHE_SCHEMA_VERSION),
            i64::try_from(lease_generation)
                .map_err(|_| CacheError::InvalidMetadata("lease generation overflow".into()))?,
            accounting_json,
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

fn verify_metadata(
    entry: &MetadataEntry,
    key: &ActionKey,
    expected_key_json: &str,
    record: &ActionRecord,
    result_digest: &Digest,
    accounting: &PhysicalByteAccounting,
) -> Result<(), CacheError> {
    verify_metadata_fields(entry, key, expected_key_json, record)?;
    if entry.result_digest != result_digest.to_string() {
        return Err(CacheError::CorruptEntry(
            "metadata result digest does not match journal record".into(),
        ));
    }
    if entry.accounting.is_some_and(|value| value != *accounting) {
        return Err(CacheError::CorruptEntry(
            "metadata physical-byte accounting does not match durable result".into(),
        ));
    }
    Ok(())
}

fn verify_metadata_fields(
    entry: &MetadataEntry,
    key: &ActionKey,
    expected_key_json: &str,
    record: &ActionRecord,
) -> Result<(), CacheError> {
    if entry.schema_version != COMPILER_CACHE_SCHEMA_VERSION {
        return Err(CacheError::CorruptEntry(
            "unsupported compiler-cache metadata schema".into(),
        ));
    }
    if entry.key_json != expected_key_json {
        return Err(CacheError::CorruptEntry(
            "published action key does not match requested key".into(),
        ));
    }
    let parsed_key: ActionKey = serde_json::from_str(&entry.key_json)
        .map_err(|error| CacheError::CorruptEntry(error.to_string()))?;
    if parsed_key != *key {
        return Err(CacheError::CorruptEntry(
            "published action key failed canonical validation".into(),
        ));
    }
    let trust_class: TrustClass = serde_json::from_str(&entry.trust_class)
        .map_err(|error| CacheError::CorruptEntry(error.to_string()))?;
    if trust_class != key.execution_policy.trust_class || record.trust_class != trust_class {
        return Err(CacheError::TrustMismatch);
    }
    if record.producer_lease_ref.as_deref()
        != Some(&format!("compiler-cache/{}", entry.lease_generation))
    {
        return Err(CacheError::CorruptEntry(
            "lease generation does not match published metadata".into(),
        ));
    }
    Ok(())
}

fn lease_reference(lease: &ProducerLease) -> String {
    format!("compiler-cache/{}", lease.generation)
}

fn storage_size_bytes(root: &Path) -> Result<u64, CacheError> {
    let mut pending = vec![root.to_path_buf()];
    let mut total = 0_u64;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(CacheError::InvalidMetadata(format!(
                "compiler-cache storage contains symlink: {}",
                path.display()
            )));
        }
        if metadata.is_file() {
            total = total.checked_add(metadata.len()).ok_or_else(|| {
                CacheError::InvalidMetadata("storage byte count overflowed".into())
            })?;
        } else if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else {
            return Err(CacheError::InvalidMetadata(format!(
                "compiler-cache storage contains unsupported entry: {}",
                path.display()
            )));
        }
    }
    Ok(total)
}

fn trust_namespace(trust_class: TrustClass) -> &'static str {
    match trust_class {
        TrustClass::Untrusted => "untrusted",
        TrustClass::Trusted => "trusted",
        TrustClass::Release => "release",
    }
}

impl<C: Clock> CompilerCacheService<C> {
    fn ensure_trust_scope(&self, key: &CompilerActionKey) -> Result<(), CacheError> {
        if key.execution_policy.trust_class != self.trust_class {
            return Err(CacheError::TrustScopeMismatch);
        }
        Ok(())
    }

    fn ensure_active_lease(&self, lease: &ProducerLease) -> Result<(), CacheError> {
        let mut journal = self.lock_journal()?;
        // Expire persisted deadlines before checking state. This rejects an
        // obviously stale producer before it can allocate an orphan CAS
        // object; append_action_and_release remains the atomic final fence.
        journal.expire_due()?;
        if journal.lease_status(&lease.action)? == Some(LeaseStatus::Active) {
            Ok(())
        } else {
            Err(CacheError::Journal(JournalError::LeaseFenced))
        }
    }

    fn ensure_quota(&self, requested: u64) -> Result<(), CacheError> {
        let current = storage_size_bytes(&self.storage_root)?;
        let requested = requested
            .checked_add(QUOTA_HEADROOM_BYTES)
            .ok_or_else(|| CacheError::InvalidMetadata("quota request overflowed".into()))?;
        let projected = current
            .checked_add(requested)
            .ok_or_else(|| CacheError::InvalidMetadata("quota projection overflowed".into()))?;
        if projected > self.max_storage_bytes {
            return Err(CacheError::QuotaExceeded {
                limit: self.max_storage_bytes,
                current,
                requested,
            });
        }
        Ok(())
    }

    fn ensure_storage_within_quota(&self) -> Result<(), CacheError> {
        let current = storage_size_bytes(&self.storage_root)?;
        if current > self.max_storage_bytes {
            return Err(CacheError::QuotaExceeded {
                limit: self.max_storage_bytes,
                current,
                requested: 0,
            });
        }
        Ok(())
    }

    fn lock_journal(&self) -> Result<MutexGuard<'_, LeaseManager<C>>, CacheError> {
        self.journal.lock().map_err(|_| CacheError::LockPoisoned)
    }

    fn lock_metadata(&self) -> Result<MutexGuard<'_, Connection>, CacheError> {
        self.metadata.lock().map_err(|_| CacheError::LockPoisoned)
    }

    fn lock_telemetry(&self) -> MutexGuard<'_, CompilerCacheTelemetry> {
        self.telemetry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn record_hit(&self) {
        let mut telemetry = self.lock_telemetry();
        telemetry.hits = telemetry.hits.saturating_add(1);
    }

    fn record_miss(&self) {
        let mut telemetry = self.lock_telemetry();
        telemetry.misses = telemetry.misses.saturating_add(1);
    }

    fn record_passthrough(&self) {
        let mut telemetry = self.lock_telemetry();
        telemetry.passthroughs = telemetry.passthroughs.saturating_add(1);
    }

    #[cfg(test)]
    fn inject_failure(&self, boundary: FailureBoundary) {
        *self.failure_boundary.lock().expect("failure boundary lock") = Some(boundary);
    }

    #[cfg(test)]
    fn fail_if_injected(&self, boundary: FailureBoundary) -> Result<(), CacheError> {
        let mut injected = self.failure_boundary.lock().expect("failure boundary lock");
        if *injected == Some(boundary) {
            *injected = None;
            return Err(CacheError::InjectedFailure);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        future::Future,
        pin::Pin,
        sync::{Arc, Barrier, Mutex},
    };
    use tempfile::TempDir;
    use velnor_action_journal::{LeaseStatus, TokioClock};
    use velnor_action_model::{
        ActionTiming, Clock, ExecutionPolicy, LogicalInstant, PlatformIdentity, Provenance,
    };

    #[derive(Clone)]
    struct TestClock {
        now: Arc<Mutex<LogicalInstant>>,
    }

    impl Default for TestClock {
        fn default() -> Self {
            Self {
                now: Arc::new(Mutex::new(LogicalInstant::from_millis(0))),
            }
        }
    }

    impl TestClock {
        fn advance(&self, millis: u64) {
            let mut now = self.now.lock().expect("clock lock");
            *now = (*now).saturating_add(millis);
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> LogicalInstant {
            *self.now.lock().expect("clock lock")
        }

        fn sleep_until(
            &self,
            _deadline: LogicalInstant,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(std::future::ready(()))
        }

        fn wake_expiry(&self) {}
    }

    fn key(seed: u8) -> CompilerActionKey {
        let digest = Digest::from_bytes(&[seed]);
        ActionKey {
            command_digest: digest.clone(),
            input_root: digest.clone(),
            image_digest: digest.clone(),
            toolchain_digest: digest.clone(),
            platform: PlatformIdentity::new("linux", "x86_64", None),
            environment_digest: digest.clone(),
            dependency_outputs: vec![digest],
            execution_policy: ExecutionPolicy::default(),
        }
    }

    fn result(action_key: CompilerActionKey) -> CompilerResult {
        let digest = Digest::from_bytes(b"compiler-output");
        CompilerResult {
            action_key,
            output_root: test_output_root(),
            stdout_digest: digest.clone(),
            stderr_digest: digest.clone(),
            exit_code: 0,
            provenance: Provenance {
                builder: "test".into(),
                source_digest: digest,
                metadata: BTreeMap::new(),
            },
            timing: ActionTiming::default(),
        }
    }

    fn test_output_root() -> Digest {
        let manifest = velnor_cas::TreeManifest {
            entries: vec![velnor_cas::TreeEntry {
                path: "libdemo.rlib".into(),
                digest: Digest::from_bytes(b"compiler-output"),
                class: velnor_cas::FileClass::Runtime,
                mode: 0o644,
            }],
        };
        let bytes = canonical_json_bytes(&manifest).expect("test output manifest");
        Digest::from_bytes(&bytes)
    }

    fn service_with_owner(
        directory: &TempDir,
        policy: CompilerCachePolicy,
        declaration: WrapperDeclaration,
        owner: &str,
    ) -> CompilerCacheService<TokioClock> {
        let mut config = CompilerCacheConfig::new(directory.path(), owner);
        config.policy = policy;
        let cache =
            CompilerCacheService::open(config, declaration, TokioClock::new()).expect("service");
        seed_test_output(&cache);
        cache
    }

    fn seed_test_output<C: Clock>(cache: &CompilerCacheService<C>) {
        cache
            .cas
            .put(b"compiler-output")
            .expect("test output object");
        cache
            .cas
            .put_tree(&velnor_cas::TreeManifest {
                entries: vec![velnor_cas::TreeEntry {
                    path: "libdemo.rlib".into(),
                    digest: Digest::from_bytes(b"compiler-output"),
                    class: velnor_cas::FileClass::Runtime,
                    mode: 0o644,
                }],
            })
            .expect("test output tree");
    }

    fn service(
        directory: &TempDir,
        policy: CompilerCachePolicy,
        declaration: WrapperDeclaration,
    ) -> CompilerCacheService<TokioClock> {
        service_with_owner(directory, policy, declaration, "test-worker")
    }

    #[test]
    fn concurrent_opens_serialize_legacy_metadata_migration() {
        let directory = tempfile::tempdir().expect("tempdir");
        let storage_root = directory.path().join("untrusted/compiler/kache");
        fs::create_dir_all(&storage_root).expect("storage root");
        let metadata_path = storage_root.join("metadata.sqlite");
        let legacy_metadata = Connection::open(&metadata_path).expect("legacy metadata");
        legacy_metadata
            .execute_batch(
                "CREATE TABLE compiler_cache_entries (
                     action_key_digest TEXT PRIMARY KEY NOT NULL,
                     action_key_json TEXT NOT NULL,
                     result_digest TEXT NOT NULL,
                     output_digest TEXT NOT NULL,
                     trust_class TEXT NOT NULL,
                     schema_version INTEGER NOT NULL,
                     lease_generation INTEGER NOT NULL
                 );
                 INSERT INTO compiler_cache_entries(
                     action_key_digest, action_key_json, result_digest,
                     output_digest, trust_class, schema_version, lease_generation
                 ) VALUES ('legacy-key', '{}', 'legacy-result', 'legacy-output',
                           'untrusted', 1, 1);",
            )
            .expect("legacy metadata schema");
        drop(legacy_metadata);

        let barrier = Arc::new(Barrier::new(8));
        std::thread::scope(|scope| {
            let handles = (0..8)
                .map(|index| {
                    let barrier = Arc::clone(&barrier);
                    let root = directory.path().to_path_buf();
                    scope.spawn(move || {
                        barrier.wait();
                        let config = CompilerCacheConfig::new(root, format!("worker-{index}"));
                        CompilerCacheService::open(
                            config,
                            WrapperDeclaration::default(),
                            TokioClock::new(),
                        )
                        .map(|cache| cache.backend())
                    })
                })
                .collect::<Vec<_>>();

            for handle in handles {
                assert_eq!(
                    handle
                        .join()
                        .expect("concurrent open thread")
                        .expect("concurrent open"),
                    CompilerCacheBackend::Kache
                );
            }
        });

        let metadata = Connection::open(metadata_path).expect("verify metadata");
        let column_count: i64 = metadata
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('compiler_cache_entries')
                 WHERE name = 'physical_byte_accounting_json'",
                [],
                |row| row.get(0),
            )
            .expect("accounting column");
        assert_eq!(column_count, 1);
        let legacy_count: i64 = metadata
            .query_row("SELECT COUNT(*) FROM compiler_cache_entries", [], |row| {
                row.get(0)
            })
            .expect("legacy record");
        assert_eq!(legacy_count, 1);
    }

    #[tokio::test]
    async fn lookup_is_a_clean_miss_before_publication() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(1);
        assert!(cache.lookup(&action_key).await.expect("lookup").is_none());
        assert_eq!(
            cache
                .lock_journal()
                .expect("journal lock")
                .lease_status(&action_key)
                .expect("lease status"),
            None
        );
        let lease = cache.begin(&action_key).await.expect("begin after miss");
        assert_eq!(
            cache
                .lock_journal()
                .expect("journal lock")
                .lease_status(&action_key)
                .expect("lease status"),
            Some(LeaseStatus::Active)
        );
        cache.abandon(&lease).await.expect("abandon test lease");
        assert_eq!(
            cache.telemetry(),
            CompilerCacheTelemetry {
                hits: 0,
                misses: 1,
                passthroughs: 0,
            }
        );
    }

    #[tokio::test]
    async fn publication_is_a_validated_hit_for_each_backend() {
        let directory = tempfile::tempdir().expect("tempdir");
        for (policy, seed) in [
            (CompilerCachePolicy::Kache, 2),
            (CompilerCachePolicy::Sccache, 8),
        ] {
            let cache = service(&directory, policy, WrapperDeclaration::default());
            let action_key = key(seed);
            let lease = cache.begin(&action_key).await.expect("lease");
            let expected = result(action_key.clone());
            cache
                .publish(lease, expected.clone())
                .await
                .expect("publish");
            assert_eq!(
                cache.lookup(&action_key).await.expect("lookup"),
                Some(expected)
            );
            assert_eq!(
                cache.telemetry(),
                CompilerCacheTelemetry {
                    hits: 1,
                    misses: 0,
                    passthroughs: 0,
                }
            );
        }
    }

    #[tokio::test]
    async fn hit_lookup_does_not_acquire_a_producer_lease() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(25);
        let lease = cache.begin(&action_key).await.expect("lease");
        let expected = result(action_key.clone());
        cache
            .publish(lease, expected.clone())
            .await
            .expect("publish");
        assert_eq!(
            cache
                .lock_journal()
                .expect("journal lock")
                .lease_status(&action_key)
                .expect("lease status"),
            Some(LeaseStatus::Released)
        );

        assert_eq!(
            cache.lookup(&action_key).await.expect("lookup"),
            Some(expected)
        );
        assert_eq!(
            cache
                .lock_journal()
                .expect("journal lock")
                .lease_status(&action_key)
                .expect("lease status after hit"),
            Some(LeaseStatus::Released)
        );
    }

    #[tokio::test]
    async fn physical_accounting_survives_publish_reopen_and_recovery() {
        let directory = tempfile::tempdir().expect("tempdir");
        let action_key = key(19);
        let expected = result(action_key.clone());
        let accounting = {
            let cache = service(
                &directory,
                CompilerCachePolicy::Kache,
                WrapperDeclaration::default(),
            );
            let lease = cache.begin(&action_key).await.expect("lease");
            cache
                .publish_with_accounting(lease, expected.clone())
                .await
                .expect("publish")
        };

        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let reopened = cache
            .lookup_with_publication_accounting(&action_key)
            .await
            .expect("reopened lookup")
            .expect("reopened hit");
        assert_eq!(reopened.result(), &expected);
        assert_eq!(reopened.publication_accounting(), accounting);

        let key_digest = action_key.digest().expect("key digest").to_string();
        {
            let metadata = cache.lock_metadata().expect("metadata lock");
            metadata
                .execute(
                    "DELETE FROM compiler_cache_entries WHERE action_key_digest = ?1",
                    [&key_digest],
                )
                .expect("delete metadata");
        }
        let recovered = cache
            .lookup_with_publication_accounting(&action_key)
            .await
            .expect("recovery lookup")
            .expect("recovery hit");
        assert_eq!(recovered.publication_accounting(), accounting);
        let metadata = cache.lock_metadata().expect("metadata lock");
        let recovered_entry = read_metadata(&metadata, &key_digest)
            .expect("read recovered metadata")
            .expect("recovered metadata");
        assert_eq!(recovered_entry.accounting, Some(accounting));
    }

    #[tokio::test]
    async fn publication_failure_boundaries_recover_without_false_accounting() {
        for (seed, boundary) in [
            (21, FailureBoundary::ResultCas),
            (22, FailureBoundary::AccountingCas),
            (23, FailureBoundary::JournalComplete),
            (24, FailureBoundary::MetadataFinalization),
        ] {
            let directory = tempfile::tempdir().expect("tempdir");
            let action_key = key(seed);
            let expected = result(action_key.clone());
            let cache = service(
                &directory,
                CompilerCachePolicy::Kache,
                WrapperDeclaration::default(),
            );
            let lease = cache.begin(&action_key).await.expect("lease");
            cache.inject_failure(boundary);
            assert!(matches!(
                cache.publish_with_accounting(lease, expected.clone()).await,
                Err(CacheError::InjectedFailure)
            ));
            drop(cache);

            let cache = service(
                &directory,
                CompilerCachePolicy::Kache,
                WrapperDeclaration::default(),
            );
            let key_digest = action_key.digest().expect("key digest").to_string();
            let latest = {
                let journal = cache.lock_journal().expect("journal lock");
                journal
                    .latest_action(&action_key)
                    .expect("latest action")
                    .expect("action record")
            };
            match boundary {
                FailureBoundary::ResultCas | FailureBoundary::AccountingCas => {
                    assert_eq!(latest.state, ActionState::Leased);
                    assert_eq!(
                        cache
                            .lock_journal()
                            .expect("journal lock")
                            .lease_status(&action_key)
                            .expect("lease status"),
                        Some(LeaseStatus::Abandoned)
                    );
                    assert!(cache
                        .lookup_with_publication_accounting(&action_key)
                        .await
                        .expect("partial publication lookup")
                        .is_none());
                    {
                        let metadata = cache.lock_metadata().expect("metadata lock");
                        assert!(read_metadata(&metadata, &key_digest)
                            .expect("partial metadata read")
                            .is_none());
                    }
                    let retry = cache.begin(&action_key).await.expect("lease takeover");
                    assert!(retry.generation > 1);
                    cache.abandon(&retry).await.expect("abandon retry lease");
                }
                FailureBoundary::JournalComplete | FailureBoundary::MetadataFinalization => {
                    assert_eq!(latest.state, ActionState::Complete);
                    let recovered = cache
                        .lookup_with_publication_accounting(&action_key)
                        .await
                        .expect("recovery lookup")
                        .expect("recovered hit");
                    let accounting_digest = latest
                        .output_digests
                        .get(ACCOUNTING_DIGEST)
                        .expect("accounting digest");
                    let durable_accounting: PhysicalByteAccounting = serde_json::from_slice(
                        &cache.cas.get(accounting_digest).expect("accounting object"),
                    )
                    .expect("durable accounting");
                    assert_eq!(recovered.result(), &expected);
                    assert_eq!(recovered.publication_accounting(), durable_accounting);
                    let metadata = cache.lock_metadata().expect("metadata lock");
                    assert_eq!(
                        read_metadata(&metadata, &key_digest)
                            .expect("recovered metadata read")
                            .expect("recovered metadata")
                            .accounting,
                        Some(durable_accounting)
                    );
                }
            }
        }
    }

    #[test]
    fn storage_quota_rejects_unbounded_compiler_output_growth() {
        let directory = tempfile::tempdir().expect("tempdir");
        let output = directory.path().join("target");
        fs::create_dir_all(&output).expect("output directory");
        fs::write(output.join("libdemo.rlib"), b"output").expect("output file");

        let mut config = CompilerCacheConfig::new(directory.path().join("cache"), "test-worker");
        config.policy = CompilerCachePolicy::Kache;
        config.max_storage_bytes = 1;
        let cache =
            CompilerCacheService::open(config, WrapperDeclaration::default(), TokioClock::new())
                .expect("service");
        assert!(matches!(
            cache.store_output_tree(&output),
            Err(CacheError::QuotaExceeded { .. })
        ));
    }

    #[tokio::test]
    async fn legacy_record_without_accounting_stays_unknown() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(20);
        let lease = cache.begin(&action_key).await.expect("lease");
        cache
            .publish_with_accounting(lease, result(action_key.clone()))
            .await
            .expect("publish");

        let mut legacy = {
            let journal = cache.lock_journal().expect("journal lock");
            journal
                .latest_action(&action_key)
                .expect("read journal")
                .expect("complete record")
        };
        legacy.output_digests.remove(ACCOUNTING_DIGEST);
        {
            let mut journal = cache.lock_journal().expect("journal lock");
            journal
                .append_action(&legacy)
                .expect("append legacy record");
        }
        let key_digest = action_key.digest().expect("key digest").to_string();
        {
            let metadata = cache.lock_metadata().expect("metadata lock");
            metadata
                .execute(
                    "UPDATE compiler_cache_entries
                     SET physical_byte_accounting_json = NULL
                     WHERE action_key_digest = ?1",
                    [&key_digest],
                )
                .expect("remove legacy accounting");
        }

        let entry = cache
            .lookup_with_publication_accounting(&action_key)
            .await
            .expect("lookup")
            .expect("legacy hit");
        assert!(!entry.publication_accounting().is_known());
        assert_eq!(entry.publication_accounting().shared_bytes(), None);
        assert_eq!(entry.publication_accounting().newly_allocated_bytes(), None);
    }

    #[tokio::test]
    async fn lookup_recovers_metadata_after_journal_completion() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(15);
        let lease = cache.begin(&action_key).await.expect("lease");
        let expected = result(action_key.clone());
        let result_digest = Digest::from_bytes(&serde_json::to_vec(&expected).expect("json"));
        cache
            .publish(lease, expected.clone())
            .await
            .expect("publish");

        let key_digest = action_key.digest().expect("key digest").to_string();
        {
            let metadata = cache.lock_metadata().expect("metadata lock");
            metadata
                .execute(
                    "DELETE FROM compiler_cache_entries WHERE action_key_digest = ?1",
                    [&key_digest],
                )
                .expect("delete metadata");
        }

        assert_eq!(
            cache.lookup(&action_key).await.expect("recovery lookup"),
            Some(expected.clone())
        );
        let metadata = cache.lock_metadata().expect("metadata lock");
        let entry = read_metadata(&metadata, &key_digest)
            .expect("read recovered metadata")
            .expect("recovered entry");
        assert_eq!(entry.result_digest, result_digest.to_string());
        assert_eq!(entry.output_digest, expected.output_root.to_string());
    }

    #[tokio::test]
    async fn legacy_complete_record_with_metadata_is_a_clean_miss() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(17);
        let lease = cache.begin(&action_key).await.expect("lease");
        let expected = result(action_key.clone());
        cache.publish(lease, expected).await.expect("publish");

        let mut legacy = {
            let journal = cache.lock_journal().expect("journal lock");
            journal
                .latest_action(&action_key)
                .expect("read journal")
                .expect("complete record")
        };
        legacy.output_digests.remove(RESULT_DIGEST);
        {
            let mut journal = cache.lock_journal().expect("journal lock");
            journal
                .append_action(&legacy)
                .expect("append legacy record");
        }

        assert!(cache.lookup(&action_key).await.expect("lookup").is_none());
    }

    #[tokio::test]
    async fn legacy_complete_record_without_result_digest_and_metadata_is_a_clean_miss() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(18);
        let lease = cache.begin(&action_key).await.expect("lease");
        cache
            .publish(lease, result(action_key.clone()))
            .await
            .expect("publish");

        let key_digest = action_key.digest().expect("key digest").to_string();
        let mut malformed = {
            let journal = cache.lock_journal().expect("journal lock");
            journal
                .latest_action(&action_key)
                .expect("read journal")
                .expect("complete record")
        };
        malformed.output_digests.remove(RESULT_DIGEST);
        {
            let mut journal = cache.lock_journal().expect("journal lock");
            journal
                .append_action(&malformed)
                .expect("append malformed record");
        }
        {
            let metadata = cache.lock_metadata().expect("metadata lock");
            metadata
                .execute(
                    "DELETE FROM compiler_cache_entries WHERE action_key_digest = ?1",
                    [&key_digest],
                )
                .expect("delete metadata");
        }

        assert!(cache.lookup(&action_key).await.expect("lookup").is_none());
    }

    #[tokio::test]
    async fn metadata_recovery_does_not_overwrite_valid_entry() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(16);
        let lease = cache.begin(&action_key).await.expect("lease");
        let expected = result(action_key.clone());
        cache
            .publish(lease, expected.clone())
            .await
            .expect("publish");

        let key_digest = action_key.digest().expect("key digest").to_string();
        let original = {
            let metadata = cache.lock_metadata().expect("metadata lock");
            read_metadata(&metadata, &key_digest)
                .expect("read metadata")
                .expect("metadata entry")
        };
        let mut alternate = {
            let journal = cache.lock_journal().expect("journal lock");
            journal
                .latest_action(&action_key)
                .expect("read journal")
                .expect("complete record")
        };
        alternate.output_digests.insert(
            OUTPUT_ROOT_DIGEST.into(),
            Digest::from_bytes(b"alternate-output"),
        );
        let alternate_digest = Digest::from_bytes(b"alternate-result");
        alternate
            .output_digests
            .insert(RESULT_DIGEST.into(), alternate_digest.clone());
        {
            let mut metadata = cache.lock_metadata().expect("metadata lock");
            finalize_metadata(
                &mut metadata,
                &action_key,
                &alternate,
                &alternate_digest,
                &PhysicalByteAccounting::unknown(),
            )
            .expect("idempotent metadata finalization");
            let preserved = read_metadata(&metadata, &key_digest)
                .expect("read preserved metadata")
                .expect("preserved entry");
            assert_eq!(preserved.result_digest, original.result_digest);
            assert_eq!(preserved.output_digest, original.output_digest);
            assert_eq!(preserved.lease_generation, original.lease_generation);
        }

        assert_eq!(
            cache.lookup(&action_key).await.expect("lookup"),
            Some(expected)
        );
    }
    #[tokio::test]
    async fn disabled_lookup_is_a_passthrough() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Off,
            WrapperDeclaration::default(),
        );
        assert!(cache.lookup(&key(7)).await.expect("lookup").is_none());
        assert_eq!(
            cache.telemetry(),
            CompilerCacheTelemetry {
                hits: 0,
                misses: 0,
                passthroughs: 1,
            }
        );
    }

    #[tokio::test]
    async fn corrupt_entry_is_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(3);
        let lease = cache.begin(&action_key).await.expect("lease");
        let expected = result(action_key.clone());
        let result_digest = Digest::from_bytes(&serde_json::to_vec(&expected).expect("json"));
        cache.publish(lease, expected).await.expect("publish");
        let path = cache
            .storage_root()
            .join("cas")
            .join(&result_digest.as_str()[..2])
            .join(&result_digest.as_str()[2..]);
        std::fs::write(path, b"corrupt").expect("corrupt object");
        assert!(matches!(
            cache.lookup(&action_key).await,
            Err(CacheError::Cas(CasError::Corrupt { .. })) | Err(CacheError::CorruptEntry(_))
        ));
    }

    #[tokio::test]
    async fn same_key_has_one_producer_lease() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Sccache,
            WrapperDeclaration::default(),
        );
        let action_key = key(4);
        let first = cache.begin(&action_key).await;
        let second = cache.begin(&action_key).await;
        assert!(first.is_ok());
        assert!(matches!(
            second,
            Err(CacheError::Journal(JournalError::LeaseBusy { .. }))
        ));
    }

    #[test]
    fn concurrent_service_instances_have_one_producer_lease() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first = Arc::new(service_with_owner(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
            "worker-a",
        ));
        let second = Arc::new(service_with_owner(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
            "worker-b",
        ));
        let action_key = key(26);
        let barrier = Arc::new(Barrier::new(2));

        let outcomes = std::thread::scope(|scope| {
            let first_barrier = Arc::clone(&barrier);
            let first_action = action_key.clone();
            let first_service = Arc::clone(&first);
            let first_handle = scope.spawn(move || {
                first_barrier.wait();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("first runtime");
                runtime
                    .block_on(first_service.begin(&first_action))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });

            let second_barrier = Arc::clone(&barrier);
            let second_action = action_key;
            let second_service = Arc::clone(&second);
            let second_handle = scope.spawn(move || {
                second_barrier.wait();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("second runtime");
                runtime
                    .block_on(second_service.begin(&second_action))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            });

            [
                first_handle.join().expect("first producer thread"),
                second_handle.join().expect("second producer thread"),
            ]
        });

        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert!(outcomes.iter().any(|outcome| {
            matches!(outcome, Err(error) if error.contains("lease is already held"))
        }));
    }

    #[tokio::test]
    async fn renewed_lease_can_publish_after_original_deadline() {
        let directory = tempfile::tempdir().expect("tempdir");
        let clock = TestClock::default();
        let mut config = CompilerCacheConfig::new(directory.path(), "test-worker");
        config.policy = CompilerCachePolicy::Kache;
        let cache =
            CompilerCacheService::open(config, WrapperDeclaration::default(), clock.clone())
                .expect("service");
        seed_test_output(&cache);
        let action_key = key(11);
        let mut lease = cache.begin(&action_key).await.expect("lease");

        clock.advance(10_000);
        cache.renew(&mut lease).await.expect("renew");
        clock.advance(20_001);

        cache
            .publish(lease, result(action_key.clone()))
            .await
            .expect("publish after renewal");
        assert!(cache.lookup(&action_key).await.expect("lookup").is_some());
    }

    #[tokio::test]
    async fn abandoned_lease_is_safely_fenced_from_publication() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(12);
        let lease = cache.begin(&action_key).await.expect("lease");
        let mut stale_lease = lease.clone();

        cache.abandon(&lease).await.expect("abandon");

        assert_eq!(
            cache
                .lock_journal()
                .expect("journal lock")
                .lease_status(&action_key)
                .expect("lease status"),
            Some(LeaseStatus::Abandoned)
        );
        assert!(matches!(
            cache.renew(&mut stale_lease).await,
            Err(CacheError::Journal(JournalError::LeaseFenced))
        ));

        assert!(matches!(
            cache.publish(lease, result(action_key.clone())).await,
            Err(CacheError::Journal(JournalError::LeaseFenced))
        ));
        assert!(cache.lookup(&action_key).await.expect("lookup").is_none());
    }

    #[tokio::test]
    async fn expired_lease_is_safely_fenced_from_publication() {
        let directory = tempfile::tempdir().expect("tempdir");
        let clock = TestClock::default();
        let mut config = CompilerCacheConfig::new(directory.path(), "test-worker");
        config.policy = CompilerCachePolicy::Kache;
        config.lease_duration_ms = 100;
        config.heartbeat_every_ms = 50;
        let cache =
            CompilerCacheService::open(config, WrapperDeclaration::default(), clock.clone())
                .expect("service");
        let action_key = key(13);
        let lease = cache.begin(&action_key).await.expect("lease");

        clock.advance(100);

        assert!(matches!(
            cache.publish(lease, result(action_key.clone())).await,
            Err(CacheError::Journal(JournalError::LeaseFenced))
        ));
        assert!(cache.lookup(&action_key).await.expect("lookup").is_none());
    }

    #[tokio::test]
    async fn fenced_publication_cannot_overwrite_existing_hit() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(14);
        let lease = cache.begin(&action_key).await.expect("lease");
        let fenced_lease = lease.clone();
        let expected = result(action_key.clone());
        cache
            .publish(lease, expected.clone())
            .await
            .expect("publish");

        let mut stale = result(action_key.clone());
        stale.output_root = Digest::from_bytes(b"stale-output");
        assert!(matches!(
            cache.publish(fenced_lease, stale).await,
            Err(CacheError::Journal(JournalError::LeaseFenced))
        ));
        assert_eq!(
            cache.lookup(&action_key).await.expect("lookup"),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn mismatched_publish_never_creates_a_hit() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(5);
        let lease = cache.begin(&action_key).await.expect("lease");
        let error = cache
            .publish(lease, result(key(6)))
            .await
            .expect_err("mismatch");
        assert!(matches!(error, CacheError::LeaseKeyMismatch));
        assert!(cache.lookup(&action_key).await.expect("lookup").is_none());
    }

    #[tokio::test]
    async fn failed_result_is_never_published() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(9);
        let lease = cache.begin(&action_key).await.expect("lease");
        let mut failed = result(action_key.clone());
        failed.exit_code = 1;
        assert!(matches!(
            cache.publish(lease.clone(), failed).await,
            Err(CacheError::FailedResult { exit_code: 1 })
        ));
        assert_eq!(
            cache
                .lock_journal()
                .expect("journal lock")
                .lease_status(&action_key)
                .expect("failed lease status"),
            Some(LeaseStatus::Abandoned)
        );
        assert!(matches!(
            cache.abandon(&lease).await,
            Err(CacheError::Journal(JournalError::LeaseFenced))
        ));
        let retry = cache.begin(&action_key).await.expect("lease takeover");
        cache.abandon(&retry).await.expect("abandon retry lease");
        assert!(cache.lookup(&action_key).await.expect("lookup").is_none());
    }

    #[tokio::test]
    async fn trust_classes_have_private_namespaces() {
        let directory = tempfile::tempdir().expect("tempdir");
        let untrusted = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let mut trusted_config = CompilerCacheConfig::new(directory.path(), "trusted-worker");
        trusted_config.trust_class = TrustClass::Trusted;
        let trusted = CompilerCacheService::open(
            trusted_config,
            WrapperDeclaration::default(),
            TokioClock::new(),
        )
        .expect("trusted service");
        seed_test_output(&trusted);
        assert_ne!(untrusted.storage_root(), trusted.storage_root());
        assert!(untrusted
            .storage_root()
            .ends_with("untrusted/compiler/kache"));
        assert!(trusted.storage_root().ends_with("trusted/compiler/kache"));

        let mut trusted_key = key(10);
        trusted_key.execution_policy.trust_class = TrustClass::Trusted;
        assert!(matches!(
            untrusted.lookup(&trusted_key).await,
            Err(CacheError::TrustScopeMismatch)
        ));
        let lease = trusted.begin(&trusted_key).await.expect("trusted lease");
        trusted
            .publish(lease, result(trusted_key.clone()))
            .await
            .expect("trusted publish");
        assert!(matches!(
            untrusted.lookup(&trusted_key).await,
            Err(CacheError::TrustScopeMismatch)
        ));
    }

    #[test]
    fn policy_defaults_to_kache_and_rejects_mixed_wrappers() {
        assert_eq!(
            resolve_backend(CompilerCachePolicy::Auto, &WrapperDeclaration::default())
                .expect("auto"),
            CompilerCacheBackend::Kache
        );
        let error = resolve_backend(
            CompilerCachePolicy::Auto,
            &WrapperDeclaration {
                sccache: true,
                kache: true,
            },
        )
        .expect_err("mixed wrappers");
        assert_eq!(error.to_string(), "compiler-cache backend conflict: sccache and kache wrappers cannot be enabled together");
    }

    #[test]
    fn config_rejects_zero_or_equal_heartbeat() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut config = CompilerCacheConfig::new(directory.path(), "test-worker");
        let expected = "compiler-cache lease duration and heartbeat must be positive and heartbeat must be strictly less than lease duration";

        for heartbeat_every_ms in [0, config.lease_duration_ms] {
            config.heartbeat_every_ms = heartbeat_every_ms;
            let error = config.validate().expect_err("invalid heartbeat");
            let CacheError::InvalidConfig(message) = error else {
                panic!("unexpected error: {error:?}");
            };
            assert_eq!(message, expected);
        }
    }

    #[test]
    fn environment_contains_one_wrapper_and_private_namespace() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Sccache,
            WrapperDeclaration::default(),
        );
        let variables = cache.environment().variables;
        assert_eq!(
            variables.get("RUSTC_WRAPPER"),
            Some(&String::from("sccache"))
        );
        assert!(!variables.contains_key("KACHE_CACHE_DIR"));
        assert_eq!(
            variables.get("SCCACHE_GHA_ENABLED"),
            Some(&String::from("false"))
        );
    }

    #[test]
    fn runtime_descriptor_has_one_backend_mount_and_wrapper() {
        let runtime = CompilerCacheRuntime::new(
            CompilerCacheBackend::Kache,
            PathBuf::from("/var/cache/velnor/v1/untrusted/compiler/kache"),
        );
        assert_eq!(runtime.backend(), CompilerCacheBackend::Kache);
        assert_eq!(
            runtime.host_path(),
            Some(Path::new("/var/cache/velnor/v1/untrusted/compiler/kache"))
        );
        assert_eq!(runtime.container_path(), Some("/var/cache/kache"));
        assert_eq!(
            runtime.environment().variables.get("RUSTC_WRAPPER"),
            Some(&String::from("kache"))
        );
        assert!(!runtime.environment().variables.contains_key("SCCACHE_DIR"));
    }
}

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
};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use velnor_action_journal::{ActionRecord, JournalError, LeaseManager};
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
const OUTPUT_ROOT_DIGEST: &str = "output_root";
const RESULT_DIGEST: &str = "compiler_cache_result";

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
    if let Some(declared) = declared_backend {
        if backend != declared {
            return Err(CacheAdmissionError::PolicyConflict { policy, declared });
        }
    }
    Ok(backend)
}

/// Exact admission failures for conflicting cache ownership.
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
        Ok(())
    }
}

/// Public key alias retained for the compiler-specific service contract.
pub type CompilerActionKey = ActionKey;

/// Public result alias retained for the compiler-specific service contract.
pub type CompilerResult = ActionResult;

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

/// Durable compiler-cache service backed by CAS and the action journal.
pub struct CompilerCacheService<C: Clock> {
    backend: CompilerCacheBackend,
    storage_root: PathBuf,
    owner: String,
    trust_class: TrustClass,
    lease_duration_ms: u64,
    heartbeat_every_ms: u64,
    cas: CasStore,
    metadata: Mutex<Connection>,
    journal: Mutex<LeaseManager<C>>,
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
        let metadata = Connection::open(storage_root.join("metadata.sqlite"))?;
        metadata.busy_timeout(std::time::Duration::from_secs(5))?;
        initialize_metadata(&metadata)?;
        let journal = LeaseManager::open(storage_root.join("journal.sqlite"), clock)?;
        Ok(Self {
            backend,
            storage_root,
            owner: config.owner,
            trust_class: config.trust_class,
            lease_duration_ms: config.lease_duration_ms,
            heartbeat_every_ms: config.heartbeat_every_ms,
            cas,
            metadata: Mutex::new(metadata),
            journal: Mutex::new(journal),
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
        if self.backend == CompilerCacheBackend::Off {
            return Ok(None);
        }
        self.ensure_trust_scope(key)?;
        let expected_key_json = canonical_key_json(key)?;
        let record = {
            let journal = self.lock_journal()?;
            let Some(record) = journal.latest_action(key)? else {
                return Ok(None);
            };
            if record.state != ActionState::Complete {
                return Ok(None);
            }
            if record.trust_class != key.execution_policy.trust_class {
                return Err(CacheError::TrustMismatch);
            }
            record
        };
        let key_digest = key.digest()?.to_string();
        let Some(result_digest) = record.output_digests.get(RESULT_DIGEST) else {
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
        let entry = match metadata_entry {
            Some(entry) => entry,
            None => {
                let mut metadata = self.lock_metadata()?;
                finalize_metadata(&mut metadata, key, &record, &result_digest)?;
                read_metadata(&metadata, &key_digest)?.ok_or_else(|| {
                    CacheError::InvalidMetadata(
                        "metadata finalization committed no cache entry".into(),
                    )
                })?
            }
        };
        verify_metadata(&entry, key, &expected_key_json, &record, &result_digest)?;
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
        Ok(Some(result))
    }

    async fn begin(&self, key: &CompilerActionKey) -> Result<ProducerLease, CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(key)?;
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

    async fn renew(&self, lease: &mut ProducerLease) -> Result<(), CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(&lease.action)?;
        let mut journal = self.lock_journal()?;
        journal.renew(lease)?;
        Ok(())
    }

    async fn abandon(&self, lease: &ProducerLease) -> Result<(), CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(&lease.action)?;
        let mut journal = self.lock_journal()?;
        journal.abandon(lease)?;
        Ok(())
    }

    async fn publish(
        &self,
        lease: ProducerLease,
        result: CompilerResult,
    ) -> Result<(), CacheError> {
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
        if result.exit_code != 0 {
            return Err(CacheError::FailedResult {
                exit_code: result.exit_code,
            });
        }
        let result_bytes = serde_json::to_vec(&result)?;
        let result_digest = self.cas.put(&result_bytes)?;
        let complete = ActionRecord {
            action_key: lease.action.clone(),
            state: ActionState::Complete,
            producer_lease_ref: Some(lease_reference(&lease)),
            consumer_run_ids: BTreeSet::new(),
            output_digests: BTreeMap::from([
                (OUTPUT_ROOT_DIGEST.into(), result.output_root.clone()),
                (RESULT_DIGEST.into(), result_digest.clone()),
            ]),
            timing: result.timing,
            worker_id: Some(lease.owner.clone()),
            trust_class: lease.action.execution_policy.trust_class,
        };

        // This journal transition is the fencing point. Metadata follows it;
        // the durable result digest above lets lookup finish this step after a
        // crash, without allowing a stale producer to write first.
        let mut journal = self.lock_journal()?;
        journal.append_action_and_release(&lease, &complete)?;
        drop(journal);

        let mut metadata = self.lock_metadata()?;
        finalize_metadata(&mut metadata, &lease.action, &complete, &result_digest)?;
        Ok(())
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
    #[error("invalid compiler-cache configuration: {0}")]
    InvalidConfig(String),
    #[error("compiler-cache mutex is poisoned")]
    LockPoisoned,
}

#[derive(Debug)]
struct MetadataEntry {
    key_json: String,
    result_digest: String,
    output_digest: String,
    trust_class: String,
    schema_version: u32,
    lease_generation: u64,
}

fn initialize_metadata(connection: &Connection) -> Result<(), CacheError> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         CREATE TABLE IF NOT EXISTS compiler_cache_entries (
             action_key_digest TEXT PRIMARY KEY NOT NULL,
             action_key_json TEXT NOT NULL,
             result_digest TEXT NOT NULL,
             output_digest TEXT NOT NULL,
             trust_class TEXT NOT NULL,
             schema_version INTEGER NOT NULL,
             lease_generation INTEGER NOT NULL
         );",
    )?;
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

fn read_metadata(
    connection: &Connection,
    action_key_digest: &str,
) -> Result<Option<MetadataEntry>, CacheError> {
    connection
        .query_row(
            "SELECT action_key_json, result_digest, output_digest, trust_class,
                    schema_version, lease_generation
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
                })
            },
        )
        .optional()
        .map_err(CacheError::from)
}

fn finalize_metadata(
    connection: &mut Connection,
    key: &ActionKey,
    record: &ActionRecord,
    result_digest: &Digest,
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
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO compiler_cache_entries(
             action_key_digest, action_key_json, result_digest, output_digest,
             trust_class, schema_version, lease_generation
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
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
) -> Result<(), CacheError> {
    verify_metadata_fields(entry, key, expected_key_json, record)?;
    if entry.result_digest != result_digest.to_string() {
        return Err(CacheError::CorruptEntry(
            "metadata result digest does not match journal record".into(),
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

    fn lock_journal(&self) -> Result<MutexGuard<'_, LeaseManager<C>>, CacheError> {
        self.journal.lock().map_err(|_| CacheError::LockPoisoned)
    }

    fn lock_metadata(&self) -> Result<MutexGuard<'_, Connection>, CacheError> {
        self.metadata.lock().map_err(|_| CacheError::LockPoisoned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
    };
    use tempfile::TempDir;
    use velnor_action_journal::TokioClock;
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
            output_root: digest.clone(),
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

    fn service(
        directory: &TempDir,
        policy: CompilerCachePolicy,
        declaration: WrapperDeclaration,
    ) -> CompilerCacheService<TokioClock> {
        let mut config = CompilerCacheConfig::new(directory.path(), "test-worker");
        config.policy = policy;
        CompilerCacheService::open(config, declaration, TokioClock::new()).expect("service")
    }

    #[tokio::test]
    async fn lookup_is_a_clean_miss_before_publication() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        assert!(cache.lookup(&key(1)).await.expect("lookup").is_none());
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
        }
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
            finalize_metadata(&mut metadata, &action_key, &alternate, &alternate_digest)
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

    #[tokio::test]
    async fn renewed_lease_can_publish_after_original_deadline() {
        let directory = tempfile::tempdir().expect("tempdir");
        let clock = TestClock::default();
        let mut config = CompilerCacheConfig::new(directory.path(), "test-worker");
        config.policy = CompilerCachePolicy::Kache;
        let cache =
            CompilerCacheService::open(config, WrapperDeclaration::default(), clock.clone())
                .expect("service");
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

        cache.abandon(&lease).await.expect("abandon");

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
            cache.publish(lease, failed).await,
            Err(CacheError::FailedResult { exit_code: 1 })
        ));
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

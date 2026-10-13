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
const DEFAULT_LEASE_DURATION_MS: u64 = 30_000;
const DEFAULT_HEARTBEAT_MS: u64 = 10_000;

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
        if self.lease_duration_ms == 0 || self.heartbeat_every_ms > self.lease_duration_ms {
            return Err(CacheError::InvalidConfig(
                "compiler-cache lease duration must be positive and heartbeat must not exceed it"
                    .into(),
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

impl<C: Clock> CompilerCache for CompilerCacheService<C> {
    async fn lookup(&self, key: &CompilerActionKey) -> Result<Option<CompilerResult>, CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Ok(None);
        }
        self.ensure_trust_scope(key)?;
        let expected_key_json = canonical_key_json(key)?;
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
        let metadata = self.lock_metadata()?;
        let Some(entry) = read_metadata(&metadata, &key.digest()?.to_string())? else {
            return Ok(None);
        };
        verify_metadata(&entry, key, &expected_key_json, &record)?;
        let result_digest = Digest::parse(entry.result_digest)?;
        let bytes = self.cas.get(&result_digest).map_err(CacheError::from)?;
        let result: CompilerResult = serde_json::from_slice(&bytes)
            .map_err(|error| CacheError::CorruptEntry(error.to_string()))?;
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
        drop(metadata);
        drop(journal);
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
        let key_digest = lease.action.digest()?.to_string();
        let key_json = canonical_key_json(&lease.action)?;
        let trust_json = serde_json::to_string(&lease.action.execution_policy.trust_class)?;
        let complete = ActionRecord {
            action_key: lease.action.clone(),
            state: ActionState::Complete,
            producer_lease_ref: Some(lease_reference(&lease)),
            consumer_run_ids: BTreeSet::new(),
            output_digests: BTreeMap::from([(
                String::from("output_root"),
                result.output_root.clone(),
            )]),
            timing: result.timing,
            worker_id: Some(lease.owner.clone()),
            trust_class: lease.action.execution_policy.trust_class,
        };

        let mut journal = self.lock_journal()?;
        let mut metadata = self.lock_metadata()?;
        let transaction = metadata.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO compiler_cache_entries(
                 action_key_digest, action_key_json, result_digest, output_digest,
                 trust_class, schema_version, lease_generation
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(action_key_digest) DO UPDATE SET
                 action_key_json = excluded.action_key_json,
                 result_digest = excluded.result_digest,
                 output_digest = excluded.output_digest,
                 trust_class = excluded.trust_class,
                 schema_version = excluded.schema_version,
                 lease_generation = excluded.lease_generation",
            params![
                key_digest,
                key_json,
                result_digest.to_string(),
                result.output_root.to_string(),
                trust_json,
                i64::from(COMPILER_CACHE_SCHEMA_VERSION),
                i64::try_from(lease.generation)
                    .map_err(|_| CacheError::InvalidMetadata("lease generation overflow".into()))?,
            ],
        )?;
        transaction.commit()?;
        drop(metadata);
        journal.append_action_and_release(&lease, &complete)?;
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

fn verify_metadata(
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
    use std::collections::BTreeMap;
    use tempfile::TempDir;
    use velnor_action_journal::TokioClock;
    use velnor_action_model::{ActionTiming, ExecutionPolicy, PlatformIdentity, Provenance};

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
}

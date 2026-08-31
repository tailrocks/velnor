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
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use velnor_action_journal::{ActionRecord, JournalError, LeaseManager};
use velnor_action_model::{
    canonical_json_bytes, ActionKey, ActionResult, ActionState, Clock, Digest, ProducerLease,
    TrustClass,
};
use velnor_cas::{
    BudgetCallback, CasError, CasStore, FileClass, SubsetSelector, TreeEntry, TreeManifest,
};

#[cfg(unix)]
use std::{io::Read, os::unix::fs::PermissionsExt};

/// Compiler-cache metadata schema written beside each backend namespace.
pub const COMPILER_CACHE_SCHEMA_VERSION: u32 = 1;
pub const KACHE_VERSION: &str = "0.14.2";
pub const SCCACHE_VERSION: &str = "0.16.0";
const DEFAULT_LEASE_DURATION_MS: u64 = 30_000;
const DEFAULT_HEARTBEAT_MS: u64 = 10_000;
const MAX_OUTPUT_TREE_FILES: usize = 100_000;
const MAX_OUTPUT_TREE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_COMPILER_CACHE_CAS_BYTES: u64 = 20 * 1024 * 1024 * 1024;
static OUTPUT_STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

struct CompilerCasBudget {
    metadata_path: PathBuf,
}

impl CompilerCasBudget {
    fn open(cas_root: &Path, metadata_path: &Path) -> Result<Arc<Self>, CacheError> {
        let mut connection = Connection::open(metadata_path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let initialized: Option<i64> = transaction
            .query_row(
                "SELECT used_bytes FROM compiler_cache_quota WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if initialized.is_none() {
            let initial = directory_size(cas_root)?;
            transaction.execute(
                "INSERT INTO compiler_cache_quota(id, used_bytes, inflight_bytes) VALUES (1, ?1, 0)",
                [i64::try_from(initial).map_err(|_| {
                    CacheError::InvalidOutput("compiler-cache CAS size exceeds SQLite range".into())
                })?],
            )?;
        }
        transaction.commit()?;
        Ok(Arc::new(Self {
            metadata_path: metadata_path.to_path_buf(),
        }))
    }
}

impl BudgetCallback for CompilerCasBudget {
    fn reserve(&self, bytes: u64) -> Result<(), String> {
        let mut connection = Connection::open(&self.metadata_path)
            .map_err(|error| format!("open compiler-cache quota: {error}"))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("set compiler-cache quota timeout: {error}"))?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin compiler-cache quota: {error}"))?;
        let (used, inflight): (i64, i64) = transaction
            .query_row(
                "SELECT used_bytes, inflight_bytes FROM compiler_cache_quota WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| format!("read compiler-cache quota: {error}"))?;
        let next = u64::try_from(used)
            .ok()
            .and_then(|used| {
                u64::try_from(inflight)
                    .ok()
                    .and_then(|inflight| used.checked_add(inflight))
            })
            .and_then(|reserved| reserved.checked_add(bytes))
            .ok_or_else(|| "compiler-cache CAS quota arithmetic overflow".to_string())?;
        if next > MAX_COMPILER_CACHE_CAS_BYTES {
            return Err(format!(
                "compiler-cache CAS quota exceeded: {next} > {MAX_COMPILER_CACHE_CAS_BYTES} bytes"
            ));
        }
        transaction
            .execute(
                "UPDATE compiler_cache_quota SET inflight_bytes = inflight_bytes + ?1 WHERE id = 1",
                [i64::try_from(bytes)
                    .map_err(|_| "compiler-cache reservation exceeds SQLite range".to_string())?],
            )
            .map_err(|error| format!("reserve compiler-cache quota: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("commit compiler-cache quota: {error}"))
    }

    fn release(&self, bytes: u64) {
        self.finish_reservation(bytes, false);
    }

    fn commit(&self, bytes: u64) {
        self.finish_reservation(bytes, true);
    }
}

impl CompilerCasBudget {
    fn finish_reservation(&self, bytes: u64, committed: bool) {
        let Ok(mut connection) = Connection::open(&self.metadata_path) else {
            eprintln!("forensics.lifecycle: compiler-cache quota cleanup could not open metadata");
            return;
        };
        if connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .is_err()
        {
            eprintln!("forensics.lifecycle: compiler-cache quota cleanup timeout setup failed");
            return;
        }
        let Ok(transaction) = connection.transaction_with_behavior(TransactionBehavior::Immediate)
        else {
            eprintln!("forensics.lifecycle: compiler-cache quota cleanup transaction failed");
            return;
        };
        let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
        let result = if committed {
            transaction.execute(
                "UPDATE compiler_cache_quota
                 SET used_bytes = used_bytes + ?1, inflight_bytes = inflight_bytes - ?1
                 WHERE id = 1 AND inflight_bytes >= ?1",
                [bytes],
            )
        } else {
            transaction.execute(
                "UPDATE compiler_cache_quota SET inflight_bytes = inflight_bytes - ?1
                 WHERE id = 1 AND inflight_bytes >= ?1",
                [bytes],
            )
        };
        if result
            .and_then(|changed| {
                if changed == 1 {
                    transaction.commit()
                } else {
                    Err(rusqlite::Error::QueryReturnedNoRows)
                }
            })
            .is_err()
        {
            eprintln!("forensics.lifecycle: compiler-cache quota reservation cleanup failed");
        }
    }
}

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
            || self.heartbeat_every_ms.saturating_mul(2) >= self.lease_duration_ms
        {
            return Err(CacheError::InvalidConfig(
                "compiler-cache lease duration and heartbeat must be positive and heartbeat must leave a full renewal margin".into(),
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

/// Scoped compiler producer lease.
///
/// Call [`Self::renew`] during a long compile. Dropping an unpublished guard
/// fences the lease, including when the compile future is cancelled or fails.
pub struct CompilerLeaseGuard<'a, C: Clock> {
    service: &'a CompilerCacheService<C>,
    lease: Option<ProducerLease>,
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
    cas_budget: Arc<CompilerCasBudget>,
    metadata: Mutex<Connection>,
    journal: Mutex<LeaseManager<C>>,
}

/// Production compiler-cache service type used by the synchronous Docker
/// executor.
pub type ProductionCompilerCache = CompilerCacheService<velnor_action_journal::TokioClock>;

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
        let metadata_path = storage_root.join("metadata.sqlite");
        let metadata = Connection::open(&metadata_path)?;
        initialize_metadata(&metadata)?;
        let cas_budget = CompilerCasBudget::open(cas.root(), &metadata_path)?;
        let journal = LeaseManager::open(storage_root.join("journal.sqlite"), clock)?;
        Ok(Self {
            backend,
            storage_root,
            owner: config.owner,
            trust_class: config.trust_class,
            lease_duration_ms: config.lease_duration_ms,
            heartbeat_every_ms: config.heartbeat_every_ms,
            cas,
            cas_budget,
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

    /// Return a digest-identified, immutable output tree stored in this
    /// service's trust/backend namespace.
    pub fn store_output_tree(&self, root: &Path) -> Result<Digest, CacheError> {
        if !root.is_dir() || fs::symlink_metadata(root)?.file_type().is_symlink() {
            return Err(CacheError::InvalidOutput(format!(
                "compiler output root is not a regular directory: {}",
                root.display()
            )));
        }
        let mut entries = Vec::new();
        let mut budget = OutputTreeBudget::default();
        collect_output_tree(
            root,
            root,
            &self.cas,
            &mut entries,
            &mut budget,
            self.cas_budget.as_ref(),
        )?;
        Ok(self
            .cas
            .put_tree_with_budget(&TreeManifest { entries }, self.cas_budget.as_ref())?)
    }

    /// Materialize a previously stored output tree through a private staging
    /// directory, then atomically replace the destination directory.
    pub fn materialize_output_tree(
        &self,
        root_digest: &Digest,
        destination: &Path,
    ) -> Result<(), CacheError> {
        let parent = destination.parent().ok_or_else(|| {
            CacheError::InvalidOutput(format!(
                "compiler output destination has no parent: {}",
                destination.display()
            ))
        })?;
        fs::create_dir_all(parent)?;
        if let Ok(metadata) = fs::symlink_metadata(destination) {
            if metadata.file_type().is_symlink() {
                return Err(CacheError::InvalidOutput(format!(
                    "compiler output destination is a symlink: {}",
                    destination.display()
                )));
            }
            if !metadata.is_dir() {
                return Err(CacheError::InvalidOutput(format!(
                    "compiler output destination is not a directory: {}",
                    destination.display()
                )));
            }
        }
        let sequence = OUTPUT_STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let staging = parent.join(format!(
            ".{}.compiler-restore-{nonce}-{sequence}",
            destination
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("output")
        ));
        let backup = parent.join(format!(
            ".{}.compiler-backup-{nonce}-{sequence}",
            destination
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("output")
        ));
        let result = (|| {
            fs::create_dir(&staging)?;
            self.cas
                .materialize_subset(root_digest, SubsetSelector::RuntimeFiles, &staging)?;
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
                remove_output_path(&backup)?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    /// Begin a producer lease with cancellation-safe abandonment.
    pub async fn begin_guard(
        &self,
        key: &CompilerActionKey,
    ) -> Result<CompilerLeaseGuard<'_, C>, CacheError> {
        let lease = self.begin_sync(key)?;
        Ok(CompilerLeaseGuard {
            service: self,
            lease: Some(lease),
        })
    }

    /// Begin a producer lease for synchronous execution paths.
    ///
    /// The runner's Docker command boundary is deliberately synchronous and
    /// runs on a blocking thread. Keep the durable transition synchronous at
    /// its storage boundary instead of entering a nested Tokio runtime.
    pub fn begin_sync(&self, key: &CompilerActionKey) -> Result<ProducerLease, CacheError> {
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
            if let Err(cleanup_error) = journal.abandon(&lease) {
                eprintln!(
                    "forensics.lifecycle: compiler cache begin cleanup failed: {cleanup_error}"
                );
            }
            return Err(error.into());
        }
        Ok(lease)
    }

    /// Look up a validated compiler result from a synchronous execution path.
    pub fn lookup_sync(
        &self,
        key: &CompilerActionKey,
    ) -> Result<Option<CompilerResult>, CacheError> {
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
        self.validate_output_tree(&result.output_root)?;
        drop(metadata);
        drop(journal);
        Ok(Some(result))
    }

    /// Renew a producer lease from a synchronous heartbeat.
    pub fn renew_sync(&self, lease: &mut ProducerLease) -> Result<(), CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(&lease.action)?;
        let mut journal = self.lock_journal()?;
        journal.renew(lease)?;
        Ok(())
    }

    /// Abandon a producer lease from a synchronous cancellation/failure path.
    pub fn abandon_sync(&self, lease: &ProducerLease) -> Result<(), CacheError> {
        if self.backend == CompilerCacheBackend::Off {
            return Err(CacheError::Disabled);
        }
        self.ensure_trust_scope(&lease.action)?;
        let mut journal = self.lock_journal()?;
        journal.abandon(lease)?;
        Ok(())
    }

    pub fn publish_sync(
        &self,
        lease: ProducerLease,
        result: CompilerResult,
    ) -> Result<(), CacheError> {
        let lease_for_cleanup = lease.clone();
        let outcome = self.publish_sync_inner(lease, result);
        if outcome.is_err() {
            // Publication may fail after the result envelope or metadata has
            // been written. Fencing the producer remains mandatory; a
            // released lease simply makes this cleanup a harmless no-op.
            if let Err(cleanup_error) = self.abandon_sync(&lease_for_cleanup) {
                eprintln!(
                    "forensics.lifecycle: compiler cache publication cleanup failed: {cleanup_error}"
                );
            }
        }
        outcome
    }

    fn publish_sync_inner(
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
        self.validate_output_tree(&result.output_root)?;
        let result_bytes = serde_json::to_vec(&result)?;
        let result_digest = self
            .cas
            .put_with_budget(&result_bytes, self.cas_budget.as_ref())?;
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

impl<C: Clock> CompilerLeaseGuard<'_, C> {
    /// Return the current fencing token for diagnostics or a heartbeat.
    #[must_use]
    pub fn lease(&self) -> Option<&ProducerLease> {
        self.lease.as_ref()
    }

    /// Extend the durable lease before its heartbeat deadline.
    pub async fn renew(&mut self) -> Result<(), CacheError> {
        self.renew_sync()
    }

    /// Extend the durable lease before its heartbeat deadline synchronously.
    pub fn renew_sync(&mut self) -> Result<(), CacheError> {
        let lease = self.lease.as_mut().ok_or(CacheError::LeaseConsumed)?;
        self.service.renew_sync(lease)
    }

    /// Publish the result and consume the guard only after fencing succeeds.
    pub async fn publish(&mut self, result: CompilerResult) -> Result<(), CacheError> {
        self.publish_sync(result)
    }

    /// Publish the result and consume the guard only after fencing succeeds.
    pub fn publish_sync(&mut self, result: CompilerResult) -> Result<(), CacheError> {
        let lease = self
            .lease
            .as_ref()
            .ok_or(CacheError::LeaseConsumed)?
            .clone();
        self.service.publish_sync(lease, result)?;
        self.lease.take();
        Ok(())
    }
}

impl<C: Clock> Drop for CompilerLeaseGuard<'_, C> {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        // LeaseManager is synchronous, so Drop cannot await. Keep the
        // fail-closed fencing attempt here and surface a failure for recovery
        // telemetry instead of silently leaving the lease to expire.
        if let Err(error) = self.service.abandon_sync(&lease) {
            eprintln!("forensics.lifecycle: compiler cache lease abandon on drop failed: {error}");
        }
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
        self.lookup_sync(key)
    }

    async fn begin(&self, key: &CompilerActionKey) -> Result<ProducerLease, CacheError> {
        self.begin_sync(key)
    }

    async fn renew(&self, lease: &mut ProducerLease) -> Result<(), CacheError> {
        self.renew_sync(lease)
    }

    async fn abandon(&self, lease: &ProducerLease) -> Result<(), CacheError> {
        self.abandon_sync(lease)
    }

    async fn publish(
        &self,
        lease: ProducerLease,
        result: CompilerResult,
    ) -> Result<(), CacheError> {
        self.publish_sync(lease, result)
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
    #[error("compiler-cache producer lease has already been consumed")]
    LeaseConsumed,
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
    #[error("invalid compiler output: {0}")]
    InvalidOutput(String),
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
         );
         CREATE TABLE IF NOT EXISTS compiler_cache_quota (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             used_bytes INTEGER NOT NULL,
             inflight_bytes INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

fn directory_size(path: &Path) -> Result<u64, CacheError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(CacheError::InvalidOutput(format!(
            "compiler-cache CAS path is a symlink: {}",
            path.display()
        )));
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        total = total
            .checked_add(directory_size(&entry?.path())?)
            .ok_or_else(|| CacheError::InvalidOutput("compiler-cache CAS size overflow".into()))?;
    }
    Ok(total)
}

fn collect_output_tree(
    root: &Path,
    current: &Path,
    cas: &CasStore,
    entries: &mut Vec<TreeEntry>,
    budget: &mut OutputTreeBudget,
    cas_budget: &dyn BudgetCallback,
) -> Result<(), CacheError> {
    let mut children = fs::read_dir(current)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.path());
    for child in children {
        let path = child.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| CacheError::InvalidOutput(error.to_string()))?;
        let file_type = fs::symlink_metadata(&path)?.file_type();
        if file_type.is_dir() {
            collect_output_tree(root, &path, cas, entries, budget, cas_budget)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(CacheError::InvalidOutput(format!(
                "compiler output contains non-regular entry: {}",
                path.display()
            )));
        }
        let (bytes, mode) = read_output_file(root, relative)?;
        if bytes.len() as u64 > MAX_OUTPUT_TREE_BYTES {
            return Err(CacheError::InvalidOutput(format!(
                "compiler output file exceeds {MAX_OUTPUT_TREE_BYTES} bytes: {}",
                path.display()
            )));
        }
        budget.account(&path, bytes.len() as u64)?;
        entries.push(TreeEntry {
            path: relative.to_string_lossy().replace('\\', "/"),
            digest: cas.put_with_budget(&bytes, cas_budget)?,
            class: if mode & 0o111 != 0 {
                FileClass::Executable
            } else {
                FileClass::Runtime
            },
            mode,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn read_output_file(root: &Path, relative: &Path) -> Result<(Vec<u8>, u32), CacheError> {
    let directory_flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW;
    let mut parent = open_output_directory(root, directory_flags)?;
    let mut components = relative.components().peekable();
    let Some(std::path::Component::Normal(file_name)) = components.next_back() else {
        return Err(CacheError::InvalidOutput(
            "compiler output file has no name".into(),
        ));
    };
    for component in components {
        let std::path::Component::Normal(name) = component else {
            return Err(CacheError::InvalidOutput(format!(
                "compiler output path is not normal: {}",
                relative.display()
            )));
        };
        parent = std::fs::File::from(
            rustix::fs::openat(&parent, name, directory_flags, rustix::fs::Mode::empty())
                .map_err(std::io::Error::from)?,
        );
    }
    let file_flags =
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW;
    let mut file = std::fs::File::from(
        rustix::fs::openat(&parent, file_name, file_flags, rustix::fs::Mode::empty())
            .map_err(std::io::Error::from)?,
    );
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(CacheError::InvalidOutput(format!(
            "compiler output entry is not a regular file: {}",
            relative.display()
        )));
    }
    if metadata.len() > MAX_OUTPUT_TREE_BYTES {
        return Err(CacheError::InvalidOutput(format!(
            "compiler output file exceeds {MAX_OUTPUT_TREE_BYTES} bytes: {}",
            relative.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok((bytes, metadata.permissions().mode() & 0o777))
}

#[cfg(unix)]
fn open_output_directory(
    root: &Path,
    directory_flags: rustix::fs::OFlags,
) -> Result<std::fs::File, CacheError> {
    let base = if root.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut current = std::fs::File::from(
        rustix::fs::open(base, directory_flags, rustix::fs::Mode::empty())
            .map_err(std::io::Error::from)?,
    );
    for component in root.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => {
                current = std::fs::File::from(
                    rustix::fs::openat(&current, name, directory_flags, rustix::fs::Mode::empty())
                        .map_err(std::io::Error::from)?,
                );
            }
            std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                return Err(CacheError::InvalidOutput(
                    "compiler output root contains a non-normal path component".into(),
                ));
            }
        }
    }
    Ok(current)
}

#[cfg(not(unix))]
fn read_output_file(root: &Path, relative: &Path) -> Result<(Vec<u8>, u32), CacheError> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(CacheError::InvalidOutput(format!(
            "compiler output entry is not a regular file: {}",
            path.display()
        )));
    }
    Ok((fs::read(path)?, 0o644))
}

#[derive(Default)]
struct OutputTreeBudget {
    files: usize,
    bytes: u64,
}

impl OutputTreeBudget {
    fn account(&mut self, path: &Path, bytes: u64) -> Result<(), CacheError> {
        self.files = self.files.checked_add(1).ok_or_else(|| {
            CacheError::InvalidOutput("compiler output file count overflow".into())
        })?;
        if self.files > MAX_OUTPUT_TREE_FILES {
            return Err(CacheError::InvalidOutput(format!(
                "compiler output exceeds {MAX_OUTPUT_TREE_FILES} files at {}",
                path.display()
            )));
        }
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(|| {
            CacheError::InvalidOutput("compiler output byte count overflow".into())
        })?;
        if self.bytes > MAX_OUTPUT_TREE_BYTES {
            return Err(CacheError::InvalidOutput(format!(
                "compiler output exceeds {MAX_OUTPUT_TREE_BYTES} bytes at {}",
                path.display()
            )));
        }
        Ok(())
    }
}

fn remove_output_path(path: &Path) -> Result<(), CacheError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
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
    fn validate_output_tree(&self, root_digest: &Digest) -> Result<(), CacheError> {
        let manifest = self.cas.validate_tree(root_digest)?;
        if manifest
            .entries
            .iter()
            .any(|entry| !matches!(entry.class, FileClass::Executable | FileClass::Runtime))
        {
            return Err(CacheError::InvalidOutput(
                "compiler output tree contains a non-runtime entry".into(),
            ));
        }
        Ok(())
    }

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
            output_root: empty_tree_digest(),
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
        let cache =
            CompilerCacheService::open(config, declaration, TokioClock::new()).expect("service");
        cache
            .cas
            .put_tree(&TreeManifest {
                entries: Vec::new(),
            })
            .expect("empty output tree");
        cache
    }

    fn service_with_clock(
        directory: &TempDir,
        clock: TestClock,
    ) -> CompilerCacheService<TestClock> {
        let mut config = CompilerCacheConfig::new(directory.path(), "test-worker");
        config.policy = CompilerCachePolicy::Kache;
        config.lease_duration_ms = 30;
        config.heartbeat_every_ms = 10;
        let cache = CompilerCacheService::open(config, WrapperDeclaration::default(), clock)
            .expect("service");
        cache
            .cas
            .put_tree(&TreeManifest {
                entries: Vec::new(),
            })
            .expect("empty output tree");
        cache
    }

    fn empty_tree_digest() -> Digest {
        Digest::from_bytes(
            &canonical_json_bytes(&TreeManifest {
                entries: Vec::new(),
            })
            .expect("tree JSON"),
        )
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
    async fn publication_rejects_a_missing_output_tree() {
        let directory = tempfile::tempdir().expect("tempdir");
        let cache = service(
            &directory,
            CompilerCachePolicy::Kache,
            WrapperDeclaration::default(),
        );
        let action_key = key(12);
        let lease = cache.begin(&action_key).await.expect("lease");
        let mut result = result(action_key.clone());
        result.output_root = Digest::from_bytes(b"missing-output-tree");
        assert!(matches!(
            cache.publish(lease, result).await,
            Err(CacheError::Cas(CasError::Absent { .. }))
        ));
        assert!(cache.lookup(&action_key).await.expect("lookup").is_none());
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
    async fn lease_guard_renews_and_abandons_on_drop() {
        let directory = tempfile::tempdir().expect("tempdir");
        let clock = TestClock::default();
        let cache = service_with_clock(&directory, clock.clone());
        let action_key = key(7);
        let mut guard = cache.begin_guard(&action_key).await.expect("lease guard");
        assert_eq!(guard.lease().map(|lease| lease.generation), Some(1));

        clock.advance(10);
        guard.renew().await.expect("renew lease");
        assert_eq!(
            guard.lease().map(|lease| lease.expires_at),
            Some(LogicalInstant::from_millis(40))
        );
        drop(guard);

        let journal = cache.lock_journal().expect("journal lock");
        assert_eq!(
            journal.lease_status(&action_key).expect("lease status"),
            Some(velnor_action_journal::LeaseStatus::Abandoned)
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
        let journal = cache.lock_journal().expect("journal lock");
        assert_eq!(
            journal.lease_status(&action_key).expect("lease status"),
            Some(velnor_action_journal::LeaseStatus::Abandoned)
        );
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
        trusted
            .cas
            .put_tree(&TreeManifest {
                entries: Vec::new(),
            })
            .expect("empty output tree");
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
    fn heartbeat_requires_a_full_renewal_margin() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut config = CompilerCacheConfig::new(directory.path(), "test-worker");
        config.lease_duration_ms = 20;
        config.heartbeat_every_ms = 10;
        assert!(matches!(
            CompilerCacheService::open(config, WrapperDeclaration::default(), TokioClock::new()),
            Err(CacheError::InvalidConfig(_))
        ));
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

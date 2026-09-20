//! Host-wide `max_jobs=N` permit ledger shared by every local lane.
//!
//! One top-level job lifecycle holds exactly one permit, from the point
//! capacity is committed for acquisition (or assignable readiness) until
//! terminal work and owned cleanup are confirmed. Reserved, acquiring,
//! provisioning, assignable, running, cleaning, and uncertain states are
//! all included in the single count: row presence is occupancy, whatever
//! the state. Offered but unacquired work is durable queued demand, not
//! occupied capacity. Demand ordering and permit acquisition share one
//! immediate transaction, so no lane can spend a permit ahead of older
//! eligible work.
//!
//! Lanes ([`PermitLane`]) share the one `N` and the same oldest-first demand
//! queue. There is no per-lane reservation.
//!
//! Durability and crash recovery:
//!
//! * The ledger is a host-wide SQLite database. Every daemon on the host
//!   must resolve to the same file; multi-process contention is bounded by
//!   a busy timeout, and every mutation runs in an immediate transaction.
//! * Mutations are fenced by the mutable host epoch, while each acquisition
//!   also has an immutable lease generation. Cleanup can release only the
//!   exact lease it owns; a database trigger rejects holder-only deletes from
//!   pre-migration writers. Old daemon binaries must be stopped before this
//!   schema migration is deployed.
//! * Capacity is advertised only after reconciliation:
//!   [`PermitLedger::advertised_free`] returns `None` until
//!   [`PermitLedger::reconcile`] has run in the current epoch.
//!   Reconciliation never deletes: observed-but-unrecorded work is adopted
//!   as counted occupancy, and recorded-but-unobserved work is marked
//!   [`PermitState::Uncertain`] (still counted). A cleanup failure retains
//!   its visible reservation; occupied work is never erased by resetting a
//!   semaphore.
//! * Acquisition is idempotent per holder: a duplicate delivery for the
//!   same holder returns [`AcquireOutcome::AlreadyHeld`] without spending
//!   a second permit. A fresh grant atomically changes the oldest eligible
//!   demand to granted while inserting its permit.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};

/// SQLite busy timeout for multi-process ledger contention.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// One host-authoritative config path. Daemons must not derive the roster
/// location from `--permit-ledger`: doing so would let two differently
/// configured processes each accept a self-consistent but disjoint ledger.
pub const HOST_DEMAND_SOURCE_ROSTER_PATH: &str = "/etc/velnor/permit-ledger.sources";
const HOST_ROSTER_REPAIR_HINT: &str = "install a root-owned readable regular file with no group/world write at /etc/velnor/permit-ledger.sources; include one permit-ledger entry, every state-db, every Scale Set demand-db, and every native-slot marker root, then restart all daemons";

/// An unrefreshed offer this old is no longer eligible to block another
/// lane. Active queues refresh on redelivery; the original age remains in
/// the row so a later redelivery keeps its place.
pub const DEMAND_STALE_AFTER_SECS: u64 = 300;

/// A local lane sharing the one host-wide `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermitLane {
    /// Velnor-native daemon/slot acquisitions.
    Native,
    /// Official-runner Scale Set acquisitions.
    ScaleSet,
}

impl PermitLane {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::ScaleSet => "scale-set",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "native" => Some(Self::Native),
            "scale-set" => Some(Self::ScaleSet),
            _ => None,
        }
    }
}

/// Lifecycle of one durable demand. Only `Eligible` rows take part in the
/// oldest-first admission decision. A granted row owns a permit; terminal
/// and cancelled rows cannot block later work or be revived by redelivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DemandState {
    Eligible,
    Granted,
    Terminal,
    Cancelled,
}

impl DemandState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Granted => "granted",
            Self::Terminal => "terminal",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "eligible" => Some(Self::Eligible),
            "granted" => Some(Self::Granted),
            "terminal" => Some(Self::Terminal),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// One durable demand observation. The complete first-seen instant and
/// `sequence` define queue order; redelivery only refreshes `updated_unix`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermitDemand {
    pub holder: String,
    pub lane: PermitLane,
    pub scope: String,
    pub first_seen_unix: u64,
    /// Nanoseconds within `first_seen_unix`, retained for RFC 3339 sources.
    pub first_seen_subsec_nanos: u32,
    pub sequence: i64,
    pub state: DemandState,
    pub updated_unix: u64,
}

/// One demand observation to mirror into the host-wide queue. Batch writes
/// preserve input sequence ties and commit all observations together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermitDemandObservation<'a> {
    pub holder: &'a str,
    pub lane: PermitLane,
    pub scope: &'a str,
    pub first_seen_unix: u64,
    pub first_seen_subsec_nanos: u32,
    pub observed_unix: u64,
}

/// Metadata retained for startup replay between global publication and the
/// Scale Set demand-store commit. It contains the fields the local durable
/// row needs; acquire URLs and credentials are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaleSetDemandPublication {
    pub request_id: i64,
    pub scale_set_id: i32,
    pub repo_owner: String,
    pub repo_name: String,
    pub job_id_hash: i64,
    pub labels_hash: String,
    pub event_name: String,
}

/// Durable global-first publication awaiting a source-store commit or
/// startup replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingScaleSetDemandPublication {
    pub holder: String,
    pub source_db: PathBuf,
    pub generation: u64,
    pub first_seen_unix: u64,
    pub first_seen_subsec_nanos: u32,
    pub publication: ScaleSetDemandPublication,
}

/// Authoritative host-wide roster of daemon state databases, Scale Set
/// demand databases, and native marker roots. The fingerprint is derived from
/// the sorted canonical paths, so every daemon sharing a permit ledger must
/// read the same complete source set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermitDemandSourceRoster {
    pub fingerprint: String,
    /// Canonical host-wide ledger identity declared by the roster file.
    pub ledger_path: Option<PathBuf>,
    /// Every daemon operational database path declared by the host roster.
    pub state_db_paths: Vec<PathBuf>,
    /// Scale Set demand databases declared by the host roster.
    pub paths: Vec<PathBuf>,
    /// Every native slot directory whose in-flight marker participates in
    /// host-wide epoch reconciliation.
    pub native_marker_dirs: Vec<PathBuf>,
}

impl PermitDemandSourceRoster {
    /// Build a host roster from every daemon state database, Scale Set demand
    /// database, and native slot marker directory.
    pub fn from_host_paths(
        state_db_paths: impl IntoIterator<Item = PathBuf>,
        paths: impl IntoIterator<Item = PathBuf>,
        native_marker_dirs: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, LedgerError> {
        Self::from_complete_paths(None, state_db_paths, paths, native_marker_dirs)
    }

    /// Build a complete roster and bind it to one canonical host ledger.
    pub fn for_ledger(
        ledger_path: &Path,
        state_db_paths: impl IntoIterator<Item = PathBuf>,
        paths: impl IntoIterator<Item = PathBuf>,
        native_marker_dirs: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, LedgerError> {
        let ledger_path = canonicalize_ledger_identity(ledger_path)?;
        Self::from_complete_paths(
            Some(&ledger_path),
            state_db_paths,
            paths,
            native_marker_dirs,
        )
    }

    fn from_complete_paths(
        ledger_path: Option<&Path>,
        state_db_paths: impl IntoIterator<Item = PathBuf>,
        paths: impl IntoIterator<Item = PathBuf>,
        native_marker_dirs: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, LedgerError> {
        let mut canonical_state_dbs = Vec::new();
        for path in state_db_paths {
            let canonical_path = path.canonicalize().map_err(|error| {
                LedgerError::DemandSourcesUnready(format!(
                    "canonicalize daemon state database {}: {error}",
                    path.display()
                ))
            })?;
            if !canonical_path.is_file() {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "daemon state database {} is not a file",
                    canonical_path.display()
                )));
            }
            if canonical_path.to_str().is_none() {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "daemon state database path is not valid UTF-8: {}",
                    canonical_path.display()
                )));
            }
            canonical_state_dbs.push(canonical_path);
        }
        if canonical_state_dbs.is_empty() {
            return Err(LedgerError::DemandSourcesUnready(
                "host roster must explicitly list every daemon state database".to_owned(),
            ));
        }
        canonical_state_dbs.sort();
        if canonical_state_dbs
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(LedgerError::DemandSourcesUnready(
                "host roster contains duplicate daemon state database paths".to_owned(),
            ));
        }
        let mut canonical = Vec::new();
        for path in paths {
            let canonical_path = path.canonicalize().map_err(|error| {
                LedgerError::DemandSourcesUnready(format!(
                    "canonicalize demand source {}: {error}",
                    path.display()
                ))
            })?;
            if !canonical_path.is_file() {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "demand source {} is not a file",
                    canonical_path.display()
                )));
            }
            if canonical_path.to_str().is_none() {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "demand source path is not valid UTF-8: {}",
                    canonical_path.display()
                )));
            }
            canonical.push(canonical_path);
        }
        let mut canonical_markers = Vec::new();
        for path in native_marker_dirs {
            let canonical_path = path.canonicalize().map_err(|error| {
                LedgerError::DemandSourcesUnready(format!(
                    "canonicalize native marker directory {}: {error}",
                    path.display()
                ))
            })?;
            if !canonical_path.is_dir() {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "native marker path {} is not a directory",
                    canonical_path.display()
                )));
            }
            if canonical_path.to_str().is_none() {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "native marker path is not valid UTF-8: {}",
                    canonical_path.display()
                )));
            }
            canonical_markers.push(canonical_path);
        }
        canonical.sort();
        if canonical.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(LedgerError::DemandSourcesUnready(
                "demand source roster contains duplicate paths".to_owned(),
            ));
        }
        if ledger_path.is_some_and(|ledger| canonical.iter().any(|source| source == ledger)) {
            return Err(LedgerError::DemandSourcesUnready(
                "roster lists the permit ledger itself as a demand source".to_owned(),
            ));
        }
        canonical_markers.sort();
        if canonical_markers.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(LedgerError::DemandSourcesUnready(
                "demand source roster contains duplicate native marker directories".to_owned(),
            ));
        }
        let mut hasher = Sha256::new();
        hasher.update(b"velnor-host-admission-roster-v2\0");
        if let Some(ledger_path) = ledger_path {
            let text = ledger_path.to_str().ok_or_else(|| {
                LedgerError::DemandSourcesUnready(format!(
                    "host permit ledger path is not valid UTF-8: {}",
                    ledger_path.display()
                ))
            })?;
            hasher.update(b"permit-ledger\0");
            hasher.update((text.len() as u64).to_be_bytes());
            hasher.update(text.as_bytes());
        }
        for path in &canonical_state_dbs {
            let text = path.to_str().ok_or_else(|| {
                LedgerError::DemandSourcesUnready(format!(
                    "daemon state database path is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            hasher.update(b"state-db\0");
            hasher.update((text.len() as u64).to_be_bytes());
            hasher.update(text.as_bytes());
        }
        for path in &canonical {
            let text = path.to_str().ok_or_else(|| {
                LedgerError::DemandSourcesUnready(format!(
                    "demand source path is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            hasher.update(b"demand\0");
            hasher.update((text.len() as u64).to_be_bytes());
            hasher.update(text.as_bytes());
        }
        for path in &canonical_markers {
            let text = path.to_str().ok_or_else(|| {
                LedgerError::DemandSourcesUnready(format!(
                    "native marker path is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            hasher.update(b"native-slot\0");
            hasher.update((text.len() as u64).to_be_bytes());
            hasher.update(text.as_bytes());
        }
        Ok(Self {
            fingerprint: format!("sha256:{}", digest_hex(&hasher.finalize())),
            ledger_path: ledger_path.map(Path::to_path_buf),
            state_db_paths: canonical_state_dbs,
            paths: canonical,
            native_marker_dirs: canonical_markers,
        })
    }
}

fn canonicalize_ledger_identity(path: &Path) -> Result<PathBuf, LedgerError> {
    if path.exists() {
        return path.canonicalize().map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "canonicalize host permit ledger {}: {error}",
                path.display()
            ))
        });
    }
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(LedgerError::DemandSourcesUnready(format!(
            "host permit ledger {} is a dangling symlink or cannot be resolved",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        LedgerError::DemandSourcesUnready(format!(
            "host permit ledger path has no file name: {}",
            path.display()
        ))
    })?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "canonicalize host permit ledger directory {}: {error}",
            parent.display()
        ))
    })?;
    if !canonical_parent.is_dir() {
        return Err(LedgerError::DemandSourcesUnready(format!(
            "host permit ledger parent {} is not a directory",
            canonical_parent.display()
        )));
    }
    Ok(canonical_parent.join(file_name))
}

/// Lifecycle state of one held permit. Every state counts toward `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermitState {
    Reserved,
    Acquiring,
    Provisioning,
    Assignable,
    Running,
    Cleaning,
    Uncertain,
}

impl PermitState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Acquiring => "acquiring",
            Self::Provisioning => "provisioning",
            Self::Assignable => "assignable",
            Self::Running => "running",
            Self::Cleaning => "cleaning",
            Self::Uncertain => "uncertain",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "reserved" => Some(Self::Reserved),
            "acquiring" => Some(Self::Acquiring),
            "provisioning" => Some(Self::Provisioning),
            "assignable" => Some(Self::Assignable),
            "running" => Some(Self::Running),
            "cleaning" => Some(Self::Cleaning),
            "uncertain" => Some(Self::Uncertain),
            _ => None,
        }
    }
}

/// One counted occupant of the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermitHolder {
    pub holder: String,
    pub lane: PermitLane,
    pub state: PermitState,
    pub acquired_unix: u64,
    pub updated_unix: u64,
    pub generation: u64,
    /// Immutable identity for this acquisition, independent of host epoch.
    pub lease_generation: u64,
    /// Host pid of the acquiring process, when the lane records one.
    /// Same-holder redelivery may adopt a dead attempt; startup never uses
    /// local pid or root evidence alone to erase another daemon's row.
    pub pid: Option<u32>,
    /// Stable process identity captured with `pid`. Legacy rows without this
    /// value cannot prove that a reused PID still names the acquiring owner.
    pub pid_identity: Option<String>,
}

/// Outcome of [`PermitLedger::adopt_if_pid_dead_with_lease_generation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptOutcome {
    /// The dead attempt's row was adopted: same holder, new pid, new state.
    Adopted,
    /// The exact previous process still owns the exact Acquiring lease.
    LiveHolder,
    /// Identity, demand, lane, or lifecycle state does not prove a safe
    /// retry. Admission must remain closed for this request.
    NotAdoptable,
    /// The row vanished between calls; retry the acquire.
    Missing,
    /// The caller fenced on a stale generation; re-read and retry.
    StaleGeneration,
}

/// Outcome of [`PermitLedger::acquire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireOutcome {
    /// A fresh permit was granted.
    Acquired,
    /// The holder already holds a permit (duplicate delivery); no second
    /// permit was spent.
    AlreadyHeld,
    /// `occupied >= max_jobs`; no permit was granted.
    Full,
    /// Capacity is available, but an older eligible demand must acquire
    /// first. The demand remains durable and keeps its original age.
    Deferred,
    /// Shared admission is closed until its source roster and current epoch
    /// have been attested.
    NotReady,
    /// This demand was already terminal or cancelled and cannot be revived.
    Closed,
    /// The caller fenced on a stale generation; re-read and retry.
    StaleGeneration,
    /// No `max_jobs` was ever configured; refusing rather than guessing.
    NotConfigured,
}

/// What [`PermitLedger::reconcile`] did. Reconciliation adopts and marks;
/// it never deletes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Observed holders that had no row; adopted as counted occupancy.
    pub adopted: Vec<String>,
    /// Recorded holders that were not observed; marked uncertain (counted).
    pub marked_uncertain: Vec<String>,
    /// Recorded holders confirmed by observation.
    pub confirmed: Vec<String>,
}

/// Errors from ledger operations.
#[derive(Debug)]
pub enum LedgerError {
    Storage(rusqlite::Error),
    UnknownLane(String),
    UnknownState(String),
    UnknownDemandState(String),
    UnknownHolder(String),
    DemandLaneMismatch {
        holder: String,
        expected: PermitLane,
        seen: String,
    },
    DemandScopeMismatch {
        holder: String,
        expected: String,
        seen: String,
    },
    StaleGeneration {
        expected: u64,
        seen: u64,
    },
    DemandSourcesUnready(String),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "permit ledger storage: {error}"),
            Self::UnknownLane(lane) => write!(f, "permit ledger holds unknown lane {lane:?}"),
            Self::UnknownState(state) => write!(f, "permit ledger holds unknown state {state:?}"),
            Self::UnknownDemandState(state) => {
                write!(f, "permit ledger holds unknown demand state {state:?}")
            }
            Self::UnknownHolder(holder) => {
                write!(f, "permit ledger holds no permit for {holder:?}")
            }
            Self::DemandLaneMismatch {
                holder,
                expected,
                seen,
            } => write!(
                f,
                "permit demand {holder:?} belongs to lane {seen:?}, not {expected:?}"
            ),
            Self::DemandScopeMismatch {
                holder,
                expected,
                seen,
            } => write!(
                f,
                "permit demand {holder:?} belongs to scope {seen:?}, not expected scope {expected:?}"
            ),
            Self::StaleGeneration { expected, seen } => write!(
                f,
                "permit ledger generation moved from {seen} to {expected}; re-read and retry"
            ),
            Self::DemandSourcesUnready(reason) => {
                write!(f, "permit demand source roster is not ready: {reason}")
            }
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<rusqlite::Error> for LedgerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

/// Current Unix time in seconds, used for durable demand and permit
/// observation timestamps.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Current Unix time as seconds plus exact subsecond nanoseconds. Callers
/// should sample this once at demand ingress and carry both parts through
/// ledger open/observation so crossing a second boundary cannot reorder FIFO.
pub fn unix_now_parts() -> (u64, u32) {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| (elapsed.as_secs(), elapsed.subsec_nanos()))
        .unwrap_or((0, 0))
}

static PROCESS_ROSTERS: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();

/// The single host-authoritative roster location. The argument is retained
/// for callers that already hold a ledger path, but is deliberately ignored.
#[must_use]
pub fn demand_source_roster_path(_ledger_path: &Path) -> PathBuf {
    #[cfg(any(test, feature = "test-support"))]
    {
        // Test builds isolate parallel fixtures beside their unique ledger.
        let mut name = _ledger_path.as_os_str().to_os_string();
        name.push(".sources");
        return PathBuf::from(name);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        PathBuf::from(HOST_DEMAND_SOURCE_ROSTER_PATH)
    }
}

/// Stable sidecar lock path that roster writers must lock exclusively while
/// replacing the host roster. Readers hold a shared lock through the ledger
/// transaction that uses the roster snapshot.
#[must_use]
pub fn demand_source_roster_lock_path(ledger_path: &Path) -> PathBuf {
    let mut name = demand_source_roster_path(ledger_path)
        .as_os_str()
        .to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

struct DemandSourceRosterLock {
    _file: File,
}

fn lock_demand_source_roster(
    ledger_path: &Path,
    operation: rustix::fs::FlockOperation,
) -> Result<DemandSourceRosterLock, LedgerError> {
    let lock_path = demand_source_roster_lock_path(ledger_path);
    #[cfg(any(test, feature = "test-support"))]
    let (flags, mode) = (
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::from_raw_mode(0o600),
    );
    #[cfg(not(any(test, feature = "test-support")))]
    let (flags, mode) = (
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    );
    let descriptor = rustix::fs::open(&lock_path, flags, mode).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "open host roster lock {}: {error}; create a stable root-owned regular file and restart all daemons",
            lock_path.display()
        ))
    })?;
    let file: File = descriptor.into();
    let opened = file.metadata().map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "inspect host roster lock {}: {error}",
            lock_path.display()
        ))
    })?;
    let named = std::fs::symlink_metadata(&lock_path).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "recheck host roster lock {}: {error}",
            lock_path.display()
        ))
    })?;
    if !opened.file_type().is_file() || !named.file_type().is_file() {
        return Err(LedgerError::DemandSourcesUnready(format!(
            "host roster lock {} must be a stable regular file, not a symlink",
            lock_path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != named.dev() || opened.ino() != named.ino() {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "host roster lock {} changed while being opened",
                lock_path.display()
            )));
        }
        #[cfg(not(any(test, feature = "test-support")))]
        if opened.uid() != 0 || opened.mode() & 0o022 != 0 || opened.mode() & 0o444 == 0 {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "host roster lock {} must be root-owned, readable, and not group/world writable",
                lock_path.display()
            )));
        }
    }
    rustix::fs::flock(&file, operation).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "lock host roster {}: {error}",
            lock_path.display()
        ))
    })?;
    Ok(DemandSourceRosterLock { _file: file })
}

/// Read the host roster. It declares the canonical permit ledger, every
/// daemon state database, Scale Set demand database, and native slot
/// directory; relative paths resolve beside the fixed roster file.
pub fn read_demand_source_roster(
    ledger_path: &Path,
) -> Result<PermitDemandSourceRoster, LedgerError> {
    let _lock = lock_demand_source_roster(ledger_path, rustix::fs::FlockOperation::LockShared)?;
    let roster_path = demand_source_roster_path(ledger_path);
    read_demand_source_roster_at(ledger_path, &roster_path)
}

fn read_demand_source_roster_at(
    ledger_path: &Path,
    roster_path: &Path,
) -> Result<PermitDemandSourceRoster, LedgerError> {
    #[cfg(not(any(test, feature = "test-support")))]
    let contents = read_host_roster(roster_path)?;
    #[cfg(any(test, feature = "test-support"))]
    let contents = std::fs::read_to_string(roster_path).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!("read roster {}: {error}", roster_path.display()))
    })?;
    let base = roster_path.parent().unwrap_or_else(|| Path::new("."));
    let mut state_db_paths = Vec::new();
    let mut paths = Vec::new();
    let mut native_marker_dirs = Vec::new();
    let mut declared_ledger: Option<PathBuf> = None;
    for (line_index, line) in contents.lines().enumerate() {
        let entry = line.trim();
        if entry.is_empty() || entry.starts_with('#') {
            continue;
        }
        let (kind, raw_path) = if let Some(path) = entry.strip_prefix("permit-ledger ") {
            ("permit-ledger", path.trim())
        } else if let Some(path) = entry.strip_prefix("permit-ledger=") {
            ("permit-ledger", path.trim())
        } else if let Some(path) = entry.strip_prefix("native-slot ") {
            ("native-slot", path.trim())
        } else if let Some(path) = entry.strip_prefix("native-slot=") {
            ("native-slot", path.trim())
        } else if let Some(path) = entry.strip_prefix("demand-db ") {
            ("demand", path.trim())
        } else if let Some(path) = entry.strip_prefix("demand-db=") {
            ("demand", path.trim())
        } else if let Some(path) = entry.strip_prefix("state-db ") {
            ("state-db", path.trim())
        } else if let Some(path) = entry.strip_prefix("state-db=") {
            ("state-db", path.trim())
        } else {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "unknown roster entry on line {} in {}",
                line_index + 1,
                roster_path.display()
            )));
        };
        let path = PathBuf::from(raw_path);
        let resolved = if path.is_absolute() {
            path
        } else {
            base.join(path)
        };
        match kind {
            "permit-ledger" => {
                if declared_ledger.replace(resolved).is_some() {
                    return Err(LedgerError::DemandSourcesUnready(format!(
                        "roster {} declares the permit ledger more than once",
                        roster_path.display()
                    )));
                }
            }
            "native-slot" => native_marker_dirs.push(resolved),
            "state-db" => state_db_paths.push(resolved),
            "demand" => paths.push(resolved),
            _ => {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "unknown roster entry on line {} in {}",
                    line_index + 1,
                    roster_path.display()
                )));
            }
        }
        if paths.len() + native_marker_dirs.len() + state_db_paths.len() > 8192 {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "roster {} has more than 8192 entries (line {})",
                roster_path.display(),
                line_index + 1
            )));
        }
    }
    let declared_ledger = declared_ledger.ok_or_else(|| {
        LedgerError::DemandSourcesUnready(format!(
            "roster {} is missing a permit-ledger entry",
            roster_path.display()
        ))
    })?;
    let declared_ledger = canonicalize_ledger_identity(&declared_ledger)?;
    let actual_ledger = canonicalize_ledger_identity(ledger_path)?;
    if declared_ledger != actual_ledger {
        return Err(LedgerError::DemandSourcesUnready(format!(
            "roster {} names ledger {}, but this daemon resolved {}",
            roster_path.display(),
            declared_ledger.display(),
            actual_ledger.display()
        )));
    }
    PermitDemandSourceRoster::from_complete_paths(
        Some(&actual_ledger),
        state_db_paths,
        paths,
        native_marker_dirs,
    )
}

#[cfg(not(any(test, feature = "test-support")))]
fn read_host_roster(path: &Path) -> Result<String, LedgerError> {
    use std::io::Read;

    let expected_path = Path::new(HOST_DEMAND_SOURCE_ROSTER_PATH);
    let canonical = path.canonicalize().map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "host roster {} is missing or unreadable: {error}; {HOST_ROSTER_REPAIR_HINT}",
            path.display(),
        ))
    })?;
    if canonical != expected_path {
        return Err(LedgerError::DemandSourcesUnready(format!(
            "host roster path must resolve to {}, got {}",
            expected_path.display(),
            canonical.display()
        )));
    }
    let link_metadata = std::fs::symlink_metadata(path).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "inspect host roster {}: {error}; {HOST_ROSTER_REPAIR_HINT}",
            path.display(),
        ))
    })?;
    if !link_metadata.file_type().is_file() {
        return Err(LedgerError::DemandSourcesUnready(format!(
            "host roster {} must be a regular non-symlink file",
            path.display()
        )));
    }
    let mut file = std::fs::File::open(path).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!("open host roster {}: {error}", path.display()))
    })?;
    let metadata = file.metadata().map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "inspect open host roster {}: {error}",
            path.display()
        ))
    })?;
    let final_link_metadata = std::fs::symlink_metadata(path).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "recheck host roster {}: {error}",
            path.display()
        ))
    })?;
    if !final_link_metadata.file_type().is_file() {
        return Err(LedgerError::DemandSourcesUnready(format!(
            "host roster {} changed to a symlink or non-file while being opened",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.dev() != final_link_metadata.dev()
            || metadata.ino() != final_link_metadata.ino()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o444 == 0
        {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "host roster {} must be a stable, root-owned, readable regular file that is not group/world writable",
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    {
        return Err(LedgerError::DemandSourcesUnready(
            "the host roster ownership check requires a Unix host".to_owned(),
        ));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!("read host roster {}: {error}", path.display()))
    })?;
    Ok(contents)
}

fn process_roster_identity(ledger_path: &Path) -> Option<String> {
    let rosters = PROCESS_ROSTERS.get_or_init(|| Mutex::new(HashMap::new()));
    rosters
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(ledger_path)
        .cloned()
}

fn remember_process_roster_identity(
    ledger_path: &Path,
    fingerprint: &str,
) -> Result<(), LedgerError> {
    let rosters = PROCESS_ROSTERS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut rosters = rosters
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match rosters.get(ledger_path) {
        Some(existing) if existing != fingerprint => Err(LedgerError::DemandSourcesUnready(
            "the permit demand roster changed while this daemon process was running; restart all daemons after installing the new roster".to_owned(),
        )),
        Some(_) => Ok(()),
        None => {
            rosters.insert(ledger_path.to_path_buf(), fingerprint.to_owned());
            Ok(())
        }
    }
}

/// Host-wide `max_jobs=N` permit ledger.
#[derive(Debug)]
pub struct PermitLedger {
    path: PathBuf,
    conn: Connection,
    source_roster_path: PathBuf,
    expected_roster_identity: Option<String>,
}

/// Move the former native-only queue into the host-wide queue before any
/// admission operation can observe the new schema. The transaction makes
/// the migration safe when multiple daemon processes open the ledger at
/// once. Native `first_seen_unix` and relative tie order survive the move;
/// the new global sequence starts after rows already present in the shared
/// queue.
fn migrate_legacy_native_demand(conn: &mut Connection) -> Result<(), LedgerError> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'native_demand'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'native_demand'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        tx.commit()?;
        return Ok(());
    }
    let legacy_rows: Vec<(String, String, i64, i64, String, i64)> = {
        let mut select = tx.prepare(
            "SELECT request_id, scope, first_seen_unix, sequence, state, updated_unix
             FROM native_demand ORDER BY first_seen_unix, sequence, request_id",
        )?;
        select
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?
    };
    let mut next_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM permit_demands",
        [],
        |row| row.get(0),
    )?;
    for (request_id, scope, first_seen, _legacy_sequence, raw_state, updated) in legacy_rows {
        let state = DemandState::parse(&raw_state)
            .ok_or_else(|| LedgerError::UnknownDemandState(raw_state.clone()))?;
        let holder = format!("native/{request_id}");
        let already_migrated: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM permit_demands WHERE holder = ?1)",
            params![holder],
            |row| row.get(0),
        )?;
        if already_migrated {
            continue;
        }
        let sequence = next_sequence;
        next_sequence = next_sequence.saturating_add(1);
        tx.execute(
            "INSERT INTO permit_demands
             (holder, lane, scope, first_seen_unix, sequence, state, updated_unix)
             VALUES (?1, 'native', ?2, ?3, ?4, ?5, ?6)",
            params![holder, scope, first_seen, sequence, state.as_str(), updated],
        )?;
    }
    tx.execute_batch("DROP TABLE native_demand;")?;
    tx.commit()?;
    Ok(())
}

fn read_demand(conn: &Connection, holder: &str) -> Result<Option<PermitDemand>, LedgerError> {
    let row: Option<(String, String, String, i64, i64, i64, String, i64)> = conn
        .query_row(
            "SELECT holder, lane, scope, first_seen_unix, first_seen_subsec_nanos,
                    sequence, state, updated_unix
             FROM permit_demands WHERE holder = ?1",
            params![holder],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(
            holder,
            raw_lane,
            scope,
            first_seen,
            first_seen_subsec_nanos,
            sequence,
            raw_state,
            updated,
        )| {
            let lane = PermitLane::parse(&raw_lane)
                .ok_or_else(|| LedgerError::UnknownLane(raw_lane.clone()))?;
            let state = DemandState::parse(&raw_state)
                .ok_or_else(|| LedgerError::UnknownDemandState(raw_state.clone()))?;
            Ok(PermitDemand {
                holder,
                lane,
                scope,
                first_seen_unix: first_seen.max(0) as u64,
                first_seen_subsec_nanos: first_seen_subsec_nanos.clamp(0, 999_999_999) as u32,
                sequence,
                state,
                updated_unix: updated.max(0) as u64,
            })
        },
    )
    .transpose()
}

fn check_demand_lane(
    holder: &str,
    demand: &PermitDemand,
    lane: PermitLane,
) -> Result<(), LedgerError> {
    if demand.lane == lane {
        return Ok(());
    }
    Err(LedgerError::DemandLaneMismatch {
        holder: holder.to_owned(),
        expected: lane,
        seen: demand.lane.as_str().to_owned(),
    })
}

fn demand_precision_schema_is_current(conn: &Connection) -> Result<bool, LedgerError> {
    let has_subsecond = {
        let mut statement = conn.prepare("PRAGMA table_info(permit_demands)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        let mut found = false;
        for column in columns {
            if column? == "first_seen_subsec_nanos" {
                found = true;
                break;
            }
        }
        found
    };
    if !has_subsecond {
        return Ok(false);
    }
    let index_table: Option<String> = conn
        .query_row(
            "SELECT tbl_name FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_permit_demands_oldest'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if index_table.as_deref() != Some("permit_demands") {
        return Ok(false);
    }
    let mut statement = conn.prepare("PRAGMA index_info('idx_permit_demands_oldest')")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(2))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns.iter().map(String::as_str).eq([
        "state",
        "first_seen_unix",
        "first_seen_subsec_nanos",
        "sequence",
    ]))
}

/// Add exact fractional age to ledgers created before RFC 3339 demand
/// timestamps were mirrored. Healthy ledgers skip the write transaction;
/// concurrent first opens recheck under the lock and converge once.
fn ensure_demand_precision_schema(conn: &mut Connection) -> Result<(), LedgerError> {
    if demand_precision_schema_is_current(conn)? {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if demand_precision_schema_is_current(&tx)? {
        tx.commit()?;
        return Ok(());
    }
    let has_subsecond = {
        let mut statement = tx.prepare("PRAGMA table_info(permit_demands)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        let mut found = false;
        for column in columns {
            if column? == "first_seen_subsec_nanos" {
                found = true;
                break;
            }
        }
        found
    };
    if !has_subsecond {
        tx.execute_batch(
            "ALTER TABLE permit_demands
             ADD COLUMN first_seen_subsec_nanos INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_permit_demands_oldest;
         CREATE INDEX idx_permit_demands_oldest
         ON permit_demands (state, first_seen_unix, first_seen_subsec_nanos, sequence);",
    )?;
    tx.commit()?;
    Ok(())
}

const PERMIT_LEASE_INSERT_TRIGGER_SQL: &str = "CREATE TRIGGER trg_permits_lease_insert_fence
    BEFORE INSERT ON permits
    WHEN COALESCE(NEW.lease_generation, 0) <= 0
      OR NEW.admission_roster_hash IS NULL
    BEGIN
        SELECT RAISE(ABORT, 'permit lease identity missing');
    END;";

const PERMIT_LEASE_DELETE_TRIGGER_SQL: &str = "CREATE TRIGGER trg_permits_release_fence
    BEFORE DELETE ON permits
    WHEN NOT EXISTS (
        SELECT 1 FROM permit_release_authorizations AS a
        WHERE a.holder = OLD.holder
          AND a.lease_generation = OLD.lease_generation
    )
    BEGIN
        SELECT RAISE(ABORT, 'exact permit lease release required');
    END;";

const PERMIT_ADMISSION_TRIGGER_SQL: &str = "CREATE TRIGGER trg_permits_admission_fence
    BEFORE INSERT ON permits
    WHEN COALESCE((SELECT demand_sources_ready FROM permit_meta WHERE id = 1), 0) != 1
      OR COALESCE((SELECT generation != reconciled_generation
                   FROM permit_meta WHERE id = 1), 1) != 0
      OR EXISTS(SELECT 1 FROM permit_pending_demand_updates)
      OR NEW.admission_roster_hash IS NULL
      OR NEW.admission_roster_hash !=
         (SELECT demand_roster_hash FROM permit_meta WHERE id = 1)
    BEGIN
        SELECT RAISE(ABORT, 'permit ledger admission gate closed');
    END;";

fn normalized_schema_sql(sql: &str) -> String {
    // SQLite retains trigger source text in sqlite_master. Collapse only
    // whitespace and ASCII case; preserve every operator and punctuation
    // token so `!=` cannot verify as `=` (or `NOT EXISTS` as a different
    // predicate) during a safety-fence upgrade. SQLite omits the statement's
    // final semicolon when it stores the source text.
    sql.trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn schema_sql_matches(actual: &str, expected: &str) -> bool {
    normalized_schema_sql(actual) == normalized_schema_sql(expected)
}

fn permit_lease_index_is_current(conn: &Connection) -> Result<bool, LedgerError> {
    let mut list = conn.prepare("PRAGMA index_list('permits')")?;
    let indexes = list
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get::<_, i64>(4)? != 0,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(list);
    let Some((_, unique, partial)) = indexes
        .into_iter()
        .find(|(name, _, _)| name == "idx_permits_lease_generation")
    else {
        return Ok(false);
    };
    if !unique || partial {
        return Ok(false);
    }
    let mut info = conn.prepare("PRAGMA index_info('idx_permits_lease_generation')")?;
    let columns = info
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns == [(0, Some("lease_generation".to_owned()))])
}

fn permit_lease_schema_has_conflicts(conn: &Connection) -> Result<bool, LedgerError> {
    for (name, expected) in [
        (
            "trg_permits_lease_insert_fence",
            PERMIT_LEASE_INSERT_TRIGGER_SQL,
        ),
        ("trg_permits_release_fence", PERMIT_LEASE_DELETE_TRIGGER_SQL),
    ] {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()?;
        if sql.is_some_and(|sql| !schema_sql_matches(&sql, expected)) {
            return Ok(true);
        }
    }
    let named_index_exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_permits_lease_generation')",
        [],
        |row| row.get(0),
    )?;
    Ok(named_index_exists && !permit_lease_index_is_current(conn)?)
}

fn permit_lease_schema_is_current(conn: &Connection) -> Result<bool, LedgerError> {
    if !table_has_column(conn, "permit_meta", "next_lease_generation")?
        || !table_has_column(conn, "permits", "lease_generation")?
        || !table_has_column(conn, "permits", "pid_identity")?
        || !table_has_column(conn, "permits", "admission_roster_hash")?
        || !table_has_column(conn, "permit_release_authorizations", "holder")?
        || !table_has_column(conn, "permit_release_authorizations", "lease_generation")?
    {
        return Ok(false);
    }
    if !permit_lease_index_is_current(conn)? {
        return Ok(false);
    }
    let (counter, maximum, missing): (i64, i64, bool) = conn.query_row(
        "SELECT next_lease_generation,
                COALESCE((SELECT MAX(lease_generation) FROM permits), 0),
                EXISTS(SELECT 1 FROM permits WHERE lease_generation <= 0)
         FROM permit_meta WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if missing || counter < maximum {
        return Ok(false);
    }
    let insert_fence: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name = 'trg_permits_lease_insert_fence'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let delete_fence: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name = 'trg_permits_release_fence'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let insert_fence_ready =
        insert_fence.is_some_and(|sql| schema_sql_matches(&sql, PERMIT_LEASE_INSERT_TRIGGER_SQL));
    let delete_fence_ready =
        delete_fence.is_some_and(|sql| schema_sql_matches(&sql, PERMIT_LEASE_DELETE_TRIGGER_SQL));
    Ok(insert_fence_ready && delete_fence_ready)
}

/// Install immutable per-acquisition identity separate from the mutable
/// host epoch. Existing permits receive one stable, unique identity once.
fn ensure_permit_lease_schema(conn: &mut Connection) -> Result<(), LedgerError> {
    if permit_lease_schema_is_current(conn)? {
        return Ok(());
    }
    if permit_lease_schema_has_conflicts(conn)? {
        return Err(LedgerError::DemandSourcesUnready(
            "permit lease schema contains a malformed named release fence or lease index; repair the ledger schema before reopening it".to_owned(),
        ));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if permit_lease_schema_is_current(&tx)? {
        tx.commit()?;
        return Ok(());
    }
    if !table_has_column(&tx, "permit_meta", "next_lease_generation")? {
        tx.execute_batch(
            "ALTER TABLE permit_meta
             ADD COLUMN next_lease_generation INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !table_has_column(&tx, "permits", "lease_generation")? {
        tx.execute_batch(
            "ALTER TABLE permits
             ADD COLUMN lease_generation INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !table_has_column(&tx, "permits", "pid_identity")? {
        tx.execute_batch("ALTER TABLE permits ADD COLUMN pid_identity TEXT;")?;
    }
    if !table_has_column(&tx, "permits", "admission_roster_hash")? {
        tx.execute_batch("ALTER TABLE permits ADD COLUMN admission_roster_hash TEXT;")?;
    }
    let mut next: i64 = tx.query_row(
        "SELECT MAX(next_lease_generation,
                    COALESCE((SELECT MAX(lease_generation) FROM permits), 0))
         FROM permit_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let holders: Vec<String> = {
        let mut statement = tx.prepare(
            "SELECT holder FROM permits WHERE lease_generation <= 0
             ORDER BY acquired_unix, holder",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    for holder in holders {
        next = next.checked_add(1).ok_or_else(|| {
            LedgerError::DemandSourcesUnready(
                "permit lease generation space is exhausted".to_owned(),
            )
        })?;
        tx.execute(
            "UPDATE permits SET lease_generation = ?1 WHERE holder = ?2",
            params![next, holder],
        )?;
    }
    tx.execute(
        "UPDATE permit_meta SET next_lease_generation = MAX(next_lease_generation, ?1)
         WHERE id = 1",
        params![next],
    )?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS permit_release_authorizations (
             holder TEXT NOT NULL,
             lease_generation INTEGER NOT NULL,
             PRIMARY KEY (holder, lease_generation)
         );",
    )?;
    if !permit_lease_index_is_current(&tx)? {
        tx.execute_batch(
            "CREATE UNIQUE INDEX idx_permits_lease_generation
             ON permits (lease_generation);",
        )?;
    }
    if tx
        .query_row(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'trigger' AND name = 'trg_permits_lease_insert_fence'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_none()
    {
        tx.execute_batch(PERMIT_LEASE_INSERT_TRIGGER_SQL)?;
    }
    if tx
        .query_row(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'trigger' AND name = 'trg_permits_release_fence'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_none()
    {
        tx.execute_batch(PERMIT_LEASE_DELETE_TRIGGER_SQL)?;
    }
    tx.commit()?;
    Ok(())
}

fn next_lease_generation_tx(tx: &Transaction<'_>) -> Result<i64, LedgerError> {
    let next: i64 = tx.query_row(
        "SELECT next_lease_generation FROM permit_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let next = next.checked_add(1).ok_or_else(|| {
        LedgerError::DemandSourcesUnready("permit lease generation space is exhausted".to_owned())
    })?;
    tx.execute(
        "UPDATE permit_meta SET next_lease_generation = ?1 WHERE id = 1",
        params![next],
    )?;
    Ok(next)
}

fn table_has_column(conn: &Connection, table: &str, wanted: &str) -> Result<bool, LedgerError> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == wanted {
            return Ok(true);
        }
    }
    Ok(false)
}

fn demand_source_fence_schema_is_current(conn: &Connection) -> Result<bool, LedgerError> {
    if !table_has_column(conn, "permit_meta", "demand_sources_ready")?
        || !table_has_column(conn, "permit_meta", "demand_roster_hash")?
        || !table_has_column(conn, "permits", "admission_roster_hash")?
    {
        return Ok(false);
    }
    let source_table_exists: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'permit_demand_sources'
         )",
        [],
        |row| row.get(0),
    )?;
    if !source_table_exists {
        return Ok(false);
    }
    let pending_table_exists: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table'
               AND name = 'permit_pending_demand_updates'
         )",
        [],
        |row| row.get(0),
    )?;
    if !pending_table_exists {
        return Ok(false);
    }
    for column in [
        "scale_set_id",
        "request_id",
        "first_seen_unix",
        "first_seen_subsec_nanos",
        "repo_owner",
        "repo_name",
        "job_id_hash",
        "labels_hash",
        "event_name",
        "publication_version",
    ] {
        if !table_has_column(conn, "permit_pending_demand_updates", column)? {
            return Ok(false);
        }
    }
    let trigger_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name = 'trg_permits_admission_fence'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(trigger_sql.is_some_and(|sql| schema_sql_matches(&sql, PERMIT_ADMISSION_TRIGGER_SQL)))
}

/// Install the cross-version insertion fence once. The permit token has no
/// default, so pre-migration code cannot insert even after another daemon
/// finishes startup reconciliation.
fn ensure_demand_source_fence_schema(conn: &mut Connection) -> Result<(), LedgerError> {
    if demand_source_fence_schema_is_current(conn)? {
        return Ok(());
    }
    let existing_trigger: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'trigger' AND name = 'trg_permits_admission_fence'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if existing_trigger.is_some_and(|sql| !schema_sql_matches(&sql, PERMIT_ADMISSION_TRIGGER_SQL)) {
        return Err(LedgerError::DemandSourcesUnready(
            "permit admission schema contains a malformed roster fence; repair the ledger schema before reopening it".to_owned(),
        ));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if demand_source_fence_schema_is_current(&tx)? {
        tx.commit()?;
        return Ok(());
    }
    if !table_has_column(&tx, "permit_meta", "demand_sources_ready")? {
        tx.execute_batch(
            "ALTER TABLE permit_meta
             ADD COLUMN demand_sources_ready INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !table_has_column(&tx, "permit_meta", "demand_roster_hash")? {
        tx.execute_batch("ALTER TABLE permit_meta ADD COLUMN demand_roster_hash TEXT;")?;
    }
    if !table_has_column(&tx, "permits", "admission_roster_hash")? {
        tx.execute_batch("ALTER TABLE permits ADD COLUMN admission_roster_hash TEXT;")?;
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS permit_demand_sources (
             source_id TEXT PRIMARY KEY,
             path TEXT NOT NULL UNIQUE,
             state TEXT NOT NULL CHECK (state IN ('pending', 'ready'))
         );
         CREATE TABLE IF NOT EXISTS permit_pending_demand_updates (
             holder TEXT PRIMARY KEY,
             source_id TEXT NOT NULL,
             created_unix INTEGER NOT NULL,
             generation INTEGER NOT NULL DEFAULT 0,
             scale_set_id INTEGER NOT NULL DEFAULT -1,
             request_id INTEGER NOT NULL DEFAULT -1,
             first_seen_unix INTEGER NOT NULL DEFAULT 0,
             first_seen_subsec_nanos INTEGER NOT NULL DEFAULT 0,
             repo_owner TEXT NOT NULL DEFAULT '',
             repo_name TEXT NOT NULL DEFAULT '',
             job_id_hash INTEGER NOT NULL DEFAULT 0,
             labels_hash TEXT NOT NULL DEFAULT '',
             event_name TEXT NOT NULL DEFAULT '',
             publication_version INTEGER NOT NULL DEFAULT 0
         );",
    )?;
    if tx
        .query_row(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'trigger' AND name = 'trg_permits_admission_fence'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_none()
    {
        tx.execute_batch(PERMIT_ADMISSION_TRIGGER_SQL)?;
    }
    for (column, definition) in [
        ("generation", "INTEGER NOT NULL DEFAULT 0"),
        ("scale_set_id", "INTEGER NOT NULL DEFAULT -1"),
        ("request_id", "INTEGER NOT NULL DEFAULT -1"),
        ("first_seen_unix", "INTEGER NOT NULL DEFAULT 0"),
        ("first_seen_subsec_nanos", "INTEGER NOT NULL DEFAULT 0"),
        ("repo_owner", "TEXT NOT NULL DEFAULT ''"),
        ("repo_name", "TEXT NOT NULL DEFAULT ''"),
        ("job_id_hash", "INTEGER NOT NULL DEFAULT 0"),
        ("labels_hash", "TEXT NOT NULL DEFAULT ''"),
        ("event_name", "TEXT NOT NULL DEFAULT ''"),
        ("publication_version", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !table_has_column(&tx, "permit_pending_demand_updates", column)? {
            tx.execute_batch(&format!(
                "ALTER TABLE permit_pending_demand_updates ADD COLUMN {column} {definition};"
            ))?;
        }
    }
    tx.execute(
        "UPDATE permit_meta SET demand_sources_ready = 0 WHERE id = 1",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

fn ensure_demand_tx(
    tx: &Transaction<'_>,
    holder: &str,
    lane: PermitLane,
    scope: &str,
    first_seen_unix: u64,
    first_seen_subsec_nanos: u32,
    updated_unix: u64,
    initial_state: DemandState,
) -> Result<PermitDemand, LedgerError> {
    if let Some(demand) = read_demand(tx, holder)? {
        check_demand_lane(holder, &demand, lane)?;
        if !scope.is_empty() && demand.scope != scope {
            return Err(LedgerError::DemandScopeMismatch {
                holder: holder.to_owned(),
                expected: scope.to_owned(),
                seen: demand.scope,
            });
        }
        return Ok(demand);
    }
    let sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM permit_demands",
        [],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO permit_demands
         (holder, lane, scope, first_seen_unix, first_seen_subsec_nanos,
          sequence, state, updated_unix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            holder,
            lane.as_str(),
            scope,
            i64::try_from(first_seen_unix).unwrap_or(i64::MAX),
            i64::from(first_seen_subsec_nanos.min(999_999_999)),
            sequence,
            initial_state.as_str(),
            i64::try_from(updated_unix).unwrap_or(i64::MAX),
        ],
    )?;
    Ok(PermitDemand {
        holder: holder.to_owned(),
        lane,
        scope: scope.to_owned(),
        first_seen_unix,
        first_seen_subsec_nanos: first_seen_subsec_nanos.min(999_999_999),
        sequence,
        state: initial_state,
        updated_unix,
    })
}

fn observe_demand_tx(
    tx: &Transaction<'_>,
    observation: PermitDemandObservation<'_>,
) -> Result<PermitDemand, LedgerError> {
    let held_lane: Option<String> = tx
        .query_row(
            "SELECT lane FROM permits WHERE holder = ?1",
            params![observation.holder],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(held_lane) = held_lane.as_deref()
        && held_lane != observation.lane.as_str()
    {
        return Err(LedgerError::DemandLaneMismatch {
            holder: observation.holder.to_owned(),
            expected: observation.lane,
            seen: held_lane.to_owned(),
        });
    }
    let initial_state = if held_lane.is_some() {
        DemandState::Granted
    } else {
        DemandState::Eligible
    };
    let existing = ensure_demand_tx(
        tx,
        observation.holder,
        observation.lane,
        observation.scope,
        observation.first_seen_unix,
        observation.first_seen_subsec_nanos,
        observation.observed_unix,
        initial_state,
    )?;
    if observation.lane == PermitLane::ScaleSet
        && existing.state == DemandState::Eligible
        && existing.first_seen_unix == observation.first_seen_unix
        && existing.first_seen_subsec_nanos != observation.first_seen_subsec_nanos
    {
        // Scale Set's durable RFC 3339 timestamp is authoritative. This
        // repairs whole-second mirrors created before the fractional field
        // existed without changing the immutable global sequence.
        tx.execute(
            "UPDATE permit_demands SET first_seen_subsec_nanos = ?1
             WHERE holder = ?2 AND state = 'eligible'",
            params![
                i64::from(observation.first_seen_subsec_nanos.min(999_999_999)),
                observation.holder,
            ],
        )?;
    }
    if matches!(existing.state, DemandState::Eligible | DemandState::Granted) {
        tx.execute(
            "UPDATE permit_demands SET updated_unix = MAX(updated_unix, ?1) WHERE holder = ?2",
            params![
                i64::try_from(observation.observed_unix).unwrap_or(i64::MAX),
                observation.holder
            ],
        )?;
    }
    read_demand(tx, observation.holder)?
        .ok_or_else(|| LedgerError::UnknownHolder(observation.holder.to_owned()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceDemandObservation {
    source_id: String,
    local_sequence: i64,
    holder: String,
    scope: String,
    first_seen_unix: u64,
    first_seen_subsec_nanos: u32,
    observed_unix: u64,
}

fn source_id(path: &Path) -> Result<String, LedgerError> {
    let text = path.to_str().ok_or_else(|| {
        LedgerError::DemandSourcesUnready(format!(
            "demand source path is not valid UTF-8: {}",
            path.display()
        ))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"velnor-permit-demand-source-v1\0");
    hasher.update(text.as_bytes());
    Ok(format!("sha256:{}", digest_hex(&hasher.finalize())))
}

fn digest_hex(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn read_source_snapshot(
    path: &Path,
    now_unix: u64,
) -> Result<Vec<SourceDemandObservation>, LedgerError> {
    let source = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        LedgerError::DemandSourcesUnready(format!("open demand source {}: {error}", path.display()))
    })?;
    source.busy_timeout(BUSY_TIMEOUT).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "configure demand source {}: {error}",
            path.display()
        ))
    })?;
    let source_id = source_id(path)?;
    let mut statement = source
        .prepare(
            "SELECT request_id, scale_set_id, first_seen_at, sequence, updated_at
             FROM scaleset_demand
             WHERE state IN ('eligible', 'mirror_pending')",
        )
        .map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "read demand source schema {}: {error}",
                path.display()
            ))
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "query demand source {}: {error}",
                path.display()
            ))
        })?;
    let fresh_after = now_unix.saturating_sub(DEMAND_STALE_AFTER_SECS);
    let mut observations = Vec::new();
    for row in rows {
        let (request_id, scale_set_id, first_seen, sequence, updated_at) =
            row.map_err(|error| {
                LedgerError::DemandSourcesUnready(format!(
                    "decode demand source {}: {error}",
                    path.display()
                ))
            })?;
        let first_seen = velnor_model::Timestamp::parse(&first_seen).map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "parse first_seen_at in demand source {}: {error}",
                path.display()
            ))
        })?;
        let first_seen = first_seen.as_offset_datetime();
        let first_seen_unix = u64::try_from(first_seen.unix_timestamp()).map_err(|_| {
            LedgerError::DemandSourcesUnready(format!(
                "demand source {} has a timestamp before the Unix epoch",
                path.display()
            ))
        })?;
        let updated_at = velnor_model::Timestamp::parse(&updated_at).map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "parse updated_at in demand source {}: {error}",
                path.display()
            ))
        })?;
        let observed_unix = u64::try_from(updated_at.as_offset_datetime().unix_timestamp())
            .map_err(|_| {
                LedgerError::DemandSourcesUnready(format!(
                    "demand source {} has an update timestamp before the Unix epoch",
                    path.display()
                ))
            })?;
        if observed_unix <= fresh_after {
            continue;
        }
        observations.push(SourceDemandObservation {
            source_id: source_id.clone(),
            local_sequence: sequence,
            holder: format!("scaleset/{scale_set_id}/{request_id}"),
            scope: format!("scaleset/{scale_set_id}"),
            first_seen_unix,
            first_seen_subsec_nanos: first_seen.nanosecond(),
            observed_unix,
        });
    }
    Ok(observations)
}

/// Databases that may contain released worker cleanup state. Scale Set demand
/// databases are included for older/single-store layouts; the authoritative
/// state-db roster is required because demand and worker state can be split.
fn cleanup_state_database_paths(roster: &PermitDemandSourceRoster) -> Vec<PathBuf> {
    let mut paths = roster.state_db_paths.clone();
    paths.extend(roster.paths.iter().cloned());
    paths.sort();
    paths.dedup();
    paths
}

/// A released Scale Set worker may still own raw diagnostics, JIT material,
/// or its private state tree after its permit row is removed. Admission must
/// stay closed until the authoritative state-db roster records that cleanup
/// finished; demand and worker state may live in separate databases.
fn source_has_pending_released_state_cleanup(path: &Path) -> Result<bool, LedgerError> {
    let source = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "open demand source cleanup state {}: {error}",
            path.display()
        ))
    })?;
    source.busy_timeout(BUSY_TIMEOUT).map_err(|error| {
        LedgerError::DemandSourcesUnready(format!(
            "configure demand source cleanup state {}: {error}",
            path.display()
        ))
    })?;
    let (has_workers, has_runtime): (bool, bool) = source
        .query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'scaleset_workers'),
                 EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'scaleset_worker_runtime')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "inspect demand source cleanup schema {}: {error}",
                path.display()
            ))
        })?;
    if !has_workers && !has_runtime {
        // Native-only operational databases need not have Scale Set tables.
        return Ok(false);
    }
    if !has_workers || !has_runtime {
        return Err(LedgerError::DemandSourcesUnready(format!(
            "state database {} has a partial Scale Set cleanup registry; migrate it before admission",
            path.display()
        )));
    }
    source
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM scaleset_workers AS w
                 JOIN scaleset_worker_runtime AS r USING (ownership_id)
                 WHERE w.worker_state = 'permit_released'
                   AND r.state_dir_cleanup_pending = 1
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "read demand source cleanup state {}: {error}",
                path.display()
            ))
        })
}

fn collect_source_snapshot(
    roster: &PermitDemandSourceRoster,
    now_unix: u64,
) -> Result<Vec<SourceDemandObservation>, LedgerError> {
    let mut observations = Vec::new();
    for path in &roster.paths {
        observations.extend(read_source_snapshot(path, now_unix)?);
    }
    // Legacy sources only carry local sequence numbers; same-second
    // cross-source ties have no recoverable historical order. Use this
    // stable order for new global sequence allocation.
    observations.sort_by(|left, right| {
        (
            left.first_seen_unix,
            left.first_seen_subsec_nanos,
            &left.source_id,
            left.local_sequence,
            &left.holder,
        )
            .cmp(&(
                right.first_seen_unix,
                right.first_seen_subsec_nanos,
                &right.source_id,
                right.local_sequence,
                &right.holder,
            ))
    });
    Ok(observations)
}

fn import_source_snapshot_tx(
    tx: &Transaction<'_>,
    observations: &[SourceDemandObservation],
) -> Result<(), LedgerError> {
    for source in observations {
        observe_demand_tx(
            tx,
            PermitDemandObservation {
                holder: &source.holder,
                lane: PermitLane::ScaleSet,
                scope: &source.scope,
                first_seen_unix: source.first_seen_unix,
                first_seen_subsec_nanos: source.first_seen_subsec_nanos,
                observed_unix: source.observed_unix,
            },
        )?;
    }
    Ok(())
}

fn close_source_gate_tx(tx: &Transaction<'_>) -> Result<(), LedgerError> {
    tx.execute(
        "UPDATE permit_meta SET demand_sources_ready = 0 WHERE id = 1",
        [],
    )?;
    tx.execute("UPDATE permit_demand_sources SET state = 'pending'", [])?;
    Ok(())
}

/// Resolve global-first queue publications only after the corresponding
/// eligible row is visible in its configured demand database. An absent row
/// leaves the pending marker in place and keeps all admission closed.
fn resolve_pending_demand_updates_tx(
    tx: &Transaction<'_>,
    observations: &[SourceDemandObservation],
) -> Result<bool, LedgerError> {
    let observed: HashMap<(&str, &str), &SourceDemandObservation> = observations
        .iter()
        .map(|observation| {
            (
                (observation.source_id.as_str(), observation.holder.as_str()),
                observation,
            )
        })
        .collect();
    let mut statement = tx.prepare(
        "SELECT holder, source_id FROM permit_pending_demand_updates ORDER BY source_id, holder",
    )?;
    let pending = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (holder, source_id) in pending {
        let Some(observation) = observed.get(&(source_id.as_str(), holder.as_str())) else {
            continue;
        };
        observe_demand_tx(
            tx,
            PermitDemandObservation {
                holder: &observation.holder,
                lane: PermitLane::ScaleSet,
                scope: &observation.scope,
                first_seen_unix: observation.first_seen_unix,
                first_seen_subsec_nanos: observation.first_seen_subsec_nanos,
                observed_unix: observation.observed_unix,
            },
        )?;
        tx.execute(
            "DELETE FROM permit_pending_demand_updates WHERE holder = ?1 AND source_id = ?2",
            params![holder, source_id],
        )?;
    }
    let remains: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM permit_pending_demand_updates)",
        [],
        |row| row.get(0),
    )?;
    Ok(!remains)
}

fn registered_sources_match(
    tx: &Transaction<'_>,
    roster: &PermitDemandSourceRoster,
) -> Result<bool, LedgerError> {
    let expected: Vec<(String, String)> = roster
        .paths
        .iter()
        .map(|path| {
            let path_text = path.to_str().ok_or_else(|| {
                LedgerError::DemandSourcesUnready(format!(
                    "demand source path is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            Ok((source_id(path)?, path_text.to_owned()))
        })
        .collect::<Result<_, LedgerError>>()?;
    let mut statement =
        tx.prepare("SELECT source_id, path FROM permit_demand_sources ORDER BY path")?;
    let mut actual = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut expected = expected;
    expected.sort();
    actual.sort();
    Ok(actual == expected)
}

fn refresh_registered_sources_tx(
    tx: &Transaction<'_>,
    ledger_path: &Path,
    roster_path: &Path,
    expected_identity: Option<&str>,
    now_unix: u64,
) -> Result<bool, LedgerError> {
    let Some(expected_identity) = expected_identity else {
        close_source_gate_tx(tx)?;
        return Ok(false);
    };
    let roster = match read_demand_source_roster_at(ledger_path, roster_path) {
        Ok(roster) => roster,
        Err(LedgerError::DemandSourcesUnready(_)) => {
            close_source_gate_tx(tx)?;
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if roster.fingerprint != expected_identity {
        close_source_gate_tx(tx)?;
        return Ok(false);
    }
    for path in cleanup_state_database_paths(&roster) {
        match source_has_pending_released_state_cleanup(&path) {
            // Keep the roster's normal readiness stamp intact while a
            // released worker drains its state tree. Both advertised_free
            // and every acquire check this durable bit; preserving the stamp
            // lets capacity reopen immediately once cleanup clears it.
            Ok(true) => return Ok(false),
            Ok(false) => {}
            Err(LedgerError::DemandSourcesUnready(_)) => {
                close_source_gate_tx(tx)?;
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
    }
    close_source_gate_tx(tx)?;
    let stored_identity: Option<String> = tx.query_row(
        "SELECT demand_roster_hash FROM permit_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if stored_identity.as_deref() != Some(expected_identity)
        || !registered_sources_match(tx, &roster)?
    {
        return Ok(false);
    }
    let observations = match collect_source_snapshot(&roster, now_unix) {
        Ok(observations) => observations,
        Err(LedgerError::DemandSourcesUnready(_)) => return Ok(false),
        Err(error) => return Err(error),
    };
    if let Err(error) = import_source_snapshot_tx(tx, &observations) {
        match error {
            LedgerError::Storage(_) | LedgerError::DemandSourcesUnready(_) => return Ok(false),
            other => return Err(other),
        }
    }
    if !resolve_pending_demand_updates_tx(tx, &observations)? {
        return Ok(false);
    }
    let latest_roster = match read_demand_source_roster_at(ledger_path, roster_path) {
        Ok(roster) => roster,
        Err(LedgerError::DemandSourcesUnready(_)) => return Ok(false),
        Err(error) => return Err(error),
    };
    if latest_roster.fingerprint != expected_identity {
        return Ok(false);
    }
    tx.execute("UPDATE permit_demand_sources SET state = 'ready'", [])?;
    tx.execute(
        "UPDATE permit_meta SET demand_sources_ready = 1 WHERE id = 1",
        [],
    )?;
    Ok(true)
}

impl PermitLedger {
    /// Open (creating parent directories and schema) the ledger database.
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                LedgerError::Storage(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
                    Some(format!("create ledger dir {}: {error}", parent.display())),
                ))
            })?;
        }
        // Resolve aliases before opening SQLite. Resolving after `open` can
        // bind this Connection to one file while recording a different path
        // in `PermitLedger`; a later symlink retarget would then direct
        // follow-up opens to a replacement ledger.
        let canonical_path = canonicalize_ledger_identity(path)?;
        let mut conn = Connection::open(&canonical_path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS permit_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                max_jobs INTEGER,
                generation INTEGER NOT NULL DEFAULT 0,
                reconciled_generation INTEGER NOT NULL DEFAULT -1,
                demand_sources_ready INTEGER NOT NULL DEFAULT 0,
                demand_roster_hash TEXT,
                next_lease_generation INTEGER NOT NULL DEFAULT 0
            );
            INSERT OR IGNORE INTO permit_meta (id, max_jobs, generation, reconciled_generation)
                VALUES (1, NULL, 0, -1);
            CREATE TABLE IF NOT EXISTS permits (
                holder TEXT PRIMARY KEY,
                lane TEXT NOT NULL,
                state TEXT NOT NULL,
                acquired_unix INTEGER NOT NULL,
                updated_unix INTEGER NOT NULL,
                generation INTEGER NOT NULL,
                pid INTEGER,
                admission_roster_hash TEXT,
                lease_generation INTEGER NOT NULL DEFAULT 0,
                pid_identity TEXT
            );
            CREATE TABLE IF NOT EXISTS permit_demands (
                holder TEXT PRIMARY KEY,
                lane TEXT NOT NULL,
                scope TEXT NOT NULL,
                first_seen_unix INTEGER NOT NULL,
                first_seen_subsec_nanos INTEGER NOT NULL DEFAULT 0,
                sequence INTEGER NOT NULL UNIQUE,
                state TEXT NOT NULL,
                updated_unix INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_permit_demands_oldest
                ON permit_demands
                    (state, first_seen_unix, first_seen_subsec_nanos, sequence);",
        )?;
        ensure_demand_precision_schema(&mut conn)?;
        ensure_permit_lease_schema(&mut conn)?;
        ensure_demand_source_fence_schema(&mut conn)?;
        migrate_legacy_native_demand(&mut conn)?;
        let source_roster_path = demand_source_roster_path(&canonical_path);
        let expected_roster_identity = process_roster_identity(&canonical_path);
        let mut ledger = Self {
            path: canonical_path,
            conn,
            source_roster_path,
            expected_roster_identity,
        };
        #[cfg(not(any(test, feature = "test-support")))]
        if ledger.expected_roster_identity.is_none()
            && let Ok(roster) =
                read_demand_source_roster_at(&ledger.path, &ledger.source_roster_path)
        {
            if remember_process_roster_identity(&ledger.path, &roster.fingerprint).is_ok() {
                ledger.expected_roster_identity = Some(roster.fingerprint);
            } else {
                ledger.expected_roster_identity = process_roster_identity(&ledger.path);
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        {
            if !ledger.source_roster_path.exists() {
                let test_state_db = ledger.path.with_extension("test-state.db");
                Connection::open(&test_state_db).map_err(|error| {
                    LedgerError::DemandSourcesUnready(format!(
                        "create isolated test state database {}: {error}",
                        test_state_db.display()
                    ))
                })?;
                let roster_contents = format!(
                    "permit-ledger {}\nstate-db {}\n",
                    ledger.path.to_string_lossy(),
                    test_state_db.to_string_lossy(),
                );
                std::fs::write(&ledger.source_roster_path, roster_contents).map_err(|error| {
                    LedgerError::DemandSourcesUnready(format!(
                        "write isolated test roster {}: {error}",
                        ledger.source_roster_path.display()
                    ))
                })?;
            }
            if ledger.expected_roster_identity.is_none() {
                let roster =
                    read_demand_source_roster_at(&ledger.path, &ledger.source_roster_path)?;
                let generation = ledger.generation()?;
                ledger.reconcile_host_roster(&roster, generation, &[])?;
            }
        }
        Ok(ledger)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Keep admission closed after an unreadable roster/source or a caller
    /// that failed host-roster validation.
    pub fn close_demand_source_gate(&mut self) -> Result<(), LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE permit_meta SET demand_sources_ready = 0 WHERE id = 1",
            [],
        )?;
        tx.execute("UPDATE permit_demand_sources SET state = 'pending'", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Register and atomically backfill the complete host roster. Every
    /// source snapshot, global queue import, registry-ready mark, and roster
    /// identity update commit under one `BEGIN IMMEDIATE` transaction.
    pub fn configure_demand_source_roster(
        &mut self,
        roster: &PermitDemandSourceRoster,
    ) -> Result<(), LedgerError> {
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        let declared = match read_demand_source_roster_at(&self.path, &self.source_roster_path) {
            Ok(declared) => declared,
            Err(error) => {
                self.close_demand_source_gate()?;
                return Err(error);
            }
        };
        if declared != *roster || roster.ledger_path.as_deref() != Some(self.path.as_path()) {
            self.close_demand_source_gate()?;
            return Err(LedgerError::DemandSourcesUnready(
                "configured host roster does not match the canonical permit ledger or current roster file".to_owned(),
            ));
        }
        if let Err(error) = remember_process_roster_identity(&self.path, &roster.fingerprint) {
            self.close_demand_source_gate()?;
            return Err(error);
        }
        self.expected_roster_identity = Some(roster.fingerprint.clone());
        let snapshot = match collect_source_snapshot(roster, unix_now()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.close_demand_source_gate()?;
                return Err(error);
            }
        };

        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current_roster = read_demand_source_roster_at(&self.path, &self.source_roster_path);
        let current_roster = match current_roster {
            Ok(current) if current == *roster => current,
            Ok(_) => {
                tx.execute(
                    "UPDATE permit_meta SET demand_sources_ready = 0 WHERE id = 1",
                    [],
                )?;
                tx.execute("UPDATE permit_demand_sources SET state = 'pending'", [])?;
                tx.commit()?;
                return Err(LedgerError::DemandSourcesUnready(
                    "host roster changed while its sources were being imported".to_owned(),
                ));
            }
            Err(error) => {
                tx.execute(
                    "UPDATE permit_meta SET demand_sources_ready = 0 WHERE id = 1",
                    [],
                )?;
                tx.execute("UPDATE permit_demand_sources SET state = 'pending'", [])?;
                tx.commit()?;
                return Err(error);
            }
        };
        let stored_fingerprint: Option<String> = tx.query_row(
            "SELECT demand_roster_hash FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let roster_changed = stored_fingerprint.as_deref() != Some(&roster.fingerprint);
        tx.execute(
            "UPDATE permit_meta
             SET demand_sources_ready = 0, demand_roster_hash = ?1
             WHERE id = 1",
            params![roster.fingerprint],
        )?;
        if roster_changed {
            let stale_at = unix_now().saturating_sub(DEMAND_STALE_AFTER_SECS + 1);
            tx.execute(
                "UPDATE permit_demands SET updated_unix = MIN(updated_unix, ?1)
                 WHERE lane = 'scale-set' AND state = 'eligible'",
                params![i64::try_from(stale_at).unwrap_or(i64::MAX)],
            )?;
            tx.execute("DELETE FROM permit_demand_sources", [])?;
            tx.execute(
                "UPDATE permit_meta SET reconciled_generation = -1 WHERE id = 1",
                [],
            )?;
        } else {
            tx.execute("UPDATE permit_demand_sources SET state = 'pending'", [])?;
        }
        for path in &current_roster.paths {
            let id = source_id(path)?;
            let path_text = path.to_str().ok_or_else(|| {
                LedgerError::DemandSourcesUnready(format!(
                    "demand source path is not valid UTF-8: {}",
                    path.display()
                ))
            })?;
            tx.execute(
                "INSERT INTO permit_demand_sources (source_id, path, state)
                 VALUES (?1, ?2, 'pending')
                 ON CONFLICT(source_id) DO UPDATE SET path = excluded.path, state = 'pending'",
                params![id, path_text],
            )?;
        }
        let import_result = import_source_snapshot_tx(&tx, &snapshot);
        if let Err(error) = import_result {
            tx.execute(
                "UPDATE permit_meta SET demand_sources_ready = 0 WHERE id = 1",
                [],
            )?;
            tx.execute("UPDATE permit_demand_sources SET state = 'pending'", [])?;
            tx.commit()?;
            return Err(error);
        }
        if !resolve_pending_demand_updates_tx(&tx, &snapshot)? {
            tx.execute(
                "UPDATE permit_meta SET demand_sources_ready = 0 WHERE id = 1",
                [],
            )?;
            tx.execute("UPDATE permit_demand_sources SET state = 'pending'", [])?;
            tx.commit()?;
            return Err(LedgerError::DemandSourcesUnready(
                "a Scale Set queue publication has no committed eligible source row; replay the offer before admission can reopen".to_owned(),
            ));
        }
        if read_demand_source_roster_at(&self.path, &self.source_roster_path)? != *roster {
            tx.execute(
                "UPDATE permit_meta SET demand_sources_ready = 0 WHERE id = 1",
                [],
            )?;
            tx.execute("UPDATE permit_demand_sources SET state = 'pending'", [])?;
            tx.commit()?;
            return Err(LedgerError::DemandSourcesUnready(
                "host roster changed before source import committed".to_owned(),
            ));
        }
        tx.execute("UPDATE permit_demand_sources SET state = 'ready'", [])?;
        tx.execute(
            "UPDATE permit_meta SET demand_sources_ready = 1 WHERE id = 1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Whether the durable source registry and this process's roster
    /// identity are both ready.
    pub fn demand_sources_ready(&self) -> Result<bool, LedgerError> {
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        let ready: bool = self.conn.query_row(
            "SELECT demand_sources_ready = 1 AND demand_roster_hash IS NOT NULL
                    AND NOT EXISTS(
                        SELECT 1 FROM permit_demand_sources WHERE state != 'ready'
                     )
                    AND NOT EXISTS(SELECT 1 FROM permit_pending_demand_updates)
             FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if !ready {
            return Ok(false);
        }
        let Some(expected) = self.expected_roster_identity.as_deref() else {
            return Ok(false);
        };
        let current_roster =
            match read_demand_source_roster_at(&self.path, &self.source_roster_path) {
                Ok(roster) => roster,
                Err(_) => return Ok(false),
            };
        if current_roster.fingerprint != expected {
            return Ok(false);
        }
        let stored: Option<String> = self.conn.query_row(
            "SELECT demand_roster_hash FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(stored.as_deref() == Some(expected))
    }

    /// Configured host-wide `N`, when one was set.
    pub fn max_jobs(&self) -> Result<Option<u32>, LedgerError> {
        let max: Option<i64> =
            self.conn
                .query_row("SELECT max_jobs FROM permit_meta WHERE id = 1", [], |row| {
                    row.get(0)
                })?;
        Ok(max.and_then(|max| u32::try_from(max).ok()))
    }

    /// Set the host-wide `N`. Only the daemon startup path calls this;
    /// slots and one-shot acquisitions never resize the ledger.
    pub fn set_max_jobs(&mut self, max_jobs: u32) -> Result<(), LedgerError> {
        self.conn.execute(
            "UPDATE permit_meta SET max_jobs = ?1 WHERE id = 1",
            params![i64::from(max_jobs)],
        )?;
        Ok(())
    }

    /// Adopt the first fallback capacity atomically across daemon starts.
    /// Returns true only for the process that initialized an unset ledger.
    pub fn set_max_jobs_if_unset(&mut self, max_jobs: u32) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let configured: Option<i64> =
            tx.query_row("SELECT max_jobs FROM permit_meta WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        let adopted = configured.is_none();
        if adopted {
            tx.execute(
                "UPDATE permit_meta SET max_jobs = ?1 WHERE id = 1 AND max_jobs IS NULL",
                params![i64::from(max_jobs)],
            )?;
        }
        tx.commit()?;
        Ok(adopted)
    }

    /// Current generation. Callers fence mutations on this value.
    pub fn generation(&self) -> Result<u64, LedgerError> {
        let generation: i64 = self.conn.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(generation.max(0) as u64)
    }

    /// Start a new epoch (daemon startup): bump the generation and require
    /// a fresh [`Self::reconcile`] before capacity is advertised.
    pub fn begin_epoch(&mut self) -> Result<u64, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let generation: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let next = generation.saturating_add(1);
        tx.execute(
            "UPDATE permit_meta
             SET generation = ?1, reconciled_generation = -1
             WHERE id = 1",
            params![next],
        )?;
        tx.commit()?;
        Ok(next.max(0) as u64)
    }

    /// Whether [`Self::reconcile`] ran in the current generation.
    pub fn reconciled(&self) -> Result<bool, LedgerError> {
        let (generation, reconciled): (i64, i64) = self.conn.query_row(
            "SELECT generation, reconciled_generation FROM permit_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(generation == reconciled)
    }

    /// Free capacity (`N - occupied`), only after this epoch reconciled.
    /// `None` means "do not advertise yet", never "infinite".
    pub fn advertised_free(&self) -> Result<Option<u32>, LedgerError> {
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        let Some(expected_identity) = self.expected_roster_identity.as_deref() else {
            return Ok(None);
        };
        let roster = match read_demand_source_roster_at(&self.path, &self.source_roster_path) {
            Ok(roster) => roster,
            Err(_) => return Ok(None),
        };
        if roster.fingerprint != expected_identity {
            return Ok(None);
        }
        for path in cleanup_state_database_paths(&roster) {
            match source_has_pending_released_state_cleanup(&path) {
                Ok(false) => {}
                Ok(true) | Err(LedgerError::DemandSourcesUnready(_)) => return Ok(None),
                Err(error) => return Err(error),
            }
        }
        let (
            max,
            generation,
            reconciled,
            sources_ready,
            stored_identity,
            pending_sources,
            pending_updates,
            occupied,
        ): (Option<i64>, i64, i64, bool, Option<String>, bool, bool, i64) = self.conn.query_row(
            "SELECT max_jobs, generation, reconciled_generation,
                    demand_sources_ready = 1, demand_roster_hash,
                    EXISTS(SELECT 1 FROM permit_demand_sources WHERE state != 'ready'),
                    EXISTS(SELECT 1 FROM permit_pending_demand_updates),
                    (SELECT COUNT(*) FROM permits)
             FROM permit_meta WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )?;
        if generation != reconciled
            || !sources_ready
            || pending_sources
            || pending_updates
            || stored_identity.as_deref() != Some(expected_identity)
        {
            return Ok(None);
        }
        let Some(max) = max.and_then(|value| u32::try_from(value).ok()) else {
            return Ok(None);
        };
        Ok(Some(max.saturating_sub(
            u32::try_from(occupied.max(0)).unwrap_or(u32::MAX),
        )))
    }

    /// Counted occupants across every lane and state.
    pub fn occupied(&self) -> Result<u32, LedgerError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM permits", [], |row| row.get(0))?;
        Ok(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
    }

    /// Counted occupants in one lane.
    pub fn occupied_by_lane(&self, lane: PermitLane) -> Result<u32, LedgerError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM permits WHERE lane = ?1",
            params![lane.as_str()],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
    }

    /// Every counted occupant, ordered by holder.
    pub fn holders(&self) -> Result<Vec<PermitHolder>, LedgerError> {
        let mut stmt = self.conn.prepare(
            "SELECT holder, lane, state, acquired_unix, updated_unix, generation, pid,
                    pid_identity, lease_generation
             FROM permits ORDER BY holder",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, i64>(8)?,
            ))
        })?;
        let mut holders = Vec::new();
        for row in rows {
            let (
                holder,
                lane,
                state,
                acquired_unix,
                updated_unix,
                generation,
                pid,
                pid_identity,
                lease_generation,
            ) = row?;
            let lane = PermitLane::parse(&lane).ok_or_else(|| LedgerError::UnknownLane(lane))?;
            let state =
                PermitState::parse(&state).ok_or_else(|| LedgerError::UnknownState(state))?;
            holders.push(PermitHolder {
                holder,
                lane,
                state,
                acquired_unix: acquired_unix.max(0) as u64,
                updated_unix: updated_unix.max(0) as u64,
                generation: generation.max(0) as u64,
                lease_generation: lease_generation.max(0) as u64,
                pid: pid.and_then(|pid| u32::try_from(pid).ok()),
                pid_identity,
            });
        }
        Ok(holders)
    }

    /// Read one durable demand row.
    pub fn demand(&self, holder: &str) -> Result<Option<PermitDemand>, LedgerError> {
        read_demand(&self.conn, holder)
    }

    /// Record that a lane currently observes eligible demand.
    ///
    /// On first observation, the first-seen instant and a host-wide
    /// immutable sequence are persisted. Redelivery preserves both and
    /// refreshes only `updated_unix`. If the holder already owns a permit, a
    /// newly created row starts granted so it cannot block younger demand.
    pub fn observe_demand(
        &mut self,
        holder: &str,
        lane: PermitLane,
        scope: &str,
        first_seen_unix: u64,
        observed_unix: u64,
    ) -> Result<PermitDemand, LedgerError> {
        let (now, now_subsec_nanos) = unix_now_parts();
        let first_seen_subsec_nanos = if first_seen_unix == observed_unix && first_seen_unix == now
        {
            now_subsec_nanos
        } else {
            0
        };
        self.observe_demand_with_subsecond(
            holder,
            lane,
            scope,
            first_seen_unix,
            first_seen_subsec_nanos,
            observed_unix,
        )
    }

    /// Record demand with exact fractional age from an RFC 3339 source.
    /// The nanosecond value is clamped to the valid subsecond range.
    pub fn observe_demand_with_subsecond(
        &mut self,
        holder: &str,
        lane: PermitLane,
        scope: &str,
        first_seen_unix: u64,
        first_seen_subsec_nanos: u32,
        observed_unix: u64,
    ) -> Result<PermitDemand, LedgerError> {
        let mut observed = self.observe_demands([PermitDemandObservation {
            holder,
            lane,
            scope,
            first_seen_unix,
            first_seen_subsec_nanos,
            observed_unix,
        }])?;
        observed
            .pop()
            .ok_or_else(|| LedgerError::UnknownHolder(holder.to_owned()))
    }

    /// Mirror a queue snapshot under one immediate transaction. A peer
    /// acquire cannot commit between rows and observe a partial queue.
    pub fn observe_demands<'a>(
        &mut self,
        observations: impl IntoIterator<Item = PermitDemandObservation<'a>>,
    ) -> Result<Vec<PermitDemand>, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut demands = Vec::new();
        for observation in observations {
            demands.push(observe_demand_tx(&tx, observation)?);
        }
        tx.commit()?;
        Ok(demands)
    }

    /// Rename one legacy unscoped native demand after the caller proved its
    /// persisted scope resolves to the new canonical scope. Re-key only an
    /// unheld, open row; held or closed legacy identities stay intact for
    /// marker recovery and cannot be revived by a new scoped request.
    pub fn rekey_legacy_native_demand(
        &mut self,
        old_holder: &str,
        new_holder: &str,
        expected_old_scope: &str,
        canonical_scope: &str,
    ) -> Result<bool, LedgerError> {
        if !old_holder.starts_with("native/")
            || old_holder.starts_with("native/v1/")
            || !new_holder.starts_with("native/v1/")
            || expected_old_scope.is_empty()
            || canonical_scope.is_empty()
        {
            return Err(LedgerError::DemandSourcesUnready(
                "native holder re-key requires one legacy holder, scoped v1 holder, and verified non-empty scopes".to_owned(),
            ));
        }
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let demand = read_demand(&tx, old_holder)?;
        let held: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM permits WHERE holder = ?1)",
            params![old_holder],
            |row| row.get(0),
        )?;
        let Some(demand) = demand else {
            if held {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "legacy native holder {old_holder:?} has a permit but no durable demand scope; retain it and repair through marker recovery"
                )));
            }
            tx.commit()?;
            return Ok(false);
        };
        check_demand_lane(old_holder, &demand, PermitLane::Native)?;
        if demand.scope != expected_old_scope {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "legacy native demand {old_holder:?} changed scope before re-key"
            )));
        }
        if held {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "legacy native holder {old_holder:?} still has a permit; preserve its lease and marker identity until cleanup completes"
            )));
        }
        if !matches!(demand.state, DemandState::Eligible | DemandState::Granted) {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "legacy native demand {old_holder:?} is {:?}; closed or uncertain identity cannot be re-keyed",
                demand.state
            )));
        }
        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM permit_demands WHERE holder = ?1
                 UNION ALL SELECT 1 FROM permits WHERE holder = ?1
             )",
            params![new_holder],
            |row| row.get(0),
        )?;
        if target_exists {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "scoped native holder {new_holder:?} already exists while legacy holder {old_holder:?} remains"
            )));
        }
        let updated = tx.execute(
            "UPDATE permit_demands SET holder = ?1, scope = ?2 WHERE holder = ?3",
            params![new_holder, canonical_scope, old_holder],
        )?;
        if updated != 1 {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "legacy native demand {old_holder:?} changed during re-key"
            )));
        }
        tx.commit()?;
        Ok(true)
    }

    /// Publish a Scale Set offer into the shared queue before its source
    /// database can expose it as eligible. The durable pending row blocks
    /// every acquire until the source commit is visible or replay repairs it.
    /// A fresh offer's age is sampled only after this transaction owns the
    /// ledger writer lock, giving concurrent admissions a single order point.
    pub fn begin_scale_set_offer(
        &mut self,
        holder: &str,
        scope: &str,
        source_path: &Path,
        generation: u64,
        first_seen: Option<(u64, u32)>,
        publication: &ScaleSetDemandPublication,
    ) -> Result<PermitDemand, LedgerError> {
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        let ledger_path = self.path.clone();
        let roster_path = self.source_roster_path.clone();
        let expected_identity = self.expected_roster_identity.clone();
        let canonical_source = source_path.canonicalize().map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "resolve Scale Set demand source {}: {error}",
                source_path.display()
            ))
        })?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (current, reconciled): (i64, i64) = tx.query_row(
            "SELECT generation, reconciled_generation FROM permit_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if current.max(0) as u64 != generation {
            return Err(LedgerError::StaleGeneration {
                expected: current.max(0) as u64,
                seen: generation,
            });
        }
        if current != reconciled {
            return Err(LedgerError::DemandSourcesUnready(
                "host permit generation has not completed full-roster reconciliation".to_owned(),
            ));
        }
        let now = unix_now();
        if !refresh_registered_sources_tx(
            &tx,
            &ledger_path,
            &roster_path,
            expected_identity.as_deref(),
            now,
        )? {
            return Err(LedgerError::DemandSourcesUnready(
                "a configured demand source or queue publication is not ready".to_owned(),
            ));
        }
        let roster = read_demand_source_roster_at(&ledger_path, &roster_path)?;
        if roster.fingerprint != expected_identity.as_deref().unwrap_or_default()
            || !roster.paths.iter().any(|path| path == &canonical_source)
        {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "Scale Set source {} is absent from the current host roster",
                canonical_source.display()
            )));
        }
        let source_id = source_id(&canonical_source)?;
        let (first_seen_unix, first_seen_subsec_nanos) = first_seen.unwrap_or_else(unix_now_parts);
        let demand = observe_demand_tx(
            &tx,
            PermitDemandObservation {
                holder,
                lane: PermitLane::ScaleSet,
                scope,
                first_seen_unix,
                first_seen_subsec_nanos,
                observed_unix: now,
            },
        )?;
        if matches!(demand.state, DemandState::Terminal | DemandState::Cancelled) {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "Scale Set holder {holder:?} already has a closed global demand"
            )));
        }
        tx.execute(
            "INSERT OR IGNORE INTO permit_pending_demand_updates
             (holder, source_id, created_unix, generation, scale_set_id, request_id,
              first_seen_unix, first_seen_subsec_nanos, repo_owner, repo_name,
              job_id_hash, labels_hash, event_name, publication_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 2)",
            params![
                holder,
                source_id,
                i64::try_from(now).unwrap_or(i64::MAX),
                i64::try_from(generation).unwrap_or(i64::MAX),
                publication.scale_set_id,
                publication.request_id,
                i64::try_from(demand.first_seen_unix).unwrap_or(i64::MAX),
                i64::from(demand.first_seen_subsec_nanos),
                publication.repo_owner,
                publication.repo_name,
                publication.job_id_hash,
                publication.labels_hash,
                publication.event_name,
            ],
        )?;
        let pending: (
            String,
            i64,
            i64,
            i64,
            i64,
            i64,
            String,
            String,
            i64,
            String,
            String,
            i64,
        ) = tx.query_row(
            "SELECT source_id, generation, scale_set_id, request_id, first_seen_unix,
                    first_seen_subsec_nanos, repo_owner, repo_name,
                    job_id_hash, labels_hash, event_name, publication_version
             FROM permit_pending_demand_updates WHERE holder = ?1",
            params![holder],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )?;
        let (
            pending_source,
            pending_generation,
            pending_scale_set_id,
            pending_request_id,
            pending_first_seen,
            pending_subsec,
            pending_owner,
            pending_repo,
            pending_job_hash,
            pending_labels_hash,
            pending_event,
            publication_version,
        ) = pending;
        if pending_source != source_id
            || pending_generation.max(0) as u64 != generation
            || pending_scale_set_id != i64::from(publication.scale_set_id)
            || pending_request_id != publication.request_id
            || pending_first_seen.max(0) as u64 != demand.first_seen_unix
            || pending_subsec.clamp(0, 999_999_999) as u32 != demand.first_seen_subsec_nanos
            || pending_owner != publication.repo_owner
            || pending_repo != publication.repo_name
            || pending_job_hash != publication.job_id_hash
            || pending_labels_hash != publication.labels_hash
            || pending_event != publication.event_name
            || publication_version != 2
        {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "Scale Set holder {holder:?} has conflicting or unrecoverable pending publication data"
            )));
        }
        tx.commit()?;
        Ok(demand)
    }

    /// Finish global-first Scale Set publication after its eligible source
    /// row is durably committed. Repeated calls are safe; if this process
    /// crashes first, `acquire` repairs the marker from the roster snapshot.
    pub fn complete_scale_set_offer(
        &mut self,
        holder: &str,
        source_path: &Path,
        generation: u64,
    ) -> Result<(), LedgerError> {
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        let ledger_path = self.path.clone();
        let roster_path = self.source_roster_path.clone();
        let expected_identity = self.expected_roster_identity.clone();
        let canonical_source = source_path.canonicalize().map_err(|error| {
            LedgerError::DemandSourcesUnready(format!(
                "resolve Scale Set demand source {}: {error}",
                source_path.display()
            ))
        })?;
        let source = source_id(&canonical_source)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (current, reconciled): (i64, i64) = tx.query_row(
            "SELECT generation, reconciled_generation FROM permit_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if current.max(0) as u64 != generation || current != reconciled {
            return Err(LedgerError::DemandSourcesUnready(
                "host permit generation changed before Scale Set queue publication completed"
                    .to_owned(),
            ));
        }
        let roster = read_demand_source_roster_at(&ledger_path, &roster_path)?;
        if roster.fingerprint != expected_identity.as_deref().unwrap_or_default()
            || !roster.paths.iter().any(|path| path == &canonical_source)
        {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "Scale Set source {} is absent from the current host roster",
                canonical_source.display()
            )));
        }
        let marker_source: Option<String> = tx
            .query_row(
                "SELECT source_id FROM permit_pending_demand_updates WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        if marker_source.is_none() {
            tx.commit()?;
            return Ok(());
        }
        if marker_source.as_deref() != Some(source.as_str()) {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "Scale Set holder {holder:?} has a pending publication for another source"
            )));
        }
        let observations = read_source_snapshot(&canonical_source, unix_now())?;
        let matching = observations
            .into_iter()
            .find(|observation| observation.holder == holder)
            .ok_or_else(|| {
                LedgerError::DemandSourcesUnready(format!(
                    "Scale Set holder {holder:?} is not durably eligible in {}; shared admission remains closed",
                    canonical_source.display()
                ))
            })?;
        let _ = resolve_pending_demand_updates_tx(&tx, &[matching])?;
        tx.commit()?;
        Ok(())
    }

    /// Read durable global-first publications that have not yet been
    /// confirmed by their Scale Set source database. Startup replays these
    /// records before source registration can open admission.
    pub fn pending_scale_set_demand_publications(
        &self,
    ) -> Result<Vec<PendingScaleSetDemandPublication>, LedgerError> {
        let mut statement = self.conn.prepare(
            "SELECT pending.holder, source.path, pending.generation,
                    pending.scale_set_id, pending.request_id,
                    pending.first_seen_unix, pending.first_seen_subsec_nanos,
                    pending.repo_owner, pending.repo_name, pending.job_id_hash,
                    pending.labels_hash, pending.event_name,
                    pending.publication_version
             FROM permit_pending_demand_updates AS pending
             LEFT JOIN permit_demand_sources AS source
               ON source.source_id = pending.source_id
             ORDER BY pending.source_id, pending.holder",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, i64>(12)?,
            ))
        })?;
        let mut pending = Vec::new();
        for row in rows {
            let (
                holder,
                source_db,
                generation,
                scale_set_id,
                request_id,
                first_seen_unix,
                first_seen_subsec_nanos,
                repo_owner,
                repo_name,
                job_id_hash,
                labels_hash,
                event_name,
                version,
            ) = row?;
            if version != 2 {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "pending Scale Set publication for {holder:?} has unsupported metadata version {version}; admission remains closed"
                )));
            }
            let source_db = source_db.ok_or_else(|| {
                LedgerError::DemandSourcesUnready(format!(
                    "pending Scale Set publication for {holder:?} names an unregistered source; admission remains closed"
                ))
            })?;
            let scale_set_id = i32::try_from(scale_set_id).map_err(|_| {
                LedgerError::DemandSourcesUnready(format!(
                    "pending Scale Set publication for {holder:?} has an invalid scale-set ID"
                ))
            })?;
            if generation < 0
                || first_seen_unix < 0
                || !(0..=999_999_999).contains(&first_seen_subsec_nanos)
                || repo_owner.is_empty()
                || repo_name.is_empty()
                || event_name.is_empty()
                || labels_hash.is_empty()
            {
                return Err(LedgerError::DemandSourcesUnready(format!(
                    "pending Scale Set publication for {holder:?} has invalid replay metadata; admission remains closed"
                )));
            }
            pending.push(PendingScaleSetDemandPublication {
                holder,
                source_db: PathBuf::from(source_db),
                generation: generation as u64,
                first_seen_unix: first_seen_unix as u64,
                first_seen_subsec_nanos: first_seen_subsec_nanos as u32,
                publication: ScaleSetDemandPublication {
                    request_id,
                    scale_set_id,
                    repo_owner,
                    repo_name,
                    job_id_hash,
                    labels_hash,
                    event_name,
                },
            });
        }
        Ok(pending)
    }

    /// Cancel an eligible demand that its lane has confirmed is no longer
    /// available upstream. A held permit cannot be cancelled through this
    /// path; cleanup must release it first.
    pub fn cancel_demand(&mut self, holder: &str) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE permit_demands SET state = 'cancelled', updated_unix = ?1
             WHERE holder = ?2 AND state = 'eligible'
               AND NOT EXISTS (SELECT 1 FROM permits WHERE holder = ?2)",
            params![unix_now() as i64, holder],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Close an upstream demand only when no permit is currently held.
    /// This path is for terminal callbacks that prove a request never spent
    /// capacity; it cannot delete or authorize deletion of a permit row.
    pub fn close_demand_if_unheld(
        &mut self,
        holder: &str,
        final_state: DemandState,
    ) -> Result<bool, LedgerError> {
        if !matches!(final_state, DemandState::Terminal | DemandState::Cancelled) {
            return Err(LedgerError::DemandSourcesUnready(
                "unheld demand close requires terminal or cancelled state".to_owned(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM permit_demands WHERE holder = ?1)",
            params![holder],
            |row| row.get(0),
        )?;
        if !exists {
            tx.commit()?;
            return Ok(false);
        }
        let changed = tx.execute(
            "UPDATE permit_demands SET state = ?1, updated_unix = ?2
             WHERE holder = ?3
               AND NOT EXISTS (SELECT 1 FROM permits WHERE holder = ?3)
               AND state NOT IN ('terminal', 'cancelled')",
            params![final_state.as_str(), unix_now() as i64, holder],
        )?;
        let state: String = tx.query_row(
            "SELECT state FROM permit_demands WHERE holder = ?1",
            params![holder],
            |row| row.get(0),
        )?;
        let confirmed = DemandState::parse(&state) == Some(final_state)
            && !tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM permits WHERE holder = ?1)",
                params![holder],
                |row| row.get(0),
            )?;
        tx.commit()?;
        Ok(changed > 0 || confirmed)
    }

    /// Current state of one holder's permit, if held.
    pub fn holder_state(&self, holder: &str) -> Result<Option<PermitState>, LedgerError> {
        let state: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        state
            .map(|state| PermitState::parse(&state).ok_or_else(|| LedgerError::UnknownState(state)))
            .transpose()
    }

    /// Immutable identity of the currently held acquisition, if any. Marker
    /// reconciliation uses it to confirm an exact old lease and never
    /// fabricates a replacement after release committed but marker unlink did
    /// not.
    pub fn permit_lease_generation(&self, holder: &str) -> Result<Option<u64>, LedgerError> {
        let generation: Option<i64> = self
            .conn
            .query_row(
                "SELECT lease_generation FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        Ok(generation.map(|value| value.max(0) as u64))
    }

    /// Acquire one permit for `holder`, fenced on `generation`.
    ///
    /// Idempotent: a holder that already holds keeps its permit (its state
    /// is left untouched) and reports [`AcquireOutcome::AlreadyHeld`]. A
    /// fresh holder acquires only when capacity is available and it is the
    /// oldest eligible demand. Permit insertion and the eligible-to-granted
    /// transition commit together.
    ///
    /// `pid` records the acquiring host process as diagnostic recovery
    /// evidence; lanes whose holders are not host processes pass `None`.
    pub fn acquire(
        &mut self,
        holder: &str,
        lane: PermitLane,
        state: PermitState,
        generation: u64,
        pid: Option<u32>,
    ) -> Result<AcquireOutcome, LedgerError> {
        self.acquire_with_lease_generation(holder, lane, state, generation, pid, None)
            .map(|(outcome, _)| outcome)
    }

    /// Acquire one permit and return its immutable per-acquisition identity
    /// for both new and already-held rows. The latter identity fences any
    /// later dead-owner adoption against a same-holder replacement.
    pub fn acquire_with_lease_generation(
        &mut self,
        holder: &str,
        lane: PermitLane,
        state: PermitState,
        generation: u64,
        pid: Option<u32>,
        pid_identity: Option<&str>,
    ) -> Result<(AcquireOutcome, Option<u64>), LedgerError> {
        self.acquire_at(
            holder,
            lane,
            state,
            generation,
            pid,
            pid_identity,
            unix_now_parts(),
        )
    }

    fn acquire_at(
        &mut self,
        holder: &str,
        lane: PermitLane,
        state: PermitState,
        generation: u64,
        pid: Option<u32>,
        pid_identity: Option<&str>,
        now_parts: (u64, u32),
    ) -> Result<(AcquireOutcome, Option<u64>), LedgerError> {
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        let ledger_path = self.path.clone();
        let roster_path = self.source_roster_path.clone();
        let expected_roster_identity = self.expected_roster_identity.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (current, reconciled): (i64, i64) = tx.query_row(
            "SELECT generation, reconciled_generation FROM permit_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if current.max(0) as u64 != generation {
            return Ok((AcquireOutcome::StaleGeneration, None));
        }
        let (now, now_subsec_nanos) = now_parts;
        if current != reconciled {
            tx.commit()?;
            return Ok((AcquireOutcome::NotReady, None));
        }
        let sources_ready = refresh_registered_sources_tx(
            &tx,
            &ledger_path,
            &roster_path,
            expected_roster_identity.as_deref(),
            now,
        )?;
        if !sources_ready {
            tx.commit()?;
            return Ok((AcquireOutcome::NotReady, None));
        }
        let held: Option<(String, String)> = tx
            .query_row(
                "SELECT lane, state FROM permits WHERE holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((held_lane, held_state)) = held {
            if held_lane != lane.as_str() {
                return Err(LedgerError::DemandLaneMismatch {
                    holder: holder.to_owned(),
                    expected: lane,
                    seen: held_lane,
                });
            }
            let held_state = PermitState::parse(&held_state)
                .ok_or_else(|| LedgerError::UnknownState(held_state.clone()))?;
            // Only a pre-RunService Acquiring lease can be a native
            // duplicate delivery. Running, cleaning, uncertain, and all
            // other held states stay counted and cannot proceed as an
            // unowned duplicate.
            if held_state != PermitState::Acquiring {
                return Ok((AcquireOutcome::Closed, None));
            }
            // An existing permit must already have its granted queue record.
            // Never manufacture or reopen that record here: terminal or
            // missing demand is evidence uncertainty, not permission to
            // revive a held job.
            let demand = read_demand(&tx, holder)?;
            let Some(demand) = demand else {
                return Ok((AcquireOutcome::Closed, None));
            };
            if demand.state != DemandState::Granted {
                return Ok((AcquireOutcome::Closed, None));
            }
            let lease_generation: i64 = tx.query_row(
                "SELECT lease_generation FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )?;
            tx.commit()?;
            return Ok((
                AcquireOutcome::AlreadyHeld,
                Some(lease_generation.max(0) as u64),
            ));
        }
        let max: Option<i64> =
            tx.query_row("SELECT max_jobs FROM permit_meta WHERE id = 1", [], |row| {
                row.get(0)
            })?;
        let Some(max) = max.and_then(|max| u32::try_from(max).ok()) else {
            return Ok((AcquireOutcome::NotConfigured, None));
        };

        let demand = ensure_demand_tx(
            &tx,
            holder,
            lane,
            "",
            now,
            now_subsec_nanos,
            now,
            DemandState::Eligible,
        )?;
        if demand.state == DemandState::Terminal || demand.state == DemandState::Cancelled {
            return Ok((AcquireOutcome::Closed, None));
        }
        if demand.state == DemandState::Granted {
            // A granted demand without its permit can only come from an
            // interrupted older release path. It is eligible again because
            // no capacity is currently held for it.
            tx.execute(
                "UPDATE permit_demands SET state = 'eligible', updated_unix = ?1
                 WHERE holder = ?2",
                params![i64::try_from(now).unwrap_or(i64::MAX), holder],
            )?;
        } else {
            tx.execute(
                "UPDATE permit_demands SET updated_unix = ?1 WHERE holder = ?2",
                params![i64::try_from(now).unwrap_or(i64::MAX), holder],
            )?;
        }
        let occupied: i64 = tx.query_row("SELECT COUNT(*) FROM permits", [], |row| row.get(0))?;
        if occupied.max(0) as u64 >= u64::from(max) {
            tx.commit()?;
            return Ok((AcquireOutcome::Full, None));
        }
        let fresh_after = now.saturating_sub(DEMAND_STALE_AFTER_SECS);
        let oldest: String = tx.query_row(
            "SELECT holder FROM permit_demands WHERE state = 'eligible'
               AND updated_unix > ?1
             ORDER BY first_seen_unix, first_seen_subsec_nanos, sequence LIMIT 1",
            params![i64::try_from(fresh_after).unwrap_or(i64::MAX)],
            |row| row.get(0),
        )?;
        if oldest != holder {
            tx.commit()?;
            return Ok((AcquireOutcome::Deferred, None));
        }
        let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
        let admission_roster_hash = expected_roster_identity.as_deref().ok_or_else(|| {
            LedgerError::DemandSourcesUnready("no process roster identity".to_owned())
        })?;
        let lease_generation = next_lease_generation_tx(&tx)?;
        tx.execute(
            "INSERT INTO permits
                (holder, lane, state, acquired_unix, updated_unix, generation, pid,
                 pid_identity, admission_roster_hash, lease_generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                holder,
                lane.as_str(),
                state.as_str(),
                now_i64,
                now_i64,
                i64::try_from(generation).unwrap_or(i64::MAX),
                pid.map(i64::from),
                pid_identity,
                admission_roster_hash,
                lease_generation,
            ],
        )?;
        tx.execute(
            "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
             WHERE holder = ?2",
            params![now_i64, holder],
        )?;
        tx.commit()?;
        Ok((AcquireOutcome::Acquired, Some(lease_generation as u64)))
    }

    /// Adopt only a dead native process's exact pre-RunService acquisition.
    /// The caller supplies the immutable lease observed on `AlreadyHeld`;
    /// this transaction verifies that lease, current epoch, acquiring state,
    /// and granted demand before changing owner. Running, uncertain, cleaning,
    /// terminal, cancelled, or unknown states are never retried here.
    pub fn adopt_if_pid_dead_with_lease_generation(
        &mut self,
        holder: &str,
        lane: PermitLane,
        state: PermitState,
        generation: u64,
        expected_lease_generation: u64,
        pid: u32,
        pid_identity: &str,
        is_same_process: &dyn Fn(u32, &str) -> bool,
    ) -> Result<(AdoptOutcome, Option<u64>), LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if current.max(0) as u64 != generation {
            return Ok((AdoptOutcome::StaleGeneration, None));
        }
        let row: Option<(String, String, Option<i64>, Option<String>, i64)> = tx
            .query_row(
                "SELECT lane, state, pid, pid_identity, lease_generation
                 FROM permits WHERE holder = ?1",
                params![holder],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((row_lane, row_state, row_pid, row_pid_identity, row_lease_generation)) = row
        else {
            return Ok((AdoptOutcome::Missing, None));
        };
        if lane != PermitLane::Native
            || state != PermitState::Acquiring
            || row_lane != lane.as_str()
            || row_lease_generation.max(0) as u64 != expected_lease_generation
            || PermitState::parse(&row_state) != Some(PermitState::Acquiring)
        {
            return Ok((AdoptOutcome::NotAdoptable, None));
        }
        let demand = read_demand(&tx, holder)?;
        if demand.is_none_or(|demand| demand.state != DemandState::Granted) {
            return Ok((AdoptOutcome::NotAdoptable, None));
        }
        let Some(row_pid) = row_pid.and_then(|pid| u32::try_from(pid).ok()) else {
            return Ok((AdoptOutcome::NotAdoptable, None));
        };
        let Some(row_pid_identity) = row_pid_identity else {
            return Ok((AdoptOutcome::NotAdoptable, None));
        };
        if is_same_process(row_pid, &row_pid_identity) {
            return Ok((AdoptOutcome::LiveHolder, None));
        }
        let (now, _) = unix_now_parts();
        let lease_generation = next_lease_generation_tx(&tx)?;
        let updated = tx.execute(
            "UPDATE permits
             SET state = ?1, updated_unix = ?2, generation = ?3, pid = ?4,
                 pid_identity = ?5, lease_generation = ?6
             WHERE holder = ?7 AND lane = 'native' AND state = 'acquiring'
               AND lease_generation = ?8 AND pid = ?9",
            params![
                state.as_str(),
                i64::try_from(now).unwrap_or(i64::MAX),
                i64::try_from(generation).unwrap_or(i64::MAX),
                i64::from(pid),
                pid_identity,
                lease_generation,
                holder,
                i64::try_from(expected_lease_generation).unwrap_or(i64::MAX),
                i64::from(row_pid),
            ],
        )?;
        if updated == 0 {
            return Ok((AdoptOutcome::Missing, None));
        }
        tx.commit()?;
        Ok((AdoptOutcome::Adopted, Some(lease_generation as u64)))
    }

    /// Transition only the exact immutable permit lease. Unlike the epoch
    /// transition above, this preserves both acquisition identity and the
    /// row's mutable epoch fence.
    pub fn transition_if_generation(
        &mut self,
        holder: &str,
        state: PermitState,
        expected_lease_generation: u64,
    ) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let lease_generation: Option<i64> = tx
            .query_row(
                "SELECT lease_generation FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        if lease_generation.map(|value| value.max(0) as u64) != Some(expected_lease_generation) {
            tx.commit()?;
            return Ok(false);
        }
        let updated = tx.execute(
            "UPDATE permits SET state = ?1, updated_unix = ?2
             WHERE holder = ?3 AND lease_generation = ?4",
            params![
                state.as_str(),
                i64::try_from(unix_now()).unwrap_or(i64::MAX),
                holder,
                i64::try_from(expected_lease_generation).unwrap_or(i64::MAX),
            ],
        )?;
        tx.commit()?;
        Ok(updated > 0)
    }

    /// Release only the exact immutable lease acquired by this attempt.
    /// A stale cleanup replay cannot release a later same-holder acquisition.
    pub fn release_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, expected_lease_generation, DemandState::Terminal)
    }

    /// Requeue only the exact immutable lease acquired by this attempt.
    pub fn release_to_eligible_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, expected_lease_generation, DemandState::Eligible)
    }

    /// Cancel only the exact immutable lease acquired by this attempt.
    pub fn release_cancelled_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, LedgerError> {
        self.release_with_demand_state(holder, expected_lease_generation, DemandState::Cancelled)
    }

    fn release_with_demand_state(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
        next_demand_state: DemandState,
    ) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let permit: Option<(String, i64)> = tx
            .query_row(
                "SELECT lane, lease_generation FROM permits WHERE holder = ?1",
                params![holder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((raw_lane, actual_lease_generation)) = permit.as_ref() else {
            let current_demand = read_demand(&tx, holder)?;
            // A repeated cleanup may find that its exact lease was removed
            // before a crash. The durable final demand state is its replay
            // receipt; a missing or different receipt keeps recovery closed.
            let already_final =
                current_demand.is_some_and(|demand| demand.state == next_demand_state);
            tx.commit()?;
            return Ok(already_final);
        };
        if (*actual_lease_generation).max(0) as u64 != expected_lease_generation {
            tx.commit()?;
            return Ok(false);
        }
        let Some(demand) = read_demand(&tx, holder)? else {
            // Keep the lease counted when its queue receipt is absent.
            tx.commit()?;
            return Ok(false);
        };
        let permit_lane = PermitLane::parse(raw_lane)
            .ok_or_else(|| LedgerError::UnknownLane(raw_lane.clone()))?;
        if demand.lane != permit_lane {
            tx.commit()?;
            return Ok(false);
        }
        if next_demand_state == DemandState::Eligible
            && !matches!(demand.state, DemandState::Eligible | DemandState::Granted)
        {
            tx.commit()?;
            return Ok(false);
        }
        let lease_generation = *actual_lease_generation;
        let expected_lease_generation_i64 =
            i64::try_from(expected_lease_generation).unwrap_or(i64::MAX);
        let removed = {
            // The delete trigger rejects every writer that cannot name the
            // exact lease being removed. The authorization row lives only
            // inside this immediate transaction and is removed before it
            // commits.
            tx.execute(
                "INSERT INTO permit_release_authorizations (holder, lease_generation)
                 VALUES (?1, ?2)",
                params![holder, lease_generation],
            )?;
            let removed = tx.execute(
                "DELETE FROM permits WHERE holder = ?1 AND lease_generation = ?2",
                params![holder, expected_lease_generation_i64],
            )?;
            tx.execute(
                "DELETE FROM permit_release_authorizations
                 WHERE holder = ?1 AND lease_generation = ?2",
                params![holder, lease_generation],
            )?;
            removed
        };
        if removed == 0 {
            tx.commit()?;
            return Ok(false);
        }
        match next_demand_state {
            DemandState::Terminal | DemandState::Cancelled => {
                tx.execute(
                    "UPDATE permit_demands SET state = ?1, updated_unix = ?2
                     WHERE holder = ?3",
                    params![next_demand_state.as_str(), now, holder],
                )?;
            }
            DemandState::Eligible => {
                // Never revive work that has already been closed.
                tx.execute(
                    "UPDATE permit_demands SET state = 'eligible', updated_unix = ?1
                     WHERE holder = ?2 AND state IN ('eligible', 'granted')",
                    params![now, holder],
                )?;
            }
            DemandState::Granted => {}
        }
        tx.commit()?;
        Ok(removed > 0)
    }

    /// Retain an uncertain permit after cleanup could not be confirmed.
    /// The permit and demand transition commit together so failure cannot
    /// expose false capacity or leave a served demand blocking the queue.
    pub fn retain_uncertain(&mut self, holder: &str, generation: u64) -> Result<(), LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: i64 = tx.query_row(
            "SELECT generation FROM permit_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        if current.max(0) as u64 != generation {
            return Err(LedgerError::StaleGeneration {
                expected: current.max(0) as u64,
                seen: generation,
            });
        }
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let updated = tx.execute(
            "UPDATE permits SET state = 'uncertain', updated_unix = ?1, generation = ?2
             WHERE holder = ?3",
            params![now, i64::try_from(generation).unwrap_or(i64::MAX), holder],
        )?;
        if updated == 0 {
            return Err(LedgerError::UnknownHolder(holder.to_owned()));
        }
        tx.execute(
            "UPDATE permit_demands SET state = 'terminal', updated_unix = ?1
             WHERE holder = ?2 AND state IN ('eligible', 'granted')",
            params![now, holder],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Retain uncertain occupancy only for the exact immutable permit
    /// lease. A stale guard cannot retag a newer same-holder acquisition.
    pub fn retain_uncertain_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, LedgerError> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let lease_generation: Option<i64> = tx
            .query_row(
                "SELECT lease_generation FROM permits WHERE holder = ?1",
                params![holder],
                |row| row.get(0),
            )
            .optional()?;
        if lease_generation.map(|value| value.max(0) as u64) != Some(expected_lease_generation) {
            tx.commit()?;
            return Ok(false);
        }
        let now = i64::try_from(unix_now()).unwrap_or(i64::MAX);
        let updated = tx.execute(
            "UPDATE permits SET state = 'uncertain', updated_unix = ?1
             WHERE holder = ?2 AND lease_generation = ?3",
            params![
                now,
                holder,
                i64::try_from(expected_lease_generation).unwrap_or(i64::MAX),
            ],
        )?;
        if updated > 0 {
            tx.execute(
                "UPDATE permit_demands SET state = 'terminal', updated_unix = ?1
                 WHERE holder = ?2 AND state IN ('eligible', 'granted')",
                params![now, holder],
            )?;
        }
        tx.commit()?;
        Ok(updated > 0)
    }

    /// Reconcile durable occupancy against observed live work. This partial
    /// API never opens admission; only `reconcile_host_roster` can attest all
    /// demand sources and mark the current epoch ready.
    ///
    /// `alive` is the caller's attested live set: `(holder, lane, state)`.
    /// Observed holders without a row are adopted as counted occupancy;
    /// recorded holders outside the set are marked uncertain (still
    /// counted). Nothing is ever deleted here.
    pub fn reconcile(
        &mut self,
        alive: &[(&str, PermitLane, PermitState)],
    ) -> Result<ReconcileReport, LedgerError> {
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        self.reconcile_inner(alive, None, None)
    }

    /// Reconcile after the caller attested every demand store and native
    /// marker directory from the complete host roster. Only this path may
    /// mark the generation ready for acquisition.
    pub fn reconcile_host_roster(
        &mut self,
        roster: &PermitDemandSourceRoster,
        expected_generation: u64,
        alive: &[(&str, PermitLane, PermitState)],
    ) -> Result<ReconcileReport, LedgerError> {
        let _roster_lock =
            lock_demand_source_roster(&self.path, rustix::fs::FlockOperation::LockShared)?;
        let declared = match read_demand_source_roster_at(&self.path, &self.source_roster_path) {
            Ok(declared) => declared,
            Err(error) => {
                self.close_demand_source_gate()?;
                return Err(error);
            }
        };
        if declared != *roster || roster.ledger_path.as_deref() != Some(self.path.as_path()) {
            self.close_demand_source_gate()?;
            return Err(LedgerError::DemandSourcesUnready(
                "full-host reconciliation roster does not match the fixed host roster".to_owned(),
            ));
        }
        self.configure_demand_source_roster(roster)?;
        self.reconcile_inner(alive, Some(roster), Some(expected_generation))
    }

    fn reconcile_inner(
        &mut self,
        alive: &[(&str, PermitLane, PermitState)],
        complete_roster: Option<&PermitDemandSourceRoster>,
        expected_generation: Option<u64>,
    ) -> Result<ReconcileReport, LedgerError> {
        let ledger_path = self.path.clone();
        let roster_path = self.source_roster_path.clone();
        let expected_identity = self.expected_roster_identity.clone();
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (generation, previous_reconciled, sources_ready, stored_identity): (
            i64,
            i64,
            bool,
            Option<String>,
        ) = tx.query_row(
            "SELECT generation, reconciled_generation, demand_sources_ready = 1,
                    demand_roster_hash
             FROM permit_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        let roster = match read_demand_source_roster_at(&ledger_path, &roster_path) {
            Ok(roster) => roster,
            Err(error) => {
                close_source_gate_tx(&tx)?;
                tx.commit()?;
                return Err(error);
            }
        };
        let expected = expected_identity.as_deref();
        if !sources_ready
            || expected.is_none()
            || stored_identity.as_deref() != expected
            || roster.fingerprint != expected.unwrap_or_default()
            || !registered_sources_match(&tx, &roster)?
        {
            close_source_gate_tx(&tx)?;
            tx.commit()?;
            return Err(LedgerError::DemandSourcesUnready(
                "full-host source registry is not ready or no longer matches its roster".to_owned(),
            ));
        }
        if let Some(expected_generation) = expected_generation
            && generation.max(0) as u64 != expected_generation
        {
            tx.commit()?;
            return Err(LedgerError::StaleGeneration {
                expected: generation.max(0) as u64,
                seen: expected_generation,
            });
        }
        if complete_roster.is_none()
            && (!roster.paths.is_empty() || !roster.native_marker_dirs.is_empty())
        {
            tx.commit()?;
            return Err(LedgerError::DemandSourcesUnready(
                "partial reconciliation cannot open admission; attest the complete host roster"
                    .to_owned(),
            ));
        }
        if complete_roster.is_some_and(|complete| complete != &roster) {
            close_source_gate_tx(&tx)?;
            tx.commit()?;
            return Err(LedgerError::DemandSourcesUnready(
                "host roster changed before full reconciliation committed".to_owned(),
            ));
        }
        let admission_roster_hash = expected.unwrap_or_default();
        // Reconcile inserts may adopt live holders. The immediate transaction
        // excludes every concurrent acquire while the trigger's generation
        // predicate is temporarily satisfied; low-level/partial reconciles
        // restore the prior readiness value before commit.
        tx.execute(
            "UPDATE permit_meta SET reconciled_generation = generation WHERE id = 1",
            [],
        )?;
        let mut report = ReconcileReport::default();
        let now = unix_now() as i64;
        for (holder, lane, state) in alive {
            let held: Option<String> = tx
                .query_row(
                    "SELECT holder FROM permits WHERE holder = ?1",
                    params![*holder],
                    |row| row.get(0),
                )
                .optional()?;
            if held.is_none() {
                let lease_generation = next_lease_generation_tx(&tx)?;
                tx.execute(
                    "INSERT INTO permits
                        (holder, lane, state, acquired_unix, updated_unix, generation,
                         admission_roster_hash, lease_generation)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        *holder,
                        lane.as_str(),
                        state.as_str(),
                        now,
                        now,
                        generation,
                        admission_roster_hash,
                        lease_generation
                    ],
                )?;
                report.adopted.push((*holder).to_string());
            } else {
                report.confirmed.push((*holder).to_string());
            }
            let demand = ensure_demand_tx(
                &tx,
                holder,
                *lane,
                "",
                now.max(0) as u64,
                0,
                now.max(0) as u64,
                DemandState::Granted,
            )?;
            if demand.state == DemandState::Eligible {
                tx.execute(
                    "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
                     WHERE holder = ?2",
                    params![now, holder],
                )?;
            }
        }
        let mut select = tx.prepare("SELECT holder, lane FROM permits")?;
        let recorded: Vec<(String, String)> = select
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        drop(select);
        for (holder, raw_lane) in &recorded {
            if alive.iter().any(|(live, _, _)| live == holder) {
                continue;
            }
            let lane = PermitLane::parse(raw_lane)
                .ok_or_else(|| LedgerError::UnknownLane(raw_lane.clone()))?;
            tx.execute(
                "UPDATE permits SET state = 'uncertain', updated_unix = ?1, generation = ?2
                 WHERE holder = ?3",
                params![now, generation, holder],
            )?;
            let demand = ensure_demand_tx(
                &tx,
                holder,
                lane,
                "",
                now.max(0) as u64,
                0,
                now.max(0) as u64,
                DemandState::Granted,
            )?;
            if demand.state == DemandState::Eligible {
                tx.execute(
                    "UPDATE permit_demands SET state = 'granted', updated_unix = ?1
                     WHERE holder = ?2",
                    params![now, holder],
                )?;
            }
            report.marked_uncertain.push(holder.clone());
        }
        let final_reconciled = if complete_roster.is_some() {
            generation
        } else {
            previous_reconciled
        };
        tx.execute(
            "UPDATE permit_meta SET reconciled_generation = ?1 WHERE id = 1",
            params![final_reconciled],
        )?;
        tx.commit()?;
        report.adopted.sort();
        report.marked_uncertain.sort();
        report.confirmed.sort();
        Ok(report)
    }

    /// Number of permits in `state` (observability; every state counts).
    pub fn occupied_in_state(&self, state: PermitState) -> Result<u32, LedgerError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM permits WHERE state = ?1",
            params![state.as_str()],
            |row| row.get(0),
        )?;
        Ok(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
    }
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

    fn temp_ledger(name: &str) -> (PermitLedger, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-ledger-{name}-{}-{}",
            std::process::id(),
            unix_now(),
        ));
        let path = dir.join("permit-ledger.db");
        let ledger = PermitLedger::open(&path).unwrap();
        (ledger, dir)
    }

    fn reconcile_host(
        ledger: &mut PermitLedger,
        generation: u64,
        alive: &[(&str, PermitLane, PermitState)],
    ) -> ReconcileReport {
        let roster = read_demand_source_roster(ledger.path()).unwrap();
        ledger
            .reconcile_host_roster(&roster, generation, alive)
            .unwrap()
    }

    fn assert_open_rejects_schema_mutation(name: &str, mutation: &str) {
        let (ledger, dir) = temp_ledger(name);
        let path = ledger.path().to_path_buf();
        drop(ledger);
        let raw = Connection::open(&path).unwrap();
        raw.execute_batch(mutation).unwrap();
        drop(raw);
        let error = match PermitLedger::open(&path) {
            Ok(_) => panic!("malformed permit schema was accepted: {name}"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            LedgerError::DemandSourcesUnready(message)
                if message.contains("malformed")
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_named_lease_fences_and_index_fail_closed() {
        assert_open_rejects_schema_mutation(
            "bad-insert-fence",
            "DROP TRIGGER trg_permits_lease_insert_fence;
             CREATE TRIGGER trg_permits_lease_insert_fence
             BEFORE INSERT ON permits
             WHEN COALESCE(NEW.lease_generation, 0) > 0
               AND NEW.admission_roster_hash IS NOT NULL
             BEGIN SELECT RAISE(ABORT, 'wrong insert fence'); END;",
        );
        assert_open_rejects_schema_mutation(
            "bad-delete-fence",
            "DROP TRIGGER trg_permits_release_fence;
             CREATE TRIGGER trg_permits_release_fence
             BEFORE DELETE ON permits
             WHEN EXISTS (SELECT 1 FROM permit_release_authorizations AS a
                          WHERE a.holder = OLD.holder
                            AND a.lease_generation = OLD.lease_generation)
             BEGIN SELECT RAISE(ABORT, 'wrong delete fence'); END;",
        );
        assert_open_rejects_schema_mutation(
            "bad-lease-index",
            "DROP INDEX idx_permits_lease_generation;
             CREATE UNIQUE INDEX idx_permits_lease_generation ON permits(holder);",
        );
    }

    #[test]
    fn inverted_admission_operator_fails_schema_verification() {
        assert_open_rejects_schema_mutation(
            "bad-admission-fence",
            "DROP TRIGGER trg_permits_admission_fence;
             CREATE TRIGGER trg_permits_admission_fence
             BEFORE INSERT ON permits
             WHEN COALESCE((SELECT demand_sources_ready FROM permit_meta WHERE id = 1), 0) = 1
             BEGIN SELECT RAISE(ABORT, 'wrong admission fence'); END;",
        );
    }

    #[test]
    fn stale_lease_cannot_transition_same_holder_reacquisition() {
        let (mut ledger, dir) = temp_ledger("stale-transition");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        reconcile_host(&mut ledger, generation, &[]);
        let (first_outcome, first_lease) = ledger
            .acquire_with_lease_generation(
                "native/stale-transition",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(std::process::id()),
                Some("test-identity-1"),
            )
            .unwrap();
        assert_eq!(first_outcome, AcquireOutcome::Acquired);
        let first_lease = first_lease.unwrap();
        assert!(ledger
            .release_to_eligible_if_generation("native/stale-transition", first_lease)
            .unwrap());
        let (second_outcome, second_lease) = ledger
            .acquire_with_lease_generation(
                "native/stale-transition",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(std::process::id()),
                Some("test-identity-2"),
            )
            .unwrap();
        assert_eq!(second_outcome, AcquireOutcome::Acquired);
        let second_lease = second_lease.unwrap();
        assert_ne!(first_lease, second_lease);
        assert!(!ledger
            .transition_if_generation("native/stale-transition", PermitState::Running, first_lease,)
            .unwrap());
        assert_eq!(
            ledger.holder_state("native/stale-transition").unwrap(),
            Some(PermitState::Acquiring)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn host_roster_requires_explicit_state_databases() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-roster-state-dbs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ledger_path = dir.join("permit-ledger.db");
        let state_db = dir.join("daemon-state.db");
        let demand_db = dir.join("scale-demand.db");
        let marker_dir = dir.join("native-slot");
        std::fs::write(&state_db, b"").unwrap();
        std::fs::write(&demand_db, b"").unwrap();
        std::fs::create_dir(&marker_dir).unwrap();
        let roster_path = demand_source_roster_path(&ledger_path);

        std::fs::write(
            &roster_path,
            format!(
                "permit-ledger {}\ndemand-db {}\nnative-slot {}\n",
                ledger_path.display(),
                demand_db.display(),
                marker_dir.display(),
            ),
        )
        .unwrap();
        let error = read_demand_source_roster(&ledger_path).unwrap_err();
        assert!(matches!(
            error,
            LedgerError::DemandSourcesUnready(message)
                if message.contains("every daemon state database")
        ));

        std::fs::write(
            &roster_path,
            format!(
                "permit-ledger {}\nstate-db {}\ndemand-db {}\nnative-slot {}\n",
                ledger_path.display(),
                state_db.display(),
                demand_db.display(),
                marker_dir.display(),
            ),
        )
        .unwrap();
        let parsed = read_demand_source_roster(&ledger_path).unwrap();
        let constructed = PermitDemandSourceRoster::for_ledger(
            &ledger_path,
            [state_db],
            [demand_db],
            [marker_dir],
        )
        .unwrap();
        assert_eq!(parsed, constructed);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn permit_lease_identity_survives_epoch_and_rejects_stale_release() {
        let (mut ledger, _dir) = temp_ledger("immutable-lease-generation");
        ledger.set_max_jobs(1).unwrap();
        let first_epoch = ledger.begin_epoch().unwrap();
        let roster = read_demand_source_roster(ledger.path()).unwrap();
        ledger
            .reconcile_host_roster(&roster, first_epoch, &[])
            .unwrap();

        let (first, first_lease) = ledger
            .acquire_with_lease_generation(
                "native/replayed-request",
                PermitLane::Native,
                PermitState::Acquiring,
                first_epoch,
                Some(101),
                Some("pid-101-start-1"),
            )
            .unwrap();
        assert_eq!(first, AcquireOutcome::Acquired);
        let first_lease = first_lease.unwrap();
        assert!(ledger
            .release_to_eligible_if_generation("native/replayed-request", first_lease)
            .unwrap());

        let (second, second_lease) = ledger
            .acquire_with_lease_generation(
                "native/replayed-request",
                PermitLane::Native,
                PermitState::Acquiring,
                first_epoch,
                Some(102),
                Some("pid-102-start-2"),
            )
            .unwrap();
        assert_eq!(second, AcquireOutcome::Acquired);
        let second_lease = second_lease.unwrap();
        assert_ne!(first_lease, second_lease);
        assert!(!ledger
            .release_if_generation("native/replayed-request", first_lease)
            .unwrap());
        assert!(!ledger
            .retain_uncertain_if_generation("native/replayed-request", first_lease)
            .unwrap());
        assert_eq!(ledger.occupied().unwrap(), 1);

        let next_epoch = ledger.begin_epoch().unwrap();
        let roster = read_demand_source_roster(ledger.path()).unwrap();
        ledger
            .reconcile_host_roster(
                &roster,
                next_epoch,
                &[(
                    "native/replayed-request",
                    PermitLane::Native,
                    PermitState::Running,
                )],
            )
            .unwrap();
        assert!(ledger
            .release_if_generation("native/replayed-request", second_lease)
            .unwrap());
        assert!(ledger
            .release_if_generation("native/replayed-request", second_lease)
            .unwrap());
        assert_eq!(ledger.occupied().unwrap(), 0);
    }

    #[test]
    fn migrated_ledger_fences_legacy_insert_and_holder_only_delete() {
        let (mut ledger, dir) = temp_ledger("legacy-writer-fence");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.begin_epoch().unwrap();
        reconcile_host(&mut ledger, generation, &[]);

        let legacy_insert = ledger.conn.execute(
            "INSERT INTO permits
                (holder, lane, state, acquired_unix, updated_unix, generation, pid)
             VALUES ('native/legacy-writer', 'native', 'acquiring', 1, 1, 0, NULL)",
            [],
        );
        assert!(legacy_insert.is_err(), "legacy insert bypassed lease fence");
        let roster_hash: String = ledger
            .conn
            .query_row(
                "SELECT demand_roster_hash FROM permit_meta WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let missing_lease = ledger.conn.execute(
            "INSERT INTO permits
                (holder, lane, state, acquired_unix, updated_unix, generation, pid,
                 admission_roster_hash)
             VALUES ('native/missing-lease', 'native', 'acquiring', 1, 1, ?1, NULL, ?2)",
            params![i64::try_from(generation).unwrap(), roster_hash],
        );
        assert!(
            missing_lease.is_err(),
            "insert with valid roster but no immutable lease bypassed lease fence"
        );
        assert_eq!(ledger.occupied().unwrap(), 0);

        let holder = "native/replaced-lease";
        let now = unix_now();
        ledger
            .observe_demand(holder, PermitLane::Native, "scope", now, now)
            .unwrap();
        let (first, first_lease) = ledger
            .acquire_with_lease_generation(
                holder,
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                None,
                None,
            )
            .unwrap();
        assert_eq!(first, AcquireOutcome::Acquired);
        let first_lease = first_lease.unwrap();
        assert!(ledger
            .release_to_eligible_if_generation(holder, first_lease)
            .unwrap());
        let (second, second_lease) = ledger
            .acquire_with_lease_generation(
                holder,
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                None,
                None,
            )
            .unwrap();
        assert_eq!(second, AcquireOutcome::Acquired);
        let second_lease = second_lease.unwrap();
        assert_ne!(first_lease, second_lease);

        let legacy_delete = ledger
            .conn
            .execute("DELETE FROM permits WHERE holder = ?1", params![holder]);
        assert!(
            legacy_delete.is_err(),
            "holder-only delete released replacement lease"
        );
        assert_eq!(
            ledger.permit_lease_generation(holder).unwrap(),
            Some(second_lease)
        );
        assert!(ledger.release_if_generation(holder, second_lease).unwrap());
        let authorization_rows: i64 = ledger
            .conn
            .query_row(
                "SELECT COUNT(*) FROM permit_release_authorizations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(authorization_rows, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lease_migration_preserves_existing_hold_and_blocks_unfenced_delete() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-lease-migration-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permit-ledger.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE permit_meta (
                     id INTEGER PRIMARY KEY CHECK (id = 1),
                     max_jobs INTEGER,
                     generation INTEGER NOT NULL,
                     reconciled_generation INTEGER NOT NULL
                 );
                 INSERT INTO permit_meta VALUES (1, 1, 1, 1);
                 CREATE TABLE permits (
                     holder TEXT PRIMARY KEY,
                     lane TEXT NOT NULL,
                     state TEXT NOT NULL,
                     acquired_unix INTEGER NOT NULL,
                     updated_unix INTEGER NOT NULL,
                     generation INTEGER NOT NULL,
                     pid INTEGER
                 );
                 INSERT INTO permits VALUES
                     ('native/legacy-held', 'native', 'running', 1, 1, 1, 101);
                 CREATE TABLE permit_demands (
                     holder TEXT PRIMARY KEY,
                     lane TEXT NOT NULL,
                     scope TEXT NOT NULL,
                     first_seen_unix INTEGER NOT NULL,
                     first_seen_subsec_nanos INTEGER NOT NULL DEFAULT 0,
                     sequence INTEGER NOT NULL UNIQUE,
                     state TEXT NOT NULL,
                     updated_unix INTEGER NOT NULL
                 );
                 INSERT INTO permit_demands VALUES
                     ('native/legacy-held', 'native', 'scope', 1, 0, 1, 'granted', 1);",
            )
            .unwrap();
        }

        let mut ledger = PermitLedger::open(&path).unwrap();
        let lease = ledger
            .permit_lease_generation("native/legacy-held")
            .unwrap()
            .expect("migration must preserve the existing hold");
        assert!(lease > 0);
        let old_delete = ledger.conn.execute(
            "DELETE FROM permits WHERE holder = 'native/legacy-held'",
            [],
        );
        assert!(old_delete.is_err(), "legacy writer deleted migrated hold");
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert!(ledger
            .release_if_generation("native/legacy-held", lease)
            .unwrap());
        assert_eq!(ledger.occupied().unwrap(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn generation_fenced_release_preserves_lease_without_demand_receipt() {
        let (mut ledger, _dir) = temp_ledger("release-needs-demand-receipt");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        let (outcome, lease) = ledger
            .acquire_with_lease_generation(
                "native/missing-demand-receipt",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(203),
                Some("pid-203-start-1"),
            )
            .unwrap();
        assert_eq!(outcome, AcquireOutcome::Acquired);
        let lease = lease.unwrap();

        ledger
            .conn
            .execute(
                "DELETE FROM permit_demands WHERE holder = ?1",
                params!["native/missing-demand-receipt"],
            )
            .unwrap();
        assert!(!ledger
            .release_if_generation("native/missing-demand-receipt", lease)
            .unwrap());
        assert_eq!(ledger.occupied().unwrap(), 1);

        ledger
            .conn
            .execute(
                "INSERT INTO permit_demands
                 (holder, lane, scope, first_seen_unix, first_seen_subsec_nanos,
                  sequence, state, updated_unix)
                 VALUES ('native/missing-demand-receipt', 'native', 'scope', 1, 0,
                         1, 'unrecognized', 1)",
                [],
            )
            .unwrap();
        assert!(matches!(
            ledger.release_if_generation("native/missing-demand-receipt", lease),
            Err(LedgerError::UnknownDemandState(_))
        ));
        assert_eq!(ledger.occupied().unwrap(), 1);
    }

    #[test]
    fn duplicate_running_holder_is_closed_instead_of_already_held() {
        let (mut ledger, dir) = temp_ledger("running-duplicate-closed");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = "native/v1/0123456789abcdef/request-1";
        assert_eq!(
            ledger
                .acquire(
                    holder,
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    Some(101),
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger.demand(holder).unwrap().unwrap().state,
            DemandState::Granted
        );
        assert_eq!(
            ledger
                .acquire(
                    holder,
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(202),
                )
                .unwrap(),
            AcquireOutcome::Closed
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fractional_age_schema_migration_preserves_existing_demand() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-ledger-precision-migration-{}-{}",
            std::process::id(),
            unix_now(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permit-ledger.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE permit_demands (
                    holder TEXT PRIMARY KEY,
                    lane TEXT NOT NULL,
                    scope TEXT NOT NULL,
                    first_seen_unix INTEGER NOT NULL,
                    sequence INTEGER NOT NULL UNIQUE,
                    state TEXT NOT NULL,
                    updated_unix INTEGER NOT NULL
                );
                CREATE INDEX idx_permit_demands_oldest
                    ON permit_demands (state, first_seen_unix, sequence);
                INSERT INTO permit_demands
                    (holder, lane, scope, first_seen_unix, sequence, state, updated_unix)
                VALUES ('native/legacy', 'native', 'scope', 123, 4, 'eligible', 456);",
            )
            .unwrap();

        let ledger = PermitLedger::open(&path).unwrap();
        let migrated = ledger.demand("native/legacy").unwrap().unwrap();
        assert_eq!(migrated.first_seen_unix, 123);
        assert_eq!(migrated.first_seen_subsec_nanos, 0);
        assert_eq!(migrated.sequence, 4);
        drop(ledger);

        let reopened = PermitLedger::open(&path).unwrap();
        let replayed = reopened.demand("native/legacy").unwrap().unwrap();
        assert_eq!(replayed.first_seen_unix, 123);
        assert_eq!(replayed.first_seen_subsec_nanos, 0);
        assert_eq!(replayed.sequence, 4);
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn acquire_grants_until_full_then_refuses() {
        let (mut ledger, dir) = temp_ledger("full");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();

        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger
                .acquire(
                    "b",
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(ledger.occupied().unwrap(), 2);
        // A third holder is refused; nothing was spent.
        assert_eq!(
            ledger
                .acquire(
                    "c",
                    PermitLane::Native,
                    PermitState::Reserved,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Full
        );
        assert_eq!(ledger.occupied().unwrap(), 2);

        // Lanes share the one N: a Scale Set holder is refused too.
        assert_eq!(
            ledger
                .acquire(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Full
        );
        assert_eq!(ledger.occupied_by_lane(PermitLane::ScaleSet).unwrap(), 0);

        let a_lease = ledger.permit_lease_generation("a").unwrap().unwrap();
        assert!(ledger.release_if_generation("a", a_lease).unwrap());
        // The older queued native demand gets the newly freed permit first.
        assert_eq!(
            ledger
                .acquire(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Deferred
        );
        assert_eq!(
            ledger
                .acquire(
                    "c",
                    PermitLane::Native,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        let b_lease = ledger.permit_lease_generation("b").unwrap().unwrap();
        assert!(ledger.release_if_generation("b", b_lease).unwrap());
        assert_eq!(
            ledger
                .acquire(
                    "d",
                    PermitLane::ScaleSet,
                    PermitState::Provisioning,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(ledger.occupied_by_lane(PermitLane::ScaleSet).unwrap(), 1);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn duplicate_delivery_holds_once() {
        let (mut ledger, dir) = temp_ledger("dup");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();

        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        // Same holder, redelivered: no second permit.
        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::AlreadyHeld
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        // ... and the duplicate does not evict the other waiter either.
        assert_eq!(
            ledger
                .acquire(
                    "b",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Full
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_native_scope_rekey_preserves_queue_identity_and_rejects_ambiguity() {
        let (mut ledger, dir) = temp_ledger("legacy-native-scope-rekey");
        ledger.set_max_jobs(2).unwrap();
        let old_holder = "native/request-7";
        let new_holder =
            "native/v1/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/request-7";
        let legacy_scope = "https://github.com/acme/repo";
        let canonical_scope = "https://api.github.com/repos/acme/repo";
        let original = ledger
            .observe_demand_with_subsecond(
                old_holder,
                PermitLane::Native,
                legacy_scope,
                10_000,
                123_456_789,
                10_001,
            )
            .unwrap();

        assert!(ledger
            .rekey_legacy_native_demand(
                old_holder,
                new_holder,
                "https://github.com/other/repo",
                canonical_scope,
            )
            .is_err());
        assert_eq!(ledger.demand(old_holder).unwrap(), Some(original.clone()));

        assert!(ledger
            .rekey_legacy_native_demand(old_holder, new_holder, legacy_scope, canonical_scope,)
            .unwrap());
        assert_eq!(ledger.demand(old_holder).unwrap(), None);
        let rekeyed = ledger.demand(new_holder).unwrap().unwrap();
        assert_eq!(rekeyed.scope, canonical_scope);
        assert_eq!(rekeyed.first_seen_unix, original.first_seen_unix);
        assert_eq!(
            rekeyed.first_seen_subsec_nanos,
            original.first_seen_subsec_nanos
        );
        assert_eq!(rekeyed.sequence, original.sequence);

        // A terminal legacy request remains terminal and cannot be converted
        // into a new scoped identity that would admit a duplicate delivery.
        let terminal_legacy = "native/request-8";
        let terminal_scoped =
            "native/v1/cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc/request-8";
        ledger
            .observe_demand(
                terminal_legacy,
                PermitLane::Native,
                legacy_scope,
                10_010,
                10_011,
            )
            .unwrap();
        assert!(ledger.cancel_demand(terminal_legacy).unwrap());
        assert!(ledger
            .rekey_legacy_native_demand(
                terminal_legacy,
                terminal_scoped,
                legacy_scope,
                canonical_scope,
            )
            .is_err());
        assert_eq!(
            ledger.demand(terminal_legacy).unwrap().unwrap().state,
            DemandState::Cancelled
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn acquire_never_reopens_uncertain_or_terminal_held_demand() {
        let (mut ledger, dir) = temp_ledger("closed-held-demand");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        assert_eq!(
            ledger
                .acquire(
                    "uncertain",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(101),
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        ledger.retain_uncertain("uncertain", generation).unwrap();
        assert_eq!(
            ledger
                .acquire(
                    "uncertain",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(102),
                )
                .unwrap(),
            AcquireOutcome::Closed
        );
        assert_eq!(
            ledger.holder_state("uncertain").unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger.demand("uncertain").unwrap().unwrap().state,
            DemandState::Terminal
        );

        assert_eq!(
            ledger
                .acquire(
                    "terminal-demand",
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    Some(103),
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        ledger
            .conn
            .execute(
                "UPDATE permit_demands SET state = 'terminal' WHERE holder = ?1",
                params!["terminal-demand"],
            )
            .unwrap();
        assert_eq!(
            ledger
                .acquire(
                    "terminal-demand",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(104),
                )
                .unwrap(),
            AcquireOutcome::Closed
        );
        assert_eq!(
            ledger.holder_state("terminal-demand").unwrap(),
            Some(PermitState::Running)
        );
        assert_eq!(ledger.occupied().unwrap(), 2);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_state_counts_and_transitions_keep_occupancy() {
        let (mut ledger, dir) = temp_ledger("states");
        ledger.set_max_jobs(8).unwrap();
        let generation = ledger.generation().unwrap();
        let states = [
            PermitState::Reserved,
            PermitState::Acquiring,
            PermitState::Provisioning,
            PermitState::Assignable,
            PermitState::Running,
            PermitState::Cleaning,
            PermitState::Uncertain,
        ];
        for (index, state) in states.iter().enumerate() {
            let holder = format!("job-{index}");
            assert_eq!(
                ledger
                    .acquire(&holder, PermitLane::Native, *state, generation, None)
                    .unwrap(),
                AcquireOutcome::Acquired
            );
        }
        assert_eq!(ledger.occupied().unwrap(), 7);
        let lease = ledger.permit_lease_generation("job-0").unwrap().unwrap();
        assert!(ledger
            .transition_if_generation("job-0", PermitState::Running, lease)
            .unwrap());
        assert_eq!(ledger.occupied().unwrap(), 7);
        assert_eq!(
            ledger.holder_state("job-0").unwrap(),
            Some(PermitState::Running)
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn immutable_lease_transition_survives_epoch_and_exact_release_is_replayable() {
        let (mut ledger, dir) = temp_ledger("generation");
        ledger.set_max_jobs(2).unwrap();
        let stale = ledger.generation().unwrap();
        let current = ledger.begin_epoch().unwrap();
        assert!(current > stale);

        assert_eq!(
            ledger
                .acquire("a", PermitLane::Native, PermitState::Acquiring, stale, None)
                .unwrap(),
            AcquireOutcome::StaleGeneration
        );
        reconcile_host(&mut ledger, current, &[]);
        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    current,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        let lease = ledger.permit_lease_generation("a").unwrap().unwrap();
        assert!(ledger
            .transition_if_generation("a", PermitState::Running, lease)
            .unwrap());
        assert_eq!(
            ledger.holder_state("a").unwrap(),
            Some(PermitState::Running)
        );
        // Cleanup releases the immutable lease in one immediate transaction;
        // exact replay sees the durable Terminal demand receipt.
        assert!(ledger.release_if_generation("a", lease).unwrap());
        assert!(ledger.release_if_generation("a", lease).unwrap());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oldest_eligible_is_granted_across_lanes_and_redelivery_keeps_age() {
        let (mut ledger, dir) = temp_ledger("global-order");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        let now = unix_now();
        ledger
            .observe_demand("native/older", PermitLane::Native, "scope-a", now, now)
            .unwrap();
        let younger = ledger
            .observe_demand(
                "scaleset/7/younger",
                PermitLane::ScaleSet,
                "set-7",
                now + 1,
                now + 1,
            )
            .unwrap();
        let original = (younger.first_seen_unix, younger.sequence);

        // A younger Scale Set lane has spare capacity but cannot pass the
        // older native demand before the shared grant transaction commits.
        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/younger",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Deferred
        );
        let redelivered = ledger
            .observe_demand(
                "scaleset/7/younger",
                PermitLane::ScaleSet,
                "set-7",
                now.saturating_sub(100),
                now + 2,
            )
            .unwrap();
        assert_eq!(
            (redelivered.first_seen_unix, redelivered.sequence),
            original
        );

        assert_eq!(
            ledger
                .acquire(
                    "native/older",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger.demand("native/older").unwrap().unwrap().state,
            DemandState::Granted
        );
        // Once the oldest row owns a permit, it leaves the eligible queue;
        // another free host slot is available to the next demand.
        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/younger",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scale_set_batch_hides_partial_queue_from_peer_native_acquire() {
        use std::sync::mpsc::{channel, RecvTimeoutError};

        let (mut setup, dir) = temp_ledger("atomic-demand-backfill");
        setup.set_max_jobs(1).unwrap();
        let generation = setup.begin_epoch().unwrap();
        reconcile_host(&mut setup, generation, &[]);
        let first_seen_unix = unix_now().saturating_sub(10);
        drop(setup);

        // Open both daemon connections before the batch begins, as two
        // daemons do during startup against the same host ledger.
        let peer_path = dir.join("permit-ledger.db");
        let mut peer = PermitLedger::open(&peer_path).unwrap();
        let batch_path = peer_path.clone();
        let (first_inserted_tx, first_inserted_rx) = channel();
        let (continue_batch_tx, continue_batch_rx) = channel();
        let batch = std::thread::spawn(move || {
            let mut ledger = PermitLedger::open(&batch_path).unwrap();
            let observed_unix = unix_now();
            let newer = PermitDemandObservation {
                holder: "scaleset/7/newer",
                lane: PermitLane::ScaleSet,
                scope: "scaleset/7",
                first_seen_unix,
                first_seen_subsec_nanos: 800_000_000,
                observed_unix,
            };
            let older = PermitDemandObservation {
                holder: "scaleset/7/older",
                lane: PermitLane::ScaleSet,
                scope: "scaleset/7",
                first_seen_unix,
                first_seen_subsec_nanos: 100_000_000,
                observed_unix,
            };
            let mut index = 0;
            let observations = std::iter::from_fn(move || match index {
                0 => {
                    index = 1;
                    Some(newer)
                }
                1 => {
                    // This runs after the first row was inserted, while its
                    // transaction still owns SQLite's immediate write lock.
                    let _ = first_inserted_tx.send(());
                    let _ = continue_batch_rx.recv();
                    index = 2;
                    Some(older)
                }
                _ => None,
            });
            ledger.observe_demands(observations).unwrap()
        });

        first_inserted_rx.recv().unwrap();
        let (attempting_tx, attempting_rx) = channel();
        let (native_done_tx, native_done_rx) = channel();
        let native = std::thread::spawn(move || {
            let _ = attempting_tx.send(());
            peer.observe_demand_with_subsecond(
                "native/later",
                PermitLane::Native,
                "native/scope",
                first_seen_unix,
                500_000_000,
                unix_now(),
            )
            .unwrap();
            let outcome = peer
                .acquire(
                    "native/later",
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    None,
                )
                .unwrap();
            let _ = native_done_tx.send(outcome);
        });
        attempting_rx.recv().unwrap();
        let early = match native_done_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(outcome) => Some(outcome),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("native admission thread exited"),
        };

        // Always finish the writer before asserting, so a failing regression
        // cannot strand its batch thread while the test unwinds.
        continue_batch_tx.send(()).unwrap();
        batch.join().unwrap();
        let outcome = early.or_else(|| {
            native_done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .ok()
        });
        native.join().unwrap();

        assert_eq!(early, None, "peer acquired from a partially mirrored queue");
        assert_eq!(outcome, Some(AcquireOutcome::Deferred));
        let mut ledger = PermitLedger::open(&peer_path).unwrap();
        assert_eq!(
            ledger
                .demand("scaleset/7/older")
                .unwrap()
                .unwrap()
                .first_seen_subsec_nanos,
            100_000_000
        );
        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/older",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_acquire_cannot_let_younger_cross_older() {
        use std::sync::mpsc::{channel, Receiver, Sender};

        let (mut ledger, dir) = temp_ledger("concurrent-order");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.begin_epoch().unwrap();
        reconcile_host(&mut ledger, generation, &[]);
        let now = unix_now();
        ledger
            .observe_demand("native/older", PermitLane::Native, "", now, now)
            .unwrap();
        ledger
            .observe_demand(
                "scaleset/7/younger",
                PermitLane::ScaleSet,
                "",
                now + 1,
                now + 1,
            )
            .unwrap();
        drop(ledger);

        fn spawn_acquire(
            path: PathBuf,
            holder: &'static str,
            lane: PermitLane,
            state: PermitState,
            generation: u64,
        ) -> (
            Sender<()>,
            Receiver<Result<(), String>>,
            Receiver<Result<AcquireOutcome, String>>,
            std::thread::JoinHandle<()>,
        ) {
            let (ready_tx, ready_rx) = channel();
            let (start_tx, start_rx) = channel();
            let (done_tx, done_rx) = channel();
            let worker = std::thread::spawn(move || {
                let mut ledger = match PermitLedger::open(&path) {
                    Ok(ledger) => ledger,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                if ready_tx.send(Ok(())).is_err() {
                    return;
                }
                if start_rx
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .is_err()
                {
                    let _ = done_tx.send(Err("acquire worker start timed out".to_owned()));
                    return;
                }
                let outcome = ledger
                    .acquire(holder, lane, state, generation, None)
                    .map_err(|error| error.to_string());
                let _ = done_tx.send(outcome);
            });
            (start_tx, ready_rx, done_rx, worker)
        }

        let older_path = dir.join("permit-ledger.db");
        let (older_start, older_ready, older_done, older) = spawn_acquire(
            older_path,
            "native/older",
            PermitLane::Native,
            PermitState::Acquiring,
            generation,
        );
        let younger_path = dir.join("permit-ledger.db");
        let (younger_start, younger_ready, younger_done, younger) = spawn_acquire(
            younger_path,
            "scaleset/7/younger",
            PermitLane::ScaleSet,
            PermitState::Reserved,
            generation,
        );
        let older_status = older_ready.recv_timeout(std::time::Duration::from_secs(3));
        let younger_status = younger_ready.recv_timeout(std::time::Duration::from_secs(3));
        // Release both workers even when startup fails. This prevents a
        // panic-before-ready from leaving a peer parked forever.
        let _ = older_start.send(());
        let _ = younger_start.send(());
        assert_eq!(older_status.unwrap().unwrap(), ());
        assert_eq!(younger_status.unwrap().unwrap(), ());
        let older_outcome = older_done
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .unwrap();
        let younger_outcome = younger_done
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap()
            .unwrap();
        older.join().unwrap();
        younger.join().unwrap();

        assert!(matches!(
            (older_outcome, younger_outcome),
            (
                AcquireOutcome::Acquired,
                AcquireOutcome::Full | AcquireOutcome::Deferred
            )
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn retry_release_preserves_age_and_uncertain_cleanup_keeps_occupancy() {
        let (mut ledger, dir) = temp_ledger("release-order");
        ledger.set_max_jobs(2).unwrap();
        let generation = ledger.generation().unwrap();
        let now = unix_now();
        let original = ledger
            .observe_demand("native/first", PermitLane::Native, "", now, now)
            .unwrap();
        ledger
            .observe_demand(
                "scaleset/7/second",
                PermitLane::ScaleSet,
                "",
                now + 1,
                now + 1,
            )
            .unwrap();
        assert_eq!(
            ledger
                .acquire(
                    "native/first",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        let lease = ledger
            .permit_lease_generation("native/first")
            .unwrap()
            .unwrap();
        assert!(ledger
            .release_to_eligible_if_generation("native/first", lease)
            .unwrap());
        assert_eq!(
            ledger
                .demand("native/first")
                .unwrap()
                .unwrap()
                .first_seen_unix,
            original.first_seen_unix
        );
        assert_eq!(
            ledger
                .acquire(
                    "scaleset/7/second",
                    PermitLane::ScaleSet,
                    PermitState::Reserved,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Deferred
        );
        assert_eq!(
            ledger
                .acquire(
                    "native/first",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert!(ledger.retain_uncertain("native/first", generation).is_ok());
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.demand("native/first").unwrap().unwrap().state,
            DemandState::Terminal
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_migrates_native_demand_into_global_sequence() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-ledger-migrate-{}-{}",
            std::process::id(),
            unix_now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("permit-ledger.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE native_demand (
                    request_id TEXT PRIMARY KEY,
                    scope TEXT NOT NULL,
                    first_seen_unix INTEGER NOT NULL,
                    sequence INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    updated_unix INTEGER NOT NULL
                );
                INSERT INTO native_demand VALUES
                    ('legacy-request', 'scope-a', 100, 8, 'eligible', 120);",
            )
            .unwrap();
        }
        let ledger = PermitLedger::open(&path).unwrap();
        let demand = ledger.demand("native/legacy-request").unwrap().unwrap();
        assert_eq!(demand.lane, PermitLane::Native);
        assert_eq!(demand.scope, "scope-a");
        assert_eq!(demand.first_seen_unix, 100);
        assert_eq!(demand.state, DemandState::Eligible);
        let legacy_exists: bool = ledger
            .conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'native_demand'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!legacy_exists);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unconfigured_ledgers_grant_nothing() {
        let (mut ledger, dir) = temp_ledger("unconfigured");
        let generation = ledger.generation().unwrap();
        assert_eq!(ledger.max_jobs().unwrap(), None);
        assert_eq!(
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::NotConfigured
        );
        assert_eq!(ledger.occupied().unwrap(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reconcile_adopts_marks_and_gates_advertisement() {
        let (mut ledger, dir) = temp_ledger("reconcile");
        ledger.set_max_jobs(4).unwrap();
        let generation = ledger.begin_epoch().unwrap();
        assert_eq!(ledger.advertised_free().unwrap(), None);
        assert_eq!(
            ledger
                .acquire(
                    "old",
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::NotReady
        );
        reconcile_host(&mut ledger, generation, &[]);
        assert_eq!(ledger.advertised_free().unwrap(), Some(4));
        ledger
            .acquire(
                "old",
                PermitLane::Native,
                PermitState::Running,
                generation,
                None,
            )
            .unwrap();
        assert_eq!(ledger.advertised_free().unwrap(), Some(3));

        let report = reconcile_host(
            &mut ledger,
            generation,
            &[("new", PermitLane::Native, PermitState::Running)],
        );
        assert_eq!(report.adopted, vec!["new".to_string()]);
        assert_eq!(report.marked_uncertain, vec!["old".to_string()]);
        assert!(report.confirmed.is_empty());
        // Adopted and uncertain rows both count; nothing was erased.
        assert_eq!(ledger.occupied().unwrap(), 2);
        assert_eq!(
            ledger.holder_state("old").unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(ledger.advertised_free().unwrap(), Some(2));

        // A new epoch requires a fresh reconcile before advertising.
        let generation = ledger.begin_epoch().unwrap();
        assert_eq!(ledger.advertised_free().unwrap(), None);
        let report = reconcile_host(
            &mut ledger,
            generation,
            &[
                ("new", PermitLane::Native, PermitState::Running),
                ("old", PermitLane::Native, PermitState::Cleaning),
            ],
        );
        assert!(report.adopted.is_empty());
        assert!(report.marked_uncertain.is_empty());
        assert_eq!(report.confirmed.len(), 2);
        assert_eq!(ledger.advertised_free().unwrap(), Some(2));

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pending_worker_cleanup_in_state_db_closes_admission() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-ledger-pending-worker-cleanup-state-db-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ledger_path = dir.join("permit-ledger.db");

        let state_db = dir.join("scale-state.db");
        Connection::open(&state_db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE scaleset_workers (
                    ownership_id TEXT PRIMARY KEY,
                    worker_state TEXT NOT NULL
                 );
                 CREATE TABLE scaleset_worker_runtime (
                    ownership_id TEXT PRIMARY KEY,
                    state_dir_cleanup_pending INTEGER NOT NULL
                 );
                 INSERT INTO scaleset_workers VALUES ('7/velnor-7-1', 'permit_released');
                 INSERT INTO scaleset_worker_runtime VALUES ('7/velnor-7-1', 1);",
            )
            .unwrap();
        let native_state_db = dir.join("native-state.db");
        Connection::open(&native_state_db).unwrap();

        let demand_db = dir.join("demand.db");
        Connection::open(&demand_db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE scaleset_demand (
                    request_id INTEGER PRIMARY KEY,
                    scale_set_id INTEGER NOT NULL,
                    first_seen_at TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    updated_at TEXT NOT NULL,
                    state TEXT NOT NULL
                 );",
            )
            .unwrap();
        std::fs::write(
            demand_source_roster_path(&ledger_path),
            format!(
                "permit-ledger {}\nstate-db {}\nstate-db {}\ndemand-db {}\n",
                ledger_path.display(),
                state_db.display(),
                native_state_db.display(),
                demand_db.display(),
            ),
        )
        .unwrap();

        let roster = read_demand_source_roster(&ledger_path).unwrap();
        let mut ledger = PermitLedger::open(&ledger_path).unwrap();
        ledger.set_max_jobs(1).unwrap();
        assert!(roster
            .state_db_paths
            .contains(&state_db.canonicalize().unwrap()));
        assert!(roster
            .state_db_paths
            .contains(&native_state_db.canonicalize().unwrap()));
        ledger.configure_demand_source_roster(&roster).unwrap();
        let generation = ledger.begin_epoch().unwrap();
        ledger
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();

        let now = unix_now();
        ledger
            .observe_demand("native/cleanup-fence", PermitLane::Native, "", now, now)
            .unwrap();
        assert_eq!(ledger.advertised_free().unwrap(), None);
        assert_eq!(
            ledger
                .acquire(
                    "native/cleanup-fence",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::NotReady,
            "worker state lives outside the demand DB and must still fence peers"
        );

        Connection::open(&state_db)
            .unwrap()
            .execute(
                "UPDATE scaleset_worker_runtime SET state_dir_cleanup_pending = 0",
                [],
            )
            .unwrap();
        assert_eq!(ledger.advertised_free().unwrap(), Some(1));
        assert_eq!(
            ledger
                .acquire(
                    "native/cleanup-fence",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        let lease = ledger
            .permit_lease_generation("native/cleanup-fence")
            .unwrap()
            .unwrap();
        assert!(ledger
            .release_if_generation("native/cleanup-fence", lease)
            .unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adopt_takes_over_dead_attempts_and_refuses_the_rest() {
        let (mut ledger, dir) = temp_ledger("adopt");
        ledger.set_max_jobs(5).unwrap();
        let generation = ledger.generation().unwrap();
        let (dead_outcome, dead_lease) = ledger
            .acquire_with_lease_generation(
                "dead",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(101),
                Some("pid-101-old"),
            )
            .unwrap();
        assert_eq!(dead_outcome, AcquireOutcome::Acquired);
        let dead_lease = dead_lease.unwrap();
        assert_eq!(
            ledger
                .acquire_with_lease_generation(
                    "dead",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(101),
                    Some("pid-101-old"),
                )
                .unwrap(),
            (AcquireOutcome::AlreadyHeld, Some(dead_lease))
        );
        ledger
            .acquire_with_lease_generation(
                "live",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(102),
                Some("pid-102-live"),
            )
            .unwrap();
        ledger
            .acquire(
                "noid",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                None,
            )
            .unwrap();
        ledger
            .acquire(
                "official",
                PermitLane::ScaleSet,
                PermitState::Running,
                generation,
                Some(103),
            )
            .unwrap();
        let (uncertain_outcome, uncertain_lease) = ledger
            .acquire_with_lease_generation(
                "uncertain",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                Some(104),
                Some("pid-104-old"),
            )
            .unwrap();
        assert_eq!(uncertain_outcome, AcquireOutcome::Acquired);
        let uncertain_lease = uncertain_lease.unwrap();

        let same_process = |pid: u32, identity: &str| pid == 102 && identity == "pid-102-live";
        let (adopted, new_lease) = ledger
            .adopt_if_pid_dead_with_lease_generation(
                "dead",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                dead_lease,
                999,
                "pid-999-current",
                &same_process,
            )
            .unwrap();
        assert_eq!(adopted, AdoptOutcome::Adopted);
        let new_lease = new_lease.unwrap();
        assert_ne!(dead_lease, new_lease);
        assert_eq!(
            ledger
                .adopt_if_pid_dead_with_lease_generation(
                    "dead",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    dead_lease,
                    1000,
                    "pid-1000-current",
                    &same_process,
                )
                .unwrap(),
            (AdoptOutcome::NotAdoptable, None)
        );
        // Same row, new pid and state; occupancy unchanged.
        assert_eq!(ledger.occupied().unwrap(), 5);
        assert_eq!(
            ledger.holder_state("dead").unwrap(),
            Some(PermitState::Acquiring)
        );
        let holders = ledger.holders().unwrap();
        assert_eq!(
            holders
                .iter()
                .find(|holder| holder.holder == "dead")
                .unwrap()
                .pid,
            Some(999)
        );
        // Live pid, missing pid, foreign lane, and missing row all refuse.
        assert_eq!(
            ledger
                .adopt_if_pid_dead_with_lease_generation(
                    "live",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    ledger.permit_lease_generation("live").unwrap().unwrap(),
                    999,
                    "pid-999-current",
                    &same_process,
                )
                .unwrap(),
            (AdoptOutcome::LiveHolder, None)
        );
        assert_eq!(
            ledger
                .adopt_if_pid_dead_with_lease_generation(
                    "uncertain",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    uncertain_lease,
                    999,
                    "pid-999-current",
                    &same_process,
                )
                .unwrap(),
            (AdoptOutcome::NotAdoptable, None)
        );
        assert_eq!(
            ledger
                .adopt_if_pid_dead_with_lease_generation(
                    "noid",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    ledger.permit_lease_generation("noid").unwrap().unwrap(),
                    999,
                    "pid-999-current",
                    &same_process,
                )
                .unwrap(),
            (AdoptOutcome::NotAdoptable, None)
        );
        assert_eq!(
            ledger
                .adopt_if_pid_dead_with_lease_generation(
                    "official",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    ledger.permit_lease_generation("official").unwrap().unwrap(),
                    999,
                    "pid-999-current",
                    &same_process,
                )
                .unwrap(),
            (AdoptOutcome::NotAdoptable, None)
        );
        assert_eq!(
            ledger
                .adopt_if_pid_dead_with_lease_generation(
                    "gone",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    1,
                    999,
                    "pid-999-current",
                    &same_process,
                )
                .unwrap(),
            (AdoptOutcome::Missing, None)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pid_reuse_does_not_make_an_old_acquiring_lease_look_live() {
        let (mut ledger, dir) = temp_ledger("adopt-pid-reuse");
        ledger.set_max_jobs(1).unwrap();
        let generation = ledger.generation().unwrap();
        let (outcome, old_lease) = ledger
            .acquire_with_lease_generation(
                "native/v1/pid-reuse/request-1",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                Some(123),
                Some("old-process-start-token"),
            )
            .unwrap();
        assert_eq!(outcome, AcquireOutcome::Acquired);
        let old_lease = old_lease.unwrap();

        // The kernel PID exists again, but its stable identity differs from
        // the identity captured with the held lease. It is safe to adopt the
        // dead attempt, and the new process identity is committed atomically.
        let process_matches =
            |pid: u32, identity: &str| pid == 123 && identity == "reused-process-start-token";
        let (adopted, new_lease) = ledger
            .adopt_if_pid_dead_with_lease_generation(
                "native/v1/pid-reuse/request-1",
                PermitLane::Native,
                PermitState::Acquiring,
                generation,
                old_lease,
                456,
                "new-process-start-token",
                &process_matches,
            )
            .unwrap();
        assert_eq!(adopted, AdoptOutcome::Adopted);
        assert_ne!(new_lease, Some(old_lease));
        let lease = ledger
            .holders()
            .unwrap()
            .into_iter()
            .find(|row| row.holder == "native/v1/pid-reuse/request-1")
            .unwrap();
        assert_eq!(lease.pid, Some(456));
        assert_eq!(
            lease.pid_identity.as_deref(),
            Some("new-process-start-token")
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reconcile_retains_unobserved_rows_regardless_of_process_id() {
        let (mut ledger, dir) = temp_ledger("reconcile-retention");
        ledger.set_max_jobs(8).unwrap();
        let generation = ledger.generation().unwrap();
        // Reconcile uses holder attestations. PID evidence alone, including
        // a definitely absent pid, never proves teardown completed.
        ledger
            .acquire(
                "dead",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                Some(u32::MAX),
            )
            .unwrap();
        ledger
            .acquire(
                "live",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                Some(std::process::id()),
            )
            .unwrap();
        ledger
            .acquire(
                "kept",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                Some(3),
            )
            .unwrap();
        // Even a running-state row outside the attested set becomes
        // uncertain, while remaining counted.
        ledger
            .acquire(
                "running",
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(4),
            )
            .unwrap();
        // Native attempt with no recorded process id.
        ledger
            .acquire(
                "noid",
                PermitLane::Native,
                PermitState::Uncertain,
                generation,
                None,
            )
            .unwrap();
        ledger
            .acquire(
                "official",
                PermitLane::ScaleSet,
                PermitState::Uncertain,
                generation,
                Some(5),
            )
            .unwrap();

        ledger.begin_epoch().unwrap();
        let report = ledger.reconcile(&[]).unwrap();
        assert_eq!(report.marked_uncertain.len(), 6);
        assert_eq!(ledger.occupied().unwrap(), 6);
        assert_eq!(
            ledger.holder_state("dead").unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger.holder_state("official").unwrap(),
            Some(PermitState::Uncertain)
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn occupancy_survives_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-ledger-reopen-{}-{}",
            std::process::id(),
            unix_now()
        ));
        let path = dir.join("permit-ledger.db");
        let generation = {
            let mut ledger = PermitLedger::open(&path).unwrap();
            ledger.set_max_jobs(3).unwrap();
            let generation = ledger.begin_epoch().unwrap();
            reconcile_host(&mut ledger, generation, &[]);
            ledger
                .acquire(
                    "a",
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    None,
                )
                .unwrap();
            generation
        };
        let mut ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.max_jobs().unwrap(), Some(3));
        assert_eq!(ledger.generation().unwrap(), generation);
        assert_eq!(ledger.occupied().unwrap(), 1);
        // Capacity cannot exceed N after crash recovery: the surviving row
        // still counts against fresh acquisitions.
        assert_eq!(
            ledger
                .acquire(
                    "b",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger
                .acquire(
                    "c",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );
        assert_eq!(
            ledger
                .acquire(
                    "d",
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Full
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

//! Versioned, secret-safe performance telemetry at the model boundary.
//!
//! Telemetry is deliberately separate from control-plane [`Event`] values:
//! control events describe durable state transitions, while this envelope
//! describes observations that may be streamed, sampled, or retained in a
//! bounded ring.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::time::Timestamp;

/// Stable discriminator for the telemetry wire contract.
pub const TELEMETRY_SCHEMA: &str = "velnor.telemetry.v1";
pub const DEFAULT_TELEMETRY_FILE_BYTES: u64 = 8 * 1024 * 1024;

const MAX_TEXT_LEN: usize = 512;
const SECRET_MARKERS: [&str; 10] = [
    "password",
    "passwd",
    "secret",
    "token",
    "bearer",
    "authorization",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
];

macro_rules! telemetry_field_contracts {
    (
        [$( $required_name:literal => $required_kind:ident ),* $(,)?],
        [$( $optional_name:literal => $optional_kind:ident ),* $(,)?]
    ) => {
        &[
            $(
                TelemetryFieldContract {
                    name: $required_name,
                    kind: TelemetryFieldKind::$required_kind,
                    required: true,
                },
            )*
            $(
                TelemetryFieldContract {
                    name: $optional_name,
                    kind: TelemetryFieldKind::$optional_kind,
                    required: false,
                },
            )*
        ]
    };
}

macro_rules! define_telemetry_contracts {
    (
        $(
            $variant:ident => $event_name:literal {
                lane: $lane:expr,
                requires_action_key_digest: $requires_action_key_digest:expr,
                required: [
                    $( $required_name:literal => $required_kind:ident ),* $(,)?
                ],
                optional: [
                    $( $optional_name:literal => $optional_kind:ident ),* $(,)?
                ]
            }
        ),+ $(,)?
    ) => {
        /// Lifecycle or measurement boundary represented by a telemetry record.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum TelemetryEvent {
            $( $variant ),+
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum TelemetryFieldKind {
            Boolean,
            Integer,
            NonNegativeInteger,
            String,
            StringArray,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct TelemetryFieldContract {
            name: &'static str,
            kind: TelemetryFieldKind,
            required: bool,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct TelemetryEventContract {
            name: &'static str,
            event: TelemetryEvent,
            lane: Option<TelemetryLane>,
            requires_action_key_digest: bool,
            fields: &'static [TelemetryFieldContract],
        }

        static TELEMETRY_EVENT_CONTRACTS: &[TelemetryEventContract] = &[
            $(
                TelemetryEventContract {
                    name: $event_name,
                    event: TelemetryEvent::$variant,
                    lane: $lane,
                    requires_action_key_digest: $requires_action_key_digest,
                    fields: telemetry_field_contracts!(
                        [$($required_name => $required_kind),*],
                        [$($optional_name => $optional_kind),*]
                    ),
                }
            ),+
        ];

        impl TelemetryEvent {
            fn contract(self) -> Option<&'static TelemetryEventContract> {
                TELEMETRY_EVENT_CONTRACTS
                    .iter()
                    .find(|contract| contract.event == self)
            }

            #[cfg(test)]
            fn contracts() -> &'static [TelemetryEventContract] {
                TELEMETRY_EVENT_CONTRACTS
            }
        }
    };
}

define_telemetry_contracts! {
    RunQueued => "run_queued" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "queued_for_ms" => NonNegativeInteger,
            "queue_time_present" => Boolean,
        ],
        optional: []
    },
    RunAdmitted => "run_admitted" {
        lane: None,
        requires_action_key_digest: false,
        required: [],
        optional: []
    },
    ToolPrep => "tool_prep" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "ms" => NonNegativeInteger,
            "tool" => String,
        ],
        optional: []
    },
    CacheLookup => "cache_lookup" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "hit" => Boolean,
            "lookup_ms" => NonNegativeInteger,
            "store" => String,
        ],
        optional: [
            "miss_reason" => String,
            "physical_bytes_known" => Boolean,
            "shared_bytes" => NonNegativeInteger,
            "newly_allocated_bytes" => NonNegativeInteger,
        ]
    },
    CompileStart => "compile_start" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "compiler" => String,
            "metrics_known" => Boolean,
            "unit_count" => NonNegativeInteger,
            "wrapper_calls" => NonNegativeInteger,
            "hits" => NonNegativeInteger,
            "misses" => NonNegativeInteger,
        ],
        optional: []
    },
    CompileEnd => "compile_end" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "compiler" => String,
            "exit_code" => Integer,
            "metrics_known" => Boolean,
            "ms" => NonNegativeInteger,
            "unit_count" => NonNegativeInteger,
            "wrapper_calls" => NonNegativeInteger,
            "hits" => NonNegativeInteger,
            "misses" => NonNegativeInteger,
        ],
        optional: []
    },
    LinkEnd => "link_end" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "elapsed_ms" => NonNegativeInteger,
            "exit_code" => Integer,
            "runner_kind" => String,
            "success" => Boolean,
        ],
        optional: []
    },
    TestEnd => "test_end" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "exit_code" => Integer,
            "ms" => NonNegativeInteger,
            "passed" => Boolean,
            "runner" => String,
        ],
        optional: []
    },
    ArtifactMaterialize => "artifact_materialize" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "digest" => String,
            "ms" => NonNegativeInteger,
            "subset" => String,
        ],
        optional: [
            "physical_bytes_known" => Boolean,
            "shared_bytes" => NonNegativeInteger,
            "newly_allocated_bytes" => NonNegativeInteger,
        ]
    },
    PassiveWait => "passive_wait" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "cause" => String,
            "ms" => NonNegativeInteger,
        ],
        optional: [
            "stage" => String,
            "wait_reason" => String,
            "stage_started_unix_ms" => NonNegativeInteger,
        ]
    },
    CriticalPath => "critical_path" {
        lane: None,
        requires_action_key_digest: false,
        required: [],
        optional: []
    },
    PlanSummary => "plan_summary" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "counts_scope" => String,
            "planner_dimensions_scope" => String,
            "planner_dimensions_known" => Boolean,
            "duplicates_prevented" => NonNegativeInteger,
            "selection_reasons" => StringArray,
            "logical_tasks" => NonNegativeInteger,
            "physical_actions" => NonNegativeInteger,
            "counts_capped" => Boolean,
        ],
        optional: []
    },
    NoProgress => "no_progress" {
        lane: None,
        requires_action_key_digest: false,
        required: [
            "window_ms" => NonNegativeInteger,
            "last_event" => String,
        ],
        optional: []
    },
    LeaseAcquired => "lease_acquired" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "generation" => NonNegativeInteger,
            "logical_ms" => NonNegativeInteger,
        ],
        optional: []
    },
    LeaseRenewed => "lease_renewed" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "generation" => NonNegativeInteger,
            "logical_ms" => NonNegativeInteger,
        ],
        optional: []
    },
    LeaseReleased => "lease_released" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "generation" => NonNegativeInteger,
            "logical_ms" => NonNegativeInteger,
        ],
        optional: []
    },
    LeaseAbandoned => "lease_abandoned" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "generation" => NonNegativeInteger,
            "logical_ms" => NonNegativeInteger,
        ],
        optional: []
    },
    LeaseExpired => "lease_expired" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "generation" => NonNegativeInteger,
            "logical_ms" => NonNegativeInteger,
        ],
        optional: []
    },
    SupersessionAdopted => "supersession_adopted" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "live_consumers" => NonNegativeInteger,
        ],
        optional: [
            "reason" => String,
            "retained_until_ms" => NonNegativeInteger,
        ]
    },
    ConsumerDetached => "consumer_detached" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "live_consumers" => NonNegativeInteger,
        ],
        optional: [
            "reason" => String,
            "retained_until_ms" => NonNegativeInteger,
        ]
    },
    RetainedThenReaped => "retained_then_reaped" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "live_consumers" => NonNegativeInteger,
        ],
        optional: [
            "reason" => String,
            "retained_until_ms" => NonNegativeInteger,
        ]
    },
    RetentionKillSkipped => "retention_kill_skipped" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "live_consumers" => NonNegativeInteger,
        ],
        optional: [
            "reason" => String,
            "retained_until_ms" => NonNegativeInteger,
        ]
    },
    TrustRevoked => "trust_revoked" {
        lane: Some(TelemetryLane::Velnor),
        requires_action_key_digest: true,
        required: [
            "live_consumers" => NonNegativeInteger,
        ],
        optional: [
            "reason" => String,
            "retained_until_ms" => NonNegativeInteger,
        ]
    }
}

/// Execution lane that produced an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TelemetryLane {
    Github,
    Velnor,
}

/// JSON object carried by a telemetry record after secret-safety validation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TelemetryFields(BTreeMap<String, Value>);

impl TelemetryFields {
    /// Validate and store structured fields without retaining secret-bearing
    /// keys or values.
    pub fn new(fields: BTreeMap<String, Value>) -> Result<Self, InvalidTelemetry> {
        validate_object("fields", &fields)?;
        Ok(Self(fields))
    }

    /// Borrow the validated fields for serialization or inspection.
    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, Value> {
        &self.0
    }
}

impl TryFrom<BTreeMap<String, Value>> for TelemetryFields {
    type Error = InvalidTelemetry;

    fn try_from(fields: BTreeMap<String, Value>) -> Result<Self, Self::Error> {
        Self::new(fields)
    }
}

impl<'de> Deserialize<'de> for TelemetryFields {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = BTreeMap::<String, Value>::deserialize(deserializer)?;
        Self::new(fields).map_err(D::Error::custom)
    }
}

/// Inputs for constructing a [`TelemetryEnvelope`].
#[derive(Debug)]
pub struct TelemetryEnvelopeInput<'a> {
    /// Run identity, not a credential.
    pub run_id: &'a str,
    /// Optional 64-character action digest.
    pub action_key_digest: Option<&'a str>,
    /// Producing execution lane.
    pub lane: TelemetryLane,
    /// Repository identity.
    pub repo: &'a str,
    /// Runtime trust domain.
    pub trust_domain: &'a str,
    /// Lifecycle or measurement boundary.
    pub event: TelemetryEvent,
    /// Monotonic sequence assigned by the emitting process.
    pub ts_logical: u64,
    /// Wall-clock observation time.
    pub ts_wall: Timestamp,
    /// Secret-safe structured dimensions and measurements.
    pub fields: TelemetryFields,
}

/// A versioned performance observation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TelemetryEnvelope {
    schema_version: &'static str,
    run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_key_digest: Option<String>,
    lane: TelemetryLane,
    repo: String,
    trust_domain: String,
    event: TelemetryEvent,
    ts_logical: u64,
    ts_wall: Timestamp,
    fields: TelemetryFields,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl TelemetryEnvelope {
    /// Construct a telemetry envelope after validating all free-form identity
    /// fields and the nested field object.
    pub fn new(input: TelemetryEnvelopeInput<'_>) -> Result<Self, InvalidTelemetry> {
        let run_id = validate_text("run_id", input.run_id)?;
        let repo = validate_text("repo", input.repo)?;
        let trust_domain = validate_text("trust_domain", input.trust_domain)?;
        let action_key_digest = input.action_key_digest.map(validate_digest).transpose()?;
        validate_event_contract(
            input.event,
            input.lane,
            action_key_digest.is_some(),
            &input.fields,
        )?;

        Ok(Self {
            schema_version: TELEMETRY_SCHEMA,
            run_id,
            action_key_digest,
            lane: input.lane,
            repo,
            trust_domain,
            event: input.event,
            ts_logical: input.ts_logical,
            ts_wall: input.ts_wall,
            fields: input.fields,
            extra: BTreeMap::new(),
        })
    }

    /// The run identity.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The optional stable action digest.
    #[must_use]
    pub fn action_key_digest(&self) -> Option<&str> {
        self.action_key_digest.as_deref()
    }

    /// The repository identity.
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// The trust domain.
    #[must_use]
    pub fn trust_domain(&self) -> &str {
        &self.trust_domain
    }

    /// The logical event sequence number.
    #[must_use]
    pub const fn ts_logical(&self) -> u64 {
        self.ts_logical
    }

    /// The validated structured fields.
    #[must_use]
    pub fn fields(&self) -> &TelemetryFields {
        &self.fields
    }
}

#[derive(Debug, Deserialize)]
struct TelemetryEnvelopeWire {
    schema_version: String,
    run_id: String,
    // Keep missing and explicit `null` distinct: the schema allows omission,
    // but does not allow a JSON null value.
    #[serde(default, deserialize_with = "deserialize_optional_action_key_digest")]
    action_key_digest: Option<String>,
    lane: TelemetryLane,
    repo: String,
    trust_domain: String,
    event: TelemetryEvent,
    ts_logical: u64,
    ts_wall: Timestamp,
    fields: TelemetryFields,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

fn deserialize_optional_action_key_digest<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| {
            D::Error::custom(InvalidTelemetry::rule(
                "action_key_digest",
                "must be omitted instead of null",
            ))
        })
}

impl<'de> Deserialize<'de> for TelemetryEnvelope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = TelemetryEnvelopeWire::deserialize(deserializer)?;
        if wire.schema_version != TELEMETRY_SCHEMA {
            return Err(D::Error::custom(InvalidTelemetry::rule(
                "schema_version",
                "is not v1",
            )));
        }

        let mut envelope = Self::new(TelemetryEnvelopeInput {
            run_id: &wire.run_id,
            action_key_digest: wire.action_key_digest.as_deref(),
            lane: wire.lane,
            repo: &wire.repo,
            trust_domain: &wire.trust_domain,
            event: wire.event,
            ts_logical: wire.ts_logical,
            ts_wall: wire.ts_wall,
            fields: wire.fields,
        })
        .map_err(D::Error::custom)?;
        TelemetryFields::new(wire.extra)
            .map_err(D::Error::custom)
            .map(|extra| envelope.extra = extra.0)?;
        Ok(envelope)
    }
}

/// Opaque position in the bounded telemetry stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TelemetryCursor {
    epoch: u64,
    sequence: u64,
}

impl TelemetryCursor {
    /// Construct a cursor previously returned by an emission or page.
    #[must_use]
    pub const fn from_value(value: u64) -> Self {
        Self {
            epoch: 0,
            sequence: value,
        }
    }

    /// Construct a cursor from its sink generation and sequence number.
    #[must_use]
    pub const fn from_parts(epoch: u64, sequence: u64) -> Self {
        Self { epoch, sequence }
    }

    /// Return the sink generation encoded in this cursor.
    #[must_use]
    pub const fn epoch(self) -> u64 {
        self.epoch
    }

    /// Return the stable numeric position for persistence by a caller.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.sequence
    }
}

/// An envelope paired with its ring-buffer position.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryRecord {
    cursor: TelemetryCursor,
    envelope: TelemetryEnvelope,
}

impl TelemetryRecord {
    /// The record position used as the next page's `after` cursor.
    #[must_use]
    pub const fn cursor(&self) -> TelemetryCursor {
        self.cursor
    }

    /// The validated telemetry envelope.
    #[must_use]
    pub const fn envelope(&self) -> &TelemetryEnvelope {
        &self.envelope
    }
}

/// A bounded page from the in-memory telemetry ring.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryPage {
    records: Vec<TelemetryRecord>,
    next_cursor: Option<TelemetryCursor>,
    dropped_before: Option<TelemetryCursor>,
}

impl TelemetryPage {
    /// Records after the requested cursor, in emission order.
    #[must_use]
    pub fn records(&self) -> &[TelemetryRecord] {
        &self.records
    }

    /// Cursor to pass to the next read when more records remain.
    #[must_use]
    pub const fn next_cursor(&self) -> Option<TelemetryCursor> {
        self.next_cursor
    }

    /// Oldest retained cursor when earlier records were evicted.
    #[must_use]
    pub const fn dropped_before(&self) -> Option<TelemetryCursor> {
        self.dropped_before
    }
}

/// Result of a best-effort emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetryEmission {
    cursor: TelemetryCursor,
    file_written: bool,
}

impl TelemetryEmission {
    /// Position assigned to the emitted envelope.
    #[must_use]
    pub const fn cursor(self) -> TelemetryCursor {
        self.cursor
    }

    /// Whether the NDJSON append and flush succeeded for this event.
    #[must_use]
    pub const fn file_written(self) -> bool {
        self.file_written
    }
}

/// Counters describing best-effort sink behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetrySinkStats {
    emitted: u64,
    ring_evictions: u64,
    file_writes: u64,
    file_failures: u64,
    file_rotations: u64,
    file_bytes: u64,
    file_enabled: bool,
}

impl TelemetrySinkStats {
    /// Total envelopes accepted into the in-memory ring.
    #[must_use]
    pub const fn emitted(self) -> u64 {
        self.emitted
    }

    /// Number of old ring records discarded at capacity.
    #[must_use]
    pub const fn ring_evictions(self) -> u64 {
        self.ring_evictions
    }

    /// Number of envelopes written and flushed to NDJSON.
    #[must_use]
    pub const fn file_writes(self) -> u64 {
        self.file_writes
    }

    /// Number of failed file opens/writes/flushes.
    #[must_use]
    pub const fn file_failures(self) -> u64 {
        self.file_failures
    }

    /// Number of successful file truncations used to bound retention.
    #[must_use]
    pub const fn file_rotations(self) -> u64 {
        self.file_rotations
    }

    /// Bytes currently retained in the bounded NDJSON file.
    #[must_use]
    pub const fn file_bytes(self) -> u64 {
        self.file_bytes
    }

    /// Whether the sink currently has an open NDJSON writer.
    #[must_use]
    pub const fn file_enabled(self) -> bool {
        self.file_enabled
    }
}

/// Configuration failure for a telemetry sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTelemetrySink;

impl fmt::Display for InvalidTelemetrySink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("telemetry ring capacity and file limit must be greater than zero")
    }
}

impl std::error::Error for InvalidTelemetrySink {}

#[derive(Debug)]
struct TelemetrySinkState {
    ring: VecDeque<TelemetryRecord>,
    epoch: u64,
    next_sequence: u64,
    capacity: usize,
    ring_evictions: u64,
    file_writes: u64,
    file_failures: u64,
    file_rotations: u64,
    file_bytes: u64,
    file_path: Option<PathBuf>,
    max_file_bytes: u64,
    writer: Option<BufWriter<File>>,
}

/// Thread-safe, bounded, best-effort telemetry output.
///
/// Every valid envelope is retained in the bounded ring even if the optional
/// NDJSON file is unavailable. File failures are counted and never returned to
/// the caller, so telemetry cannot make an execution fail.
#[derive(Clone, Debug)]
pub struct TelemetrySink {
    state: Arc<Mutex<TelemetrySinkState>>,
}

/// Read a bounded NDJSON sink shared by multiple Velnor processes.
///
/// Writers keep their own in-memory rings, but this reader reconstructs the
/// retained file window on every call. A file rewrite or rotation advances
/// the reader epoch, so an older opaque cursor cannot hide new records.
#[derive(Clone, Debug)]
pub struct TelemetryFileReader {
    path: PathBuf,
    max_file_bytes: u64,
    state: Arc<Mutex<TelemetryFileReaderState>>,
}

#[derive(Debug)]
struct TelemetryFileReaderState {
    epoch: u64,
    fingerprint: Option<TelemetryFileFingerprint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TelemetryFileFingerprint {
    len: u64,
    modified_ns: u64,
    first_line_hash: u64,
    generation: Option<u64>,
}

/// Failure while reading a shared telemetry file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryFileError {
    /// The file could not be read.
    Io,
    /// The file exceeded its configured byte bound.
    TooLarge,
    /// A complete line was not valid UTF-8.
    InvalidUtf8,
    /// A complete line was not a valid secret-safe telemetry envelope.
    InvalidRecord,
    /// The cursor is ahead of the current file window.
    CursorAhead,
}

impl fmt::Display for TelemetryFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Io => "telemetry file unavailable",
            Self::TooLarge => "telemetry file exceeds its configured bound",
            Self::InvalidUtf8 => "telemetry file contains invalid text",
            Self::InvalidRecord => "telemetry file contains an invalid record",
            Self::CursorAhead => "telemetry cursor is ahead of the retained file window",
        })
    }
}

impl std::error::Error for TelemetryFileError {}

impl TelemetryFileReader {
    /// Create a reader for one bounded NDJSON path.
    pub fn new(
        path: impl Into<PathBuf>,
        max_file_bytes: u64,
    ) -> Result<Self, InvalidTelemetrySink> {
        if max_file_bytes == 0 {
            return Err(InvalidTelemetrySink);
        }
        Ok(Self {
            path: path.into(),
            max_file_bytes,
            state: Arc::new(Mutex::new(TelemetryFileReaderState {
                epoch: next_epoch(),
                fingerprint: None,
            })),
        })
    }

    /// Read at most `limit` valid records after an optional cursor.
    pub fn read(
        &self,
        after: Option<TelemetryCursor>,
        limit: usize,
    ) -> Result<TelemetryPage, TelemetryFileError> {
        let _lock = match fs::metadata(&self.path) {
            Ok(_) => {
                Some(lock_telemetry_file(&self.path, false).map_err(|_| TelemetryFileError::Io)?)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return Err(TelemetryFileError::Io),
        };
        let modified_ns = fs::metadata(&self.path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos() as u64);
        let generation =
            read_telemetry_generation(&self.path).map_err(|_| TelemetryFileError::Io)?;
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err(TelemetryFileError::Io),
        };
        if bytes.len() as u64 > self.max_file_bytes {
            return Err(TelemetryFileError::TooLarge);
        }

        let complete_bytes = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position.saturating_add(1));
        let complete = &bytes[..complete_bytes];
        let text = std::str::from_utf8(complete).map_err(|_| TelemetryFileError::InvalidUtf8)?;
        let mut envelopes = Vec::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            envelopes.push(
                serde_json::from_str::<TelemetryEnvelope>(line)
                    .map_err(|_| TelemetryFileError::InvalidRecord)?,
            );
        }

        let fingerprint = telemetry_file_fingerprint(&bytes, modified_ns, generation);
        let mut state = lock_file_reader_state(&self.state);
        let first_read = state.fingerprint.is_none();
        let rotated = state.fingerprint.is_some_and(|previous| {
            fingerprint.len < previous.len
                || fingerprint.first_line_hash != previous.first_line_hash
                || (fingerprint.len == previous.len
                    && fingerprint.modified_ns != previous.modified_ns)
                || fingerprint.generation != previous.generation
        });
        state.fingerprint = Some(fingerprint);
        if first_read || rotated {
            state.epoch = telemetry_epoch(fingerprint);
        }
        let epoch = state.epoch;
        drop(state);

        let cursor_is_stale = after.is_some_and(|cursor| cursor.epoch() != epoch);
        if !cursor_is_stale && after.is_some_and(|cursor| cursor.value() > envelopes.len() as u64) {
            return Err(TelemetryFileError::CursorAhead);
        }
        let oldest = (!envelopes.is_empty()).then_some(TelemetryCursor::from_parts(epoch, 1));
        let records = envelopes
            .into_iter()
            .enumerate()
            .map(|(index, envelope)| TelemetryRecord {
                cursor: TelemetryCursor::from_parts(epoch, index as u64 + 1),
                envelope,
            })
            .filter(|record| {
                after.is_none_or(|cursor| {
                    cursor.epoch() != epoch || record.cursor.value() > cursor.value()
                })
            })
            .take(limit.saturating_add(1))
            .collect::<Vec<_>>();
        let mut records = records;
        let next_cursor = (records.len() > limit).then(|| {
            records.pop();
            records.last().map(|record| record.cursor)
        });
        Ok(TelemetryPage {
            records,
            next_cursor: next_cursor.flatten(),
            dropped_before: cursor_is_stale.then_some(oldest).flatten(),
        })
    }
}

fn lock_file_reader_state(
    state: &Mutex<TelemetryFileReaderState>,
) -> MutexGuard<'_, TelemetryFileReaderState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn telemetry_file_fingerprint(
    bytes: &[u8],
    modified_ns: u64,
    generation: Option<u64>,
) -> TelemetryFileFingerprint {
    let first_line_end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(bytes.len());
    let first_line = &bytes[..first_line_end];
    let digest = Sha256::digest(first_line);
    let mut hash_bytes = [0_u8; 8];
    hash_bytes.copy_from_slice(&digest[..8]);
    TelemetryFileFingerprint {
        len: bytes.len() as u64,
        modified_ns,
        first_line_hash: u64::from_be_bytes(hash_bytes),
        generation,
    }
}

impl TelemetrySink {
    /// Open an append-only NDJSON sink and allocate a bounded ring.
    ///
    /// A missing/unwritable path intentionally creates a memory-only sink and
    /// increments its failure counter; callers still receive successful ring
    /// emissions. Pass `None` for an intentional memory-only sink.
    pub fn new(path: Option<&Path>, capacity: usize) -> Result<Self, InvalidTelemetrySink> {
        Self::new_with_max_file_bytes(path, capacity, DEFAULT_TELEMETRY_FILE_BYTES)
    }

    /// Open a sink with an explicit byte limit for the optional NDJSON file.
    pub fn new_with_max_file_bytes(
        path: Option<&Path>,
        capacity: usize,
        max_file_bytes: u64,
    ) -> Result<Self, InvalidTelemetrySink> {
        if capacity == 0 || max_file_bytes == 0 {
            return Err(InvalidTelemetrySink);
        }

        let file_path = path.map(Path::to_path_buf);
        let opened = path.and_then(|path| open_bounded_file(path, max_file_bytes).ok());
        let (writer, file_bytes) =
            opened.map_or((None, 0), |(writer, file_bytes)| (Some(writer), file_bytes));
        let file_failures = u64::from(path.is_some() && writer.is_none());

        Ok(Self {
            state: Arc::new(Mutex::new(TelemetrySinkState {
                ring: VecDeque::with_capacity(capacity),
                epoch: next_epoch(),
                next_sequence: 1,
                capacity,
                ring_evictions: 0,
                file_writes: 0,
                file_failures,
                file_rotations: 0,
                file_bytes,
                file_path,
                max_file_bytes,
                writer,
            })),
        })
    }

    /// Emit an envelope into the ring and, when available, the NDJSON file.
    #[must_use]
    pub fn emit(&self, envelope: TelemetryEnvelope) -> TelemetryEmission {
        let mut state = lock_state(&self.state);
        let cursor = TelemetryCursor::from_parts(state.epoch, state.next_sequence);
        state.next_sequence = state.next_sequence.saturating_add(1);

        let file_written = write_ndjson(&mut state, &envelope);
        if state.ring.len() == state.capacity {
            state.ring.pop_front();
            state.ring_evictions = state.ring_evictions.saturating_add(1);
        }
        state.ring.push_back(TelemetryRecord { cursor, envelope });

        TelemetryEmission {
            cursor,
            file_written,
        }
    }

    /// Read retained records after `after`, with at most `limit` records.
    #[must_use]
    pub fn read(&self, after: Option<TelemetryCursor>, limit: usize) -> TelemetryPage {
        let state = lock_state(&self.state);
        let oldest = state.ring.front().map(|record| record.cursor);
        let cursor_is_stale = after.is_some_and(|cursor| cursor.epoch() != state.epoch);
        let dropped_before = match (state.ring_evictions > 0, oldest, after) {
            (true, Some(oldest), None) => Some(oldest),
            (true, Some(oldest), Some(after))
                if cursor_is_stale || after.value().saturating_add(1) < oldest.value() =>
            {
                Some(oldest)
            }
            _ => None,
        };

        if limit == 0 {
            return TelemetryPage {
                records: Vec::new(),
                next_cursor: None,
                dropped_before,
            };
        }

        let mut records: Vec<TelemetryRecord> = state
            .ring
            .iter()
            .filter(|record| {
                after.is_none_or(|cursor| {
                    cursor.epoch() != state.epoch || record.cursor.value() > cursor.value()
                })
            })
            .take(limit.saturating_add(1))
            .cloned()
            .collect();
        let next_cursor = (records.len() > limit).then(|| {
            records.pop();
            records.last().map(|record| record.cursor)
        });

        TelemetryPage {
            records,
            next_cursor: next_cursor.flatten(),
            dropped_before,
        }
    }

    /// Snapshot counters without exposing the underlying writer or lock.
    #[must_use]
    pub fn stats(&self) -> TelemetrySinkStats {
        let state = lock_state(&self.state);
        TelemetrySinkStats {
            emitted: state.next_sequence.saturating_sub(1),
            ring_evictions: state.ring_evictions,
            file_writes: state.file_writes,
            file_failures: state.file_failures,
            file_rotations: state.file_rotations,
            file_bytes: state.file_bytes,
            file_enabled: state.writer.is_some(),
        }
    }
}

fn lock_state(state: &Mutex<TelemetrySinkState>) -> MutexGuard<'_, TelemetrySinkState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn next_epoch() -> u64 {
    static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

    let sequence = NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    nanos ^ sequence.rotate_left(17) ^ u64::from(std::process::id())
}

fn open_bounded_file(path: &Path, max_file_bytes: u64) -> io::Result<(BufWriter<File>, u64)> {
    let _lock = lock_telemetry_file(path, true)?;
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let file_bytes = file.metadata()?.len();
    if file_bytes > max_file_bytes {
        drop(file);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        advance_telemetry_generation(path)?;
        return Ok((BufWriter::new(file), 0));
    }
    ensure_telemetry_generation(path)?;
    Ok((BufWriter::new(file), file_bytes))
}

fn rotate_file(state: &mut TelemetrySinkState) -> io::Result<()> {
    let Some(path) = state.file_path.as_deref() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "telemetry file path is unavailable",
        ));
    };
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    advance_telemetry_generation(path)?;
    state.writer = Some(BufWriter::new(file));
    state.file_bytes = 0;
    state.file_rotations = state.file_rotations.saturating_add(1);
    Ok(())
}

fn telemetry_generation_path(path: &Path) -> PathBuf {
    path.with_extension("generation")
}

fn read_telemetry_generation(path: &Path) -> io::Result<Option<u64>> {
    match fs::read_to_string(telemetry_generation_path(path)) {
        Ok(value) => value.trim().parse::<u64>().map(Some).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid telemetry generation")
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_telemetry_generation(path: &Path, generation: u64) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(telemetry_generation_path(path))?;
    file.write_all(generation.to_string().as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()
}

fn ensure_telemetry_generation(path: &Path) -> io::Result<u64> {
    if let Some(generation) = read_telemetry_generation(path)? {
        return Ok(generation);
    }
    let generation = next_epoch();
    write_telemetry_generation(path, generation)?;
    Ok(generation)
}

fn advance_telemetry_generation(path: &Path) -> io::Result<u64> {
    let generation = next_epoch();
    write_telemetry_generation(path, generation)?;
    Ok(generation)
}

fn write_ndjson(state: &mut TelemetrySinkState, envelope: &TelemetryEnvelope) -> bool {
    if state.writer.is_none() {
        return false;
    }

    let mut line = Vec::new();
    if serde_json::to_writer(&mut line, envelope).is_err() {
        state.file_failures = state.file_failures.saturating_add(1);
        state.writer = None;
        return false;
    }
    line.push(b'\n');

    let line_bytes = line.len() as u64;
    if line_bytes > state.max_file_bytes {
        state.file_failures = state.file_failures.saturating_add(1);
        state.writer = None;
        return false;
    }

    let Some(path) = state.file_path.clone() else {
        return false;
    };
    let Ok(_lock) = lock_telemetry_file(&path, true) else {
        state.file_failures = state.file_failures.saturating_add(1);
        return false;
    };
    // Sibling processes have independent sink state. Refresh the physical
    // size while holding the shared lock before deciding whether to rotate.
    state.file_bytes = match fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(_) => {
            state.file_failures = state.file_failures.saturating_add(1);
            state.writer = None;
            return false;
        }
    };

    if state.file_bytes.saturating_add(line_bytes) > state.max_file_bytes
        && rotate_file(state).is_err()
    {
        state.file_failures = state.file_failures.saturating_add(1);
        state.writer = None;
        return false;
    }

    let result = state.writer.as_mut().map_or_else(
        || Err(io::Error::other("telemetry writer unavailable")),
        |writer| writer.write_all(&line).and_then(|()| writer.flush()),
    );
    if result.is_ok() {
        state.file_bytes = state.file_bytes.saturating_add(line_bytes);
        state.file_writes = state.file_writes.saturating_add(1);
        true
    } else {
        state.file_failures = state.file_failures.saturating_add(1);
        state.writer = None;
        false
    }
}

fn telemetry_epoch(fingerprint: TelemetryFileFingerprint) -> u64 {
    if let Some(generation) = fingerprint.generation {
        generation.max(1)
    } else if fingerprint.first_line_hash == 0 {
        1
    } else {
        fingerprint.first_line_hash
    }
}

fn lock_telemetry_file(path: &Path, exclusive: bool) -> io::Result<File> {
    let lock_path = path.with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    let operation = if exclusive {
        rustix::fs::FlockOperation::NonBlockingLockExclusive
    } else {
        rustix::fs::FlockOperation::NonBlockingLockShared
    };
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        match rustix::fs::flock(&lock, operation) {
            Ok(()) => break,
            Err(error) if error == rustix::io::Errno::WOULDBLOCK && Instant::now() < deadline => {
                std::thread::yield_now();
            }
            Err(error) => {
                return Err(io::Error::new(
                    if error == rustix::io::Errno::WOULDBLOCK {
                        io::ErrorKind::WouldBlock
                    } else {
                        io::ErrorKind::Other
                    },
                    error.to_string(),
                ));
            }
        }
    }
    Ok(lock)
}

/// A telemetry construction or deserialization failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTelemetry {
    field: &'static str,
    reason: &'static str,
}

impl InvalidTelemetry {
    fn rule(field: &'static str, reason: &'static str) -> Self {
        Self { field, reason }
    }
}

impl fmt::Display for InvalidTelemetry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid telemetry field '{}': {}",
            self.field, self.reason
        )
    }
}

impl std::error::Error for InvalidTelemetry {}

fn validate_text(field: &'static str, value: &str) -> Result<String, InvalidTelemetry> {
    if value.is_empty() {
        return Err(InvalidTelemetry::rule(field, "must not be empty"));
    }
    if value.len() > MAX_TEXT_LEN {
        return Err(InvalidTelemetry::rule(field, "exceeds the length cap"));
    }
    if value.chars().any(char::is_control) {
        return Err(InvalidTelemetry::rule(
            field,
            "contains a control character",
        ));
    }
    if contains_secret_marker(value) {
        return Err(InvalidTelemetry::rule(
            field,
            "contains a secret-like marker",
        ));
    }
    Ok(value.to_owned())
}

fn validate_digest(value: &str) -> Result<String, InvalidTelemetry> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(InvalidTelemetry::rule(
            "action_key_digest",
            "must be a 64-character hexadecimal digest",
        ));
    }
    Ok(value.to_owned())
}

fn validate_object(
    field: &'static str,
    values: &BTreeMap<String, Value>,
) -> Result<(), InvalidTelemetry> {
    validate_entries(field, values)
}

fn validate_entries<'a, I>(field: &'static str, values: I) -> Result<(), InvalidTelemetry>
where
    I: IntoIterator<Item = (&'a String, &'a Value)>,
{
    for (key, value) in values {
        validate_text("field_name", key)?;
        validate_value(field, value)?;
    }
    Ok(())
}

fn validate_value(field: &'static str, value: &Value) -> Result<(), InvalidTelemetry> {
    match value {
        Value::Array(values) => values
            .iter()
            .try_for_each(|value| validate_value(field, value)),
        Value::Object(values) => validate_entries(field, values),
        Value::String(value) if contains_secret_marker(value) => Err(InvalidTelemetry::rule(
            field,
            "contains a secret-like marker",
        )),
        _ => Ok(()),
    }
}

fn contains_secret_marker(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    SECRET_MARKERS.iter().any(|marker| lowered.contains(marker))
}

fn validate_event_contract(
    event: TelemetryEvent,
    lane: TelemetryLane,
    has_action_key_digest: bool,
    fields: &TelemetryFields,
) -> Result<(), InvalidTelemetry> {
    let Some(contract) = event.contract() else {
        return Err(InvalidTelemetry::rule(
            "event",
            "has no production field contract",
        ));
    };
    if contract.requires_action_key_digest && !has_action_key_digest {
        return Err(InvalidTelemetry::rule(
            "action_key_digest",
            "is required for this event",
        ));
    }
    if contract.lane.is_some_and(|expected| expected != lane) {
        return Err(InvalidTelemetry::rule(
            "lane",
            "is not valid for this event",
        ));
    }
    for field in contract.fields {
        let Some(value) = fields.as_map().get(field.name) else {
            if field.required {
                return Err(InvalidTelemetry::rule(
                    "fields",
                    "is missing a required event field",
                ));
            }
            continue;
        };
        if !matches_field_kind(field.kind, value) {
            return Err(InvalidTelemetry::rule(
                "fields",
                "contains an event field with the wrong type",
            ));
        }
    }
    if matches!(
        event,
        TelemetryEvent::CacheLookup | TelemetryEvent::ArtifactMaterialize
    ) {
        validate_physical_byte_accounting(fields)?;
    }
    Ok(())
}

fn validate_physical_byte_accounting(fields: &TelemetryFields) -> Result<(), InvalidTelemetry> {
    let known = fields.as_map().get("physical_bytes_known");
    let has_shared = fields.as_map().contains_key("shared_bytes");
    let has_newly_allocated = fields.as_map().contains_key("newly_allocated_bytes");
    let has_components = has_shared || has_newly_allocated;

    match known {
        None if !has_components => Ok(()),
        None => Err(InvalidTelemetry::rule(
            "physical_bytes_known",
            "is required when physical-byte components are present",
        )),
        Some(Value::Bool(true)) if has_shared && has_newly_allocated => Ok(()),
        Some(Value::Bool(true)) => Err(InvalidTelemetry::rule(
            "fields",
            "known physical-byte accounting requires both byte components",
        )),
        Some(Value::Bool(false)) if !has_components => Ok(()),
        Some(Value::Bool(false)) => Err(InvalidTelemetry::rule(
            "fields",
            "unknown physical-byte accounting must omit both byte components",
        )),
        Some(_) => Err(InvalidTelemetry::rule(
            "physical_bytes_known",
            "must be a boolean",
        )),
    }
}

fn matches_field_kind(kind: TelemetryFieldKind, value: &Value) -> bool {
    match kind {
        TelemetryFieldKind::Boolean => value.is_boolean(),
        TelemetryFieldKind::Integer => matches!(
            value,
            Value::Number(number) if number.is_i64() || number.is_u64()
        ),
        TelemetryFieldKind::NonNegativeInteger => matches!(
            value,
            Value::Number(number)
                if number.is_u64() || number.as_i64().is_some_and(|value| value >= 0)
        ),
        TelemetryFieldKind::String => value.is_string(),
        TelemetryFieldKind::StringArray => value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::thread;

    fn fields() -> TelemetryFields {
        TelemetryFields::new(BTreeMap::from([
            (String::from("compiler"), json!("cargo")),
            (String::from("exit_code"), json!(0)),
            (String::from("hits"), json!(0)),
            (String::from("metrics_known"), json!(false)),
            (String::from("misses"), json!(0)),
            (String::from("ms"), json!(42)),
            (String::from("unit_count"), json!(0)),
            (String::from("wrapper_calls"), json!(0)),
        ]))
        .expect("valid telemetry fields")
    }

    fn envelope() -> TelemetryEnvelope {
        TelemetryEnvelope::new(TelemetryEnvelopeInput {
            run_id: "run-123",
            action_key_digest: Some(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ),
            lane: TelemetryLane::Velnor,
            repo: "tailrocks/velnor",
            trust_domain: "trusted",
            event: TelemetryEvent::CompileEnd,
            ts_logical: 7,
            ts_wall: Timestamp::parse("2026-08-30T00:00:00Z").expect("valid timestamp"),
            fields: fields(),
        })
        .expect("valid envelope")
    }

    fn cache_lookup_envelope(fields: BTreeMap<String, Value>) -> TelemetryEnvelope {
        TelemetryEnvelope::new(TelemetryEnvelopeInput {
            run_id: "run-123",
            action_key_digest: None,
            lane: TelemetryLane::Github,
            repo: "tailrocks/velnor",
            trust_domain: "trusted",
            event: TelemetryEvent::CacheLookup,
            ts_logical: 7,
            ts_wall: Timestamp::UNIX_EPOCH,
            fields: TelemetryFields::new(fields).expect("generic telemetry fields"),
        })
        .expect("valid cache lookup envelope")
    }

    fn artifact_materialize_envelope(fields: BTreeMap<String, Value>) -> TelemetryEnvelope {
        TelemetryEnvelope::new(TelemetryEnvelopeInput {
            run_id: "run-123",
            action_key_digest: None,
            lane: TelemetryLane::Github,
            repo: "tailrocks/velnor",
            trust_domain: "trusted",
            event: TelemetryEvent::ArtifactMaterialize,
            ts_logical: 7,
            ts_wall: Timestamp::UNIX_EPOCH,
            fields: TelemetryFields::new(fields).expect("generic telemetry fields"),
        })
        .expect("valid artifact materialize envelope")
    }

    fn assert_model_validation_result(value: &Value, expected: bool, case: &str) {
        // The model deserializer is the reviewed in-process validator for the
        // same wire contract. `schema_matches_authoritative_telemetry_contracts`
        // below independently proves that the checked-in JSON Schema mirrors
        // these executable contracts, so this test needs no mutable external
        // npm tool or network-managed validator.
        let result = serde_json::from_value::<TelemetryEnvelope>(value.clone()).is_ok();
        assert_eq!(
            result, expected,
            "in-process telemetry validation mismatch for {case}: {value}"
        );
    }

    #[test]
    fn envelope_serializes_stable_v1_shape() {
        let value = serde_json::to_value(envelope()).expect("serialize envelope");
        assert_eq!(value["schema_version"], TELEMETRY_SCHEMA);
        assert_eq!(value["event"], "compile_end");
        assert_eq!(value["fields"]["ms"], 42);
    }

    #[test]
    fn schema_matches_authoritative_telemetry_contracts() {
        let schema: Value =
            serde_json::from_str(include_str!("../../../schemas/velnor.telemetry.v1.json"))
                .expect("telemetry schema is valid JSON");
        let schema_events = schema
            .pointer("/properties/event/enum")
            .and_then(Value::as_array)
            .expect("telemetry schema declares event enum")
            .iter()
            .map(|event| event.as_str().expect("event enum value is a string"))
            .collect::<Vec<_>>();
        let contracts = TelemetryEvent::contracts();
        let expected_events = contracts
            .iter()
            .map(|contract| contract.name)
            .collect::<Vec<_>>();
        assert_eq!(schema_events, expected_events);

        let clauses = schema
            .get("allOf")
            .and_then(Value::as_array)
            .expect("telemetry event contracts are declared");
        assert_eq!(clauses.len(), contracts.len());

        for contract in contracts {
            let clause = clauses
                .iter()
                .find(|clause| {
                    clause.pointer("/if/properties/event/const")
                        == Some(&Value::String(contract.name.to_owned()))
                })
                .expect("every production event has a schema contract");
            let fields_reference = format!("#/$defs/{}_fields", contract.name);
            assert_eq!(
                clause.pointer("/then/properties/fields/$ref"),
                Some(&Value::String(fields_reference))
            );

            let then = clause
                .get("then")
                .expect("event contract has a then clause");
            if contract.requires_action_key_digest {
                assert_eq!(
                    then.get("required"),
                    Some(&serde_json::json!(["action_key_digest"]))
                );
            } else {
                assert!(then.get("required").is_none());
            }
            match contract.lane {
                Some(TelemetryLane::Github) => assert_eq!(
                    then.pointer("/properties/lane/const"),
                    Some(&json!("github"))
                ),
                Some(TelemetryLane::Velnor) => assert_eq!(
                    then.pointer("/properties/lane/const"),
                    Some(&json!("velnor"))
                ),
                None => assert!(then.pointer("/properties/lane").is_none()),
            }

            let fields = schema
                .pointer(&format!("/$defs/{}_fields", contract.name))
                .expect("event fields definition exists");
            assert_eq!(fields.get("additionalProperties"), Some(&json!(true)));
            let required = contract
                .fields
                .iter()
                .filter(|field| field.required)
                .map(|field| field.name)
                .collect::<Vec<_>>();
            assert_eq!(fields.get("required"), Some(&json!(required)));
            for field in contract.fields {
                let property = fields
                    .pointer(&format!("/properties/{}", field.name))
                    .expect("event field property exists");
                match field.kind {
                    TelemetryFieldKind::Boolean => {
                        assert_eq!(property.get("type"), Some(&json!("boolean")))
                    }
                    TelemetryFieldKind::Integer => {
                        assert_eq!(property.get("type"), Some(&json!("integer")))
                    }
                    TelemetryFieldKind::NonNegativeInteger => {
                        assert_eq!(property.get("type"), Some(&json!("integer")));
                        assert_eq!(property.get("minimum"), Some(&json!(0)));
                    }
                    TelemetryFieldKind::String => {
                        assert_eq!(property.get("type"), Some(&json!("string")))
                    }
                    TelemetryFieldKind::StringArray => {
                        assert_eq!(property.get("type"), Some(&json!("array")));
                        assert_eq!(property.pointer("/items/type"), Some(&json!("string")));
                    }
                }
            }
        }
        for event_fields in ["cache_lookup_fields", "artifact_materialize_fields"] {
            assert_eq!(
                schema
                    .pointer(&format!("/$defs/{event_fields}/allOf"))
                    .and_then(Value::as_array)
                    .map(Vec::len),
                Some(3)
            );
        }
    }

    #[test]
    fn physical_byte_telemetry_preserves_unknown_and_known_invariants() {
        let unknown = cache_lookup_envelope(BTreeMap::from([
            ("hit".into(), json!(false)),
            ("lookup_ms".into(), json!(1)),
            ("physical_bytes_known".into(), json!(false)),
            ("store".into(), json!("mbx")),
        ]));
        let serialized = serde_json::to_value(unknown).expect("serialize unknown accounting");
        assert_eq!(serialized["fields"]["physical_bytes_known"], false);
        assert!(serialized["fields"].get("shared_bytes").is_none());
        assert!(serialized["fields"].get("newly_allocated_bytes").is_none());

        let known = cache_lookup_envelope(BTreeMap::from([
            ("hit".into(), json!(true)),
            ("lookup_ms".into(), json!(1)),
            ("newly_allocated_bytes".into(), json!(23)),
            ("physical_bytes_known".into(), json!(true)),
            ("shared_bytes".into(), json!(11)),
            ("store".into(), json!("mbx")),
        ]));
        assert_eq!(
            serde_json::to_value(known).expect("serialize known accounting")["fields"]
                ["newly_allocated_bytes"],
            23
        );

        for fields in [
            BTreeMap::from([
                ("hit".into(), json!(true)),
                ("lookup_ms".into(), json!(1)),
                ("physical_bytes_known".into(), json!(false)),
                ("shared_bytes".into(), json!(11)),
                ("store".into(), json!("mbx")),
            ]),
            BTreeMap::from([
                ("hit".into(), json!(true)),
                ("lookup_ms".into(), json!(1)),
                ("physical_bytes_known".into(), json!(true)),
                ("shared_bytes".into(), json!(11)),
                ("store".into(), json!("mbx")),
            ]),
        ] {
            assert!(TelemetryEnvelope::new(TelemetryEnvelopeInput {
                run_id: "run-123",
                action_key_digest: None,
                lane: TelemetryLane::Github,
                repo: "tailrocks/velnor",
                trust_domain: "trusted",
                event: TelemetryEvent::CacheLookup,
                ts_logical: 7,
                ts_wall: Timestamp::UNIX_EPOCH,
                fields: TelemetryFields::new(fields).expect("generic fields"),
            })
            .is_err());
        }
    }

    #[test]
    fn physical_byte_wire_records_validate_against_json_schema() {
        let required_cache_fields = BTreeMap::from([
            ("hit".into(), json!(true)),
            ("lookup_ms".into(), json!(1)),
            ("store".into(), json!("mbx")),
        ]);
        let required_artifact_fields = BTreeMap::from([
            ("digest".into(), json!("abc")),
            ("ms".into(), json!(1)),
            ("subset".into(), json!("runtime")),
        ]);
        for (case, envelope) in [
            (
                "cache-omitted",
                cache_lookup_envelope(required_cache_fields.clone()),
            ),
            (
                "cache-unknown",
                cache_lookup_envelope(BTreeMap::from([
                    ("hit".into(), json!(false)),
                    ("lookup_ms".into(), json!(1)),
                    ("physical_bytes_known".into(), json!(false)),
                    ("store".into(), json!("mbx")),
                ])),
            ),
            (
                "cache-known",
                cache_lookup_envelope(BTreeMap::from([
                    ("hit".into(), json!(true)),
                    ("lookup_ms".into(), json!(1)),
                    ("newly_allocated_bytes".into(), json!(0)),
                    ("physical_bytes_known".into(), json!(true)),
                    ("shared_bytes".into(), json!(0)),
                    ("store".into(), json!("mbx")),
                ])),
            ),
            (
                "artifact-omitted",
                artifact_materialize_envelope(required_artifact_fields.clone()),
            ),
            (
                "artifact-unknown",
                artifact_materialize_envelope(BTreeMap::from([
                    ("digest".into(), json!("abc")),
                    ("ms".into(), json!(1)),
                    ("physical_bytes_known".into(), json!(false)),
                    ("subset".into(), json!("runtime")),
                ])),
            ),
            (
                "artifact-known",
                artifact_materialize_envelope(BTreeMap::from([
                    ("digest".into(), json!("abc")),
                    ("ms".into(), json!(1)),
                    ("newly_allocated_bytes".into(), json!(23)),
                    ("physical_bytes_known".into(), json!(true)),
                    ("shared_bytes".into(), json!(11)),
                    ("subset".into(), json!("runtime")),
                ])),
            ),
        ] {
            assert_model_validation_result(
                &serde_json::to_value(envelope).expect("serialize valid telemetry wire"),
                true,
                case,
            );
        }

        let known = serde_json::to_value(cache_lookup_envelope(BTreeMap::from([
            ("hit".into(), json!(true)),
            ("lookup_ms".into(), json!(1)),
            ("newly_allocated_bytes".into(), json!(23)),
            ("physical_bytes_known".into(), json!(true)),
            ("shared_bytes".into(), json!(11)),
            ("store".into(), json!("mbx")),
        ])))
        .expect("serialize known telemetry wire");
        for (case, mutation) in [
            (
                "known-missing-component",
                (|value: &mut Value| {
                    value["fields"]
                        .as_object_mut()
                        .expect("fields object")
                        .remove("shared_bytes");
                }) as fn(&mut Value),
            ),
            (
                "unknown-with-component",
                (|value: &mut Value| {
                    value["fields"]["physical_bytes_known"] = json!(false);
                }) as fn(&mut Value),
            ),
            (
                "component-without-known",
                (|value: &mut Value| {
                    value["fields"]
                        .as_object_mut()
                        .expect("fields object")
                        .remove("physical_bytes_known");
                }) as fn(&mut Value),
            ),
            (
                "known-null-component",
                (|value: &mut Value| {
                    value["fields"]["shared_bytes"] = Value::Null;
                }) as fn(&mut Value),
            ),
        ] {
            let mut invalid = known.clone();
            mutation(&mut invalid);
            assert_model_validation_result(&invalid, false, case);
            assert!(
                serde_json::from_value::<TelemetryEnvelope>(invalid).is_err(),
                "model must reject {case}"
            );
        }

        let mut artifact_null =
            serde_json::to_value(artifact_materialize_envelope(BTreeMap::from([
                ("digest".into(), json!("abc")),
                ("ms".into(), json!(1)),
                ("newly_allocated_bytes".into(), json!(23)),
                ("physical_bytes_known".into(), json!(true)),
                ("shared_bytes".into(), json!(11)),
                ("subset".into(), json!("runtime")),
            ])))
            .expect("serialize artifact telemetry wire");
        artifact_null["fields"]["newly_allocated_bytes"] = Value::Null;
        assert_model_validation_result(&artifact_null, false, "artifact-null-component");
        assert!(serde_json::from_value::<TelemetryEnvelope>(artifact_null).is_err());
    }

    #[test]
    fn envelope_rejects_missing_and_mistyped_event_fields() {
        let mut fields = envelope().fields.as_map().clone();
        fields.insert("ms".to_owned(), json!("not-an-integer"));
        let invalid_fields = TelemetryFields::new(fields).expect("generic fields are secret-safe");
        let error = TelemetryEnvelope::new(TelemetryEnvelopeInput {
            run_id: "run-123",
            action_key_digest: Some(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ),
            lane: TelemetryLane::Velnor,
            repo: "tailrocks/velnor",
            trust_domain: "trusted",
            event: TelemetryEvent::CompileEnd,
            ts_logical: 7,
            ts_wall: Timestamp::UNIX_EPOCH,
            fields: invalid_fields,
        })
        .expect_err("event field type must be enforced by the envelope");
        assert!(error.to_string().contains("wrong type"));

        let mut value = serde_json::to_value(envelope()).expect("serialize envelope");
        value["fields"]
            .as_object_mut()
            .expect("fields object")
            .remove("ms");
        let error = serde_json::from_value::<TelemetryEnvelope>(value)
            .expect_err("required event field must be enforced while parsing");
        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn ordinary_event_rejects_explicit_null_action_digest() {
        let mut value = serde_json::to_value(envelope()).expect("serialize envelope");
        value["event"] = json!("run_admitted");
        value["lane"] = json!("github");
        value["fields"] = json!({});
        value
            .as_object_mut()
            .expect("telemetry envelope object")
            .remove("action_key_digest");
        serde_json::from_value::<TelemetryEnvelope>(value.clone())
            .expect("ordinary events may omit the action digest");
        value["action_key_digest"] = Value::Null;
        let error = serde_json::from_value::<TelemetryEnvelope>(value)
            .expect_err("ordinary events must omit a null action digest");
        assert!(error.to_string().contains("must be omitted"));
    }

    #[test]
    fn journal_event_contract_rejects_missing_action_digest() {
        let mut value = serde_json::to_value(envelope()).expect("serialize envelope");
        value["event"] = json!("lease_acquired");
        value["fields"] = json!({"generation": 1, "logical_ms": 2});
        value["lane"] = json!("velnor");
        value
            .as_object_mut()
            .expect("telemetry envelope object")
            .remove("action_key_digest");
        let error = serde_json::from_value::<TelemetryEnvelope>(value)
            .expect_err("journal event must require its action digest");
        assert!(error.to_string().contains("action_key_digest"));
    }

    #[test]
    fn journal_event_contract_rejects_github_lane_with_valid_digest() {
        let mut value = serde_json::to_value(envelope()).expect("serialize envelope");
        value["event"] = json!("lease_acquired");
        value["fields"] = json!({"generation": 1, "logical_ms": 2});
        value["lane"] = json!("github");
        let error = serde_json::from_value::<TelemetryEnvelope>(value)
            .expect_err("journal event must require the Velnor lane");
        assert!(error.to_string().contains("lane"));
    }

    #[test]
    fn envelope_round_trips_and_preserves_additive_fields() {
        let mut value = serde_json::to_value(envelope()).expect("serialize envelope");
        value["future_field"] = json!({"version": 2});
        let parsed: TelemetryEnvelope = serde_json::from_value(value).expect("parse envelope");
        let serialized = serde_json::to_value(parsed).expect("serialize parsed envelope");
        assert_eq!(serialized["future_field"]["version"], 2);
    }

    #[test]
    fn invalid_identity_and_fields_fail_without_echoing_input() {
        let secret = "ghp_never-print-this";
        let error = TelemetryEnvelope::new(TelemetryEnvelopeInput {
            run_id: secret,
            action_key_digest: None,
            lane: TelemetryLane::Github,
            repo: "tailrocks/velnor",
            trust_domain: "public",
            event: TelemetryEvent::RunQueued,
            ts_logical: 0,
            ts_wall: Timestamp::UNIX_EPOCH,
            fields: fields(),
        })
        .expect_err("secret-like identity must fail");
        assert!(!error.to_string().contains(secret));

        let field_error = TelemetryFields::new(BTreeMap::from([(
            String::from("access_token"),
            json!(secret),
        )]))
        .expect_err("secret-like field must fail");
        assert!(!field_error.to_string().contains(secret));
    }

    #[test]
    fn unknown_event_and_wrong_schema_fail_closed() {
        let mut value = serde_json::to_value(envelope()).expect("serialize envelope");
        value["schema_version"] = json!("velnor.telemetry.v2");
        let error = serde_json::from_value::<TelemetryEnvelope>(value).expect_err("wrong schema");
        assert!(error.to_string().contains("schema_version"));
    }

    #[test]
    fn sink_cursor_pages_are_ordered_and_bounded() {
        let sink = TelemetrySink::new(None, 2).expect("valid capacity");
        let first = sink.emit(envelope());
        let second = sink.emit(envelope());
        let third = sink.emit(envelope());

        assert_eq!(first.cursor().value(), 1);
        assert_eq!(second.cursor().value(), 2);
        assert_eq!(third.cursor().value(), 3);
        let page = sink.read(None, 1);
        assert_eq!(page.dropped_before().map(TelemetryCursor::value), Some(2));
        assert_eq!(page.records()[0].cursor(), second.cursor());
        assert_eq!(page.next_cursor(), Some(second.cursor()));

        let final_page = sink.read(page.next_cursor(), 10);
        assert_eq!(final_page.records().len(), 1);
        assert_eq!(final_page.records()[0].cursor(), third.cursor());
        assert_eq!(final_page.next_cursor(), None);
        assert_eq!(sink.stats().ring_evictions(), 1);
    }

    #[test]
    fn sink_writes_one_valid_envelope_per_ndjson_line() {
        let path = std::env::temp_dir().join(format!(
            "velnor-telemetry-{}-{}.jsonl",
            std::process::id(),
            TelemetryCursor::from_value(1).value()
        ));
        let _ = fs::remove_file(&path);
        let sink = TelemetrySink::new(Some(&path), 4).expect("valid sink");
        assert!(sink.emit(envelope()).file_written());
        assert!(sink.emit(envelope()).file_written());

        let lines = fs::read_to_string(&path)
            .expect("read telemetry file")
            .lines()
            .map(|line| serde_json::from_str::<TelemetryEnvelope>(line).expect("valid envelope"))
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(sink.stats().file_writes(), 2);
        assert!(sink.stats().file_enabled());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unwritable_ndjson_path_never_fails_ring_emission() {
        let path = std::env::temp_dir()
            .join(format!("velnor-missing-parent-{}", std::process::id()))
            .join("telemetry.jsonl");
        let sink = TelemetrySink::new(Some(&path), 2).expect("capacity is valid");
        let emission = sink.emit(envelope());

        assert!(!emission.file_written());
        assert_eq!(sink.read(None, 10).records().len(), 1);
        assert_eq!(sink.stats().file_failures(), 1);
        assert!(!sink.stats().file_enabled());
    }

    #[test]
    fn sink_is_safe_to_share_between_emitters() {
        let sink = TelemetrySink::new(None, 8).expect("valid capacity");
        let handles = (0..4)
            .map(|_| {
                let sink = sink.clone();
                thread::spawn(move || {
                    for _ in 0..4 {
                        let _ = sink.emit(envelope());
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("emitter thread succeeds");
        }

        let stats = sink.stats();
        assert_eq!(stats.emitted(), 16);
        assert_eq!(stats.ring_evictions(), 8);
        assert_eq!(sink.read(None, 20).records().len(), 8);
    }

    #[test]
    fn sink_rejects_zero_capacity() {
        assert!(matches!(
            TelemetrySink::new(None, 0),
            Err(InvalidTelemetrySink)
        ));
    }

    #[test]
    fn sink_rejects_zero_file_limit() {
        assert!(matches!(
            TelemetrySink::new_with_max_file_bytes(None, 2, 0),
            Err(InvalidTelemetrySink)
        ));
    }

    #[test]
    fn sink_rotates_file_before_exceeding_byte_limit() {
        let path = std::env::temp_dir().join(format!(
            "velnor-telemetry-rotation-{}-{}.jsonl",
            std::process::id(),
            TelemetryCursor::from_value(2).value()
        ));
        let _ = fs::remove_file(&path);
        let line_bytes = serde_json::to_vec(&envelope())
            .expect("serialize envelope")
            .len()
            + 1;
        let sink = TelemetrySink::new_with_max_file_bytes(Some(&path), 4, (line_bytes * 2) as u64)
            .expect("valid sink");

        for _ in 0..3 {
            assert!(sink.emit(envelope()).file_written());
        }

        let stats = sink.stats();
        assert_eq!(stats.file_writes(), 3);
        assert_eq!(stats.file_rotations(), 1);
        assert_eq!(stats.file_bytes(), line_bytes as u64);
        assert!(
            fs::metadata(&path).expect("telemetry file exists").len() <= (line_bytes * 2) as u64
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn sink_recreation_does_not_hide_new_records_behind_old_cursor() {
        let first_sink = TelemetrySink::new(None, 2).expect("valid sink");
        let old_cursor = first_sink.emit(envelope()).cursor();
        let second_sink = TelemetrySink::new(None, 2).expect("valid sink");
        let new_cursor = second_sink.emit(envelope()).cursor();

        assert_ne!(old_cursor.epoch(), new_cursor.epoch());
        let page = second_sink.read(Some(old_cursor), 10);
        assert_eq!(page.records().len(), 1);
        assert_eq!(page.records()[0].cursor(), new_cursor);
    }

    #[test]
    fn shared_file_reader_reconstructs_records_and_paginates() {
        let path = std::env::temp_dir().join(format!(
            "velnor-telemetry-reader-{}-{}.jsonl",
            std::process::id(),
            TelemetryCursor::from_value(3).value()
        ));
        let _ = fs::remove_file(&path);
        let sink = TelemetrySink::new(Some(&path), 4).expect("valid sink");
        let reader =
            TelemetryFileReader::new(&path, DEFAULT_TELEMETRY_FILE_BYTES).expect("valid reader");
        let _ = sink.emit(envelope());
        let _ = sink.emit(envelope());

        let first_page = reader.read(None, 1).expect("read first page");
        assert_eq!(first_page.records().len(), 1);
        let second_page = reader
            .read(first_page.next_cursor(), 10)
            .expect("read second page");
        assert_eq!(second_page.records().len(), 1);
        assert_eq!(second_page.records()[0].cursor().value(), 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn shared_file_reader_marks_rotation_as_a_new_generation() {
        let path = std::env::temp_dir().join(format!(
            "velnor-telemetry-reader-rotation-{}-{}.jsonl",
            std::process::id(),
            TelemetryCursor::from_value(4).value()
        ));
        let _ = fs::remove_file(&path);
        let line_bytes = serde_json::to_vec(&envelope())
            .expect("serialize envelope")
            .len()
            + 1;
        let sink = TelemetrySink::new_with_max_file_bytes(Some(&path), 4, (line_bytes * 2) as u64)
            .expect("valid sink");
        let reader =
            TelemetryFileReader::new(&path, (line_bytes * 2) as u64).expect("valid reader");
        let _ = sink.emit(envelope());
        let first_page = reader.read(None, 10).expect("read initial page");
        let old_cursor = first_page.records()[0].cursor();
        let _ = sink.emit(envelope());
        let _ = sink.emit(envelope());

        let rotated_page = reader
            .read(Some(old_cursor), 10)
            .expect("read rotated page");
        assert_eq!(rotated_page.records().len(), 1);
        assert_eq!(
            rotated_page.dropped_before(),
            Some(rotated_page.records()[0].cursor())
        );
        assert_ne!(
            old_cursor.epoch(),
            rotated_page.records()[0].cursor().epoch()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn shared_file_reader_keeps_cursor_epoch_across_reader_restart() {
        let path = std::env::temp_dir().join(format!(
            "velnor-telemetry-reader-restart-{}-{}.jsonl",
            std::process::id(),
            TelemetryCursor::from_value(5).value()
        ));
        let _ = fs::remove_file(&path);
        let sink = TelemetrySink::new(Some(&path), 4).expect("valid sink");
        let first_reader =
            TelemetryFileReader::new(&path, DEFAULT_TELEMETRY_FILE_BYTES).expect("reader");
        let _ = sink.emit(envelope());
        let first_page = first_reader.read(None, 10).expect("initial page");
        let cursor = first_page.records()[0].cursor();
        let restarted_reader =
            TelemetryFileReader::new(&path, DEFAULT_TELEMETRY_FILE_BYTES).expect("reader");
        let page = restarted_reader
            .read(Some(cursor), 10)
            .expect("resumed page");
        assert!(page.records().is_empty());
        assert_eq!(page.dropped_before(), None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn separate_process_sinks_append_valid_records_to_one_file() {
        let path = std::env::temp_dir().join(format!(
            "velnor-telemetry-reader-processes-{}-{}.jsonl",
            std::process::id(),
            TelemetryCursor::from_value(6).value()
        ));
        let _ = fs::remove_file(&path);
        let first_sink = TelemetrySink::new(Some(&path), 4).expect("valid sink");
        let second_sink = TelemetrySink::new(Some(&path), 4).expect("valid sink");
        let _ = first_sink.emit(envelope());
        let _ = second_sink.emit(envelope());
        let reader = TelemetryFileReader::new(&path, DEFAULT_TELEMETRY_FILE_BYTES).expect("reader");
        assert_eq!(reader.read(None, 10).expect("records").records().len(), 2);
        let _ = fs::remove_file(path);
    }
}

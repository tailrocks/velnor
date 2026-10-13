//! Versioned, secret-safe performance telemetry at the model boundary.
//!
//! Telemetry is deliberately separate from control-plane [`Event`] values:
//! control events describe durable state transitions, while this envelope
//! describes observations that may be streamed, sampled, or retained in a
//! bounded ring.

use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    fs::{File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

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

/// Lifecycle or measurement boundary represented by a telemetry record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryEvent {
    RunQueued,
    RunAdmitted,
    ToolPrep,
    CacheLookup,
    CompileStart,
    CompileEnd,
    LinkEnd,
    TestEnd,
    ArtifactMaterialize,
    PassiveWait,
    CriticalPath,
    PlanSummary,
    NoProgress,
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
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let file_bytes = file.metadata()?.len();
    if file_bytes > max_file_bytes {
        drop(file);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        return Ok((BufWriter::new(file), 0));
    }
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
    state.writer = Some(BufWriter::new(file));
    state.file_bytes = 0;
    state.file_rotations = state.file_rotations.saturating_add(1);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{fs, thread};

    fn fields() -> TelemetryFields {
        TelemetryFields::new(BTreeMap::from([(String::from("wall_ms"), json!(42))]))
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

    #[test]
    fn envelope_serializes_stable_v1_shape() {
        let value = serde_json::to_value(envelope()).expect("serialize envelope");
        assert_eq!(value["schema_version"], TELEMETRY_SCHEMA);
        assert_eq!(value["event"], "compile_end");
        assert_eq!(value["fields"]["wall_ms"], 42);
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
}

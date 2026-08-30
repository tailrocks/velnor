//! Versioned, secret-safe performance telemetry at the model boundary.
//!
//! Telemetry is deliberately separate from control-plane [`Event`] values:
//! control events describe durable state transitions, while this envelope
//! describes observations that may be streamed, sampled, or retained in a
//! bounded ring.

use std::{collections::BTreeMap, fmt};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::time::Timestamp;

/// Stable discriminator for the telemetry wire contract.
pub const TELEMETRY_SCHEMA: &str = "velnor.telemetry.v1";

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
}

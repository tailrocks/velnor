//! Canonical model types for supersession-safe physical actions.
//!
//! This crate deliberately performs no I/O and contains no GitHub concepts.
//! Every digest is a lower-case BLAKE3-256 hexadecimal value. Canonical JSON
//! is additive-only: object keys are sorted recursively before hashing so
//! equivalent field-order permutations produce the same bytes.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, str::FromStr};
use thiserror::Error;

const DIGEST_HEX_LENGTH: usize = 64;

/// A lower-case BLAKE3-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// Hash bytes with BLAKE3-256.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self::from_hash(blake3::hash(bytes))
    }

    /// Construct a digest from an already-computed BLAKE3 hash.
    #[must_use]
    pub fn from_hash(hash: blake3::Hash) -> Self {
        Self(hash.to_hex().to_string())
    }

    /// Parse and validate a digest string.
    pub fn parse(value: impl Into<String>) -> Result<Self, DigestError> {
        let value = value.into();
        if value.len() != DIGEST_HEX_LENGTH
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DigestError::Invalid(value));
        }
        Ok(Self(value))
    }

    /// Return the validated hexadecimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Digest parse failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DigestError {
    #[error("invalid BLAKE3-256 digest '{0}'")]
    Invalid(String),
}

/// Canonicalization failure.
#[derive(Debug, Error)]
pub enum CanonicalizationError {
    #[error("value is not valid JSON: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Recursively sort JSON object keys and serialize without insignificant
/// whitespace. Arrays retain their semantic order.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalizationError> {
    let value = serde_json::to_value(value)?;
    let value = canonicalize_value(value);
    Ok(serde_json::to_vec(&value)?)
}

fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_value).collect()),
        Value::Object(values) => {
            let ordered = values
                .into_iter()
                .map(|(key, value)| (key, canonicalize_value(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(ordered.into_iter().collect())
        }
        scalar => scalar,
    }
}

/// Platform identity used in action keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformIdentity {
    pub os: String,
    pub arch: String,
    #[serde(default)]
    pub abi: Option<String>,
}

impl PlatformIdentity {
    #[must_use]
    pub fn new(os: impl Into<String>, arch: impl Into<String>, abi: Option<String>) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
            abi,
        }
    }
}

/// Trust boundary applied to an action and its durable outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustClass {
    Untrusted,
    Trusted,
    Release,
}

/// Execution properties that affect action identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub trust_class: TrustClass,
    pub network: bool,
    pub privileged: bool,
    pub timeout_ms: u64,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            trust_class: TrustClass::Untrusted,
            network: false,
            privileged: false,
            timeout_ms: 0,
        }
    }
}

/// Stable vocabulary for physical action classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionKind {
    Checkout,
    SourceClassification,
    Toolchain,
    DependencyResolution,
    Compile,
    TestCompile,
    TestExecute,
    Lint,
    Format,
    Package,
    ContainerBuild,
    ServiceSnapshot,
    IntegrationTest,
    ArtifactVerify,
    Sign,
    Publish,
    Benchmark,
    Aggregate,
}

/// Canonical identity of one physical action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionKey {
    pub command_digest: Digest,
    pub input_root: Digest,
    pub image_digest: Digest,
    pub toolchain_digest: Digest,
    pub platform: PlatformIdentity,
    pub environment_digest: Digest,
    pub dependency_outputs: Vec<Digest>,
    pub execution_policy: ExecutionPolicy,
}

impl ActionKey {
    /// Stable canonical bytes used by journals, CAS records, and planners.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalizationError> {
        canonical_json_bytes(self)
    }

    /// Digest of the canonical action key.
    pub fn digest(&self) -> Result<Digest, CanonicalizationError> {
        Ok(Digest::from_bytes(&self.canonical_bytes()?))
    }
}

/// Process-independent logical time used for persisted lease deadlines.
///
/// The value is milliseconds from the clock's chosen epoch. Persisting this
/// value instead of a process-local monotonic instant lets a restarted daemon
/// reconstruct expiry without relying on an in-memory timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct LogicalInstant(u64);

impl LogicalInstant {
    /// Construct a logical instant from milliseconds.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    /// Return the millisecond representation.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Add milliseconds without wrapping at `u64::MAX`.
    #[must_use]
    pub const fn saturating_add(self, millis: u64) -> Self {
        Self(self.0.saturating_add(millis))
    }
}

/// Clock abstraction shared by lease and scheduler state machines.
///
/// Implementations persist deadlines as [`LogicalInstant`] values and may
/// provide virtual time in tests. `sleep_until` is event-driven: callers wait
/// for the next persisted deadline instead of polling broad state.
pub trait Clock: Send + Sync {
    /// Return the current logical instant.
    fn now(&self) -> LogicalInstant;

    /// Wait until a logical deadline is reached.
    fn sleep_until(&self, deadline: LogicalInstant) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// A producer lease fencing one owner of an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerLease {
    /// Immutable physical action identity.
    pub action: ActionKey,
    /// Monotonically increasing fencing token.
    pub generation: u64,
    /// Worker identity holding the lease.
    pub owner: String,
    /// Persisted logical expiry deadline.
    pub expires_at: LogicalInstant,
    /// Requested heartbeat cadence.
    pub heartbeat_every: u64,
    /// Duration renewed from the clock's current time.
    pub lease_duration: u64,
}

/// Immutable provenance attached to an action result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub builder: String,
    pub source_digest: Digest,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

/// Timing captured for one completed action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActionTiming {
    pub started_at_ms: u64,
    pub duration_ms: u64,
    #[serde(default)]
    pub cpu_ms: Option<u64>,
}

/// Result of one physical action. Callers should construct once and publish
/// the value without mutating it afterward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    pub action_key: ActionKey,
    pub output_root: Digest,
    pub stdout_digest: Digest,
    pub stderr_digest: Digest,
    pub exit_code: i32,
    pub provenance: Provenance,
    pub timing: ActionTiming,
}

/// Durable action lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionState {
    Planned,
    Waiting,
    Leased,
    Running,
    Publishing,
    Complete,
    Failed,
    Abandoned,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde::ser::SerializeMap;

    #[derive(Clone, Debug)]
    struct OrderedObject(Vec<(&'static str, u32)>);

    impl Serialize for OrderedObject {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (key, value) in &self.0 {
                map.serialize_entry(key, value)?;
            }
            map.end()
        }
    }

    #[derive(Clone, Debug)]
    struct OrderedValueObject(Vec<(String, Value)>);

    impl Serialize for OrderedValueObject {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (key, value) in &self.0 {
                map.serialize_entry(key, value)?;
            }
            map.end()
        }
    }

    fn digest(seed: u8) -> Digest {
        Digest::from_bytes(&[seed])
    }

    fn key() -> ActionKey {
        ActionKey {
            command_digest: digest(1),
            input_root: digest(2),
            image_digest: digest(3),
            toolchain_digest: digest(4),
            platform: PlatformIdentity::new("linux", "x86_64", Some("gnu".into())),
            environment_digest: digest(5),
            dependency_outputs: vec![digest(6), digest(7)],
            execution_policy: ExecutionPolicy::default(),
        }
    }

    fn key_fields(key: &ActionKey) -> Vec<(String, Value)> {
        let value = serde_json::to_value(key).unwrap();
        let object = value.as_object().unwrap();
        [
            "command_digest",
            "input_root",
            "image_digest",
            "toolchain_digest",
            "platform",
            "environment_digest",
            "dependency_outputs",
            "execution_policy",
        ]
        .into_iter()
        .map(|name| (name.to_owned(), object[name].clone()))
        .collect()
    }

    fn permutation<const N: usize>(mut rank: usize) -> Vec<usize> {
        let mut remaining = (0..N).collect::<Vec<_>>();
        let mut result = Vec::with_capacity(N);
        for width in (1..=N).rev() {
            let factorial = (1..width).product::<usize>();
            let index = rank / factorial;
            rank %= factorial;
            result.push(remaining.remove(index));
        }
        result
    }

    #[test]
    fn digest_is_lowercase_blake3_hex() {
        let digest = Digest::from_bytes(b"velnor");
        assert_eq!(digest.as_str().len(), DIGEST_HEX_LENGTH);
        assert_eq!(digest.to_string(), digest.to_string().to_ascii_lowercase());
        assert_eq!(Digest::parse(digest.to_string()).unwrap(), digest);
    }

    #[test]
    fn digest_deserialization_validates_path_safe_shape() {
        let error = serde_json::from_str::<Digest>(r#""../escape""#).unwrap_err();
        assert!(error.to_string().contains("invalid BLAKE3-256 digest"));
    }

    #[test]
    fn canonical_json_ignores_object_field_order() {
        let left = serde_json::json!({"b": {"d": 2, "c": 1}, "a": [3, {"z": 0, "y": 1}]});
        let right = serde_json::json!({"a": [3, {"y": 1, "z": 0}], "b": {"c": 1, "d": 2}});
        assert_eq!(
            canonical_json_bytes(&left).unwrap(),
            canonical_json_bytes(&right).unwrap()
        );
    }

    proptest! {
        #[test]
        fn canonical_json_sorts_generated_field_permutations(rank in 0usize..24) {
            let order = permutation::<4>(rank);
            let names = ["a", "b", "c", "d"];
            let left = OrderedObject(order.iter().map(|index| (names[*index], *index as u32)).collect());
            let right = OrderedObject(order.iter().rev().map(|index| (names[*index], *index as u32)).collect());
            prop_assert_eq!(canonical_json_bytes(&left).unwrap(), canonical_json_bytes(&right).unwrap());
        }

        #[test]
        fn action_key_canonicalization_covers_all_fields(rank in 0usize..40_320) {
            let order = permutation::<8>(rank);
            let fields = key_fields(&key());
            let left = OrderedValueObject(order.iter().map(|index| fields[*index].clone()).collect());
            let right = OrderedValueObject(order.iter().rev().map(|index| fields[*index].clone()).collect());
            prop_assert_eq!(canonical_json_bytes(&left).unwrap(), canonical_json_bytes(&right).unwrap());
            prop_assert_eq!(canonical_json_bytes(&left).unwrap(), key().canonical_bytes().unwrap());
        }
    }

    #[test]
    fn action_key_digest_is_stable_across_round_trip() {
        let key = key();
        let bytes = key.canonical_bytes().unwrap();
        let parsed: ActionKey = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(key.digest().unwrap(), parsed.digest().unwrap());
    }

    #[test]
    fn state_vocabulary_is_complete_and_stable() {
        assert_eq!(
            serde_json::to_string(&ActionState::Publishing).unwrap(),
            "\"publishing\""
        );
        assert_eq!(
            serde_json::to_string(&ActionKind::ContainerBuild).unwrap(),
            "\"container-build\""
        );
    }
}

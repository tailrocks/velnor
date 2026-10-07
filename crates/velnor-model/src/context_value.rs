//! Lossless, ordered GitHub Actions context values.
//!
//! Values crossing the host/guest boundary use a fully tagged representation.
//! Objects carry their comparer and entries as pairs, so user keys cannot
//! collide with the wire discriminator and source order is retained.

use serde::de::{Error as _, MapAccess, SeqAccess, Visitor};
use serde::ser::Error as _;
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value as JsonValue};
use std::sync::atomic::{AtomicU64, Ordering};

static MASKED_NUMBER_MARKER_NONCE: AtomicU64 = AtomicU64::new(0);

/// `JsonTextReader` accepts at most 380 characters for an overflow integer,
/// including a leading minus sign.
const MAX_BIG_INTEGER_CHARS: usize = 380;

/// IEEE-754 values which JSON numbers cannot represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NonFinite {
    #[serde(rename = "NaN")]
    NaN,
    #[serde(rename = "Infinity")]
    PositiveInfinity,
    #[serde(rename = "-Infinity")]
    NegativeInfinity,
}

/// A context value with ordered object entries and an explicit lookup rule.
///
/// Each wire node is an object with the strict discriminator
/// `"$velnor_context_value"`. Object entries are encoded as key/value pairs;
/// data keys therefore cannot collide with the wire schema. Use [`Self::from_json`]
/// for plain input JSON: it wraps tag-shaped user objects without interpreting
/// their keys as wire metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextValue {
    Null,
    /// Newtonsoft `JTokenType.Undefined`, which has no plain JSON value.
    Undefined,
    Bool(bool),
    Number(Number),
    BigInteger(String),
    NonFinite(NonFinite),
    String(String),
    Array(Vec<ContextValue>),
    /// Newtonsoft `JTokenType.Constructor` with its name and ordered arguments.
    Constructor {
        name: String,
        arguments: Vec<ContextValue>,
    },
    Object {
        case_sensitive: bool,
        entries: Vec<(String, ContextValue)>,
    },
}

/// Private strict serde schema. Deserializing through a separate tree lets us
/// validate comparer-equal keys recursively before exposing a logical value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "$velnor_context_value", deny_unknown_fields)]
enum WireContextValue {
    #[serde(rename = "null")]
    Null {},
    #[serde(rename = "undefined")]
    Undefined {},
    #[serde(rename = "bool")]
    Bool { value: bool },
    #[serde(rename = "number")]
    Number { value: Number },
    #[serde(rename = "big_integer")]
    BigInteger { value: String },
    #[serde(rename = "non_finite")]
    NonFinite { value: NonFinite },
    #[serde(rename = "string")]
    String { value: String },
    #[serde(rename = "array")]
    Array { value: Vec<WireContextValue> },
    #[serde(rename = "constructor")]
    Constructor {
        name: String,
        arguments: Vec<WireContextValue>,
    },
    #[serde(rename = "object")]
    Object {
        case_sensitive: bool,
        entries: Vec<(String, WireContextValue)>,
    },
}

/// Invalid context structure or numeric representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextValueError {
    DuplicateObjectKey,
    InvalidBigInteger,
}

impl std::fmt::Display for ContextValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateObjectKey => {
                f.write_str("context object contains duplicate keys under its comparer")
            }
            Self::InvalidBigInteger => {
                write!(
                    f,
                    "big integer must be a canonical decimal outside the i64 range and at most {MAX_BIG_INTEGER_CHARS} characters"
                )
            }
        }
    }
}

impl std::error::Error for ContextValueError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonObjectComparer {
    OrdinalIgnoreCase,
    CaseSensitive,
    RootCaseInsensitive,
}

/// JSON syntax tree that retains object-member source order.
#[derive(Debug)]
enum OrderedJsonValue {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

#[derive(Debug)]
enum MaskedJsonNumber {
    BigInteger(String),
    NonFinite(NonFinite),
}

impl OrderedJsonValue {
    fn into_context_value(
        self,
        comparer: JsonObjectComparer,
        is_root: bool,
        masked_numbers: &[(String, MaskedJsonNumber)],
    ) -> Result<ContextValue, ContextValueError> {
        Ok(match self {
            Self::Null => ContextValue::Null,
            Self::Bool(value) => ContextValue::Bool(value),
            Self::Number(value) => ContextValue::Number(value),
            Self::String(value) => match masked_numbers.iter().find(|(marker, _)| marker == &value)
            {
                Some((_, MaskedJsonNumber::BigInteger(digits))) => {
                    ContextValue::big_integer(digits.clone())?
                }
                Some((_, MaskedJsonNumber::NonFinite(value))) => ContextValue::NonFinite(*value),
                None => ContextValue::String(value),
            },
            Self::Array(values) => ContextValue::Array(
                values
                    .into_iter()
                    .map(|value| {
                        value.into_context_value(comparer.descendant(), false, masked_numbers)
                    })
                    .collect::<Result<_, _>>()?,
            ),
            Self::Object(values) => {
                let case_sensitive = match comparer {
                    JsonObjectComparer::OrdinalIgnoreCase => false,
                    JsonObjectComparer::CaseSensitive => true,
                    JsonObjectComparer::RootCaseInsensitive => !is_root,
                };
                let entries = values
                    .into_iter()
                    .map(|(key, value)| {
                        value
                            .into_context_value(comparer.descendant(), false, masked_numbers)
                            .map(|value| (key, value))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if case_sensitive {
                    ContextValue::case_sensitive_object(entries)?
                } else {
                    ContextValue::object(entries)?
                }
            }
        })
    }
}

impl JsonObjectComparer {
    const fn descendant(self) -> Self {
        match self {
            Self::OrdinalIgnoreCase => Self::OrdinalIgnoreCase,
            Self::CaseSensitive | Self::RootCaseInsensitive => Self::CaseSensitive,
        }
    }
}

impl<'de> Deserialize<'de> for OrderedJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(OrderedJsonVisitor)
    }
}

struct OrderedJsonVisitor;

impl<'de> Visitor<'de> for OrderedJsonVisitor {
    type Value = OrderedJsonValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(OrderedJsonValue::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(OrderedJsonValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(OrderedJsonValue::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(OrderedJsonValue::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(OrderedJsonValue::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(OrderedJsonValue::Number)
            .ok_or_else(|| E::custom("JSON numbers must be finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(OrderedJsonValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(OrderedJsonValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<OrderedJsonValue>()? {
            values.push(value);
        }
        Ok(OrderedJsonValue::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut entries = Vec::new();
        while let Some(entry) = map.next_entry::<String, OrderedJsonValue>()? {
            entries.push(entry);
        }
        Ok(OrderedJsonValue::Object(entries))
    }
}

fn parse_ordered_json(
    json: &str,
    comparer: JsonObjectComparer,
) -> Result<ContextValue, serde_json::Error> {
    let nonce = MASKED_NUMBER_MARKER_NONCE.fetch_add(1, Ordering::Relaxed);
    parse_ordered_json_with_nonce(json, comparer, nonce)
}

fn parse_ordered_json_with_nonce(
    json: &str,
    comparer: JsonObjectComparer,
    mut nonce: u64,
) -> Result<ContextValue, serde_json::Error> {
    loop {
        let (masked, masked_numbers) = mask_json_numbers(json, nonce);
        let mut deserializer = serde_json::Deserializer::from_str(&masked);
        let value = OrderedJsonValue::deserialize(&mut deserializer)?;
        deserializer.end()?;
        let marker_occurrences = masked_numbers
            .iter()
            .map(|(marker, _)| count_ordered_strings(&value, marker))
            .collect::<Vec<_>>();
        if marker_occurrences.contains(&0) {
            return Err(<serde_json::Error as serde::de::Error>::custom(
                "out-of-range numeric token is only valid in a JSON value position",
            ));
        }
        if marker_occurrences.iter().any(|count| *count > 1) {
            nonce = nonce.wrapping_add(1);
            continue;
        }
        return value
            .into_context_value(comparer, true, &masked_numbers)
            .map_err(<serde_json::Error as serde::de::Error>::custom);
    }
}

/// Replace out-of-range JSON numbers with collision-checked temporary strings
/// before serde_json can reject or round them. The strings are restored only
/// in value positions after ordered parsing.
fn mask_json_numbers(json: &str, nonce: u64) -> (String, Vec<(String, MaskedJsonNumber)>) {
    let bytes = json.as_bytes();
    let mut masked = Vec::with_capacity(bytes.len());
    let mut masked_numbers = Vec::new();
    let mut position = 0;

    while position < bytes.len() {
        if bytes[position] == b'"' {
            let start = position;
            position += 1;
            while position < bytes.len() {
                match bytes[position] {
                    b'\\' => position = (position + 2).min(bytes.len()),
                    b'"' => {
                        position += 1;
                        break;
                    }
                    _ => position += 1,
                }
            }
            masked.extend_from_slice(&bytes[start..position]);
            continue;
        }

        if (bytes[position] == b'-' || bytes[position].is_ascii_digit())
            && let Some((end, is_integer)) = scan_json_number(bytes, position)
        {
            let lexeme = &json[position..end];
            let number = if is_integer && is_out_of_range_integer(lexeme) {
                Some(MaskedJsonNumber::BigInteger(lexeme.to_owned()))
            } else if !is_integer {
                overflowing_float(lexeme).map(MaskedJsonNumber::NonFinite)
            } else {
                None
            };
            if let Some(number) = number {
                let next_non_whitespace = bytes[end..]
                    .iter()
                    .copied()
                    .find(|byte| !matches!(*byte, b' ' | b'\t' | b'\r' | b'\n'));
                if next_non_whitespace == Some(b':') {
                    masked.extend_from_slice(&bytes[position..end]);
                    position = end;
                    continue;
                }

                let marker = format!(
                    "\0__velnor_masked_number_{nonce}_{}__",
                    masked_numbers.len()
                );
                let encoded = format!(
                    "\"\\u0000__velnor_masked_number_{nonce}_{}__\"",
                    masked_numbers.len()
                );
                masked.extend_from_slice(encoded.as_bytes());
                masked_numbers.push((marker, number));
                position = end;
                continue;
            }
        }

        masked.push(bytes[position]);
        position += 1;
    }

    (
        String::from_utf8_lossy(&masked).into_owned(),
        masked_numbers,
    )
}

fn scan_json_number(bytes: &[u8], start: usize) -> Option<(usize, bool)> {
    let mut position = start;
    if bytes.get(position) == Some(&b'-') {
        position += 1;
    }

    match bytes.get(position).copied()? {
        b'0' => position += 1,
        b'1'..=b'9' => {
            position += 1;
            while bytes.get(position).is_some_and(u8::is_ascii_digit) {
                position += 1;
            }
        }
        _ => return None,
    }

    let mut is_integer = true;
    if bytes.get(position) == Some(&b'.') {
        is_integer = false;
        position += 1;
        let fraction_start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == fraction_start {
            return None;
        }
    }

    if matches!(bytes.get(position).copied(), Some(b'e' | b'E')) {
        is_integer = false;
        position += 1;
        if matches!(bytes.get(position).copied(), Some(b'+' | b'-')) {
            position += 1;
        }
        let exponent_start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == exponent_start {
            return None;
        }
    }

    Some((position, is_integer))
}

fn is_out_of_range_integer(value: &str) -> bool {
    value.parse::<i64>().is_err()
}

fn out_of_range_unsigned_integer(value: &Number) -> Option<u64> {
    if value.as_i64().is_none() {
        value.as_u64()
    } else {
        None
    }
}

fn overflowing_float(value: &str) -> Option<NonFinite> {
    let value = value.parse::<f64>().ok()?;
    if value.is_infinite() {
        Some(if value.is_sign_negative() {
            NonFinite::NegativeInfinity
        } else {
            NonFinite::PositiveInfinity
        })
    } else {
        None
    }
}

fn count_ordered_strings(value: &OrderedJsonValue, target: &str) -> usize {
    match value {
        OrderedJsonValue::String(value) => usize::from(value == target),
        OrderedJsonValue::Array(values) => values
            .iter()
            .map(|value| count_ordered_strings(value, target))
            .sum(),
        OrderedJsonValue::Object(entries) => entries
            .iter()
            .map(|(_, value)| count_ordered_strings(value, target))
            .sum(),
        OrderedJsonValue::Null | OrderedJsonValue::Bool(_) | OrderedJsonValue::Number(_) => 0,
    }
}

impl ContextValue {
    /// Construct an integer outside the signed `i64` range, retaining its
    /// exact decimal digits.
    ///
    /// # Errors
    /// Returns [`ContextValueError::InvalidBigInteger`] for noncanonical or
    /// machine-range values, or values longer than the source JSON reader accepts.
    pub fn big_integer(value: impl Into<String>) -> Result<Self, ContextValueError> {
        let value = value.into();
        validate_big_integer(&value)?;
        Ok(Self::BigInteger(value))
    }

    /// Construct a nonfinite numeric context value without encoding it as a
    /// marker string that could be confused with ordinary user data.
    #[must_use]
    pub const fn non_finite(value: NonFinite) -> Self {
        Self::NonFinite(value)
    }

    /// Build a default Actions context dictionary (OrdinalIgnoreCase lookup).
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] when keys collide
    /// under the dictionary comparer, or [`ContextValueError::InvalidBigInteger`]
    /// when a nested BigInteger is not canonical.
    pub fn object(entries: Vec<(String, Self)>) -> Result<Self, ContextValueError> {
        Self::try_object(false, entries)
    }

    /// Build a case-sensitive context dictionary, as used by `env` on Unix.
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] when keys collide
    /// under the dictionary comparer, or [`ContextValueError::InvalidBigInteger`]
    /// when a nested BigInteger is not canonical.
    pub fn case_sensitive_object(entries: Vec<(String, Self)>) -> Result<Self, ContextValueError> {
        Self::try_object(true, entries)
    }

    fn try_object(
        case_sensitive: bool,
        entries: Vec<(String, Self)>,
    ) -> Result<Self, ContextValueError> {
        validate_unique_keys(case_sensitive, &entries)?;
        for (_, value) in &entries {
            value.validate()?;
        }
        Ok(Self::Object {
            case_sensitive,
            entries,
        })
    }

    /// Validate comparer uniqueness and numeric representations throughout this tree.
    ///
    /// The enum remains directly constructible for pattern matching and wire
    /// integrations. Call this at boundaries that accept values constructed
    /// from raw variants instead of the fallible object constructors.
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] for any object whose
    /// keys collide under its recorded comparer, or
    /// [`ContextValueError::InvalidBigInteger`] for a noncanonical BigInteger.
    pub fn validate(&self) -> Result<(), ContextValueError> {
        match self {
            Self::Array(values) => {
                for value in values {
                    value.validate()?;
                }
            }
            Self::Constructor { arguments, .. } => {
                for argument in arguments {
                    argument.validate()?;
                }
            }
            Self::Object {
                case_sensitive,
                entries,
            } => {
                validate_unique_keys(*case_sensitive, entries)?;
                for (_, value) in entries {
                    value.validate()?;
                }
            }
            Self::BigInteger(value) => validate_big_integer(value)?,
            Self::Null
            | Self::Undefined
            | Self::Bool(_)
            | Self::Number(_)
            | Self::NonFinite(_)
            | Self::String(_) => {}
        }
        Ok(())
    }

    fn try_from_wire(value: WireContextValue) -> Result<Self, ContextValueError> {
        Ok(match value {
            WireContextValue::Null {} => Self::Null,
            WireContextValue::Undefined {} => Self::Undefined,
            WireContextValue::Bool { value } => Self::Bool(value),
            WireContextValue::Number { value } => match out_of_range_unsigned_integer(&value) {
                Some(integer) => Self::big_integer(integer.to_string())?,
                None => Self::Number(value),
            },
            WireContextValue::BigInteger { value } => Self::big_integer(value)?,
            WireContextValue::NonFinite { value } => Self::NonFinite(value),
            WireContextValue::String { value } => Self::String(value),
            WireContextValue::Array { value } => Self::Array(
                value
                    .into_iter()
                    .map(Self::try_from_wire)
                    .collect::<Result<_, _>>()?,
            ),
            WireContextValue::Constructor { name, arguments } => Self::Constructor {
                name,
                arguments: arguments
                    .into_iter()
                    .map(Self::try_from_wire)
                    .collect::<Result<_, _>>()?,
            },
            WireContextValue::Object {
                case_sensitive,
                entries,
            } => {
                let entries = entries
                    .into_iter()
                    .map(|(key, value)| Self::try_from_wire(value).map(|value| (key, value)))
                    .collect::<Result<Vec<_>, _>>()?;
                Self::try_object(case_sensitive, entries)?
            }
        })
    }

    fn try_to_wire(&self) -> Result<WireContextValue, ContextValueError> {
        Ok(match self {
            Self::Null => WireContextValue::Null {},
            Self::Undefined => WireContextValue::Undefined {},
            Self::Bool(value) => WireContextValue::Bool { value: *value },
            Self::Number(value) => match out_of_range_unsigned_integer(value) {
                Some(integer) => WireContextValue::BigInteger {
                    value: integer.to_string(),
                },
                None => WireContextValue::Number {
                    value: value.clone(),
                },
            },
            Self::BigInteger(value) => {
                validate_big_integer(value)?;
                WireContextValue::BigInteger {
                    value: value.clone(),
                }
            }
            Self::NonFinite(value) => WireContextValue::NonFinite { value: *value },
            Self::String(value) => WireContextValue::String {
                value: value.clone(),
            },
            Self::Array(values) => WireContextValue::Array {
                value: values
                    .iter()
                    .map(Self::try_to_wire)
                    .collect::<Result<_, _>>()?,
            },
            Self::Constructor { name, arguments } => WireContextValue::Constructor {
                name: name.clone(),
                arguments: arguments
                    .iter()
                    .map(Self::try_to_wire)
                    .collect::<Result<_, _>>()?,
            },
            Self::Object {
                case_sensitive,
                entries,
            } => {
                validate_unique_keys(*case_sensitive, entries)?;
                WireContextValue::Object {
                    case_sensitive: *case_sensitive,
                    entries: entries
                        .iter()
                        .map(|(key, value)| value.try_to_wire().map(|value| (key.clone(), value)))
                        .collect::<Result<_, _>>()?,
                }
            }
        })
    }

    /// Convert ordinary JSON into the logical context tree without reading
    /// wire tags from user objects.
    ///
    /// This preserves the object iteration order already present in the
    /// `serde_json::Value`. With the workspace's default `serde_json::Map`,
    /// that order is sorted by key; use [`Self::from_json_str`] when source
    /// JSON member order matters. Since `Value` has already parsed numeric
    /// tokens, use the raw string API to retain integer digits beyond `u64`.
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] if raw object keys
    /// collide under the default case-insensitive context comparer.
    pub fn from_json(value: JsonValue) -> Result<Self, ContextValueError> {
        match value {
            JsonValue::Null => Ok(Self::Null),
            JsonValue::Bool(value) => Ok(Self::Bool(value)),
            JsonValue::Number(value) => Self::from_json_number(value),
            JsonValue::String(value) => Ok(Self::String(value)),
            JsonValue::Array(values) => values
                .into_iter()
                .map(Self::from_json)
                .collect::<Result<_, _>>()
                .map(Self::Array),
            JsonValue::Object(values) => Self::object(
                values
                    .into_iter()
                    .map(|(key, value)| Self::from_json(value).map(|value| (key, value)))
                    .collect::<Result<_, _>>()?,
            ),
        }
    }

    fn from_json_number(value: Number) -> Result<Self, ContextValueError> {
        if let Some(integer) = out_of_range_unsigned_integer(&value) {
            return Self::big_integer(integer.to_string());
        }
        Ok(Self::Number(value))
    }

    /// Parse ordinary JSON while preserving source order. All objects use
    /// OrdinalIgnoreCase lookup, matching PipelineContextData dictionaries.
    /// User objects resembling the tagged wire format remain ordinary data.
    ///
    /// # Errors
    /// Returns a JSON syntax error or a duplicate-key validation error.
    pub fn from_json_str(json: &str) -> Result<Self, serde_json::Error> {
        parse_ordered_json(json, JsonObjectComparer::OrdinalIgnoreCase)
    }

    /// Convert ordinary JSON whose objects use exact, case-sensitive key
    /// lookup. Use this for opaque JSON/JToken objects; PipelineContextData
    /// dictionaries should use [`Self::from_json`], whose comparer is
    /// OrdinalIgnoreCase.
    ///
    /// This retains only the order and numeric precision represented by the
    /// supplied `Value`; use [`Self::from_json_str_case_sensitive`] when source
    /// order or out-of-range integer digits matter.
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] if an object contains
    /// repeated exact keys.
    pub fn from_json_case_sensitive(value: JsonValue) -> Result<Self, ContextValueError> {
        match value {
            JsonValue::Null => Ok(Self::Null),
            JsonValue::Bool(value) => Ok(Self::Bool(value)),
            JsonValue::Number(value) => Self::from_json_number(value),
            JsonValue::String(value) => Ok(Self::String(value)),
            JsonValue::Array(values) => values
                .into_iter()
                .map(Self::from_json_case_sensitive)
                .collect::<Result<_, _>>()
                .map(Self::Array),
            JsonValue::Object(values) => Self::case_sensitive_object(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        Self::from_json_case_sensitive(value).map(|value| (key, value))
                    })
                    .collect::<Result<_, _>>()?,
            ),
        }
    }

    /// Parse opaque JSON while preserving source order and using exact key
    /// lookup for every object, as in JObject trees.
    ///
    /// # Errors
    /// Returns a JSON syntax error or a duplicate-key validation error.
    pub fn from_json_str_case_sensitive(json: &str) -> Result<Self, serde_json::Error> {
        parse_ordered_json(json, JsonObjectComparer::CaseSensitive)
    }

    /// Convert an opaque property bag whose root dictionary uses
    /// OrdinalIgnoreCase while nested JSON objects use exact key lookup.
    /// This matches `ResourceProperties`: its `Items` comparer applies only
    /// to the bag, while values remain ordinary JObject trees.
    ///
    /// This retains only the order and numeric precision represented by the
    /// supplied `Value`; use [`Self::from_json_str_root_case_insensitive`] when
    /// source order or out-of-range integer digits matter.
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] if the root object's
    /// keys collide under OrdinalIgnoreCase.
    pub fn from_json_root_case_insensitive(value: JsonValue) -> Result<Self, ContextValueError> {
        match value {
            JsonValue::Object(values) => Self::object(
                values
                    .into_iter()
                    .map(|(key, value)| {
                        Self::from_json_case_sensitive(value).map(|value| (key, value))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            value => Self::from_json_case_sensitive(value),
        }
    }

    /// Parse an opaque property bag while preserving source order. The root
    /// object uses OrdinalIgnoreCase lookup; nested JSON objects use exact
    /// key lookup, matching ResourceProperties.
    ///
    /// # Errors
    /// Returns a JSON syntax error or a duplicate-key validation error.
    pub fn from_json_str_root_case_insensitive(json: &str) -> Result<Self, serde_json::Error> {
        parse_ordered_json(json, JsonObjectComparer::RootCaseInsensitive)
    }

    /// Borrowing form of [`ContextValue::from_json`].
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] if raw object keys
    /// collide under the default case-insensitive context comparer.
    pub fn from_json_ref(value: &JsonValue) -> Result<Self, ContextValueError> {
        match value {
            JsonValue::Null => Ok(Self::Null),
            JsonValue::Bool(value) => Ok(Self::Bool(*value)),
            JsonValue::Number(value) => Self::from_json_number(value.clone()),
            JsonValue::String(value) => Ok(Self::String(value.clone())),
            JsonValue::Array(values) => values
                .iter()
                .map(Self::from_json_ref)
                .collect::<Result<_, _>>()
                .map(Self::Array),
            JsonValue::Object(values) => Self::object(
                values
                    .iter()
                    .map(|(key, value)| {
                        Self::from_json_ref(value).map(|value| (key.clone(), value))
                    })
                    .collect::<Result<_, _>>()?,
            ),
        }
    }

    /// Borrowing form of [`ContextValue::from_json_case_sensitive`]. Source
    /// member order and out-of-range integer digits must be handled before
    /// creating the `Value`; use [`Self::from_json_str_case_sensitive`] when
    /// the raw text is available.
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] if an object contains
    /// repeated exact keys.
    pub fn from_json_ref_case_sensitive(value: &JsonValue) -> Result<Self, ContextValueError> {
        match value {
            JsonValue::Null => Ok(Self::Null),
            JsonValue::Bool(value) => Ok(Self::Bool(*value)),
            JsonValue::Number(value) => Self::from_json_number(value.clone()),
            JsonValue::String(value) => Ok(Self::String(value.clone())),
            JsonValue::Array(values) => values
                .iter()
                .map(Self::from_json_ref_case_sensitive)
                .collect::<Result<_, _>>()
                .map(Self::Array),
            JsonValue::Object(values) => Self::case_sensitive_object(
                values
                    .iter()
                    .map(|(key, value)| {
                        Self::from_json_ref_case_sensitive(value).map(|value| (key.clone(), value))
                    })
                    .collect::<Result<_, _>>()?,
            ),
        }
    }

    /// Borrowing form of [`ContextValue::from_json_root_case_insensitive`].
    /// Source order and out-of-range integer digits must be handled before
    /// creating the `Value`; use [`Self::from_json_str_root_case_insensitive`]
    /// when the raw text is available.
    ///
    /// # Errors
    /// Returns [`ContextValueError::DuplicateObjectKey`] if the root object's
    /// keys collide under OrdinalIgnoreCase.
    pub fn from_json_ref_root_case_insensitive(
        value: &JsonValue,
    ) -> Result<Self, ContextValueError> {
        match value {
            JsonValue::Object(values) => Self::object(
                values
                    .iter()
                    .map(|(key, value)| {
                        Self::from_json_ref_case_sensitive(value).map(|value| (key.clone(), value))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            value => Self::from_json_ref_case_sensitive(value),
        }
    }

    /// Object entries in their original order, or `None` for non-objects.
    #[must_use]
    pub fn entries(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object { entries, .. } => Some(entries),
            _ => None,
        }
    }

    /// Object comparer flag, or `None` for non-objects.
    #[must_use]
    pub fn is_case_sensitive(&self) -> Option<bool> {
        match self {
            Self::Object { case_sensitive, .. } => Some(*case_sensitive),
            _ => None,
        }
    }

    /// Lookup with this object's recorded comparer.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        let Self::Object {
            case_sensitive,
            entries,
        } = self
        else {
            return None;
        };
        entries
            .iter()
            .find(|(name, _)| {
                if *case_sensitive {
                    name == key
                } else {
                    ordinal_ignore_case_eq(name, key)
                }
            })
            .map(|(_, value)| value)
    }

    /// Serialize the logical value using ordered Newtonsoft-compatible syntax
    /// for GitHub's event file. This is distinct from the tagged serde wire
    /// format. Nonfinite numbers become quoted strings, BigIntegers remain
    /// unquoted decimal tokens, and JToken-only values use `undefined` and
    /// `new Name(arguments)` syntax.
    ///
    /// # Errors
    /// Returns an error when the value contains an invalid BigInteger or
    /// duplicate object keys under its recorded comparer.
    pub fn to_github_json(&self) -> Result<String, ContextValueError> {
        self.validate()?;
        let mut output = String::new();
        self.write_github_json(&mut output);
        Ok(output)
    }

    fn write_github_json(&self, output: &mut String) {
        match self {
            Self::Null => output.push_str("null"),
            // Match Newtonsoft JsonTextWriter's extended token syntax.
            Self::Undefined => output.push_str("undefined"),
            Self::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
            Self::Number(value) => output.push_str(&value.to_string()),
            Self::BigInteger(value) => output.push_str(value),
            Self::NonFinite(value) => {
                let text = match value {
                    NonFinite::NaN => "NaN",
                    NonFinite::PositiveInfinity => "Infinity",
                    NonFinite::NegativeInfinity => "-Infinity",
                };
                write_json_string(text, output);
            }
            Self::String(value) => write_json_string(value, output),
            Self::Array(values) => {
                output.push('[');
                for (index, item) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    item.write_github_json(output);
                }
                output.push(']');
            }
            Self::Constructor { name, arguments } => {
                // Match Newtonsoft JsonTextWriter's extended constructor syntax.
                output.push_str("new ");
                output.push_str(name);
                output.push('(');
                for (index, argument) in arguments.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    argument.write_github_json(output);
                }
                output.push(')');
            }
            Self::Object { entries, .. } => {
                output.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    write_json_string(key, output);
                    output.push(':');
                    value.write_github_json(output);
                }
                output.push('}');
            }
        }
    }
}

impl Serialize for ContextValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.try_to_wire()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ContextValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WireContextValue::deserialize(deserializer)?;
        Self::try_from_wire(wire).map_err(D::Error::custom)
    }
}

fn write_json_string(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{0085}' | '\u{2028}' | '\u{2029}' => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character if character <= '\u{1f}' => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

fn validate_big_integer(value: &str) -> Result<(), ContextValueError> {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if value.len() > MAX_BIG_INTEGER_CHARS
        || digits.is_empty()
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return Err(ContextValueError::InvalidBigInteger);
    }

    let fits_machine_range = value.parse::<i64>().is_ok();
    if fits_machine_range {
        return Err(ContextValueError::InvalidBigInteger);
    }
    Ok(())
}

fn validate_unique_keys(
    case_sensitive: bool,
    entries: &[(String, ContextValue)],
) -> Result<(), ContextValueError> {
    for (index, (key, _)) in entries.iter().enumerate() {
        if entries[..index].iter().any(|(previous, _)| {
            if case_sensitive {
                previous == key
            } else {
                ordinal_ignore_case_eq(previous, key)
            }
        }) {
            return Err(ContextValueError::DuplicateObjectKey);
        }
    }
    Ok(())
}

/// Compare with ICU4X 1.4's pinned Unicode 15.1 simple uppercase data, keeping
/// dotless-i and long-s unchanged as ordinal casing does. Unicode 15.1 added no
/// cased characters, so its casing mappings match Unicode 15.0. This is
/// deterministic across hosts; exact .NET 8 Linux BMP parity is not promised
/// because that runtime delegates BMP casing to the host ICU version.
#[must_use]
pub fn ordinal_ignore_case_eq(left: &str, right: &str) -> bool {
    ordinal_ignore_case_cmp(left, right) == std::cmp::Ordering::Equal
}

/// Compare strings with pinned Unicode 15.1 simple uppercase ordinal mapping.
#[must_use]
pub fn ordinal_ignore_case_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let mapper = icu_casemap::CaseMapper::new();
    let mut left = left.chars();
    let mut right = right.chars();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(left), Some(right)) => {
                let left = simple_ordinal_uppercase(&mapper, left);
                let right = simple_ordinal_uppercase(&mapper, right);
                let order = (left as u32).cmp(&(right as u32));
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// Return whether one string contains another under the shared ordinal
/// comparer, without expanding a scalar into multiple characters.
#[must_use]
pub fn ordinal_ignore_case_contains(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mapper = icu_casemap::CaseMapper::new();
    let haystack: Vec<_> = haystack
        .chars()
        .map(|character| simple_ordinal_uppercase(&mapper, character))
        .collect();
    let needle: Vec<_> = needle
        .chars()
        .map(|character| simple_ordinal_uppercase(&mapper, character))
        .collect();
    needle.len() <= haystack.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Return whether one string starts with another under the shared ordinal
/// comparer.
#[must_use]
pub fn ordinal_ignore_case_starts_with(haystack: &str, prefix: &str) -> bool {
    let mapper = icu_casemap::CaseMapper::new();
    let haystack: Vec<_> = haystack
        .chars()
        .map(|character| simple_ordinal_uppercase(&mapper, character))
        .collect();
    let prefix: Vec<_> = prefix
        .chars()
        .map(|character| simple_ordinal_uppercase(&mapper, character))
        .collect();
    haystack.len() >= prefix.len() && haystack[..prefix.len()] == prefix
}

/// Return whether one string ends with another under the shared ordinal
/// comparer.
#[must_use]
pub fn ordinal_ignore_case_ends_with(haystack: &str, suffix: &str) -> bool {
    let mapper = icu_casemap::CaseMapper::new();
    let haystack: Vec<_> = haystack
        .chars()
        .map(|character| simple_ordinal_uppercase(&mapper, character))
        .collect();
    let suffix: Vec<_> = suffix
        .chars()
        .map(|character| simple_ordinal_uppercase(&mapper, character))
        .collect();
    haystack.len() >= suffix.len() && haystack[haystack.len() - suffix.len()..] == suffix
}

fn simple_ordinal_uppercase(mapper: &icu_casemap::CaseMapper, character: char) -> char {
    match character {
        '\u{0131}' | '\u{017f}' => character,
        _ => mapper.simple_uppercase(character),
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

    #[test]
    fn tagged_round_trip_preserves_order_comparer_and_large_integers() {
        let value = ContextValue::case_sensitive_object(vec![
            (
                "first".into(),
                ContextValue::BigInteger(u64::MAX.to_string()),
            ),
            ("second".into(), ContextValue::non_finite(NonFinite::NaN)),
        ])
        .unwrap();

        let wire = serde_json::to_vec(&value).unwrap();
        let decoded: ContextValue = serde_json::from_slice(&wire).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.is_case_sensitive(), Some(true));
        assert_eq!(
            decoded
                .entries()
                .unwrap()
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(decoded.get("FIRST").is_none());
        assert_eq!(
            decoded.entries().unwrap()[0].1.to_github_json(),
            Ok(u64::MAX.to_string())
        );
    }

    #[test]
    fn jtoken_only_variants_round_trip_and_keep_newtonsoft_syntax() {
        let value = ContextValue::Constructor {
            name: "Date".into(),
            arguments: vec![
                ContextValue::Undefined,
                ContextValue::Constructor {
                    name: "Time".into(),
                    arguments: vec![ContextValue::Number(Number::from(3))],
                },
                ContextValue::BigInteger("9223372036854775808".into()),
                ContextValue::non_finite(NonFinite::NaN),
            ],
        };

        let wire = serde_json::to_value(&value).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "$velnor_context_value": "constructor",
                "name": "Date",
                "arguments": [
                    {"$velnor_context_value": "undefined"},
                    {
                        "$velnor_context_value": "constructor",
                        "name": "Time",
                        "arguments": [{"$velnor_context_value": "number", "value": 3}]
                    },
                    {
                        "$velnor_context_value": "big_integer",
                        "value": "9223372036854775808"
                    },
                    {"$velnor_context_value": "non_finite", "value": "NaN"}
                ]
            })
        );
        let decoded: ContextValue = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(
            ContextValue::Undefined.to_github_json(),
            Ok("undefined".into())
        );
        assert_eq!(
            value.to_github_json(),
            Ok("new Date(undefined,new Time(3),9223372036854775808,\"NaN\")".into())
        );
    }

    #[test]
    fn jtoken_only_wire_tags_are_strict_and_validate_nested_arguments() {
        assert_eq!(
            serde_json::from_str::<ContextValue>(r#"{"$velnor_context_value":"undefined"}"#)
                .unwrap(),
            ContextValue::Undefined
        );

        for malformed in [
            r#"{"$velnor_context_value":"undefined","value":null}"#,
            r#"{"$velnor_context_value":"constructor","arguments":[]}"#,
            r#"{"$velnor_context_value":"constructor","name":"Date"}"#,
            r#"{"$velnor_context_value":"constructor","name":"Date","arguments":[],"extra":0}"#,
            r#"{"$velnor_context_value":"constructor","name":"Date","arguments":[{"$velnor_context_value":"big_integer","value":"3"}]}"#,
        ] {
            assert!(
                serde_json::from_str::<ContextValue>(malformed).is_err(),
                "{malformed}"
            );
        }

        let invalid = ContextValue::Constructor {
            name: "Date".into(),
            arguments: vec![ContextValue::BigInteger("3".into())],
        };
        assert_eq!(
            invalid.validate(),
            Err(ContextValueError::InvalidBigInteger)
        );
        assert!(serde_json::to_string(&invalid).is_err());
        assert_eq!(
            invalid.to_github_json(),
            Err(ContextValueError::InvalidBigInteger)
        );
        assert_eq!(
            ContextValue::BigInteger("3".into()).to_github_json(),
            Err(ContextValueError::InvalidBigInteger)
        );
    }

    #[test]
    fn raw_tag_shaped_json_remains_an_ordinary_object() {
        let raw = serde_json::json!({
            "$velnor_context_value": "number",
            "value": 4,
            "extra": "user data"
        });
        let value = ContextValue::from_json(raw.clone()).unwrap();
        assert!(matches!(value, ContextValue::Object { .. }));
        let decoded: ContextValue =
            serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(value.to_github_json().unwrap(), raw.to_string());
    }

    #[test]
    fn json_constructors_apply_the_selected_object_comparer() {
        let raw = serde_json::json!({"Path": "first", "path": "second"});
        assert_eq!(
            ContextValue::from_json(raw.clone()),
            Err(ContextValueError::DuplicateObjectKey)
        );

        let value = ContextValue::from_json_case_sensitive(raw.clone()).unwrap();
        assert_eq!(
            value.get("Path"),
            Some(&ContextValue::String("first".into()))
        );
        assert_eq!(
            value.get("path"),
            Some(&ContextValue::String("second".into()))
        );
        assert!(value.get("PATH").is_none());
        assert_eq!(value.to_github_json().unwrap(), raw.to_string());
        assert_eq!(
            ContextValue::from_json_ref_case_sensitive(&raw).unwrap(),
            value
        );
    }

    #[test]
    fn unsigned_json_integers_above_i64_use_the_big_integer_wire_variant() {
        let raw = JsonValue::Number(Number::from(i64::MAX as u64 + 1));
        let expected = ContextValue::BigInteger("9223372036854775808".into());
        let values = [
            ContextValue::from_json(raw.clone()).unwrap(),
            ContextValue::from_json_ref(&raw).unwrap(),
            ContextValue::from_json_case_sensitive(raw.clone()).unwrap(),
            ContextValue::from_json_ref_case_sensitive(&raw).unwrap(),
            ContextValue::from_json_root_case_insensitive(raw.clone()).unwrap(),
            ContextValue::from_json_ref_root_case_insensitive(&raw).unwrap(),
            ContextValue::from_json_str("9223372036854775808").unwrap(),
            ContextValue::from_json_str_case_sensitive("9223372036854775808").unwrap(),
            ContextValue::from_json_str_root_case_insensitive("9223372036854775808").unwrap(),
        ];
        let expected_wire = serde_json::json!({
            "$velnor_context_value": "big_integer",
            "value": "9223372036854775808"
        });

        for value in values {
            assert_eq!(value, expected);
            assert_eq!(serde_json::to_value(value).unwrap(), expected_wire);
        }

        let directly_constructed = ContextValue::Number(Number::from(u64::MAX));
        assert_eq!(
            serde_json::to_value(directly_constructed).unwrap(),
            serde_json::json!({
                "$velnor_context_value": "big_integer",
                "value": u64::MAX.to_string()
            })
        );
        let noncanonical_number_wire = format!(
            r#"{{"$velnor_context_value":"number","value":{}}}"#,
            u64::MAX
        );
        assert_eq!(
            serde_json::from_str::<ContextValue>(&noncanonical_number_wire).unwrap(),
            ContextValue::BigInteger(u64::MAX.to_string())
        );
    }

    #[test]
    fn case_sensitive_json_keeps_tag_shaped_data_ordinary_on_wire() {
        let raw = serde_json::json!({
            "$velnor_context_value": "non_finite",
            "value": "NaN",
            "extra": {"A": 1, "a": 2}
        });
        let value = ContextValue::from_json_case_sensitive(raw.clone()).unwrap();
        let wire = serde_json::to_vec(&value).unwrap();
        let decoded: ContextValue = serde_json::from_slice(&wire).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.to_github_json().unwrap(), raw.to_string());
    }

    #[test]
    fn root_case_insensitive_json_limits_comparer_to_the_root_bag() {
        let raw = serde_json::json!({"Properties": {"Path": "first", "path": "second"}});
        let value = ContextValue::from_json_root_case_insensitive(raw.clone()).unwrap();
        let properties = value.get("properties").unwrap();
        assert_eq!(
            properties.get("Path"),
            Some(&ContextValue::String("first".into()))
        );
        assert_eq!(
            properties.get("path"),
            Some(&ContextValue::String("second".into()))
        );
        assert_eq!(
            ContextValue::from_json_ref_root_case_insensitive(&raw).unwrap(),
            value
        );

        let collision = serde_json::json!({"Properties": 1, "properties": 2});
        assert_eq!(
            ContextValue::from_json_root_case_insensitive(collision),
            Err(ContextValueError::DuplicateObjectKey)
        );
    }

    #[test]
    fn raw_json_string_constructors_preserve_source_member_order() {
        let opaque =
            ContextValue::from_json_str_case_sensitive(r#"{"z":1,"a":{"z":2,"a":3}}"#).unwrap();
        assert_eq!(
            opaque
                .entries()
                .unwrap()
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        let nested = opaque.get("a").unwrap();
        assert_eq!(
            nested
                .entries()
                .unwrap()
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert_eq!(nested.is_case_sensitive(), Some(true));

        let bag =
            ContextValue::from_json_str_root_case_insensitive(r#"{"Properties":{"z":1,"a":2}}"#)
                .unwrap();
        assert_eq!(
            bag.get("properties").unwrap().is_case_sensitive(),
            Some(true)
        );
        assert_eq!(
            bag.entries()
                .unwrap()
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["Properties"]
        );
        assert_eq!(
            bag.get("properties")
                .unwrap()
                .entries()
                .unwrap()
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["z", "a"]
        );
    }

    #[test]
    fn raw_big_integers_keep_exact_digits_through_wire_and_event_json() {
        let json = r#"{"i64_max":9223372036854775807,"above_i64":9223372036854775808,"u64_max":18446744073709551615,"negative_below_i64":-9223372036854775809,"very_large":12345678901234567890123456789012345678901234567890}"#;
        let value = ContextValue::from_json_str_case_sensitive(json).unwrap();

        assert_eq!(
            value.get("i64_max"),
            Some(&ContextValue::Number(Number::from(i64::MAX)))
        );
        assert_eq!(
            value.get("above_i64"),
            Some(&ContextValue::BigInteger("9223372036854775808".into()))
        );
        assert_eq!(
            value.get("u64_max"),
            Some(&ContextValue::BigInteger("18446744073709551615".into()))
        );
        assert_eq!(
            value.get("negative_below_i64"),
            Some(&ContextValue::BigInteger("-9223372036854775809".into()))
        );
        assert_eq!(
            value.get("very_large"),
            Some(&ContextValue::BigInteger(
                "12345678901234567890123456789012345678901234567890".into()
            ))
        );
        let double = ContextValue::from_json_str_case_sensitive("18446744073709551616.0").unwrap();
        assert!(matches!(double, ContextValue::Number(_)));

        let wire = serde_json::to_vec(&value).unwrap();
        let decoded: ContextValue = serde_json::from_slice(&wire).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.to_github_json().unwrap(), json);
    }

    #[test]
    fn raw_json_exponent_overflow_becomes_nonfinite_and_event_string() {
        let value =
            ContextValue::from_json_str_case_sensitive(r#"{"positive":1e309,"negative":-1e309}"#)
                .unwrap();

        assert_eq!(
            value.get("positive"),
            Some(&ContextValue::NonFinite(NonFinite::PositiveInfinity))
        );
        assert_eq!(
            value.get("negative"),
            Some(&ContextValue::NonFinite(NonFinite::NegativeInfinity))
        );
        assert_eq!(
            value.to_github_json(),
            Ok(r#"{"positive":"Infinity","negative":"-Infinity"}"#.into())
        );
    }

    #[test]
    fn masked_number_markers_do_not_capture_user_strings() {
        let json =
            r#"{"source":"\u0000__velnor_masked_number_0_0__","integer":18446744073709551616}"#;
        let value =
            parse_ordered_json_with_nonce(json, JsonObjectComparer::CaseSensitive, 0).unwrap();
        assert_eq!(
            value.get("source"),
            Some(&ContextValue::String(
                "\0__velnor_masked_number_0_0__".into()
            ))
        );
        assert_eq!(
            value.get("integer"),
            Some(&ContextValue::BigInteger("18446744073709551616".into()))
        );
    }

    #[test]
    fn big_integer_object_keys_are_rejected_without_retrying() {
        assert!(ContextValue::from_json_str_case_sensitive(r#"{18446744073709551616:0}"#).is_err());

        let collision = r#"{18446744073709551616:"\u0000__velnor_masked_number_0_0__"}"#;
        assert!(
            parse_ordered_json_with_nonce(collision, JsonObjectComparer::CaseSensitive, 0).is_err()
        );
    }

    #[test]
    fn big_integer_wire_tag_requires_canonical_out_of_range_decimal() {
        for value in [
            "0",
            "-0",
            "01",
            "-01",
            "9223372036854775807",
            "-9223372036854775808",
            "+9223372036854775808",
        ] {
            let wire = format!(r#"{{"$velnor_context_value":"big_integer","value":"{value}"}}"#);
            assert!(
                serde_json::from_str::<ContextValue>(&wire).is_err(),
                "{value}"
            );
        }
        assert!(serde_json::from_str::<ContextValue>(
            r#"{"$velnor_context_value":"big_integer","value":"9223372036854775808"}"#
        )
        .is_ok());
        assert!(ContextValue::big_integer("9223372036854775808").is_ok());
        assert!(ContextValue::big_integer("18446744073709551615").is_ok());
        assert_eq!(
            ContextValue::big_integer("9223372036854775807"),
            Err(ContextValueError::InvalidBigInteger)
        );
        assert!(ContextValue::BigInteger("3".into()).validate().is_err());
        assert!(serde_json::to_string(&ContextValue::BigInteger("3".into())).is_err());
    }

    #[test]
    fn big_integer_length_matches_newtonsoft_reader_limit() {
        let max_positive = "9".repeat(MAX_BIG_INTEGER_CHARS);
        let max_negative = format!("-{}", "9".repeat(MAX_BIG_INTEGER_CHARS - 1));
        let too_long_positive = "9".repeat(MAX_BIG_INTEGER_CHARS + 1);
        let too_long_negative = format!("-{}", "9".repeat(MAX_BIG_INTEGER_CHARS));

        assert!(ContextValue::big_integer(&max_positive).is_ok());
        assert!(ContextValue::big_integer(&max_negative).is_ok());
        assert!(ContextValue::big_integer(&too_long_positive).is_err());
        assert!(ContextValue::big_integer(&too_long_negative).is_err());

        let max_wire =
            format!(r#"{{"$velnor_context_value":"big_integer","value":"{max_positive}"}}"#);
        assert!(serde_json::from_str::<ContextValue>(&max_wire).is_ok());
        let too_long_wire =
            format!(r#"{{"$velnor_context_value":"big_integer","value":"{too_long_positive}"}}"#);
        assert!(serde_json::from_str::<ContextValue>(&too_long_wire).is_err());

        let max_raw = ContextValue::from_json_str_case_sensitive(&max_positive).unwrap();
        assert_eq!(max_raw, ContextValue::BigInteger(max_positive));
        assert!(ContextValue::from_json_str_case_sensitive(&too_long_positive).is_err());
    }

    #[test]
    fn tagged_decoder_rejects_unknown_malformed_and_duplicate_object_keys() {
        for malformed in [
            r#"{"$velnor_context_value":"mystery"}"#,
            r#"{"$velnor_context_value":"null","extra":false}"#,
            r#"{"$velnor_context_value":"number"}"#,
            r#"{"$velnor_context_value":"object","case_sensitive":false,"entries":[["x",{"$velnor_context_value":"null","extra":0}]]}"#,
            r#"{"$velnor_context_value":"array","value":[],"extra":0}"#,
            r#"{"$velnor_context_value":"object","case_sensitive":false,"entries":[["A",{"$velnor_context_value":"null"}],["a",{"$velnor_context_value":"null"}]]}"#,
        ] {
            assert!(
                serde_json::from_str::<ContextValue>(malformed).is_err(),
                "{malformed}"
            );
        }
        assert!(ContextValue::object(vec![
            ("A".into(), ContextValue::Null),
            ("a".into(), ContextValue::Null),
        ])
        .is_err());
    }

    #[test]
    fn event_json_is_ordered_plain_json_and_matches_newtonsoft_nonfinite_tokens() {
        let value = ContextValue::object(vec![
            (
                "z".into(),
                ContextValue::non_finite(NonFinite::PositiveInfinity),
            ),
            (
                "a".into(),
                ContextValue::Array(vec![ContextValue::non_finite(NonFinite::NegativeInfinity)]),
            ),
        ])
        .unwrap();
        assert_eq!(
            value.to_github_json(),
            Ok(r#"{"z":"Infinity","a":["-Infinity"]}"#.into())
        );
    }

    #[test]
    fn event_json_escapes_newtonsoft_special_unicode_characters() {
        let value = ContextValue::case_sensitive_object(vec![(
            "\u{0085}\u{2028}\u{2029}".into(),
            ContextValue::String("\u{0085}\u{2028}\u{2029}".into()),
        )])
        .unwrap();

        assert_eq!(
            value.to_github_json(),
            Ok(r#"{"\u0085\u2028\u2029":"\u0085\u2028\u2029"}"#.into())
        );
    }

    #[test]
    fn ordinal_ignore_case_uses_simple_uppercase_vectors() {
        for (left, right, equal) in [
            ("a", "A", true),
            ("é", "É", true),
            ("ı", "I", false),
            ("ſ", "S", false),
            ("ᾀ", "ᾈ", true),
            ("ß", "SS", false),
            ("𐐨", "𐐀", true),
            ("ß", "ẞ", false),
            ("K", "K", false),
            ("Ω", "Ω", false),
            ("Å", "Å", false),
            ("ϑ", "Θ", true),
            ("Σ", "ς", true),
            ("ͅ", "Ι", true),
            ("\u{10d70}", "\u{10d50}", false),
        ] {
            assert_eq!(
                ordinal_ignore_case_eq(left, right),
                equal,
                "{left:?} / {right:?}"
            );
        }
        assert_eq!(
            ordinal_ignore_case_cmp("\u{10000}", "\u{e000}"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            ordinal_ignore_case_cmp("\u{10d70}", "\u{10d50}"),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn context_object_lookup_uses_shared_ordinal_comparer() {
        let value =
            ContextValue::object(vec![("straße".into(), ContextValue::Bool(true))]).unwrap();
        assert!(matches!(
            value.get("STRAßE"),
            Some(ContextValue::Bool(true))
        ));
        assert!(value.get("STRASSE").is_none());
    }

    #[test]
    fn case_sensitive_objects_allow_differently_cased_keys_but_reject_exact_duplicates() {
        assert!(ContextValue::case_sensitive_object(vec![
            ("A".into(), ContextValue::Null),
            ("a".into(), ContextValue::Null),
        ])
        .is_ok());
        assert!(ContextValue::case_sensitive_object(vec![
            ("a".into(), ContextValue::Null),
            ("a".into(), ContextValue::Null),
        ])
        .is_err());
    }
}

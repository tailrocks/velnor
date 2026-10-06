#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;
use velnor_model::{ContextValue, NonFinite};

use crate::runner_json::{
    clr_big_integer_to_f64, dotnet_double_general, is_clr_uri, parse_clr_double_text,
    parse_json_text, JsonNumber, JsonNumberKind, JsonReaderOrigin, OrderedJsonValue,
};

pub const PIPELINE_AGENT_JOB_REQUEST: &str = "PipelineAgentJobRequest";

fn single_endpoint_or_default(
    endpoints: &[Option<ServiceEndpoint>],
    mut predicate: impl FnMut(&ServiceEndpoint) -> bool,
) -> Result<Option<&ServiceEndpoint>> {
    let mut selected = None;
    for endpoint in endpoints {
        // The upstream LINQ predicates dereference every visited entry. Keep
        // null slots observable instead of treating them as non-matches.
        let endpoint = endpoint
            .as_ref()
            .context("null endpoint during endpoint selection")?;
        if predicate(endpoint) {
            if selected.is_some() {
                anyhow::bail!("multiple endpoints matched endpoint selection");
            }
            selected = Some(endpoint);
        }
    }
    Ok(selected)
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn empty_guid_string() -> String {
    "00000000-0000-0000-0000-000000000000".to_owned()
}

fn empty_resource_properties() -> ContextValue {
    ContextValue::Object {
        case_sensitive: false,
        entries: Vec::new(),
    }
}

/// Ordered exact-key map used for PipelineContextData roots. Json.NET first
/// deserializes these keys with the dictionary's exact comparer; the runner
/// later applies its OrdinalIgnoreCase context setter in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedContextData<T> {
    entries: Vec<(String, T)>,
}

impl<T> Default for OrderedContextData<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<T> OrderedContextData<T> {
    pub fn insert(&mut self, key: String, value: T) -> Option<T> {
        if let Some((_, existing)) = self.entries.iter_mut().find(|(name, _)| name == &key) {
            Some(std::mem::replace(existing, value))
        } else {
            self.entries.push((key, value));
            None
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (String, T)> {
        self.entries.iter()
    }

    pub fn get(&self, key: &str) -> Option<&T> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<T> std::ops::Index<&str> for OrderedContextData<T> {
    type Output = T;

    #[allow(clippy::panic, reason = "map indexing panics when the key is absent")]
    fn index(&self, key: &str) -> &Self::Output {
        self.get(key)
            .unwrap_or_else(|| panic!("no entry found for key {key:?}"))
    }
}

impl<T> Serialize for OrderedContextData<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, T> Deserialize<'de> for OrderedContextData<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OrderedContextDataVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T> Visitor<'de> for OrderedContextDataVisitor<T>
        where
            T: Deserialize<'de>,
        {
            type Value = OrderedContextData<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an ordered map")
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut entries = OrderedContextData::default();
                while let Some((key, value)) = map.next_entry()? {
                    entries.insert(key, value);
                }
                Ok(entries)
            }
        }

        deserializer.deserialize_any(OrderedContextDataVisitor(std::marker::PhantomData))
    }
}

pub(crate) fn ordered_context_data_pair_array_value(entries: Vec<(String, Value)>) -> Value {
    Value::Array(
        entries
            .into_iter()
            .map(|(key, value)| Value::Array(vec![Value::String(key), value]))
            .collect(),
    )
}

pub(crate) fn merge_context_data_pair_arrays(previous: Value, next: Value) -> Value {
    match (previous, next) {
        (Value::Object(mut previous), Value::Object(next)) => {
            for (key, value) in next {
                previous.insert(key, value);
            }
            Value::Object(previous)
        }
        (Value::Array(mut previous), Value::Array(next)) => {
            for pair in next {
                let Some(key) = pair
                    .as_array()
                    .and_then(|values| values.first())
                    .and_then(Value::as_str)
                else {
                    previous.push(pair);
                    continue;
                };
                if let Some(existing) = previous.iter_mut().find(|existing| {
                    existing
                        .as_array()
                        .and_then(|values| values.first())
                        .and_then(Value::as_str)
                        == Some(key)
                }) {
                    *existing = pair;
                } else {
                    previous.push(pair);
                }
            }
            Value::Array(previous)
        }
        (_, next) => next,
    }
}

macro_rules! member {
    ($name:literal, $shape:expr) => {
        Member {
            name: $name,
            aliases: &[],
            shape: $shape,
        }
    };
    ($name:literal, $shape:expr, [$($alias:literal),+ $(,)?]) => {
        Member {
            name: $name,
            aliases: &[$($alias),+],
            shape: $shape,
        }
    };
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Member {
    name: &'static str,
    aliases: &'static [&'static str],
    shape: Shape,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Raw,
    String,
    Bool,
    I32,
    I64,
    Double,
    Uri,
    Guid,
    Object(&'static [Member]),
    Array(&'static [Member]),
    JObject,
    Steps,
    MapValues(&'static [Member]),
    StringMap,
    CaseInsensitiveStringMap,
    UniqueStringMap,
    UniqueRawMap,
    StringList,
    TemplateToken,
    TemplateTokenArray,
    ActionReference,
    PipelineContextData,
    PipelineContextArray,
    ContextDataMap,
}

const AGENT_JOB_FIELDS: &[Member] = &[
    member!("MessageType", Shape::String),
    member!("Plan", Shape::Object(PLAN_FIELDS)),
    member!("Timeline", Shape::Object(TIMELINE_FIELDS)),
    member!("JobId", Shape::Guid),
    member!("JobDisplayName", Shape::String),
    member!("JobName", Shape::String),
    member!("RequestId", Shape::I64),
    member!("LockedUntil", Shape::String),
    member!("QueueTime", Shape::String),
    member!("Variables", Shape::MapValues(VARIABLE_FIELDS)),
    member!("Mask", Shape::Array(MASK_FIELDS)),
    member!("Resources", Shape::Object(JOB_RESOURCES_FIELDS)),
    member!("Steps", Shape::Steps),
    member!("EnvironmentVariables", Shape::TemplateTokenArray),
    member!("Defaults", Shape::TemplateTokenArray),
    member!("JobContainer", Shape::TemplateToken),
    member!("JobServiceContainers", Shape::TemplateToken),
    member!("JobSidecarContainers", Shape::StringMap),
    member!("JobOutputs", Shape::TemplateToken),
    member!("Workspace", Shape::Object(WORKSPACE_FIELDS)),
    member!("ContextData", Shape::ContextDataMap),
    member!(
        "ActionsEnvironment",
        Shape::Object(ACTIONS_ENVIRONMENT_FIELDS)
    ),
    member!("BillingOwnerId", Shape::String),
    member!("dependencies", Shape::StringList),
];

const PLAN_FIELDS: &[Member] = &[
    member!("ScopeIdentifier", Shape::String),
    member!("PlanType", Shape::String),
    member!("Version", Shape::I32),
    member!("PlanId", Shape::Guid),
    member!("PlanGroup", Shape::String),
    member!("ArtifactUri", Shape::Uri),
    member!("ArtifactLocation", Shape::Uri),
    member!("Definition", Shape::Raw),
    member!("Owner", Shape::Raw),
];

const TIMELINE_FIELDS: &[Member] = &[
    member!("Id", Shape::Guid),
    member!("ChangeId", Shape::I32),
    member!("Location", Shape::Uri),
];

const JOB_RESOURCES_FIELDS: &[Member] = &[
    member!("Endpoints", Shape::Array(ENDPOINT_FIELDS)),
    member!("Repositories", Shape::Array(REPOSITORY_FIELDS)),
    member!("Containers", Shape::Array(CONTAINER_FIELDS)),
];

const ENDPOINT_FIELDS: &[Member] = &[
    member!("Name", Shape::String),
    member!("Url", Shape::Uri),
    member!("Authorization", Shape::Object(AUTHORIZATION_FIELDS)),
    member!("Data", Shape::CaseInsensitiveStringMap),
    member!("OperationStatus", Shape::JObject),
];

const AUTHORIZATION_FIELDS: &[Member] = &[
    member!("Scheme", Shape::String),
    member!("Parameters", Shape::UniqueStringMap),
];

const REPOSITORY_FIELDS: &[Member] = &[
    member!("Alias", Shape::String),
    member!("Endpoint", Shape::Object(SERVICE_ENDPOINT_REFERENCE_FIELDS)),
    // Flat shorthand members (main accepted them alongside Properties).
    member!("Name", Shape::String),
    member!("Ref", Shape::String),
    member!("Version", Shape::String),
    member!("Url", Shape::String),
    // This property bag permits arbitrary JToken values.
    member!("Properties", Shape::UniqueRawMap),
];

const CONTAINER_FIELDS: &[Member] = &[
    member!("Alias", Shape::String),
    member!("Endpoint", Shape::Object(SERVICE_ENDPOINT_REFERENCE_FIELDS)),
    member!("Properties", Shape::UniqueRawMap),
];

const VARIABLE_FIELDS: &[Member] = &[
    member!("Value", Shape::String),
    member!("IsSecret", Shape::Bool),
];

const MASK_FIELDS: &[Member] = &[
    member!("Type", Shape::String),
    member!("Value", Shape::String),
];

const ACTION_STEP_FIELDS: &[Member] = &[
    member!("Type", Shape::Raw),
    member!("Id", Shape::Guid),
    member!("Name", Shape::String),
    member!("DisplayName", Shape::String, ["display_name"]),
    member!(
        "DisplayNameToken",
        Shape::TemplateToken,
        ["display_name_token"]
    ),
    member!("Enabled", Shape::Bool),
    member!("Condition", Shape::String),
    member!("ContinueOnError", Shape::TemplateToken),
    member!("TimeoutInMinutes", Shape::TemplateToken),
    member!("ContextName", Shape::String, ["context_name"]),
    member!("ParallelGroupId", Shape::String),
    member!("Background", Shape::Bool),
    member!("Reference", Shape::ActionReference),
    member!("Environment", Shape::TemplateToken),
    member!("Inputs", Shape::TemplateToken),
];

const BACKGROUND_STEP_FIELDS: &[Member] = &[
    member!("Type", Shape::Raw),
    member!("Id", Shape::Guid),
    member!("Name", Shape::String),
    member!("DisplayName", Shape::String, ["display_name"]),
    member!(
        "DisplayNameToken",
        Shape::TemplateToken,
        ["display_name_token"]
    ),
    member!("Enabled", Shape::Bool),
    member!("Condition", Shape::String),
    member!("ContinueOnError", Shape::TemplateToken),
    member!("TimeoutInMinutes", Shape::TemplateToken),
    member!("ParallelGroupId", Shape::String),
    member!("ControlType", Shape::String),
    member!("StepIds", Shape::StringList),
];

const ACTION_REFERENCE_REPOSITORY_FIELDS: &[Member] = &[
    member!("Type", Shape::Raw),
    member!("Name", Shape::String),
    member!("Ref", Shape::String),
    member!("RepositoryType", Shape::String),
    member!("Path", Shape::String),
];
const ACTION_REFERENCE_CONTAINER_FIELDS: &[Member] =
    &[member!("Type", Shape::Raw), member!("Image", Shape::String)];
const ACTION_REFERENCE_SCRIPT_FIELDS: &[Member] = &[member!("Type", Shape::Raw)];

fn member_for_name<'a>(name: &str, members: &'a [Member]) -> Option<&'a Member> {
    members.iter().find(|member| {
        clr_ordinal_ignore_case_eq(name, member.name)
            || member
                .aliases
                .iter()
                .any(|alias| clr_ordinal_ignore_case_eq(name, alias))
    })
}

fn wire_member_is_null(value: &Value, name: &str) -> bool {
    value.as_object().is_some_and(|object| {
        object
            .iter()
            .find(|(member, _)| clr_ordinal_ignore_case_eq(member, name))
            .is_some_and(|(_, value)| value.is_null())
    })
}

/// Whether the wire message carried a non-null `Resources.Containers`
/// collection for legacy sidecar aliases to resolve against.
fn wire_resources_containers_present(value: &Value) -> bool {
    value
        .as_object()
        .and_then(|object| clr_object_member(object, "Resources"))
        .and_then(Value::as_object)
        .and_then(|resources| clr_object_member(resources, "Containers"))
        .is_some_and(|containers| !containers.is_null())
}

fn validate_context_data_root_shape(value: &Value) -> Result<()> {
    if let Some(context_data) = value.as_object().and_then(|object| {
        object
            .iter()
            .find(|(name, _)| clr_ordinal_ignore_case_eq(name, "ContextData"))
            .map(|(_, value)| value)
    }) && !context_data.is_null()
        && !context_data.is_object()
    {
        anyhow::bail!("ContextData must be an object");
    }
    Ok(())
}

fn take_ordered_context_data_pairs(value: &mut Value) -> Result<Option<OrderedContextData<Value>>> {
    let Some(object) = value.as_object_mut() else {
        return Ok(None);
    };
    let Some(name) = object
        .keys()
        .find(|name| clr_ordinal_ignore_case_eq(name, "ContextData"))
        .cloned()
    else {
        return Ok(None);
    };
    let Some(Value::Array(pairs)) = object.get(&name) else {
        return Ok(None);
    };

    let mut ordered = OrderedContextData::default();
    for pair in pairs {
        let Value::Array(pair) = pair else {
            anyhow::bail!("internal ordered ContextData pair must be an array");
        };
        if pair.len() != 2 {
            anyhow::bail!("internal ordered ContextData pair must contain two values");
        }
        let key = pair[0]
            .as_str()
            .context("internal ordered ContextData key must be a string")?
            .to_owned();
        ordered.insert(key, pair[1].clone());
    }

    let mut object_pairs = serde_json::Map::new();
    for (key, value) in ordered.iter() {
        object_pairs.insert(key.clone(), value.clone());
    }
    object.insert(name, Value::Object(object_pairs));
    Ok(Some(ordered))
}

/// Match .NET 8 Linux `StringComparison.OrdinalIgnoreCase` using simple
/// uppercase mappings. .NET's ordinal table deliberately leaves dotless-i and
/// long-s unchanged even though Unicode's general uppercase mapping changes
/// them. ICU4X supplies the remaining simple mappings, including supplementary
/// scalars; versioned Unicode data can differ for newer code points.
pub(crate) fn clr_ordinal_ignore_case_eq(left: &str, right: &str) -> bool {
    velnor_model::ordinal_ignore_case_eq(left, right)
}

fn clr_string(value: &mut Value) {
    match value {
        Value::Bool(boolean) => {
            *value = Value::String(if *boolean { "True" } else { "False" }.to_owned())
        }
        Value::Number(number) => *value = Value::String(dotnet_number_string(number)),
        _ => {}
    }
}

fn clr_bool(value: &mut Value) {
    let coerced = match value {
        Value::String(string) if clr_ordinal_ignore_case_eq(string.trim(), "true") => Some(true),
        Value::String(string) if clr_ordinal_ignore_case_eq(string.trim(), "false") => Some(false),
        Value::Number(number) => number
            .as_i64()
            .map(|number| number != 0)
            .or_else(|| number.as_u64().map(|number| number != 0))
            .or_else(|| number.as_f64().map(|number| number != 0.0)),
        _ => None,
    };
    if let Some(coerced) = coerced {
        *value = Value::Bool(coerced);
    }
}

fn clr_integer(value: &mut Value) {
    match value {
        Value::String(string) => {
            if let Ok(number) = string.trim().parse::<i64>() {
                *value = Value::Number(number.into());
            }
        }
        Value::Number(number) if number.is_f64() => {
            if let Some(number) = number.as_f64().map(f64::round_ties_even)
                && number.is_finite()
                && number >= i64::MIN as f64
                && number <= i64::MAX as f64
            {
                *value = Value::Number((number as i64).into());
            }
        }
        _ => {}
    }
}

fn clr_i64_value(value: &mut Value) -> Result<()> {
    match value {
        Value::Bool(boolean) => {
            *value = Value::Number((if *boolean { 1_i64 } else { 0_i64 }).into())
        }
        Value::String(string) => {
            let number = string
                .trim()
                .parse::<i64>()
                .context("invalid CLR Int64 string")?;
            *value = Value::Number(number.into());
        }
        Value::Number(number) if number.is_f64() => {
            let converted = number
                .as_f64()
                .map(f64::round_ties_even)
                .filter(|number| {
                    number.is_finite()
                        && *number >= i64::MIN as f64
                        && *number < 9_223_372_036_854_775_808.0
                })
                .ok_or_else(|| anyhow::anyhow!("CLR Int64 value is out of range"))?;
            *value = Value::Number((converted as i64).into());
        }
        _ => {}
    }
    Ok(())
}

fn clr_i32_value(value: &mut Value) -> Result<()> {
    match value {
        Value::String(string) => {
            if string.is_empty() {
                *value = Value::Null;
                return Ok(());
            }
            let number = string
                .trim()
                .parse::<i32>()
                .context("invalid CLR Int32 string")?;
            *value = Value::Number(number.into());
        }
        Value::Number(number) if number.is_f64() => {
            let converted = number
                .as_f64()
                .map(f64::round_ties_even)
                .filter(|number| {
                    number.is_finite() && *number >= i32::MIN as f64 && *number <= i32::MAX as f64
                })
                .ok_or_else(|| anyhow::anyhow!("CLR Int32 value is out of range"))?;
            *value = Value::Number((converted as i32).into());
        }
        _ => {}
    }
    Ok(())
}

fn parse_clr_guid(value: &str) -> Result<Uuid> {
    let value = value.trim();
    if value
        .get(..9)
        .is_some_and(|prefix| clr_ordinal_ignore_case_eq(prefix, "urn:uuid:"))
    {
        anyhow::bail!("URN form is not a CLR Guid string");
    }
    let normalized = if value.starts_with('(') && value.ends_with(')') {
        &value[1..value.len() - 1]
    } else {
        value
    };
    if let Ok(guid) = Uuid::parse_str(normalized) {
        return Ok(guid);
    }
    let Some(parts) = normalized
        .strip_prefix("{0x")
        .and_then(|value| value.strip_suffix('}'))
    else {
        anyhow::bail!("invalid CLR Guid string");
    };
    let mut fields = parts.splitn(4, ',');
    let data1 = parse_clr_guid_hex(fields.next(), 8)?;
    let data2 = parse_clr_guid_hex(fields.next(), 4)?;
    let data3 = parse_clr_guid_hex(fields.next(), 4)?;
    let Some(data4) = fields.next().and_then(|value| value.strip_prefix('{')) else {
        anyhow::bail!("invalid CLR Guid X string");
    };
    let data4 = data4
        .strip_suffix('}')
        .ok_or_else(|| anyhow::anyhow!("invalid CLR Guid X string"))?;
    let bytes = data4
        .split(',')
        .map(|part| parse_clr_guid_hex(Some(part), 2).map(|value| value as u8))
        .collect::<Result<Vec<_>>>()?;
    if bytes.len() != 8 {
        anyhow::bail!("invalid CLR Guid X string");
    }
    let mut guid_bytes = [0u8; 16];
    guid_bytes[0..4].copy_from_slice(&(data1 as u32).to_be_bytes());
    guid_bytes[4..6].copy_from_slice(&(data2 as u16).to_be_bytes());
    guid_bytes[6..8].copy_from_slice(&(data3 as u16).to_be_bytes());
    guid_bytes[8..16].copy_from_slice(&bytes);
    Ok(Uuid::from_bytes(guid_bytes))
}

fn parse_clr_guid_hex(value: Option<&str>, width: usize) -> Result<u64> {
    let value = value.context("invalid CLR Guid X string")?.trim();
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.is_empty()
        || value.len() > width
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        anyhow::bail!("invalid CLR Guid X string");
    }
    u64::from_str_radix(value, 16).context("invalid CLR Guid X string")
}

fn clr_string_value(value: &Value) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value.clone())),
        Value::Bool(value) => Ok(Some(if *value { "True" } else { "False" }.to_owned())),
        Value::Number(value) => Ok(Some(dotnet_number_string(value))),
        Value::Array(_) | Value::Object(_) => {
            anyhow::bail!("expected CLR string-compatible value")
        }
    }
}

fn normalize_clr_uri(value: &mut Value) -> Result<()> {
    match value {
        Value::Null => Ok(()),
        Value::String(uri) if uri.is_empty() => {
            *value = Value::Null;
            Ok(())
        }
        Value::String(uri) if is_clr_uri(uri) => Ok(()),
        _ => anyhow::bail!("invalid CLR Uri value"),
    }
}

fn json_number_to_value(number: &JsonNumber) -> Result<Value> {
    match &number.kind {
        JsonNumberKind::Int64(value) => Ok(Value::Number((*value).into())),
        JsonNumberKind::BigInteger(_) => {
            anyhow::bail!("BigInteger requires a typed ContextValue boundary")
        }
        JsonNumberKind::Float(value) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .context("nonfinite double requires a typed ContextValue boundary"),
    }
}

fn json_number_to_double(number: &JsonNumber) -> Result<f64> {
    match &number.kind {
        // Integers convert through the truncating BigInteger cast, not the
        // round-to-nearest hardware conversion: 9007199254740995 projects to
        // 9007199254740994.0.
        JsonNumberKind::Int64(value) => clr_big_integer_to_f64(&value.to_string())
            .context("invalid CLR Int64 Double conversion"),
        JsonNumberKind::BigInteger(value) => {
            clr_big_integer_to_f64(value).context("invalid CLR BigInteger Double conversion")
        }
        JsonNumberKind::Float(value) => Ok(*value),
    }
}

fn double_to_wire_value(number: f64) -> Value {
    if number.is_finite() {
        serde_json::Number::from_f64(number)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    } else {
        Value::String(
            if number.is_nan() {
                "NaN"
            } else if number.is_sign_negative() {
                "-Infinity"
            } else {
                "Infinity"
            }
            .to_owned(),
        )
    }
}

fn json_number_clr_string(number: &JsonNumber) -> Result<String> {
    if number.origin == JsonReaderOrigin::TextReader {
        return Ok(number.lexeme.clone());
    }
    match &number.kind {
        JsonNumberKind::Int64(value) => Ok(value.to_string()),
        JsonNumberKind::BigInteger(value) => Ok(value.clone()),
        JsonNumberKind::Float(value) => {
            let number = serde_json::Number::from_f64(*value)
                .context("cannot stringify nonfinite JObject number")?;
            Ok(dotnet_number_string(&number))
        }
    }
}

fn into_clr_reference_value(value: OrderedJsonValue) -> Result<Value> {
    match value {
        OrderedJsonValue::Undefined => Ok(Value::Null),
        value => value.into_value(),
    }
}

fn clr_double_value(value: &mut Value) -> Result<()> {
    let number = match value {
        Value::Number(number) if number.is_f64() => {
            number.as_f64().context("invalid CLR Double number")?
        }
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                clr_big_integer_to_f64(&integer.to_string())
                    .context("invalid CLR Int64 Double conversion")?
            } else if let Some(integer) = number.as_u64() {
                if integer <= i64::MAX as u64 {
                    clr_big_integer_to_f64(&integer.to_string())
                        .context("invalid CLR Int64 Double conversion")?
                } else {
                    clr_big_integer_to_f64(&integer.to_string())
                        .context("invalid CLR BigInteger Double conversion")?
                }
            } else {
                anyhow::bail!("invalid CLR Double number")
            }
        }
        Value::String(string) if string.is_empty() => {
            *value = Value::Null;
            return Ok(());
        }
        Value::String(string) => parse_clr_double_text(string)
            .ok_or_else(|| anyhow::anyhow!("invalid or null CLR Double string"))?,
        Value::Null => anyhow::bail!("CLR Double cannot be null"),
        Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
            anyhow::bail!("invalid CLR Double value")
        }
    };
    if !number.is_finite() {
        *value = Value::String(
            if number.is_nan() {
                "NaN"
            } else if number.is_sign_negative() {
                "-Infinity"
            } else {
                "Infinity"
            }
            .to_owned(),
        );
        return Ok(());
    }
    let number = serde_json::Number::from_f64(number)
        .ok_or_else(|| anyhow::anyhow!("CLR Double must be finite"))?;
    *value = Value::Number(number);
    Ok(())
}

fn clr_bool_value(value: &mut Value) -> Result<()> {
    match value {
        Value::String(string) if clr_ordinal_ignore_case_eq(string.trim(), "true") => {
            *value = Value::Bool(true)
        }
        Value::String(string) if clr_ordinal_ignore_case_eq(string.trim(), "false") => {
            *value = Value::Bool(false)
        }
        Value::String(_) | Value::Array(_) | Value::Object(_) | Value::Null => {
            anyhow::bail!("invalid CLR Boolean value")
        }
        Value::Number(number) => {
            let boolean = number
                .as_i64()
                .map(|value| value != 0)
                .or_else(|| number.as_u64().map(|value| value != 0))
                .or_else(|| number.as_f64().map(|value| value != 0.0))
                .context("invalid CLR Boolean number")?;
            *value = Value::Bool(boolean);
        }
        Value::Bool(_) => {}
    }
    Ok(())
}

fn clr_discriminator(value: &Value, field: &str) -> Result<Option<i32>> {
    let Value::Number(number) = value else {
        return Ok(None);
    };
    if number.is_f64() {
        return Ok(None);
    }
    let value = number
        .as_i64()
        .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
        .and_then(|value| i32::try_from(value).ok())
        .with_context(|| format!("{field} integer is outside Int32"))?;
    Ok(Some(value))
}

fn template_token_type(value: &Value) -> Result<Option<i32>> {
    let Value::Object(object) = value else {
        return Ok(Some(0));
    };
    let discriminator = clr_object_member(object, "type");
    let Some(discriminator) = discriminator else {
        return Ok(Some(0));
    };
    let Some(kind) = clr_discriminator(discriminator, "TemplateToken type")? else {
        return Ok(None);
    };
    if matches!(kind, 0..=7) {
        Ok(Some(kind))
    } else {
        anyhow::bail!("unknown TemplateToken type {kind}");
    }
}

fn context_data_type(value: &Value) -> Result<Option<i32>> {
    let Value::Object(object) = value else {
        return Ok(Some(0));
    };
    let discriminator = clr_object_member(object, "t");
    let Some(discriminator) = discriminator else {
        return Ok(Some(0));
    };
    let Some(kind) = clr_discriminator(discriminator, "PipelineContextData type")? else {
        return Ok(None);
    };
    if matches!(kind, 0..=5) {
        Ok(Some(kind))
    } else {
        anyhow::bail!("unknown PipelineContextData type {kind}");
    }
}

fn clr_object_member<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Option<&'a Value> {
    object.get(name).or_else(|| {
        object
            .iter()
            .find(|(key, _)| clr_ordinal_ignore_case_eq(key, name))
            .map(|(_, value)| value)
    })
}

const TEMPLATE_TOKEN_FIELDS: &[Member] = &[
    member!("type", Shape::Raw),
    member!("lit", Shape::String),
    member!("expr", Shape::String),
    member!("seq", Shape::TemplateTokenArray),
    member!("map", Shape::Array(TEMPLATE_PAIR_FIELDS)),
    member!("key", Shape::TemplateToken),
    member!("value", Shape::TemplateToken),
    member!("bool", Shape::Bool),
    member!("num", Shape::Raw),
    member!("file", Shape::I32),
    member!("line", Shape::I32),
    member!("col", Shape::I32),
];

const TEMPLATE_PAIR_FIELDS: &[Member] = &[
    member!("key", Shape::TemplateToken),
    member!("value", Shape::TemplateToken),
];

const PIPELINE_CONTEXT_FIELDS: &[Member] = &[
    member!("t", Shape::Raw),
    member!("s", Shape::String),
    member!("a", Shape::PipelineContextArray),
    member!("d", Shape::Array(CONTEXT_PAIR_FIELDS)),
    member!("k", Shape::String),
    member!("v", Shape::PipelineContextData),
    member!("b", Shape::Bool),
    member!("n", Shape::Double),
];

const TEMPLATE_TOKEN_DISCRIMINATOR_FIELDS: &[Member] = &[
    member!("type", Shape::Raw),
    member!("file", Shape::I32),
    member!("line", Shape::I32),
    member!("col", Shape::I32),
];
const TEMPLATE_TOKEN_STRING_FIELDS: &[Member] = &[
    member!("type", Shape::Raw),
    member!("file", Shape::I32),
    member!("line", Shape::I32),
    member!("col", Shape::I32),
    member!("lit", Shape::String),
];
const TEMPLATE_TOKEN_SEQUENCE_FIELDS: &[Member] = &[
    member!("type", Shape::Raw),
    member!("file", Shape::I32),
    member!("line", Shape::I32),
    member!("col", Shape::I32),
    member!("seq", Shape::TemplateTokenArray),
];
const TEMPLATE_TOKEN_MAPPING_FIELDS: &[Member] = &[
    member!("type", Shape::Raw),
    member!("file", Shape::I32),
    member!("line", Shape::I32),
    member!("col", Shape::I32),
    member!("map", Shape::Array(TEMPLATE_PAIR_FIELDS)),
];
const TEMPLATE_TOKEN_EXPRESSION_FIELDS: &[Member] = &[
    member!("type", Shape::Raw),
    member!("file", Shape::I32),
    member!("line", Shape::I32),
    member!("col", Shape::I32),
    member!("expr", Shape::String),
];
const TEMPLATE_TOKEN_BOOLEAN_FIELDS: &[Member] = &[
    member!("type", Shape::Raw),
    member!("file", Shape::I32),
    member!("line", Shape::I32),
    member!("col", Shape::I32),
    member!("bool", Shape::Bool),
];
const TEMPLATE_TOKEN_NUMBER_FIELDS: &[Member] = &[
    member!("type", Shape::Raw),
    member!("file", Shape::I32),
    member!("line", Shape::I32),
    member!("col", Shape::I32),
    member!("num", Shape::Double),
];
const PIPELINE_CONTEXT_DISCRIMINATOR_FIELDS: &[Member] = &[member!("t", Shape::Raw)];
const PIPELINE_CONTEXT_STRING_FIELDS: &[Member] =
    &[member!("t", Shape::Raw), member!("s", Shape::String)];
const PIPELINE_CONTEXT_ARRAY_FIELDS: &[Member] = &[
    member!("t", Shape::Raw),
    member!("a", Shape::PipelineContextArray),
];
const PIPELINE_CONTEXT_DICTIONARY_FIELDS: &[Member] = &[
    member!("t", Shape::Raw),
    member!("d", Shape::Array(CONTEXT_PAIR_FIELDS)),
];
const PIPELINE_CONTEXT_BOOLEAN_FIELDS: &[Member] =
    &[member!("t", Shape::Raw), member!("b", Shape::Bool)];
const PIPELINE_CONTEXT_NUMBER_FIELDS: &[Member] =
    &[member!("t", Shape::Raw), member!("n", Shape::Double)];
const PIPELINE_CONTEXT_CASE_SENSITIVE_DICTIONARY_FIELDS: &[Member] = &[
    member!("t", Shape::Raw),
    member!("d", Shape::Array(CONTEXT_PAIR_FIELDS)),
];

const CONTEXT_PAIR_FIELDS: &[Member] = &[
    member!("k", Shape::String),
    member!("v", Shape::PipelineContextData),
];

const WORKSPACE_FIELDS: &[Member] = &[member!("Clean", Shape::String)];

const ACTIONS_ENVIRONMENT_FIELDS: &[Member] = &[
    member!("name", Shape::String),
    member!("url", Shape::TemplateToken),
];

const SERVICE_ENDPOINT_REFERENCE_FIELDS: &[Member] =
    &[member!("Id", Shape::String), member!("Name", Shape::String)];

fn normalize_clr_members(value: &mut Value, members: &'static [Member]) -> Result<()> {
    let Value::Object(object) = value else {
        return Ok(());
    };

    // Re-key typed CLR object members only. Dictionary keys and raw Value
    // payloads remain untouched, including Resource Properties.
    let keys: Vec<String> = object.keys().cloned().collect();
    for key in keys {
        let Some(member) = member_for_name(&key, members) else {
            continue;
        };
        let canonical = member.name;
        if key != canonical {
            if object.contains_key(canonical) {
                return Err(anyhow::anyhow!(
                    "duplicate case-insensitive member `{canonical}`"
                ));
            }
            if let Some(value) = object.remove(&key) {
                object.insert(canonical.to_owned(), value);
            }
        }
    }

    for member in members {
        let Some(value) = object.get_mut(member.name) else {
            continue;
        };
        match member.shape {
            Shape::Raw => {}
            Shape::String => clr_string(value),
            Shape::Bool => clr_bool_value(value)?,
            Shape::I32 => clr_i32_value(value)?,
            Shape::I64 => clr_i64_value(value)?,
            Shape::Double => clr_double_value(value)?,
            Shape::Uri => normalize_clr_uri(value)?,
            Shape::Guid => normalize_guid(value)?,
            Shape::Object(nested) => normalize_clr_members(value, nested)?,
            Shape::JObject => normalize_jobject_context_value(value)?,
            Shape::Array(nested) => {
                if let Value::Array(items) = value {
                    for item in items {
                        if !item.is_null() {
                            normalize_clr_members(item, nested)?;
                        }
                    }
                }
            }
            Shape::Steps => normalize_step_array(value)?,
            Shape::MapValues(nested) => {
                if let Value::Object(values) = value {
                    for item in values.values_mut() {
                        if !item.is_null() {
                            normalize_clr_members(item, nested)?;
                        }
                    }
                }
            }
            Shape::StringMap => {
                if let Value::Object(values) = value {
                    for item in values.values_mut() {
                        if !item.is_null() {
                            clr_string(item);
                        }
                    }
                }
            }
            Shape::CaseInsensitiveStringMap => normalize_string_map(value)?,
            Shape::UniqueStringMap => {
                normalize_string_map(value)?;
            }
            Shape::UniqueRawMap => normalize_resource_properties_context_value(value)?,
            Shape::StringList => normalize_string_list(value)?,
            Shape::TemplateToken => normalize_template_token(value)?,
            Shape::TemplateTokenArray => normalize_template_token_array(value)?,
            Shape::PipelineContextData => normalize_pipeline_context_data(value)?,
            Shape::PipelineContextArray => normalize_pipeline_context_array(value)?,
            Shape::ActionReference => normalize_action_reference(value)?,
            Shape::ContextDataMap => normalize_context_data_map(value)?,
        }
    }
    Ok(())
}

fn normalize_guid(value: &mut Value) -> Result<()> {
    match value {
        Value::Null => {}
        Value::String(value) => *value = parse_clr_guid(value)?.to_string(),
        _ => anyhow::bail!("expected CLR Guid string"),
    }
    Ok(())
}

fn normalize_resource_properties_context_value(value: &mut Value) -> Result<()> {
    if !value.is_object() {
        anyhow::bail!("Resource.Properties must be an object");
    }
    let context = ContextValue::from_json_root_case_insensitive(value.clone())
        .context("invalid ResourceProperties JToken tree")?;
    *value = serde_json::to_value(context).context("encode ResourceProperties JToken tree")?;
    Ok(())
}

fn normalize_jobject_context_value(value: &mut Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    if !value.is_object() {
        anyhow::bail!("ServiceEndpoint.OperationStatus must be a JObject or null");
    }
    let context = ContextValue::from_json_case_sensitive(value.clone())
        .context("invalid ServiceEndpoint.OperationStatus JObject")?;
    *value = serde_json::to_value(context).context("encode ServiceEndpoint.OperationStatus")?;
    Ok(())
}

fn normalize_string_map(value: &mut Value) -> Result<()> {
    let Value::Object(values) = value else {
        return Ok(());
    };
    let mut normalized = serde_json::Map::new();
    for (key, mut item) in std::mem::take(values) {
        if !item.is_null() {
            clr_string(&mut item);
        }
        let collision = normalized
            .keys()
            .find(|existing| clr_ordinal_ignore_case_eq(existing, &key))
            .cloned();
        if collision.is_some() {
            anyhow::bail!("case-insensitive duplicate dictionary key `{key}`");
        }
        normalized.insert(key, item);
    }
    *values = normalized;
    Ok(())
}

fn normalize_string_list(value: &mut Value) -> Result<()> {
    let Value::Array(items) = value else {
        return Ok(());
    };
    for item in items {
        if item.is_null() {
            continue;
        }
        let normalized = clr_string_value(item)?;
        if let Some(normalized) = normalized {
            *item = Value::String(normalized);
        }
    }
    Ok(())
}

fn normalize_template_token_array(value: &mut Value) -> Result<()> {
    let Value::Array(items) = value else {
        return Ok(());
    };
    for item in items {
        normalize_template_token(item)?;
    }
    Ok(())
}

fn normalize_template_token(value: &mut Value) -> Result<()> {
    if let Value::Number(number) = value {
        if !number.is_f64() {
            // TemplateTokenJsonConverter casts integer reader values through
            // Int64 before constructing a NumberToken(Double). Reject
            // BigInteger here; exact BigInteger preservation applies only to
            // typed raw JToken payloads such as ResourceProperties.
            let integer = number
                .as_i64()
                .context("TemplateToken integer is outside Int64")?;
            let converted = clr_big_integer_to_f64(&integer.to_string())
                .context("finite Int64 converts to finite Double")?;
            *value = Value::Number(
                serde_json::Number::from_f64(converted)
                    .context("finite Int64 converts to finite Double")?,
            );
        }
        return Ok(());
    }
    if value.is_array() {
        // TemplateTokenJsonConverter returns null for JSON arrays.
        *value = Value::Null;
        return Ok(());
    }
    if let Value::Object(object) = value
        && clr_object_member(object, "type").is_none()
    {
        // A discriminator-less object is a plain map (step inputs and
        // environments arrive this way): pass it through untouched instead
        // of forcing string-token shape, which would drop every member.
        return Ok(());
    }
    let Some(kind) = template_token_type(value)? else {
        *value = Value::Null;
        return Ok(());
    };
    if value.is_object() {
        let fields = match kind {
            0 => TEMPLATE_TOKEN_STRING_FIELDS,
            1 => TEMPLATE_TOKEN_SEQUENCE_FIELDS,
            2 => TEMPLATE_TOKEN_MAPPING_FIELDS,
            3 | 4 => TEMPLATE_TOKEN_EXPRESSION_FIELDS,
            5 => TEMPLATE_TOKEN_BOOLEAN_FIELDS,
            6 => TEMPLATE_TOKEN_NUMBER_FIELDS,
            7 => TEMPLATE_TOKEN_DISCRIMINATOR_FIELDS,
            _ => anyhow::bail!("unknown TemplateToken type {kind}"),
        };
        normalize_clr_converter_object(value, fields, "type", kind)?;
    }
    match kind {
        0 | 3 => {
            if let Value::Object(object) = value {
                let name = if kind == 0 { "lit" } else { "expr" };
                if let Some(token) = object.get_mut(name)
                    && let Some(normalized) = clr_string_value(token)?
                {
                    *token = Value::String(normalized);
                }
            }
        }
        1 => validate_token_array_member(value, "seq")?,
        2 => validate_token_array_member(value, "map")?,
        4 | 7 => {}
        5 => {
            if let Value::Object(object) = value
                && let Some(token) = object.get_mut("bool")
            {
                clr_bool_value(token)?;
            }
        }
        6 => {
            if let Value::Object(object) = value
                && let Some(token) = object.get_mut("num")
                && !token.is_null()
            {
                // The member pass already projected an empty string to null;
                // only convert a value it has not normalized yet.
                clr_double_value(token)?;
            }
        }
        _ => anyhow::bail!("unknown TemplateToken type {kind}"),
    }
    if kind == 2
        && let Value::Object(object) = value
        && let Some(Value::Array(pairs)) = object.get_mut("map")
    {
        for pair in pairs {
            if pair.is_null() {
                anyhow::bail!("TemplateToken map item must be an object");
            }
            normalize_clr_members(pair, TEMPLATE_PAIR_FIELDS)?;
        }
    }
    Ok(())
}

fn validate_token_array_member(value: &Value, name: &str) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let Some(member) = object.get(name) else {
        return Ok(());
    };
    if !member.is_null() && !member.is_array() {
        anyhow::bail!("TemplateToken `{name}` must be an array or null");
    }
    Ok(())
}

fn normalize_pipeline_context_array(value: &mut Value) -> Result<()> {
    let Value::Array(items) = value else {
        return Ok(());
    };
    for item in items {
        normalize_pipeline_context_data(item)?;
    }
    Ok(())
}

fn normalize_pipeline_context_data(value: &mut Value) -> Result<()> {
    if let Value::Number(number) = value {
        *value = Value::Number(context_data_number_to_double(number)?);
        return Ok(());
    }
    if value.is_array() {
        // PipelineContextDataJsonConverter returns null for JSON arrays.
        *value = Value::Null;
        return Ok(());
    }
    if value
        .as_object()
        .is_some_and(|object| clr_object_member(object, "t").is_none())
    {
        // Raw pre-hydration context carries plain JSON objects, not wire
        // envelopes. Tag the payload as the string kind without dropping any
        // member: the CLR projection reads a string envelope with no payload
        // as empty, while the typed projection surfaces the kept members as
        // an object.
        if let Value::Object(object) = value {
            object.insert("t".to_owned(), Value::from(0));
        }
        return Ok(());
    }
    let Some(kind) = context_data_type(value)? else {
        *value = Value::Null;
        return Ok(());
    };
    if value.is_object() {
        let fields = match kind {
            0 => PIPELINE_CONTEXT_STRING_FIELDS,
            1 => PIPELINE_CONTEXT_ARRAY_FIELDS,
            2 => PIPELINE_CONTEXT_DICTIONARY_FIELDS,
            3 => PIPELINE_CONTEXT_BOOLEAN_FIELDS,
            4 => PIPELINE_CONTEXT_NUMBER_FIELDS,
            5 => PIPELINE_CONTEXT_CASE_SENSITIVE_DICTIONARY_FIELDS,
            _ => anyhow::bail!("unknown PipelineContextData type {kind}"),
        };
        normalize_clr_converter_object(value, fields, "t", kind)?;
    }
    match kind {
        0 => {
            if let Value::Object(object) = value
                && let Some(string) = object.get_mut("s")
                && let Some(normalized) = clr_string_value(string)?
            {
                *string = Value::String(normalized);
            }
        }
        1 => validate_context_array_member(value, "a")?,
        2 | 5 => {
            validate_context_array_member(value, "d")?;
            if let Value::Object(object) = value
                && let Some(Value::Array(pairs)) = object.get_mut("d")
            {
                for pair in pairs.iter_mut().filter(|pair| !pair.is_null()) {
                    normalize_clr_members(pair, CONTEXT_PAIR_FIELDS)?;
                }
            }
        }
        3 => {
            if let Value::Object(object) = value
                && let Some(boolean) = object.get_mut("b")
            {
                clr_bool_value(boolean)?;
            }
        }
        4 => {
            if let Value::Object(object) = value
                && let Some(number) = object.get_mut("n")
                && !number.is_null()
            {
                // The member pass already projected an empty string to null;
                // only convert a value it has not normalized yet.
                clr_double_value(number)?;
            }
        }
        _ => anyhow::bail!("unknown PipelineContextData type {kind}"),
    }
    Ok(())
}

fn context_data_number_to_double(number: &serde_json::Number) -> Result<serde_json::Number> {
    let value = if number.is_f64() {
        number
            .as_f64()
            .context("invalid PipelineContextData Double")?
    } else {
        let integer = number
            .as_i64()
            .context("PipelineContextData integer is outside Int64")?;
        clr_big_integer_to_f64(&integer.to_string())
            .context("invalid PipelineContextData Int64 Double conversion")?
    };
    serde_json::Number::from_f64(value).context("PipelineContextData Double must be finite")
}

fn validate_context_array_member(value: &Value, name: &str) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let Some(member) = object.get(name) else {
        return Ok(());
    };
    if !member.is_null() && !member.is_array() {
        anyhow::bail!("PipelineContextData `{name}` must be an array or null");
    }
    Ok(())
}

fn normalize_context_data_map(value: &mut Value) -> Result<()> {
    if let Value::Object(entries) = value {
        for entry in entries.values_mut() {
            normalize_pipeline_context_data(entry)?;
        }
    }
    Ok(())
}

fn normalize_action_reference(value: &mut Value) -> Result<()> {
    let Value::Object(object) = value else {
        *value = Value::Null;
        return Ok(());
    };
    let Some(kind) = clr_action_reference_kind(clr_object_member(object, "Type")) else {
        *value = Value::Null;
        return Ok(());
    };
    let fields = match kind {
        1 => ACTION_REFERENCE_REPOSITORY_FIELDS,
        2 => ACTION_REFERENCE_CONTAINER_FIELDS,
        3 => ACTION_REFERENCE_SCRIPT_FIELDS,
        _ => anyhow::bail!("unknown action reference kind {kind}"),
    };
    normalize_clr_converter_object(value, fields, "Type", kind)
}

fn clr_action_reference_kind(value: Option<&Value>) -> Option<i32> {
    match value? {
        Value::Number(number) => number
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .or_else(|| number.as_u64().and_then(|value| i32::try_from(value).ok()))
            .filter(|kind| matches!(kind, 1..=3)),
        Value::String(value) => {
            let value = value.trim();
            if clr_ordinal_ignore_case_eq(value, "Repository") {
                Some(1)
            } else if clr_ordinal_ignore_case_eq(value, "ContainerRegistry") {
                Some(2)
            } else if clr_ordinal_ignore_case_eq(value, "Script") {
                Some(3)
            } else {
                value
                    .parse::<i32>()
                    .ok()
                    .filter(|kind| matches!(kind, 1..=3))
            }
        }
        _ => None,
    }
}

fn normalize_clr_converter_object(
    value: &mut Value,
    members: &'static [Member],
    discriminator: &str,
    kind: i32,
) -> Result<()> {
    let Value::Object(object) = value else {
        return Ok(());
    };
    object.retain(|name, _| !clr_ordinal_ignore_case_eq(name, discriminator));
    object.insert(discriminator.to_owned(), Value::from(kind));
    normalize_mutable_converter_collection_aliases(value, members)?;
    normalize_clr_members(value, members)?;
    let Value::Object(object) = value else {
        return Ok(());
    };
    object.retain(|name, _| members.iter().any(|member| member.name == name));
    Ok(())
}

/// Newtonsoft populates mutable backing lists rather than assigning a fresh
/// list when a case-distinct alias names the same CLR property. Normalize each
/// occurrence before appending so an invalid earlier assignment still fails;
/// a later null clears the backing list, and a later array starts it again.
fn normalize_mutable_converter_collection_aliases(
    value: &mut Value,
    members: &'static [Member],
) -> Result<()> {
    let Value::Object(object) = value else {
        return Ok(());
    };
    for member in members {
        if !is_mutable_converter_collection(member) {
            continue;
        }
        let names = object
            .keys()
            .filter(|name| member_for_name(name, std::slice::from_ref(member)).is_some())
            .cloned()
            .collect::<Vec<_>>();
        if names.len() < 2 {
            continue;
        }
        let mut merged: Option<Value> = None;
        for name in names {
            let mut next = object
                .remove(&name)
                .with_context(|| format!("collected converter member {name} is missing"))?;
            normalize_converter_collection_occurrence(member, &mut next)?;
            merged = Some(match merged {
                Some(previous) => merge_ordered_clr_value(member.shape, previous, next),
                None => next,
            });
        }
        if let Some(merged) = merged {
            object.insert(member.name.to_owned(), merged);
        }
    }
    Ok(())
}

fn is_mutable_converter_collection(member: &Member) -> bool {
    // Table identity compares by content: the field tables are `const`
    // items, so each use site may hold a distinct address and pointer
    // equality is unspecified.
    match (member.name, member.shape) {
        ("seq", Shape::TemplateTokenArray) | ("a", Shape::PipelineContextArray) => true,
        ("map", Shape::Array(fields)) => fields == TEMPLATE_PAIR_FIELDS,
        ("d", Shape::Array(fields)) => fields == CONTEXT_PAIR_FIELDS,
        _ => false,
    }
}

fn normalize_converter_collection_occurrence(member: &Member, value: &mut Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    if !value.is_array() {
        anyhow::bail!(
            "converter collection `{}` must be an array or null",
            member.name
        );
    }
    match member.shape {
        Shape::TemplateTokenArray => normalize_template_token_array(value),
        Shape::PipelineContextArray => normalize_pipeline_context_array(value),
        Shape::Array(fields) => {
            let Value::Array(items) = value else {
                anyhow::bail!(
                    "converter collection `{}` must be an array or null",
                    member.name
                );
            };
            for item in items {
                if item.is_null() {
                    if fields == TEMPLATE_PAIR_FIELDS {
                        anyhow::bail!("TemplateToken map item must be an object");
                    }
                    continue;
                }
                normalize_clr_members(item, fields)?;
            }
            Ok(())
        }
        _ => anyhow::bail!(
            "converter collection `{}` has an unsupported shape",
            member.name
        ),
    }
}

fn step_kind_from_value(value: &Value) -> Option<ActionStepKind> {
    match value {
        Value::Number(number) if !number.is_f64() => match number.as_i64()? {
            4 => Some(ActionStepKind::Action),
            5 => Some(ActionStepKind::BackgroundStepControl),
            _ => None,
        },
        Value::String(value) => {
            if clr_ordinal_ignore_case_eq(value.trim(), "action") || value.trim() == "4" {
                Some(ActionStepKind::Action)
            } else if clr_ordinal_ignore_case_eq(value.trim(), "backgroundstepcontrol")
                || value.trim() == "5"
            {
                Some(ActionStepKind::BackgroundStepControl)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn normalize_step_array(value: &mut Value) -> Result<()> {
    let Value::Array(items) = value else {
        return Ok(());
    };
    for item in items {
        if item.is_null() {
            continue;
        }
        let Some(object) = item.as_object() else {
            *item = Value::Null;
            continue;
        };
        let step_type = object
            .iter()
            .find(|(name, _)| clr_ordinal_ignore_case_eq(name, "type"))
            .map(|(_, value)| value);
        // The discriminator rules when the wire sends one: an explicit but
        // unrecognizable type is not a step. When it is absent, an actionable
        // reference still describes an ordinary action (main never required
        // the discriminator); a contentless object is not a step either.
        let kind = match step_type {
            Some(value) => match step_kind_from_value(value) {
                Some(kind) => kind,
                None => {
                    *item = Value::Null;
                    continue;
                }
            },
            None => {
                let actionable = object.iter().any(|(name, value)| {
                    clr_ordinal_ignore_case_eq(name, "reference") && !value.is_null()
                });
                if !actionable {
                    *item = Value::Null;
                    continue;
                }
                ActionStepKind::Action
            }
        };
        normalize_clr_members(
            item,
            match kind {
                ActionStepKind::Action => ACTION_STEP_FIELDS,
                ActionStepKind::BackgroundStepControl => BACKGROUND_STEP_FIELDS,
            },
        )?;
        if let Value::Object(object) = item {
            let ignored_members: &[&str] = match kind {
                ActionStepKind::Action => &["ControlType", "StepIds"],
                ActionStepKind::BackgroundStepControl => &[
                    "Reference",
                    "ContextName",
                    "Background",
                    "Environment",
                    "Inputs",
                ],
            };
            object.retain(|name, _| {
                !ignored_members
                    .iter()
                    .any(|member| clr_ordinal_ignore_case_eq(name, member))
            });
            object.insert(
                "Type".to_owned(),
                Value::String(
                    match kind {
                        ActionStepKind::Action => "Action",
                        ActionStepKind::BackgroundStepControl => "BackgroundStepControl",
                    }
                    .to_owned(),
                ),
            );
        }
    }
    Ok(())
}

fn deserialize_nullable_guid_string<'de, D>(
    deserializer: D,
) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_else(empty_guid_string))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WireAgentJobRequestMessage {
    #[serde(rename = "MessageType", alias = "messageType")]
    pub message_type: Option<String>,
    #[serde(rename = "Plan", alias = "plan")]
    pub plan: Option<TaskOrchestrationPlanReference>,
    #[serde(rename = "Timeline", alias = "timeline")]
    pub timeline: Option<TimelineReference>,
    #[serde(
        rename = "JobId",
        alias = "jobId",
        default = "empty_guid_string",
        deserialize_with = "deserialize_nullable_guid_string"
    )]
    pub job_id: String,
    #[serde(rename = "JobDisplayName", alias = "jobDisplayName")]
    pub job_display_name: Option<String>,
    #[serde(rename = "JobName", alias = "jobName")]
    pub job_name: Option<String>,
    #[serde(
        rename = "RequestId",
        alias = "requestId",
        deserialize_with = "deserialize_null_default"
    )]
    pub request_id: i64,
    #[serde(default, rename = "LockedUntil", alias = "lockedUntil")]
    pub locked_until: Option<String>,
    /// When GitHub first queued this job (unassigned). Used to fail-close
    /// jobs that sat in the org queue past `VELNOR_QUEUE_WAIT_SECS`.
    #[serde(default, rename = "QueueTime", alias = "queueTime")]
    pub queue_time: Option<String>,
    #[serde(
        default,
        rename = "Variables",
        alias = "variables",
        deserialize_with = "deserialize_null_default"
    )]
    pub variables: BTreeMap<String, Option<VariableValue>>,
    #[serde(
        default,
        rename = "Mask",
        alias = "mask",
        deserialize_with = "deserialize_null_default"
    )]
    pub mask: Vec<Option<MaskHint>>,
    #[serde(default, rename = "Resources", alias = "resources")]
    pub resources: Option<WireJobResources>,
    #[serde(
        default,
        rename = "Steps",
        alias = "steps",
        deserialize_with = "deserialize_null_default"
    )]
    pub steps: Vec<Option<ActionStep>>,
    #[serde(
        default,
        rename = "EnvironmentVariables",
        alias = "environmentVariables",
        deserialize_with = "deserialize_null_default"
    )]
    pub environment_variables: Vec<Value>,
    #[serde(
        default,
        rename = "Defaults",
        alias = "defaults",
        deserialize_with = "deserialize_null_default"
    )]
    pub defaults: Vec<Value>,
    #[serde(default, rename = "JobContainer", alias = "jobContainer")]
    pub job_container: Option<Value>,
    #[serde(
        default,
        rename = "JobServiceContainers",
        alias = "jobServiceContainers"
    )]
    pub job_service_containers: Option<Value>,
    #[serde(
        default,
        rename = "JobSidecarContainers",
        alias = "jobSidecarContainers"
    )]
    pub job_sidecar_containers: Option<BTreeMap<String, Option<String>>>,
    #[serde(default, rename = "JobOutputs", alias = "jobOutputs")]
    pub job_outputs: Option<Value>,
    #[serde(default, rename = "Workspace", alias = "workspace")]
    pub workspace: Option<Value>,
    #[serde(default, rename = "ContextData", alias = "contextData")]
    pub context_data: Option<OrderedContextData<Value>>,
    #[serde(default, rename = "ActionsEnvironment", alias = "actionsEnvironment")]
    pub actions_environment: Option<Value>,
    #[serde(default, rename = "BillingOwnerId", alias = "billingOwnerId")]
    pub billing_owner_id: Option<String>,
    #[serde(
        default,
        rename = "dependencies",
        deserialize_with = "deserialize_null_default"
    )]
    pub actions_dependencies: Vec<Option<String>>,
    /// `Some(alias)` means the CLR deserialization callback replaced a
    /// string-token JobContainer with the matching resource mapping. The
    /// StringToken.Value maps a null backing literal to the empty string.
    #[serde(skip)]
    pub job_container_resource_alias: Option<String>,
    #[serde(skip)]
    explicit_null_plan: bool,
    #[serde(skip)]
    explicit_null_timeline: bool,
    #[serde(skip)]
    explicit_null_resources: bool,
    /// Whether the wire message carried a non-null `Resources.Containers`
    /// collection. Legacy sidecar aliases resolve strictly against a present
    /// collection and pass through unresolved when there is none.
    #[serde(skip)]
    explicit_containers_collection: bool,
}

/// Consumer-facing job model. Collection entries required by Velnor are
/// non-null here; nullable source slots stay in [`WireAgentJobRequestMessage`]
/// until `materialize_runtime` rejects them with their original index.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AgentJobRequestMessage {
    #[serde(rename = "MessageType", alias = "messageType")]
    pub message_type: String,
    #[serde(rename = "Plan", alias = "plan")]
    pub plan: TaskOrchestrationPlanReference,
    #[serde(rename = "Timeline", alias = "timeline")]
    pub timeline: TimelineReference,
    #[serde(
        rename = "JobId",
        alias = "jobId",
        default = "empty_guid_string",
        deserialize_with = "deserialize_nullable_guid_string"
    )]
    pub job_id: String,
    #[serde(rename = "JobDisplayName", alias = "jobDisplayName")]
    pub job_display_name: String,
    #[serde(rename = "JobName", alias = "jobName")]
    pub job_name: Option<String>,
    #[serde(
        rename = "RequestId",
        alias = "requestId",
        deserialize_with = "deserialize_null_default"
    )]
    pub request_id: i64,
    #[serde(default, rename = "LockedUntil", alias = "lockedUntil")]
    pub locked_until: Option<String>,
    #[serde(default, rename = "QueueTime", alias = "queueTime")]
    pub queue_time: Option<String>,
    #[serde(default, rename = "Variables", alias = "variables")]
    pub variables: BTreeMap<String, VariableValue>,
    #[serde(default, rename = "Mask", alias = "mask")]
    pub mask: Vec<MaskHint>,
    #[serde(default, rename = "Resources", alias = "resources")]
    pub resources: JobResources,
    /// `None` slots are retained until the execute-policy gate so dry-run
    /// planning keeps the source indices. Execution rejects them before the
    /// runner projects steps into its stable action slice.
    #[serde(
        default,
        rename = "Steps",
        alias = "steps",
        deserialize_with = "deserialize_null_default"
    )]
    pub steps: Vec<Option<ActionStep>>,
    #[serde(
        default,
        rename = "EnvironmentVariables",
        alias = "environmentVariables",
        deserialize_with = "deserialize_null_default"
    )]
    pub environment_variables: Vec<Value>,
    #[serde(
        default,
        rename = "Defaults",
        alias = "defaults",
        deserialize_with = "deserialize_null_default"
    )]
    pub defaults: Vec<Value>,
    #[serde(default, rename = "JobContainer", alias = "jobContainer")]
    pub job_container: Option<Value>,
    #[serde(
        default,
        rename = "JobServiceContainers",
        alias = "jobServiceContainers"
    )]
    pub job_service_containers: Option<Value>,
    #[serde(
        default,
        rename = "JobSidecarContainers",
        alias = "jobSidecarContainers"
    )]
    pub job_sidecar_containers: BTreeMap<String, Option<String>>,
    #[serde(default, rename = "JobOutputs", alias = "jobOutputs")]
    pub job_outputs: Option<Value>,
    #[serde(default, rename = "Workspace", alias = "workspace")]
    pub workspace: Option<Value>,
    #[serde(default, rename = "ContextData", alias = "contextData")]
    pub context_data: OrderedContextData<Value>,
    #[serde(default, rename = "ActionsEnvironment", alias = "actionsEnvironment")]
    pub actions_environment: Option<Value>,
    #[serde(default, rename = "BillingOwnerId", alias = "billingOwnerId")]
    pub billing_owner_id: Option<String>,
    #[serde(
        default,
        rename = "dependencies",
        deserialize_with = "deserialize_null_default"
    )]
    pub actions_dependencies: Vec<Option<String>>,
    #[serde(skip)]
    pub job_container_resource_alias: Option<String>,
}

impl WireAgentJobRequestMessage {
    pub fn parse_json(body: &str) -> Result<Self> {
        let value = parse_json_text(body).context("parse AgentJobRequestMessage")?;
        let value = value.into_clr_value(Shape::Object(AGENT_JOB_FIELDS))?;
        Self::from_ordered_normalized_value(value)
    }

    /// Validate just the CLR `OnDeserialized` callback against an acquired
    /// wire value. Protocol code uses this before converting a later local
    /// DTO materialization failure into an acquired job with identity only.
    pub fn validate_deserialization_callback_from_value(value: &Value) -> Result<()> {
        const CALLBACK_FIELDS: &[Member] = &[
            member!("JobContainer", Shape::TemplateToken),
            member!("JobServiceContainers", Shape::TemplateToken),
            member!("JobSidecarContainers", Shape::StringMap),
            member!(
                "Resources",
                Shape::Object(&[member!("Containers", Shape::Array(CONTAINER_FIELDS))])
            ),
        ];

        let mut normalized = value.clone();
        normalize_clr_members(&mut normalized, CALLBACK_FIELDS)?;
        Self::validate_deserialization_callback_from_normalized_value(&normalized)
    }

    /// Validate the callback after protocol parsing has already normalized
    /// CLR members and wrapped raw JToken trees as typed ContextValues.
    pub fn validate_deserialization_callback_from_normalized_value(value: &Value) -> Result<()> {
        let normalized = value
            .as_object()
            .context("AgentJobRequestMessage must be an object")?;
        let mut projection = serde_json::Map::new();
        for name in [
            "JobContainer",
            "JobServiceContainers",
            "JobSidecarContainers",
            "Resources",
        ] {
            if let Some(value) = normalized.get(name) {
                projection.insert(name.to_owned(), value.clone());
            }
        }
        let explicit_containers_collection =
            wire_resources_containers_present(&Value::Object(projection.clone()));
        let mut message: Self = serde_json::from_value(Value::Object(projection))
            .context("parse callback fields for AgentJobRequestMessage")?;
        message.explicit_containers_collection = explicit_containers_collection;
        message.validate_jtoken_context_shapes()?;
        message.on_deserialized()
    }

    /// Parse an acquired wire value with CLR's case-insensitive member lookup
    /// while preserving dictionary keys and raw JSON values.
    pub fn from_value(mut value: Value) -> Result<Self> {
        normalize_clr_members(&mut value, AGENT_JOB_FIELDS)?;
        Self::from_normalized_value(value)
    }

    /// Materialize protocol-normalized JSON. Raw JToken fields at this
    /// boundary already carry the strict ContextValue wire form, so this
    /// method never guesses from marker strings or tag-shaped user objects.
    pub fn from_normalized_value(value: Value) -> Result<Self> {
        validate_context_data_root_shape(&value)?;
        Self::from_normalized_value_inner(value)
    }

    /// Materialize the internal ordered-pair representation produced by the
    /// ordered JSON reader and the acquire protocol boundary.
    pub(crate) fn from_ordered_normalized_value(mut value: Value) -> Result<Self> {
        let context_data = take_ordered_context_data_pairs(&mut value)?;
        let mut message = Self::from_normalized_value_inner(value)?;
        if let Some(context_data) = context_data {
            message.context_data = Some(context_data);
        }
        Ok(message)
    }

    fn from_normalized_value_inner(value: Value) -> Result<Self> {
        let explicit_null_plan = wire_member_is_null(&value, "Plan");
        let explicit_null_timeline = wire_member_is_null(&value, "Timeline");
        let explicit_null_resources = wire_member_is_null(&value, "Resources");
        let explicit_containers_collection = wire_resources_containers_present(&value);
        let explicit_display_names = wire_step_display_name_presence(&value);
        let mut message: Self =
            serde_json::from_value(value).context("parse AgentJobRequestMessage")?;
        message.explicit_null_plan = explicit_null_plan;
        message.explicit_null_timeline = explicit_null_timeline;
        message.explicit_null_resources = explicit_null_resources;
        message.explicit_containers_collection = explicit_containers_collection;
        for (slot, is_explicit) in message.steps.iter_mut().zip(explicit_display_names) {
            if let Some(step) = slot {
                step.display_name_is_explicit = is_explicit;
            }
        }
        for step in message.steps.iter_mut().flatten() {
            if let Some(kind) = step.r#type.as_ref().and_then(step_kind_from_value) {
                step.kind = Some(kind);
                match kind {
                    ActionStepKind::Action => {
                        step.control_type = None;
                        step.step_ids.clear();
                    }
                    ActionStepKind::BackgroundStepControl => {
                        step.reference = None;
                        step.context_name = None;
                        step.background = false;
                        step.environment = None;
                        step.inputs = None;
                    }
                }
            }
        }
        message.validate_jtoken_context_shapes()?;
        message.on_deserialized()?;
        Ok(message)
    }

    fn validate_jtoken_context_shapes(&self) -> Result<()> {
        fn validate_resource_properties(properties: &ContextValue) -> Result<()> {
            let ContextValue::Object {
                case_sensitive: false,
                entries,
            } = properties
            else {
                anyhow::bail!("Resource.Properties must be a case-insensitive ContextValue map");
            };
            for (_, value) in entries {
                validate_jobject_tree(value)?;
            }
            Ok(())
        }

        fn validate_jobject_tree(value: &ContextValue) -> Result<()> {
            match value {
                ContextValue::Array(values) => {
                    for value in values {
                        validate_jobject_tree(value)?;
                    }
                }
                ContextValue::Constructor { arguments, .. } => {
                    for value in arguments {
                        validate_jobject_tree(value)?;
                    }
                }
                ContextValue::Object {
                    case_sensitive,
                    entries,
                } => {
                    if !*case_sensitive {
                        anyhow::bail!("nested JToken JObject must be case-sensitive");
                    }
                    for (_, value) in entries {
                        validate_jobject_tree(value)?;
                    }
                }
                ContextValue::Null
                | ContextValue::Undefined
                | ContextValue::Bool(_)
                | ContextValue::Number(_)
                | ContextValue::BigInteger(_)
                | ContextValue::NonFinite(_)
                | ContextValue::String(_) => {}
            }
            Ok(())
        }

        fn validate_operation_status(status: Option<&ContextValue>) -> Result<()> {
            if let Some(status) = status {
                if !matches!(
                    status,
                    ContextValue::Object {
                        case_sensitive: true,
                        ..
                    }
                ) {
                    anyhow::bail!(
                        "ServiceEndpoint.OperationStatus must be a case-sensitive JObject"
                    );
                }
                validate_jobject_tree(status)?;
            }
            Ok(())
        }

        if let Some(resources) = &self.resources {
            for container in resources.containers.iter().flatten() {
                validate_resource_properties(&container.properties)?;
            }
            for repository in resources.repositories.iter().flatten() {
                validate_resource_properties(&repository.properties)?;
            }
            for endpoint in resources.endpoints.iter().flatten() {
                validate_operation_status(endpoint.operation_status.as_ref())?;
            }
        }
        Ok(())
    }

    /// Match JobRunner's `Endpoints.Single(...)` selection.
    pub fn system_connection_single(&self) -> Result<&ServiceEndpoint> {
        self.system_connection_single_or_default()?
            .context("missing SystemVssConnection endpoint")
    }

    /// Match JobDispatcher's `Endpoints.SingleOrDefault(...)` selection.
    pub fn system_connection_single_or_default(&self) -> Result<Option<&ServiceEndpoint>> {
        let Some(resources) = &self.resources else {
            return Ok(None);
        };
        single_endpoint_or_default(&resources.endpoints, |endpoint| {
            endpoint
                .name
                .as_deref()
                .is_some_and(|name| clr_ordinal_ignore_case_eq(name, "SystemVssConnection"))
        })
    }

    /// Expand typed PipelineContextData wire envelopes into values consumed by
    /// Velnor's expression evaluator. Context and dictionary keys stay exact.
    pub fn materialize_context_data(&self) -> Result<BTreeMap<String, Value>> {
        self.context_data
            .as_ref()
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|(name, value)| Ok((name.clone(), pipeline_context_value(value)?)))
            .collect()
    }

    /// Preserve typed context scalars, including .NET nonfinite doubles, and
    /// the comparer of typed dictionary context values.
    pub fn materialize_context_values(&self) -> Result<BTreeMap<String, ContextValue>> {
        self.context_data
            .as_ref()
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|(name, value)| Ok((name.clone(), pipeline_context_context_value(value)?)))
            .collect()
    }

    pub fn materialize_context_values_ordered(&self) -> Result<Vec<(String, ContextValue)>> {
        self.context_data
            .as_ref()
            .into_iter()
            .flat_map(|values| values.iter())
            .map(|(name, value)| Ok((name.clone(), pipeline_context_context_value(value)?)))
            .collect()
    }

    fn on_deserialized(&mut self) -> Result<()> {
        if let Some(alias) = self.job_container.as_ref().and_then(string_token_literal)
            && let Some(resources) = self.resources.as_ref()
            && let Some(container) =
                single_container_or_default(&resources.containers, Some(&alias))?
        {
            self.job_container = Some(container_template_token(container)?);
            self.job_container_resource_alias = Some(alias);
        }

        if let Some(sidecars) = self.job_sidecar_containers.as_ref()
            && !sidecars.is_empty()
            && self
                .job_service_containers
                .as_ref()
                .is_none_or(template_token_is_null)
        {
            let resources = self
                .resources
                .as_ref()
                .context("null Resources during sidecar-container callback")?;
            if resources.containers.is_empty() && !self.explicit_containers_collection {
                // No Containers collection to resolve against: leave legacy
                // sidecars unresolved instead of failing. A present
                // collection resolves strictly below.
                return Ok(());
            }
            let mut entries = Vec::with_capacity(sidecars.len());
            for (network_alias, resource_alias) in sidecars {
                let resource =
                    single_container_or_default(&resources.containers, resource_alias.as_deref())?
                        .with_context(|| {
                            format!(
                                "missing container resource for sidecar alias {resource_alias:?}"
                            )
                        })?;
                entries.push((
                    string_token(network_alias),
                    container_template_token(resource)?,
                ));
            }
            self.job_service_containers = Some(mapping_token(entries));
        }
        Ok(())
    }

    /// Admit the CLR-shaped wire DTO into the runner's stable runtime model.
    /// This is intentionally called only after acquire identity is durable;
    /// failures here are terminal job-admission failures, never HTTP/JSON
    /// acquire retries.
    pub fn materialize_runtime(&self) -> Result<AgentJobRequestMessage> {
        if self.explicit_null_plan {
            anyhow::bail!("acquired AgentJobRequestMessage has null Plan");
        }
        if self.explicit_null_timeline {
            anyhow::bail!("acquired AgentJobRequestMessage has null Timeline");
        }
        if self.explicit_null_resources {
            anyhow::bail!("acquired AgentJobRequestMessage has null Resources");
        }
        let plan = self.plan.clone().unwrap_or_default();
        let timeline = self.timeline.clone().unwrap_or_default();
        let resources = self
            .resources
            .as_ref()
            .map(WireJobResources::materialize_runtime)
            .transpose()?
            .unwrap_or_default();
        let variables = self
            .variables
            .iter()
            .map(|(name, value)| {
                Ok((
                    name.clone(),
                    value
                        .clone()
                        .with_context(|| format!("null Variables[{name:?}] entry"))?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let mask = self
            .mask
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .clone()
                    .with_context(|| format!("null Mask[{index}] entry"))
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(AgentJobRequestMessage {
            message_type: self.message_type.clone().unwrap_or_default(),
            plan,
            timeline,
            job_id: self.job_id.clone(),
            job_display_name: self.job_display_name.clone().unwrap_or_default(),
            job_name: self.job_name.clone(),
            request_id: self.request_id,
            locked_until: self.locked_until.clone(),
            queue_time: self.queue_time.clone(),
            variables,
            mask,
            resources,
            // Keep wire slots and indices until the existing execute-policy
            // gate can reject null steps only for paths that will execute.
            steps: self.steps.clone(),
            environment_variables: self.environment_variables.clone(),
            defaults: self.defaults.clone(),
            job_container: self.job_container.clone(),
            job_service_containers: self.job_service_containers.clone(),
            job_sidecar_containers: self.job_sidecar_containers.clone().unwrap_or_default(),
            job_outputs: self.job_outputs.clone(),
            workspace: self.workspace.clone(),
            context_data: self.context_data.clone().unwrap_or_default(),
            actions_environment: self.actions_environment.clone(),
            billing_owner_id: self.billing_owner_id.clone(),
            actions_dependencies: self.actions_dependencies.clone(),
            job_container_resource_alias: self.job_container_resource_alias.clone(),
        })
    }
}

fn wire_step_display_name_presence(value: &Value) -> Vec<bool> {
    value
        .get("Steps")
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .map(|step| {
                    step.get("DisplayName")
                        .and_then(Value::as_str)
                        .is_some_and(|name| !name.is_empty())
                })
                .collect()
        })
        .unwrap_or_default()
}

impl AgentJobRequestMessage {
    pub fn parse_json(body: &str) -> Result<Self> {
        WireAgentJobRequestMessage::parse_json(body)?.materialize_runtime()
    }

    pub fn from_value(value: Value) -> Result<Self> {
        WireAgentJobRequestMessage::from_value(value)?.materialize_runtime()
    }

    pub fn system_connection_single(&self) -> Result<&ServiceEndpoint> {
        self.system_connection_single_or_default()?
            .context("missing SystemVssConnection endpoint")
    }

    pub fn system_connection_single_or_default(&self) -> Result<Option<&ServiceEndpoint>> {
        let mut selected = None;
        for endpoint in &self.resources.endpoints {
            if endpoint
                .name
                .as_deref()
                .is_some_and(|name| clr_ordinal_ignore_case_eq(name, "SystemVssConnection"))
            {
                if selected.is_some() {
                    anyhow::bail!("multiple endpoints matched endpoint selection");
                }
                selected = Some(endpoint);
            }
        }
        Ok(selected)
    }

    pub fn materialize_context_data(&self) -> Result<BTreeMap<String, Value>> {
        self.context_data
            .iter()
            .map(|(name, value)| Ok((name.clone(), pipeline_context_value(value)?)))
            .collect()
    }

    pub fn materialize_context_values(&self) -> Result<BTreeMap<String, ContextValue>> {
        self.context_data
            .iter()
            .map(|(name, value)| Ok((name.clone(), pipeline_context_context_value(value)?)))
            .collect()
    }

    pub fn materialize_context_values_ordered(&self) -> Result<Vec<(String, ContextValue)>> {
        self.context_data
            .iter()
            .map(|(name, value)| Ok((name.clone(), pipeline_context_context_value(value)?)))
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WireJobResources {
    #[serde(
        rename = "Endpoints",
        alias = "endpoints",
        deserialize_with = "deserialize_null_default"
    )]
    pub endpoints: Vec<Option<ServiceEndpoint>>,
    #[serde(
        rename = "Repositories",
        alias = "repositories",
        deserialize_with = "deserialize_null_default"
    )]
    pub repositories: Vec<Option<RepositoryResource>>,
    #[serde(
        rename = "Containers",
        alias = "containers",
        deserialize_with = "deserialize_null_default"
    )]
    pub containers: Vec<Option<ContainerResource>>,
}

impl WireJobResources {
    fn materialize_runtime(&self) -> Result<JobResources> {
        fn materialize_entries<T: Clone>(name: &str, entries: &[Option<T>]) -> Result<Vec<T>> {
            entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    entry
                        .clone()
                        .with_context(|| format!("null {name}[{index}] resource entry"))
                })
                .collect()
        }

        Ok(JobResources {
            endpoints: materialize_entries("Resources.Endpoints", &self.endpoints)?,
            // CLR only enumerates Containers inside the conditional
            // JobContainer/sidecar callbacks. Keep unused null slots available
            // to the runtime instead of failing admission globally.
            repositories: self.repositories.clone(),
            containers: self.containers.clone(),
        })
    }
}

trait OrderedJsonValueExt: Sized {
    fn collapse_exact_properties(self) -> Self;
    fn into_value(self) -> Result<Value>;
    fn into_clr_string_value(self) -> Result<Option<String>>;
    fn into_clr_value(self, shape: Shape) -> Result<Value>;
    fn into_clr_jobject(self) -> Result<Value>;
    fn into_clr_resource_properties(self) -> Result<Value>;
    fn into_jtoken_context_value(self) -> Result<ContextValue>;
    fn into_clr_object(self, members: &'static [Member]) -> Result<Value>;
    fn into_clr_map_values(self, members: &'static [Member]) -> Result<Value>;
    fn into_clr_string_map(self, reject_case_aliases: bool) -> Result<Value>;
    fn into_clr_case_insensitive_map(self) -> Result<Value>;
    fn into_clr_context_map(self) -> Result<Value>;
    fn into_clr_action_reference(self) -> Result<Value>;
    fn into_clr_step(self) -> Result<Value>;
    fn into_clr_template_token(self) -> Result<Value>;
    fn into_clr_template_token_with_existing(self, existing: Option<Value>) -> Result<Value>;
    fn into_clr_context_data(self) -> Result<Value>;
    fn into_clr_context_data_with_existing(self, existing: Option<Value>) -> Result<Value>;
    fn into_clr_converter_object(
        self,
        members: &'static [Member],
        discriminator: &'static str,
        kind: i32,
    ) -> Result<Value>;
}

impl OrderedJsonValueExt for OrderedJsonValue {
    fn collapse_exact_properties(self) -> Self {
        match self {
            Self::Array(values) => Self::Array(
                values
                    .into_iter()
                    .map(Self::collapse_exact_properties)
                    .collect(),
            ),
            Self::Object(entries) => {
                let mut collapsed: Vec<(String, OrderedJsonValue)> = Vec::new();
                for (name, value) in entries {
                    let value = value.collapse_exact_properties();
                    if let Some((_, previous)) =
                        collapsed.iter_mut().find(|(existing, _)| existing == &name)
                    {
                        *previous = value;
                    } else {
                        collapsed.push((name, value));
                    }
                }
                Self::Object(collapsed)
            }
            value => value,
        }
    }

    fn into_value(self) -> Result<Value> {
        match self {
            Self::Null => Ok(Value::Null),
            Self::Undefined => anyhow::bail!("undefined requires a typed CLR boundary"),
            Self::Constructor { .. } => {
                anyhow::bail!("constructor requires a typed CLR boundary")
            }
            Self::Bool(value) => Ok(Value::Bool(value)),
            Self::Number(value) => json_number_to_value(&value),
            Self::NonFinite { .. } => {
                anyhow::bail!("nonfinite double requires a typed ContextValue boundary")
            }
            Self::String(value) => Ok(Value::String(value)),
            Self::Array(items) => items
                .into_iter()
                .map(Self::into_value)
                .collect::<Result<Vec<_>>>()
                .map(Value::Array),
            Self::Object(entries) => {
                let mut object = serde_json::Map::new();
                for (name, value) in entries {
                    object.insert(name, value.into_value()?);
                }
                Ok(Value::Object(object))
            }
        }
    }

    fn into_clr_string_value(self) -> Result<Option<String>> {
        match self {
            Self::Number(value) => Ok(Some(json_number_clr_string(&value)?)),
            Self::NonFinite {
                origin: JsonReaderOrigin::TextReader,
                lexeme,
                ..
            } => Ok(Some(lexeme)),
            Self::NonFinite {
                value,
                origin: JsonReaderOrigin::JObjectReader,
                ..
            } => Ok(Some(non_finite_double_string(value).to_owned())),
            value => clr_string_value(&value.into_value()?),
        }
    }

    fn into_clr_value(self, shape: Shape) -> Result<Value> {
        match shape {
            Shape::Raw => self.into_value(),
            Shape::String => Ok(self
                .into_clr_string_value()?
                .map(Value::String)
                .unwrap_or(Value::Null)),
            Shape::Bool => {
                let mut value = self.into_value()?;
                clr_bool_value(&mut value)?;
                Ok(value)
            }
            Shape::I32 => {
                let mut value = self.into_value()?;
                clr_i32_value(&mut value)?;
                Ok(value)
            }
            Shape::I64 => {
                let mut value = self.into_value()?;
                clr_i64_value(&mut value)?;
                Ok(value)
            }
            Shape::Uri => match self {
                Self::Null | Self::Undefined => Ok(Value::Null),
                Self::String(value) if value.is_empty() => Ok(Value::Null),
                Self::String(value) if is_clr_uri(&value) => Ok(Value::String(value)),
                _ => anyhow::bail!("invalid CLR Uri value"),
            },
            Shape::Double => {
                let number = match self {
                    Self::Number(value) => json_number_to_double(&value)?,
                    Self::NonFinite { value, .. } => match value {
                        NonFinite::NaN => f64::NAN,
                        NonFinite::PositiveInfinity => f64::INFINITY,
                        NonFinite::NegativeInfinity => f64::NEG_INFINITY,
                    },
                    Self::String(value) if value.is_empty() => return Ok(Value::Null),
                    Self::String(value) => parse_clr_double_text(&value)
                        .ok_or_else(|| anyhow::anyhow!("invalid or null CLR Double string"))?,
                    Self::Null => anyhow::bail!("CLR Double cannot be null"),
                    Self::Bool(_)
                    | Self::Array(_)
                    | Self::Object(_)
                    | Self::Undefined
                    | Self::Constructor { .. } => {
                        anyhow::bail!("invalid CLR Double value")
                    }
                };
                Ok(double_to_wire_value(number))
            }
            Shape::Guid => {
                let mut value = self.into_value()?;
                normalize_guid(&mut value)?;
                Ok(value)
            }
            Shape::Object(members) => self.into_clr_object(members),
            Shape::JObject => self.into_clr_jobject(),
            Shape::Array(members) => match self {
                Self::Array(values) => values
                    .into_iter()
                    .map(|value| value.into_clr_value(Shape::Object(members)))
                    .collect::<Result<Vec<_>>>()
                    .map(Value::Array),
                value => into_clr_reference_value(value),
            },
            Shape::Steps => match self {
                Self::Array(values) => values
                    .into_iter()
                    .map(OrderedJsonValue::into_clr_step)
                    .collect::<Result<Vec<_>>>()
                    .map(Value::Array),
                value => into_clr_reference_value(value),
            },
            Shape::MapValues(members) => self.into_clr_map_values(members),
            Shape::StringMap => self.into_clr_string_map(false),
            Shape::UniqueStringMap => self.into_clr_string_map(true),
            Shape::CaseInsensitiveStringMap => self.into_clr_case_insensitive_map(),
            // The custom converter returns a fresh ResourceProperties object
            // for each Properties member, so its raw JToken keys and values
            // are kept intact here.
            Shape::UniqueRawMap => self.into_clr_resource_properties(),
            Shape::StringList => match self {
                Self::Undefined => Ok(Value::Null),
                Self::Array(values) => values
                    .into_iter()
                    .map(|value| {
                        Ok(value
                            .into_clr_string_value()?
                            .map(Value::String)
                            .unwrap_or(Value::Null))
                    })
                    .collect::<Result<Vec<_>>>()
                    .map(Value::Array),
                value => value.into_value(),
            },
            Shape::TemplateToken => self.into_clr_template_token(),
            Shape::TemplateTokenArray => match self {
                Self::Undefined => Ok(Value::Null),
                Self::Array(values) => collapse_trailing_elisions(values)
                    .into_iter()
                    .map(OrderedJsonValue::into_clr_template_token)
                    .collect::<Result<Vec<_>>>()
                    .map(Value::Array),
                value => into_clr_reference_value(value),
            },
            Shape::ActionReference => self.into_clr_action_reference(),
            Shape::PipelineContextData => self.into_clr_context_data(),
            Shape::PipelineContextArray => match self {
                Self::Undefined => Ok(Value::Null),
                Self::Array(values) => collapse_trailing_elisions(values)
                    .into_iter()
                    .map(OrderedJsonValue::into_clr_context_data)
                    .collect::<Result<Vec<_>>>()
                    .map(Value::Array),
                value => into_clr_reference_value(value),
            },
            Shape::ContextDataMap => self.into_clr_context_map(),
        }
    }

    fn into_clr_jobject(mut self) -> Result<Value> {
        self.set_number_origin(JsonReaderOrigin::JObjectReader);
        match self {
            Self::Null => Ok(Value::Null),
            Self::Undefined => {
                anyhow::bail!("ServiceEndpoint.OperationStatus must be a JObject or null")
            }
            Self::Object(entries) => {
                let value = ContextValue::case_sensitive_object(ordered_context_entries(entries)?)
                    .context("invalid ServiceEndpoint.OperationStatus JObject")?;
                let value = sort_jtoken_object_entries(value);
                serde_json::to_value(value).context("encode ServiceEndpoint.OperationStatus")
            }
            _ => anyhow::bail!("ServiceEndpoint.OperationStatus must be a JObject or null"),
        }
    }

    fn into_clr_resource_properties(mut self) -> Result<Value> {
        self.set_number_origin(JsonReaderOrigin::JObjectReader);
        let entries = match self {
            Self::Object(entries) => entries,
            _ => return Err(anyhow::anyhow!("Resource.Properties must be an object")),
        };
        let value = ContextValue::object(ordered_context_entries(entries)?)
            .context("invalid ResourceProperties JToken tree")?;
        let value = sort_jtoken_object_entries(value);
        serde_json::to_value(value).context("encode ResourceProperties JToken tree")
    }

    fn into_jtoken_context_value(self) -> Result<ContextValue> {
        Ok(match self {
            Self::Null => ContextValue::Null,
            Self::Undefined => ContextValue::Undefined,
            Self::Bool(value) => ContextValue::Bool(value),
            Self::Number(value) => match value.kind {
                JsonNumberKind::Int64(value) => ContextValue::Number(value.into()),
                JsonNumberKind::BigInteger(value) => ContextValue::big_integer(value)?,
                JsonNumberKind::Float(value) => {
                    if let Some(number) = serde_json::Number::from_f64(value) {
                        ContextValue::Number(number)
                    } else {
                        ContextValue::non_finite(if value.is_nan() {
                            NonFinite::NaN
                        } else if value.is_sign_negative() {
                            NonFinite::NegativeInfinity
                        } else {
                            NonFinite::PositiveInfinity
                        })
                    }
                }
            },
            Self::NonFinite { value, .. } => ContextValue::non_finite(value),
            Self::String(value) => ContextValue::String(value),
            Self::Array(values) => ContextValue::Array(
                values
                    .into_iter()
                    .map(Self::into_jtoken_context_value)
                    .collect::<Result<_>>()?,
            ),
            Self::Constructor { name, arguments } => ContextValue::Constructor {
                name,
                arguments: arguments
                    .into_iter()
                    .map(Self::into_jtoken_context_value)
                    .collect::<Result<_>>()?,
            },
            Self::Object(entries) => {
                ContextValue::case_sensitive_object(ordered_context_entries(entries)?)
                    .context("invalid nested JToken JObject")?
            }
        })
    }

    fn into_clr_object(self, members: &'static [Member]) -> Result<Value> {
        let entries = match self {
            Self::Object(entries) => entries,
            Self::Undefined => return Ok(Value::Null),
            value => return value.into_value(),
        };
        let mut object = serde_json::Map::new();
        for (name, value) in entries {
            let Some(member) = member_for_name(&name, members) else {
                // Newtonsoft ignores unknown CLR properties. Drop their
                // complete subtrees before generic Value conversion, which
                // cannot faithfully represent BigInteger/nonfinite JTokens.
                continue;
            };
            if members == CONTEXT_PAIR_FIELDS
                && member.name == "v"
                && matches!(&value, OrderedJsonValue::Constructor { .. })
            {
                anyhow::bail!("PipelineContextData dictionary value is a constructor");
            }
            let canonical = member.name;
            let previous = object.remove(canonical);
            let value = match member.shape {
                Shape::TemplateToken => {
                    value.into_clr_template_token_with_existing(previous.clone())?
                }
                Shape::PipelineContextData => {
                    value.into_clr_context_data_with_existing(previous.clone())?
                }
                shape => value.into_clr_value(shape)?,
            };
            if let Some(previous) = previous {
                let merged = if members == AUTHORIZATION_FIELDS && member.name == "Parameters" {
                    retain_nonempty_authorization_parameters(previous, value)
                } else if converter_members_replace_duplicates(members) {
                    value
                } else {
                    merge_ordered_clr_value(member.shape, previous, value)
                };
                object.insert(canonical.to_owned(), merged);
            } else {
                object.insert(canonical.to_owned(), value);
            }
        }
        Ok(Value::Object(object))
    }

    fn into_clr_map_values(self, members: &'static [Member]) -> Result<Value> {
        let entries = match self {
            Self::Object(entries) => entries,
            Self::Undefined => return Ok(Value::Null),
            value => return value.into_value(),
        };
        let mut object = serde_json::Map::new();
        for (name, value) in entries {
            let value = value.into_clr_value(Shape::Object(members))?;
            object.insert(name, value);
        }
        Ok(Value::Object(object))
    }

    fn into_clr_string_map(self, reject_case_aliases: bool) -> Result<Value> {
        let entries = match self {
            Self::Object(entries) => entries,
            Self::Undefined => return Ok(Value::Null),
            value => return value.into_value(),
        };
        let mut object = serde_json::Map::new();
        for (name, value) in entries {
            let value = value
                .into_clr_string_value()?
                .map(Value::String)
                .unwrap_or(Value::Null);
            if reject_case_aliases {
                if object.contains_key(&name) {
                    object.insert(name, value);
                    continue;
                }
                if object
                    .keys()
                    .any(|existing| clr_ordinal_ignore_case_eq(existing, &name))
                {
                    anyhow::bail!("case-insensitive duplicate dictionary key `{name}`");
                }
                object.insert(name, value);
            } else {
                object.insert(name, value);
            }
        }
        Ok(Value::Object(object))
    }

    fn into_clr_case_insensitive_map(self) -> Result<Value> {
        let entries = match self {
            Self::Object(entries) => entries,
            Self::Undefined => return Ok(Value::Null),
            value => return value.into_value(),
        };
        let mut object = serde_json::Map::new();
        for (name, value) in entries {
            let value = value
                .into_clr_string_value()?
                .map(Value::String)
                .unwrap_or(Value::Null);
            insert_case_insensitive_ordered(&mut object, name, value);
        }
        Ok(Value::Object(object))
    }

    fn into_clr_context_map(self) -> Result<Value> {
        let entries = match self {
            Self::Object(entries) => entries,
            Self::Null | Self::Undefined => return Ok(Value::Null),
            _ => anyhow::bail!("ContextData must be an object"),
        };
        let mut object = Vec::new();
        for (name, value) in entries {
            if matches!(&value, OrderedJsonValue::Constructor { .. }) {
                anyhow::bail!("PipelineContextData dictionary value is a constructor");
            }
            let value = value.into_clr_context_data()?;
            if let Some((_, existing)) = object.iter_mut().find(|(key, _)| key == &name) {
                *existing = value;
            } else {
                object.push((name, value));
            }
        }
        Ok(ordered_context_data_pair_array_value(object))
    }

    fn into_clr_action_reference(self) -> Result<Value> {
        let value = self.collapse_exact_properties();
        let Self::Object(entries) = &value else {
            return match value {
                Self::Undefined => Ok(Value::Null),
                value => value.into_value(),
            };
        };
        let kind = ordered_member(entries, "Type").and_then(|value| match value {
            OrderedJsonValue::Number(JsonNumber {
                kind: JsonNumberKind::Int64(number),
                ..
            }) => clr_action_reference_kind(Some(&Value::Number((*number).into()))),
            OrderedJsonValue::String(value) => {
                clr_action_reference_kind(Some(&Value::String(value.clone())))
            }
            _ => None,
        });
        let Some(kind) = kind else {
            // ActionStepDefinitionReferenceConverter returns null when Type
            // is missing or cannot select one of its concrete subclasses.
            return Ok(Value::Null);
        };
        let members = match kind {
            1 => ACTION_REFERENCE_REPOSITORY_FIELDS,
            2 => ACTION_REFERENCE_CONTAINER_FIELDS,
            3 => ACTION_REFERENCE_SCRIPT_FIELDS,
            _ => anyhow::bail!("unknown action reference kind {kind}"),
        };
        value.into_clr_converter_object(members, "Type", kind)
    }

    fn into_clr_step(self) -> Result<Value> {
        let value = self.collapse_exact_properties();
        let Self::Object(entries) = &value else {
            return match value {
                Self::Undefined => Ok(Value::Null),
                value => value.into_value(),
            };
        };
        let (members, kind) = match ordered_member(entries, "Type").and_then(ordered_step_kind) {
            Some(ActionStepKind::Action) => (ACTION_STEP_FIELDS, 4),
            Some(ActionStepKind::BackgroundStepControl) => (BACKGROUND_STEP_FIELDS, 5),
            None => return value.into_value(),
        };
        value.into_clr_converter_object(members, "Type", kind)
    }

    fn into_clr_template_token(self) -> Result<Value> {
        self.into_clr_template_token_with_existing(None)
    }

    fn into_clr_template_token_with_existing(self, existing: Option<Value>) -> Result<Value> {
        let value = self.collapse_exact_properties();
        let has_discriminator = match &value {
            Self::Object(entries) => ordered_member(entries, "type").is_some(),
            _ => true,
        };
        if !has_discriminator {
            // A discriminator-less object is a plain map (step inputs and
            // environments arrive this way): pass it through untouched
            // instead of forcing string-token shape, which would drop
            // every member. A plain map replaces any aliased existing
            // value; token-shaped duplicates keep existing below.
            let _ = existing;
            return value.into_value();
        }
        let Self::Object(entries) = &value else {
            return match value {
                Self::Number(number) => match number.kind {
                    JsonNumberKind::Int64(value) => {
                        let converted = clr_big_integer_to_f64(&value.to_string())
                            .context("invalid CLR Int64 Double conversion")?;
                        Ok(double_to_wire_value(converted))
                    }
                    JsonNumberKind::Float(value) => Ok(double_to_wire_value(value)),
                    JsonNumberKind::BigInteger(_) => {
                        anyhow::bail!("TemplateToken integer is outside Int64")
                    }
                },
                Self::NonFinite { value, .. } => Ok(serde_json::json!({
                    "type": 6,
                    "num": non_finite_double_string(value),
                })),
                Self::Array(_) => Ok(Value::Null),
                Self::Undefined => Ok(Value::Null),
                value => value.into_value(),
            };
        };
        // TemplateTokenJsonConverter treats the discriminator as a typed
        // member and reads only fields belonging to the selected token kind.
        let kind = match ordered_member(entries, "type") {
            Some(value) => match ordered_converter_i32(value, "TemplateToken type")? {
                Some(kind) => kind,
                None => return Ok(existing.unwrap_or(Value::Null)),
            },
            None => 0,
        };
        let members: &[Member] = match kind {
            0 => TEMPLATE_TOKEN_STRING_FIELDS,
            1 => TEMPLATE_TOKEN_SEQUENCE_FIELDS,
            2 => TEMPLATE_TOKEN_MAPPING_FIELDS,
            3 => TEMPLATE_TOKEN_EXPRESSION_FIELDS,
            4 => TEMPLATE_TOKEN_EXPRESSION_FIELDS,
            5 => TEMPLATE_TOKEN_BOOLEAN_FIELDS,
            6 => TEMPLATE_TOKEN_NUMBER_FIELDS,
            _ => TEMPLATE_TOKEN_DISCRIMINATOR_FIELDS,
        };
        value.into_clr_converter_object(members, "type", kind)
    }

    fn into_clr_context_data(self) -> Result<Value> {
        self.into_clr_context_data_with_existing(None)
    }

    fn into_clr_context_data_with_existing(self, existing: Option<Value>) -> Result<Value> {
        let value = self.collapse_exact_properties();
        let Self::Object(entries) = &value else {
            return match value {
                Self::Array(_) => Ok(Value::Null),
                Self::Number(number) => match number.kind {
                    JsonNumberKind::Int64(value) => {
                        let converted = clr_big_integer_to_f64(&value.to_string())
                            .context("invalid CLR Int64 Double conversion")?;
                        Ok(double_to_wire_value(converted))
                    }
                    JsonNumberKind::Float(value) => Ok(double_to_wire_value(value)),
                    JsonNumberKind::BigInteger(_) => {
                        anyhow::bail!("PipelineContextData integer is outside Int64")
                    }
                },
                Self::NonFinite { value, .. } => Ok(serde_json::json!({
                    "t": 4,
                    "n": non_finite_double_string(value),
                })),
                Self::Undefined | Self::Constructor { .. } => Ok(Value::Null),
                value => value.into_value(),
            };
        };
        // PipelineContextDataJsonConverter defaults objects without `t` to
        // StringContextData. Its query spelling is lower-case and exact-key
        // priority comes from JObject.TryGetValue.
        let kind = match ordered_member(entries, "t") {
            Some(value) => match ordered_converter_i32(value, "PipelineContextData type")? {
                Some(kind) => kind,
                None => return Ok(existing.unwrap_or(Value::Null)),
            },
            None => 0,
        };
        let members: &[Member] = match kind {
            0 => PIPELINE_CONTEXT_STRING_FIELDS,
            1 => PIPELINE_CONTEXT_ARRAY_FIELDS,
            2 => PIPELINE_CONTEXT_DICTIONARY_FIELDS,
            3 => PIPELINE_CONTEXT_BOOLEAN_FIELDS,
            4 => PIPELINE_CONTEXT_NUMBER_FIELDS,
            5 => PIPELINE_CONTEXT_CASE_SENSITIVE_DICTIONARY_FIELDS,
            _ => PIPELINE_CONTEXT_DISCRIMINATOR_FIELDS,
        };
        value.into_clr_converter_object(members, "t", kind)
    }

    fn into_clr_converter_object(
        self,
        members: &'static [Member],
        discriminator: &'static str,
        kind: i32,
    ) -> Result<Value> {
        let Self::Object(entries) = self else {
            return self.into_value();
        };
        let mut object = serde_json::Map::new();
        for (name, mut value) in entries {
            let Some(member) = member_for_name(&name, members) else {
                // JObject.Populate ignores members the selected concrete
                // token/context-data class does not expose.
                continue;
            };
            value.set_number_origin(JsonReaderOrigin::JObjectReader);
            let previous = object.remove(member.name);
            let normalized = match member.shape {
                Shape::TemplateToken => {
                    value.into_clr_template_token_with_existing(previous.clone())?
                }
                Shape::PipelineContextData => {
                    value.into_clr_context_data_with_existing(previous.clone())?
                }
                shape => value.into_clr_value(shape)?,
            };
            // Mutable backing lists keep their existing collection as aliases
            // populate the same CLR property. Scalar properties are assigned
            // in order, and converter-selected Type remains constructor-fixed.
            let normalized = match previous {
                Some(previous) if is_mutable_converter_collection(member) => {
                    merge_ordered_clr_value(member.shape, previous, normalized)
                }
                _ => normalized,
            };
            object.insert(member.name.to_owned(), normalized);
        }
        object.insert(discriminator.to_owned(), Value::from(kind));
        Ok(Value::Object(object))
    }
}

fn converter_members_replace_duplicates(members: &'static [Member]) -> bool {
    members == TEMPLATE_TOKEN_DISCRIMINATOR_FIELDS
        || members == TEMPLATE_TOKEN_STRING_FIELDS
        || members == TEMPLATE_TOKEN_SEQUENCE_FIELDS
        || members == TEMPLATE_TOKEN_MAPPING_FIELDS
        || members == TEMPLATE_TOKEN_EXPRESSION_FIELDS
        || members == TEMPLATE_TOKEN_BOOLEAN_FIELDS
        || members == TEMPLATE_TOKEN_NUMBER_FIELDS
        || members == PIPELINE_CONTEXT_DISCRIMINATOR_FIELDS
        || members == PIPELINE_CONTEXT_STRING_FIELDS
        || members == PIPELINE_CONTEXT_ARRAY_FIELDS
        || members == PIPELINE_CONTEXT_DICTIONARY_FIELDS
        || members == PIPELINE_CONTEXT_BOOLEAN_FIELDS
        || members == PIPELINE_CONTEXT_NUMBER_FIELDS
        || members == PIPELINE_CONTEXT_CASE_SENSITIVE_DICTIONARY_FIELDS
}

fn ordered_action_reference_type(value: &OrderedJsonValue) -> Option<(ActionReferenceType, i32)> {
    let kind = match value {
        OrderedJsonValue::Number(_) => ordered_i32(value)?,
        OrderedJsonValue::String(value) => {
            let value = value.trim();
            if clr_ordinal_ignore_case_eq(value, "Repository") {
                1
            } else if clr_ordinal_ignore_case_eq(value, "ContainerRegistry") {
                2
            } else if clr_ordinal_ignore_case_eq(value, "Script") {
                3
            } else {
                value.parse::<i32>().ok()?
            }
        }
        _ => return None,
    };
    let value = match kind {
        1 => ActionReferenceType::Repository,
        2 => ActionReferenceType::ContainerRegistry,
        3 => ActionReferenceType::Script,
        _ => return None,
    };
    Some((value, kind))
}

fn ordered_member<'a>(
    entries: &'a [(String, OrderedJsonValue)],
    name: &str,
) -> Option<&'a OrderedJsonValue> {
    entries
        .iter()
        .rfind(|(key, _)| key == name)
        .or_else(|| {
            entries
                .iter()
                .find(|(key, _)| clr_ordinal_ignore_case_eq(key, name))
        })
        .map(|(_, value)| value)
}

fn ordered_i32(value: &OrderedJsonValue) -> Option<i32> {
    let OrderedJsonValue::Number(number) = value else {
        return None;
    };
    match &number.kind {
        JsonNumberKind::Int64(value) => i32::try_from(*value).ok(),
        JsonNumberKind::BigInteger(_) | JsonNumberKind::Float(_) => None,
    }
}

fn ordered_converter_i32(value: &OrderedJsonValue, field: &str) -> Result<Option<i32>> {
    let OrderedJsonValue::Number(number) = value else {
        return Ok(None);
    };
    let JsonNumberKind::Int64(integer) = &number.kind else {
        return match &number.kind {
            JsonNumberKind::Float(_) => Ok(None),
            _ => {
                anyhow::bail!("{field} integer is outside Int32")
            }
        };
    };
    i32::try_from(*integer)
        .map(Some)
        .with_context(|| format!("{field} integer is outside Int32"))
}

/// Collapse a trailing run of elision holes to a single hole in
/// converter backing arrays. `[,]` parses to two holes (one per empty
/// slot), which raw JToken trees keep; the typed token/context converters
/// project the trailing run to one null instead.
fn collapse_trailing_elisions(values: Vec<OrderedJsonValue>) -> Vec<OrderedJsonValue> {
    let mut trimmed = values;
    let mut trailing_holes = 0;
    while trimmed
        .last()
        .is_some_and(|value| matches!(value, OrderedJsonValue::Undefined))
    {
        trimmed.pop();
        trailing_holes += 1;
    }
    if trailing_holes > 0 {
        trimmed.push(OrderedJsonValue::Undefined);
    }
    trimmed
}

/// Sort JToken object entries into canonical byte order. The `from_value`
/// path iterates `serde_json::Map`s, which are already byte-ordered, so the
/// ordered-reader path must sort explicitly for both parse paths to agree.
fn sort_jtoken_object_entries(value: ContextValue) -> ContextValue {
    match value {
        ContextValue::Object {
            case_sensitive,
            mut entries,
        } => {
            for (_, member) in entries.iter_mut() {
                let sorted =
                    sort_jtoken_object_entries(std::mem::replace(member, ContextValue::Null));
                *member = sorted;
            }
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            ContextValue::Object {
                case_sensitive,
                entries,
            }
        }
        ContextValue::Array(values) => {
            ContextValue::Array(values.into_iter().map(sort_jtoken_object_entries).collect())
        }
        ContextValue::Constructor { name, arguments } => ContextValue::Constructor {
            name,
            arguments: arguments
                .into_iter()
                .map(sort_jtoken_object_entries)
                .collect(),
        },
        other => other,
    }
}

fn ordered_context_entries(
    entries: Vec<(String, OrderedJsonValue)>,
) -> Result<Vec<(String, ContextValue)>> {
    let mut context_entries: Vec<(String, ContextValue)> = Vec::with_capacity(entries.len());
    for (name, value) in entries {
        let value = value.into_jtoken_context_value()?;
        if let Some(existing) = context_entries
            .iter_mut()
            .find(|(existing, _)| existing == &name)
        {
            // Json.NET's JObject and dictionary readers replace an exact
            // duplicate member with the last value while retaining its slot.
            existing.1 = value;
        } else {
            context_entries.push((name, value));
        }
    }
    Ok(context_entries)
}

fn ordered_step_kind(value: &OrderedJsonValue) -> Option<ActionStepKind> {
    match value {
        OrderedJsonValue::Number(_) => match ordered_i32(value)? {
            4 => Some(ActionStepKind::Action),
            5 => Some(ActionStepKind::BackgroundStepControl),
            _ => None,
        },
        OrderedJsonValue::String(value) => {
            let value = value.trim();
            if clr_ordinal_ignore_case_eq(value, "Action") || value == "4" {
                Some(ActionStepKind::Action)
            } else if clr_ordinal_ignore_case_eq(value, "BackgroundStepControl") || value == "5" {
                Some(ActionStepKind::BackgroundStepControl)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn merge_ordered_clr_value(shape: Shape, previous: Value, next: Value) -> Value {
    match shape {
        Shape::Object(members) => merge_ordered_clr_objects(members, previous, next),
        Shape::Array(_)
        | Shape::Steps
        | Shape::StringList
        | Shape::TemplateTokenArray
        | Shape::PipelineContextArray => merge_ordered_clr_arrays(previous, next),
        Shape::ContextDataMap => merge_context_data_pair_arrays(previous, next),
        Shape::MapValues(_) | Shape::StringMap | Shape::UniqueStringMap => {
            merge_ordered_clr_maps(previous, next, false)
        }
        Shape::CaseInsensitiveStringMap => merge_ordered_clr_maps(previous, next, true),
        // ResourcePropertiesJsonConverter replaces `existingValue` with a
        // new bag, unlike ordinary writable fields populated by Json.NET.
        Shape::UniqueRawMap => next,
        Shape::JObject => next,
        Shape::TemplateToken
        | Shape::PipelineContextData
        | Shape::ActionReference
        | Shape::Raw
        | Shape::String
        | Shape::Bool
        | Shape::I32
        | Shape::I64
        | Shape::Double
        | Shape::Uri
        | Shape::Guid => next,
    }
}

fn merge_ordered_clr_arrays(previous: Value, next: Value) -> Value {
    match (previous, next) {
        (Value::Array(mut previous), Value::Array(next)) => {
            previous.extend(next);
            Value::Array(previous)
        }
        (_, next) => next,
    }
}

fn merge_ordered_clr_maps(previous: Value, next: Value, case_insensitive: bool) -> Value {
    match (previous, next) {
        (Value::Object(mut previous), Value::Object(next)) => {
            for (name, value) in next {
                if case_insensitive {
                    insert_case_insensitive_ordered(&mut previous, name, value);
                } else {
                    previous.insert(name, value);
                }
            }
            Value::Object(previous)
        }
        (_, next) => next,
    }
}

fn merge_ordered_clr_objects(members: &'static [Member], previous: Value, next: Value) -> Value {
    let (mut previous, next) = match (previous, next) {
        (Value::Object(previous), Value::Object(next)) => (previous, next),
        (_, next) => return next,
    };
    for (name, value) in next {
        let Some(member) = member_for_name(&name, members) else {
            previous.insert(name, value);
            continue;
        };
        if members == AUTHORIZATION_FIELDS && member.name == "Parameters" {
            // EndpointAuthorization.OnDeserialized copies the later
            // serialized parameter dictionary into a fresh
            // OrdinalIgnoreCase dictionary only when it contains entries.
            // A null or empty later dictionary leaves the earlier backing
            // dictionary in place.
            if let Some(old_value) = previous.remove(member.name) {
                previous.insert(
                    member.name.to_owned(),
                    retain_nonempty_authorization_parameters(old_value, value),
                );
            }
            continue;
        }
        let Some(old_value) = previous.remove(member.name) else {
            previous.insert(member.name.to_owned(), value);
            continue;
        };
        previous.insert(
            member.name.to_owned(),
            merge_ordered_clr_value(member.shape, old_value, value),
        );
    }
    Value::Object(previous)
}

fn retain_nonempty_authorization_parameters(previous: Value, next: Value) -> Value {
    match &next {
        Value::Null => previous,
        Value::Object(values) if values.is_empty() => previous,
        _ => next,
    }
}

fn insert_case_insensitive_ordered(
    object: &mut serde_json::Map<String, Value>,
    name: String,
    value: Value,
) {
    if let Some(existing) = object
        .keys()
        .find(|existing| clr_ordinal_ignore_case_eq(existing, &name))
        .cloned()
    {
        object.insert(existing, value);
    } else {
        object.insert(name, value);
    }
}

fn aliases_equal(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => clr_ordinal_ignore_case_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn single_container_or_default<'a>(
    containers: &'a [Option<ContainerResource>],
    alias: Option<&str>,
) -> Result<Option<&'a ContainerResource>> {
    let mut matching = None;
    for container in containers {
        // The pinned callback's LINQ predicate dereferences each entry. A null
        // slot therefore fails only when alias resolution actually enumerates
        // this collection.
        let container = container
            .as_ref()
            .context("null container resource during deserialization callback")?;
        if aliases_equal(container.alias.as_deref(), alias) {
            if matching.is_some() {
                anyhow::bail!("multiple container resources match alias {alias:?}");
            }
            matching = Some(container);
        }
    }
    Ok(matching)
}

fn string_token_literal(value: &Value) -> Option<String> {
    if let Value::String(value) = value {
        return Some(value.clone());
    }
    let Value::Object(object) = value else {
        return None;
    };
    match template_token_type(value).ok().flatten()? {
        0 => {
            if clr_object_member(object, "type").is_none() {
                return None;
            }
            Some(
                clr_object_member(object, "lit")
                    .and_then(|value| clr_string_value(value).ok().flatten())
                    .unwrap_or_default(),
            )
        }
        _ => None,
    }
}

fn template_token_is_null(value: &Value) -> bool {
    value.is_null() || matches!(template_token_type(value), Ok(Some(7)))
}

fn string_token(value: &str) -> Value {
    serde_json::json!({ "type": 0, "lit": value })
}

fn mapping_token(entries: Vec<(Value, Value)>) -> Value {
    serde_json::json!({
        "type": 2,
        "map": entries
            .into_iter()
            .map(|(key, value)| serde_json::json!({ "key": key, "value": value }))
            .collect::<Vec<_>>(),
    })
}

fn container_template_token(resource: &ContainerResource) -> Result<Value> {
    let mut entries = Vec::new();
    for name in ["image", "options"] {
        let Some(value) = resource.property_value(name) else {
            continue;
        };
        let Some(value) = context_value_string(value)? else {
            continue;
        };
        if !value.is_empty() {
            entries.push((string_token(name), string_token(&value)));
        }
    }

    if let Some(environment) = resource.property_string_map("env")?
        && !environment.is_empty()
    {
        let environment = environment
            .into_iter()
            .map(|(key, value)| {
                let value = string_token(value.as_deref().unwrap_or_default());
                (string_token(&key), value)
            })
            .collect();
        entries.push((string_token("env"), mapping_token(environment)));
    }
    if let Some(items) = resource.property_string_list("ports")?
        && !items.is_empty()
    {
        let sequence = serde_json::json!({
            "type": 1,
            "seq": items
                .into_iter()
                .map(|item| string_token(item.as_deref().unwrap_or_default()))
                .collect::<Vec<_>>(),
        });
        entries.push((string_token("ports"), sequence));
    }
    if resource.property_value("volumes").is_some() {
        // Carry only presence so the adapter can reject this unsupported
        // field before it parses or expands any untrusted volume tokens.
        entries.push((string_token("volumes"), serde_json::json!({ "type": 7 })));
    }
    if resource.property_value("credentials").is_some() {
        // Keep only presence visible to the adapter so it can reject the
        // unsupported field without moving secrets into plans or loggable
        // TemplateTokens.
        entries.push((
            string_token("credentials"),
            serde_json::json!({ "type": 7 }),
        ));
    }
    Ok(mapping_token(entries))
}

fn pipeline_context_value(value: &Value) -> Result<Value> {
    let Value::Object(object) = value else {
        return match value {
            // PipelineContextDataJsonConverter returns null for JSON arrays.
            Value::Array(_) => Ok(Value::Null),
            Value::Number(number) => Ok(Value::Number(context_data_number_to_double(number)?)),
            _ => Ok(value.clone()),
        };
    };
    if clr_object_member(object, "t").is_none() {
        // Raw pre-hydration context carries plain JSON objects, not wire
        // envelopes; materialize them member-wise instead of misreading the
        // untagged object as an empty string envelope.
        let mut values = serde_json::Map::new();
        for (key, member) in object {
            values.insert(key.clone(), pipeline_context_value(member)?);
        }
        return Ok(Value::Object(values));
    }
    let Some(kind) = context_data_type(value)? else {
        return Ok(Value::Null);
    };
    let find = |name: &str| clr_object_member(object, name);
    match kind {
        0 => Ok(Value::String(
            find("s")
                .map(clr_string_value)
                .transpose()?
                .flatten()
                .unwrap_or_default(),
        )),
        1 => {
            let mut values = Vec::new();
            if let Some(Value::Array(items)) = find("a") {
                for item in items {
                    values.push(pipeline_context_value(item)?);
                }
            }
            Ok(Value::Array(values))
        }
        2 | 5 => {
            let mut values = serde_json::Map::new();
            if let Some(Value::Array(items)) = find("d") {
                for pair in items {
                    let pair = pair.as_object().context(
                        "PipelineContextData dictionary contains a null or non-object pair",
                    )?;
                    let key = pair
                        .iter()
                        .find(|(key, _)| clr_ordinal_ignore_case_eq(key, "k"))
                        .and_then(|(_, value)| value.as_str());
                    let key =
                        key.context("PipelineContextData dictionary pair has no string key")?;
                    let value = pair
                        .iter()
                        .find(|(name, _)| clr_ordinal_ignore_case_eq(name, "v"))
                        .map(|(_, value)| pipeline_context_value(value))
                        .transpose()?
                        .unwrap_or(Value::Null);
                    if kind == 2
                        && values
                            .keys()
                            .any(|existing| clr_ordinal_ignore_case_eq(existing, key))
                    {
                        anyhow::bail!(
                            "duplicate case-insensitive PipelineContextData dictionary key `{key}`"
                        );
                    }
                    if kind == 5 && values.contains_key(key) {
                        anyhow::bail!(
                            "duplicate case-sensitive PipelineContextData dictionary key `{key}`"
                        );
                    }
                    values.insert(key.to_owned(), value);
                }
            }
            Ok(Value::Object(values))
        }
        3 => Ok(find("b").cloned().unwrap_or(Value::Bool(false))),
        4 => {
            let number = find("n").cloned().unwrap_or(Value::Number(0.into()));
            if number
                .as_str()
                .is_some_and(|value| matches!(value, "NaN" | "Infinity" | "-Infinity"))
            {
                anyhow::bail!(
                    "nonfinite PipelineContextData requires ContextValue materialization"
                );
            }
            match number {
                Value::Number(number) => Ok(Value::Number(context_data_number_to_double(&number)?)),
                Value::String(value) if value.is_empty() => Ok(Value::Null),
                Value::String(value) => {
                    let number = parse_clr_double_text(&value)
                        .ok_or_else(|| anyhow::anyhow!("invalid or null CLR Double string"))?;
                    let number = serde_json::Number::from_f64(number).context(
                        "nonfinite PipelineContextData requires ContextValue materialization",
                    )?;
                    Ok(Value::Number(number))
                }
                Value::Null => Ok(Value::Null),
                _ => anyhow::bail!("invalid PipelineContextData number"),
            }
        }
        _ => anyhow::bail!("unknown PipelineContextData type {kind}"),
    }
}

fn pipeline_context_context_value(value: &Value) -> Result<ContextValue> {
    let Some(object) = value.as_object() else {
        return match value {
            // PipelineContextDataJsonConverter returns null for JSON arrays.
            Value::Array(_) => Ok(ContextValue::Null),
            Value::Number(number) => {
                Ok(ContextValue::Number(context_data_number_to_double(number)?))
            }
            _ => ContextValue::from_json(value.clone()).map_err(anyhow::Error::from),
        };
    };
    if clr_object_member(object, "t").is_none() {
        // Raw pre-hydration context carries plain JSON objects, not wire
        // envelopes; materialize them member-wise instead of misreading the
        // untagged object as an empty string envelope.
        let entries = object
            .iter()
            .map(|(key, member)| {
                pipeline_context_context_value(member).map(|value| (key.clone(), value))
            })
            .collect::<Result<Vec<_>>>()?;
        return ContextValue::object(entries)
            .map_err(anyhow::Error::from)
            .context("invalid raw PipelineContextData object");
    }
    let Some(kind) = context_data_type(value)? else {
        return Ok(ContextValue::Null);
    };
    let member = |name: &str| clr_object_member(object, name);
    match kind {
        0 => {
            if member("s").is_none() {
                // A string envelope with no string payload carries no string;
                // surface its remaining members as an object so raw
                // pre-hydration payloads keep their shape. The discriminator
                // itself is envelope framing, not data.
                let entries = object
                    .iter()
                    .filter(|(key, _)| !clr_ordinal_ignore_case_eq(key, "t"))
                    .map(|(key, value)| {
                        pipeline_context_context_value(value).map(|value| (key.clone(), value))
                    })
                    .collect::<Result<Vec<_>>>()?;
                return ContextValue::object(entries)
                    .map_err(anyhow::Error::from)
                    .context("invalid string-envelope object members");
            }
            Ok(ContextValue::String(
                member("s")
                    .map(clr_string_value)
                    .transpose()?
                    .flatten()
                    .unwrap_or_default(),
            ))
        }
        1 => {
            let values = match member("a") {
                Some(Value::Array(values)) => values
                    .iter()
                    .map(pipeline_context_context_value)
                    .collect::<Result<Vec<_>>>()?,
                Some(Value::Null) | None => Vec::new(),
                Some(_) => anyhow::bail!("PipelineContextData array must be an array or null"),
            };
            Ok(ContextValue::Array(values))
        }
        2 | 5 => {
            let mut entries = Vec::new();
            if let Some(Value::Array(pairs)) = member("d") {
                for pair in pairs {
                    let pair = pair.as_object().context(
                        "PipelineContextData dictionary contains a null or non-object pair",
                    )?;
                    let key = pair
                        .iter()
                        .find(|(name, _)| clr_ordinal_ignore_case_eq(name, "k"))
                        .and_then(|(_, value)| value.as_str())
                        .context("PipelineContextData dictionary pair has no string key")?;
                    let value = pair
                        .iter()
                        .find(|(name, _)| clr_ordinal_ignore_case_eq(name, "v"))
                        .map(|(_, value)| pipeline_context_context_value(value))
                        .transpose()?
                        .unwrap_or(ContextValue::Null);
                    let duplicate = entries
                        .iter()
                        .any(|(existing, _): &(String, ContextValue)| {
                            if kind == 2 {
                                clr_ordinal_ignore_case_eq(existing, key)
                            } else {
                                existing == key
                            }
                        });
                    if duplicate {
                        anyhow::bail!("duplicate PipelineContextData dictionary key `{key}`");
                    }
                    entries.push((key.to_owned(), value));
                }
            } else if member("d").is_some_and(|value| !value.is_null()) {
                anyhow::bail!("PipelineContextData dictionary must be an array or null");
            }
            if kind == 2 {
                ContextValue::object(entries)
                    .map_err(anyhow::Error::from)
                    .context("invalid case-insensitive PipelineContextData dictionary")
            } else {
                ContextValue::case_sensitive_object(entries)
                    .map_err(anyhow::Error::from)
                    .context("invalid case-sensitive PipelineContextData dictionary")
            }
        }
        3 => match member("b") {
            Some(Value::Bool(value)) => Ok(ContextValue::Bool(*value)),
            Some(Value::Null) | None => Ok(ContextValue::Bool(false)),
            Some(Value::Number(value)) => Ok(ContextValue::Bool(
                value
                    .as_i64()
                    .map(|value| value != 0)
                    .or_else(|| value.as_u64().map(|value| value != 0))
                    .or_else(|| value.as_f64().map(|value| value != 0.0))
                    .context("invalid PipelineContextData boolean")?,
            )),
            Some(_) => anyhow::bail!("invalid PipelineContextData boolean"),
        },
        4 => {
            let value = member("n").cloned().unwrap_or(Value::Number(0.into()));
            match value {
                Value::Number(value) => {
                    Ok(ContextValue::Number(context_data_number_to_double(&value)?))
                }
                Value::String(value) if value == "NaN" => {
                    Ok(ContextValue::non_finite(NonFinite::NaN))
                }
                Value::String(value) if value == "Infinity" => {
                    Ok(ContextValue::non_finite(NonFinite::PositiveInfinity))
                }
                Value::String(value) if value == "-Infinity" => {
                    Ok(ContextValue::non_finite(NonFinite::NegativeInfinity))
                }
                Value::String(value) if value.is_empty() => Ok(ContextValue::Null),
                Value::String(value) => {
                    let parsed = parse_clr_double_text(&value)
                        .ok_or_else(|| anyhow::anyhow!("invalid PipelineContextData number"))?;
                    let number = serde_json::Number::from_f64(parsed)
                        .map(ContextValue::Number)
                        .unwrap_or_else(|| {
                            ContextValue::non_finite(if parsed.is_nan() {
                                NonFinite::NaN
                            } else if parsed.is_sign_negative() {
                                NonFinite::NegativeInfinity
                            } else {
                                NonFinite::PositiveInfinity
                            })
                        });
                    Ok(number)
                }
                Value::Null => Ok(ContextValue::Null),
                _ => anyhow::bail!("invalid PipelineContextData number"),
            }
        }
        _ => Ok(ContextValue::Null),
    }
}

/// Materialize a typed TemplateToken into a lossless context scalar/tree.
/// Nonfinite values are recognized only under NumberToken's `num` member;
/// ordinary strings elsewhere remain strings.
pub fn template_token_context_value(value: &Value) -> Result<ContextValue> {
    let Value::Object(object) = value else {
        return if value.is_array() {
            Ok(ContextValue::Null)
        } else {
            ContextValue::from_json(value.clone()).map_err(anyhow::Error::from)
        };
    };
    let kind = template_token_type(value)?.unwrap_or(0);
    let member = |name: &str| {
        object
            .iter()
            .find(|(key, _)| clr_ordinal_ignore_case_eq(key, name))
            .map(|(_, value)| value)
    };
    match kind {
        0 => match member("lit") {
            Some(value) => match clr_string_value(value)? {
                Some(value) => Ok(ContextValue::String(value)),
                None => Ok(ContextValue::Null),
            },
            None => Ok(ContextValue::Null),
        },
        1 => {
            let values = match member("seq") {
                Some(Value::Array(values)) => values
                    .iter()
                    .map(template_token_context_value)
                    .collect::<Result<Vec<_>>>()?,
                Some(Value::Null) | None => Vec::new(),
                Some(_) => anyhow::bail!("TemplateToken sequence must be an array or null"),
            };
            Ok(ContextValue::Array(values))
        }
        2 => {
            let mut entries = Vec::new();
            if let Some(Value::Array(pairs)) = member("map") {
                for pair in pairs {
                    let pair = pair
                        .as_object()
                        .context("TemplateToken mapping contains a null or non-object pair")?;
                    let key = pair
                        .iter()
                        .find(|(name, _)| clr_ordinal_ignore_case_eq(name, "key"))
                        .map(|(_, value)| template_token_context_value(value))
                        .transpose()?
                        .context("TemplateToken mapping pair has no key")?;
                    let key = match key {
                        ContextValue::String(key) => key,
                        ContextValue::BigInteger(key) => key,
                        ContextValue::Bool(key) => key.to_string(),
                        ContextValue::Number(key) => key.to_string(),
                        ContextValue::NonFinite(NonFinite::NaN) => "NaN".to_owned(),
                        ContextValue::NonFinite(NonFinite::PositiveInfinity) => {
                            "Infinity".to_owned()
                        }
                        ContextValue::NonFinite(NonFinite::NegativeInfinity) => {
                            "-Infinity".to_owned()
                        }
                        ContextValue::Null
                        | ContextValue::Undefined
                        | ContextValue::Array(_)
                        | ContextValue::Constructor { .. }
                        | ContextValue::Object { .. } => {
                            anyhow::bail!("TemplateToken mapping key must be scalar")
                        }
                    };
                    let value = pair
                        .iter()
                        .find(|(name, _)| clr_ordinal_ignore_case_eq(name, "value"))
                        .map(|(_, value)| template_token_context_value(value))
                        .transpose()?
                        .unwrap_or(ContextValue::Null);
                    entries.push((key, value));
                }
            } else if member("map").is_some_and(|value| !value.is_null()) {
                anyhow::bail!("TemplateToken mapping must be an array or null");
            }
            ContextValue::object(entries)
                .map_err(anyhow::Error::from)
                .context("duplicate TemplateToken mapping key")
        }
        3 | 4 => match member("expr") {
            Some(value) => match clr_string_value(value)? {
                Some(value) => Ok(ContextValue::String(value)),
                None => Ok(ContextValue::Null),
            },
            None => Ok(ContextValue::Null),
        },
        5 => {
            let mut boolean = member("bool").cloned().unwrap_or(Value::Bool(false));
            clr_bool(&mut boolean);
            match boolean {
                Value::Bool(value) => Ok(ContextValue::Bool(value)),
                _ => anyhow::bail!("invalid TemplateToken boolean"),
            }
        }
        6 => {
            let mut number = member("num").cloned().unwrap_or(Value::Number(0.into()));
            if number.is_null() {
                return Ok(ContextValue::Null);
            }
            clr_double_value(&mut number)?;
            match number {
                Value::Number(value) => Ok(ContextValue::Number(value)),
                Value::String(value) if value == "NaN" => {
                    Ok(ContextValue::non_finite(NonFinite::NaN))
                }
                Value::String(value) if value == "Infinity" => {
                    Ok(ContextValue::non_finite(NonFinite::PositiveInfinity))
                }
                Value::String(value) if value == "-Infinity" => {
                    Ok(ContextValue::non_finite(NonFinite::NegativeInfinity))
                }
                Value::Null => Ok(ContextValue::Null),
                _ => anyhow::bail!("invalid TemplateToken number"),
            }
        }
        7 => Ok(ContextValue::Null),
        _ => Ok(ContextValue::Null),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TaskOrchestrationPlanReference {
    #[serde(default, rename = "ScopeIdentifier", alias = "scopeIdentifier")]
    pub scope_identifier: Option<String>,
    #[serde(default, rename = "PlanType", alias = "planType")]
    pub plan_type: Option<String>,
    #[serde(default, rename = "Version", alias = "version")]
    pub version: Option<i32>,
    #[serde(
        rename = "PlanId",
        alias = "planId",
        default = "empty_guid_string",
        deserialize_with = "deserialize_nullable_guid_string"
    )]
    pub plan_id: String,
    #[serde(default, rename = "PlanGroup", alias = "planGroup")]
    pub plan_group: Option<String>,
    #[serde(default, rename = "ArtifactUri", alias = "artifactUri")]
    pub artifact_uri: Option<String>,
    #[serde(default, rename = "ArtifactLocation", alias = "artifactLocation")]
    pub artifact_location: Option<String>,
    #[serde(default, rename = "Definition", alias = "definition")]
    pub definition: Option<Value>,
    #[serde(default, rename = "Owner", alias = "owner")]
    pub owner: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TimelineReference {
    #[serde(
        rename = "Id",
        alias = "id",
        default = "empty_guid_string",
        deserialize_with = "deserialize_nullable_guid_string"
    )]
    pub id: String,
    #[serde(default, rename = "ChangeId", alias = "changeId")]
    pub change_id: Option<i32>,
    #[serde(default, rename = "Location", alias = "location")]
    pub location: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobResources {
    #[serde(
        rename = "Endpoints",
        alias = "endpoints",
        deserialize_with = "deserialize_null_default"
    )]
    pub endpoints: Vec<ServiceEndpoint>,
    #[serde(
        rename = "Repositories",
        alias = "repositories",
        deserialize_with = "deserialize_null_default"
    )]
    pub repositories: Vec<Option<RepositoryResource>>,
    #[serde(
        rename = "Containers",
        alias = "containers",
        deserialize_with = "deserialize_null_default"
    )]
    pub containers: Vec<Option<ContainerResource>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    #[serde(default, rename = "Name", alias = "name")]
    pub name: Option<String>,
    #[serde(default, rename = "Url", alias = "url")]
    pub url: Option<String>,
    #[serde(default, rename = "Authorization", alias = "authorization")]
    pub authorization: Option<EndpointAuthorization>,
    #[serde(
        default,
        rename = "Data",
        alias = "data",
        deserialize_with = "deserialize_null_default"
    )]
    pub data: BTreeMap<String, Option<String>>,
    #[serde(default, rename = "OperationStatus", alias = "operationStatus")]
    pub operation_status: Option<ContextValue>,
}

impl ServiceEndpoint {
    pub fn data_value(&self, name: &str) -> Option<&Option<String>> {
        self.data
            .iter()
            .find(|(key, _)| clr_ordinal_ignore_case_eq(key, name))
            .map(|(_, value)| value)
    }

    pub fn data_string(&self, name: &str) -> Option<&str> {
        self.data_value(name).and_then(Option::as_deref)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointAuthorization {
    #[serde(default, rename = "Scheme", alias = "scheme")]
    pub scheme: Option<String>,
    #[serde(
        default,
        rename = "Parameters",
        alias = "parameters",
        deserialize_with = "deserialize_null_default"
    )]
    pub parameters: BTreeMap<String, Option<String>>,
}

impl EndpointAuthorization {
    pub fn parameter_value(&self, name: &str) -> Option<&Option<String>> {
        self.parameters
            .iter()
            .find(|(key, _)| clr_ordinal_ignore_case_eq(key, name))
            .map(|(_, value)| value)
    }

    pub fn parameter_string(&self, name: &str) -> Option<&str> {
        self.parameter_value(name).and_then(Option::as_deref)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryResource {
    #[serde(default, rename = "Alias", alias = "alias")]
    pub alias: Option<String>,
    #[serde(default, rename = "Endpoint", alias = "endpoint")]
    pub endpoint: Option<Value>,
    // Flat shorthand members (main accepted them alongside Properties;
    // consumers prefer the Properties bag and fall back to these).
    #[serde(default, rename = "Name", alias = "name")]
    pub name: Option<String>,
    #[serde(default, rename = "Ref", alias = "ref")]
    pub git_ref: Option<String>,
    #[serde(default, rename = "Version", alias = "version")]
    pub version: Option<String>,
    #[serde(default, rename = "Url", alias = "url")]
    pub url: Option<String>,
    #[serde(
        default = "empty_resource_properties",
        rename = "Properties",
        alias = "properties"
    )]
    pub properties: ContextValue,
}

impl RepositoryResource {
    pub fn property_value(&self, name: &str) -> Option<&ContextValue> {
        self.properties.get(name)
    }

    pub fn property_string(&self, name: &str) -> Option<String> {
        self.property_value(name)
            .and_then(|value| context_value_string(value).ok().flatten())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerResource {
    #[serde(default, rename = "Alias", alias = "alias")]
    pub alias: Option<String>,
    #[serde(default, rename = "Endpoint", alias = "endpoint")]
    pub endpoint: Option<Value>,
    #[serde(
        default = "empty_resource_properties",
        rename = "Properties",
        alias = "properties"
    )]
    pub properties: ContextValue,
}

impl ContainerResource {
    pub fn property_value(&self, name: &str) -> Option<&ContextValue> {
        self.properties.get(name)
    }

    pub fn property_string(&self, name: &str) -> Option<String> {
        self.property_value(name)
            .and_then(|value| context_value_string(value).ok().flatten())
    }

    pub fn property_string_list(&self, name: &str) -> Result<Option<Vec<Option<String>>>> {
        let Some(value) = self.property_value(name) else {
            return Ok(None);
        };
        if matches!(value, ContextValue::Null) {
            return Ok(None);
        }
        let ContextValue::Array(items) = value else {
            anyhow::bail!("container property `{name}` must be a string array or null");
        };
        items
            .iter()
            .map(context_value_string)
            .collect::<Result<Vec<_>>>()
            .map(Some)
    }

    pub fn property_string_map(
        &self,
        name: &str,
    ) -> Result<Option<BTreeMap<String, Option<String>>>> {
        let Some(value) = self.property_value(name) else {
            return Ok(None);
        };
        if matches!(value, ContextValue::Null) {
            return Ok(None);
        }
        let ContextValue::Object { entries: items, .. } = value else {
            anyhow::bail!("container property `{name}` must be a string map or null");
        };
        items
            .iter()
            .map(|(key, value)| Ok((key.clone(), context_value_string(value)?)))
            .collect::<Result<BTreeMap<_, _>>>()
            .map(Some)
    }
}

fn context_value_string(value: &ContextValue) -> Result<Option<String>> {
    match value {
        ContextValue::Null => Ok(None),
        ContextValue::Bool(value) => Ok(Some(if *value { "True" } else { "False" }.to_owned())),
        ContextValue::Number(value) => Ok(Some(dotnet_number_string(value))),
        ContextValue::BigInteger(value) => Ok(Some(value.clone())),
        ContextValue::NonFinite(value) => Ok(Some(non_finite_double_string(*value).to_owned())),
        ContextValue::String(value) => Ok(Some(value.clone())),
        ContextValue::Undefined
        | ContextValue::Array(_)
        | ContextValue::Constructor { .. }
        | ContextValue::Object { .. } => {
            anyhow::bail!("expected CLR string-compatible JToken value")
        }
    }
}

fn dotnet_number_string(value: &serde_json::Number) -> String {
    value
        .as_f64()
        .filter(|_| value.is_f64())
        .map(dotnet_double_general)
        .unwrap_or_else(|| value.to_string())
}

fn non_finite_double_string(value: NonFinite) -> &'static str {
    match value {
        NonFinite::NaN => "NaN",
        NonFinite::PositiveInfinity => "Infinity",
        NonFinite::NegativeInfinity => "-Infinity",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableValue {
    #[serde(default, rename = "Value", alias = "value")]
    pub value: Option<String>,
    #[serde(
        default,
        rename = "IsSecret",
        alias = "isSecret",
        deserialize_with = "deserialize_null_default"
    )]
    pub is_secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaskHint {
    #[serde(default, rename = "Type", alias = "type")]
    pub r#type: Option<String>,
    #[serde(default, rename = "Value", alias = "value")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionStep {
    #[serde(default, rename = "Type", alias = "type")]
    pub r#type: Option<Value>,
    #[serde(default, rename = "Id", alias = "id")]
    pub id: Option<String>,
    #[serde(skip)]
    pub kind: Option<ActionStepKind>,
    #[serde(default, rename = "Name", alias = "name")]
    pub name: Option<String>,
    #[serde(
        default,
        rename = "DisplayName",
        alias = "displayName",
        alias = "display_name"
    )]
    pub display_name: Option<String>,
    /// True only when the acquired CLR DTO populated `Action.DisplayName`.
    /// Workflow-file recovery and generated fallbacks leave this false.
    #[serde(skip)]
    pub display_name_is_explicit: bool,
    /// Template token form of the display name — the broker sends names with
    /// (or without) expressions here and leaves DisplayName null; the runner
    /// evaluates it at runtime (actions/runner GenerateDisplayName).
    #[serde(
        default,
        rename = "DisplayNameToken",
        alias = "displayNameToken",
        alias = "display_name_token"
    )]
    pub display_name_token: Option<Value>,
    #[serde(default = "default_true", rename = "Enabled", alias = "enabled")]
    pub enabled: bool,
    #[serde(default, rename = "Condition", alias = "condition")]
    pub condition: Option<String>,
    #[serde(default, rename = "ContinueOnError", alias = "continueOnError")]
    pub continue_on_error: Option<Value>,
    #[serde(default, rename = "TimeoutInMinutes", alias = "timeoutInMinutes")]
    pub timeout_in_minutes: Option<Value>,
    #[serde(
        default,
        rename = "ContextName",
        alias = "contextName",
        alias = "context_name"
    )]
    pub context_name: Option<String>,
    #[serde(default, rename = "ParallelGroupId", alias = "parallelGroupId")]
    pub parallel_group_id: Option<String>,
    #[serde(default, rename = "Background", alias = "background")]
    pub background: bool,
    #[serde(default, rename = "ControlType", alias = "controlType")]
    pub control_type: Option<String>,
    #[serde(default, rename = "StepIds", alias = "stepIds")]
    pub step_ids: Vec<Option<String>>,
    #[serde(default, rename = "Reference", alias = "reference")]
    pub reference: Option<ActionStepDefinitionReference>,
    #[serde(default, rename = "Environment", alias = "environment")]
    pub environment: Option<Value>,
    #[serde(default, rename = "Inputs", alias = "inputs")]
    pub inputs: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionStepKind {
    Action,
    BackgroundStepControl,
}

impl ActionStep {
    /// The display name template: the plain DisplayName when the server sent
    /// one (legacy/back-compat), else the DisplayNameToken rendered to a
    /// `${{ ... }}` template string for runtime evaluation.
    pub fn display_name_template(&self) -> Option<String> {
        if let Some(name) = self.display_name.as_deref().filter(|name| !name.is_empty()) {
            return Some(name.to_string());
        }
        self.display_name_token
            .as_ref()
            .and_then(display_token_template)
            .filter(|name| !name.is_empty())
    }

    pub fn reference_type(&self) -> Option<ActionReferenceType> {
        self.reference
            .as_ref()
            .and_then(|reference| reference.r#type)
    }

    pub fn step_kind(&self) -> Option<ActionStepKind> {
        // A step without an explicit discriminator is an ordinary action
        // (main had no kind gate at all). The `kind` field keeps the wire
        // truth; behavior defaults here so direct deserializations and
        // reference-bearing shorthand steps all execute as actions.
        Some(self.kind.unwrap_or(ActionStepKind::Action))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionStepDefinitionReference {
    #[serde(
        default,
        rename = "Type",
        alias = "type",
        deserialize_with = "deserialize_action_reference_type"
    )]
    pub r#type: Option<ActionReferenceType>,
    #[serde(default, rename = "Name", alias = "name")]
    pub name: Option<String>,
    #[serde(default, rename = "Ref", alias = "ref")]
    pub git_ref: Option<String>,
    #[serde(default, rename = "RepositoryType", alias = "repositoryType")]
    pub repository_type: Option<String>,
    #[serde(default, rename = "Path", alias = "path")]
    pub path: Option<String>,
    #[serde(default, rename = "Image", alias = "image")]
    pub image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ActionReferenceType {
    Repository,
    ContainerRegistry,
    Script,
}

/// Render a scalar template token to display text: literals verbatim,
/// expression tokens back to `${{ ... }}` so the executor's resolver
/// evaluates them with the job contexts.
fn display_token_template(token: &Value) -> Option<String> {
    match token {
        Value::String(value) => Some(value.clone()),
        Value::Object(object) => {
            if let Some(expr) = object
                .get("expr")
                .or_else(|| object.get("Expr"))
                .and_then(Value::as_str)
            {
                return Some(format!("${{{{ {expr} }}}}"));
            }
            object
                .get("lit")
                .or_else(|| object.get("Lit"))
                .or_else(|| object.get("value"))
                .or_else(|| object.get("Value"))
                .and_then(display_token_template)
        }
        _ => None,
    }
}

fn default_true() -> bool {
    true
}

fn deserialize_action_reference_type<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ActionReferenceType>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };

    match value {
        Value::Number(number) => match number.as_i64() {
            Some(1) => Ok(Some(ActionReferenceType::Repository)),
            Some(2) => Ok(Some(ActionReferenceType::ContainerRegistry)),
            Some(3) => Ok(Some(ActionReferenceType::Script)),
            _ => Ok(None),
        },
        Value::String(value) => {
            let value = value.as_str();
            if clr_ordinal_ignore_case_eq(value, "repository") {
                Ok(Some(ActionReferenceType::Repository))
            } else if clr_ordinal_ignore_case_eq(value, "containerregistry")
                || clr_ordinal_ignore_case_eq(value, "container_registry")
                || clr_ordinal_ignore_case_eq(value, "container-registry")
            {
                Ok(Some(ActionReferenceType::ContainerRegistry))
            } else if clr_ordinal_ignore_case_eq(value, "script") {
                Ok(Some(ActionReferenceType::Script))
            } else {
                match value.trim().parse::<i32>() {
                    Ok(1) => Ok(Some(ActionReferenceType::Repository)),
                    Ok(2) => Ok(Some(ActionReferenceType::ContainerRegistry)),
                    Ok(3) => Ok(Some(ActionReferenceType::Script)),
                    _ => Ok(None),
                }
            }
        }
        _ => Ok(None),
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
    fn converter_collection_case_aliases_append_and_null_resets_backing_lists() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{
                "JobContainer":{"type":1,"seq":[{"type":0,"lit":"a"}],"Seq":[{"type":0,"lit":"b"}]},
                "JobServiceContainers":{"type":2,"map":[{"key":{"type":0,"lit":"ka"},"value":{"type":0,"lit":"va"}}],"Map":[{"key":{"type":0,"lit":"kb"},"value":{"type":0,"lit":"vb"}}]},
                "ContextData":{
                    "array":{"t":1,"a":[{"t":0,"s":"a"}],"A":[{"t":0,"s":"b"}]},
                    "dictionary":{"t":2,"d":[{"k":"a","v":{"t":0,"s":"a"}}],"D":[{"k":"b","v":{"t":0,"s":"b"}}]},
                    "caseSensitive":{"t":5,"d":[{"k":"a","v":{"t":0,"s":"a"}}],"D":[{"k":"b","v":{"t":0,"s":"b"}}]},
                    "reset":{"t":1,"a":[{"t":0,"s":"discard"}],"A":null},
                    "duplicate":{"t":1,"a":[{"t":0,"s":"discard"}],"a":[{"t":0,"s":"kept"}],"A":[{"t":0,"s":"appended"}]}
                }
            }"#,
        )
        .unwrap();

        let sequence = message.job_container.unwrap();
        assert_eq!(sequence["seq"][0]["lit"], "a");
        assert_eq!(sequence["seq"][1]["lit"], "b");
        let mapping = message.job_service_containers.unwrap();
        assert_eq!(mapping["map"][0]["value"]["lit"], "va");
        assert_eq!(mapping["map"][1]["value"]["lit"], "vb");
        let context = message.context_data.unwrap();
        assert_eq!(context["array"]["a"][0]["s"], "a");
        assert_eq!(context["array"]["a"][1]["s"], "b");
        assert_eq!(context["dictionary"]["d"].as_array().unwrap().len(), 2);
        assert_eq!(context["caseSensitive"]["d"].as_array().unwrap().len(), 2);
        assert!(
            context["reset"]["a"].is_null(),
            "a later null alias resets the list"
        );
        assert_eq!(context["duplicate"]["a"][0]["s"], "kept");
        assert_eq!(context["duplicate"]["a"][1]["s"], "appended");
        assert_eq!(context["duplicate"]["a"].as_array().unwrap().len(), 2);

        // The Value-based API has already lost input member ordering. It still
        // combines both case aliases instead of dropping one collection.
        let mut token = serde_json::json!({
            "type": 1,
            "seq": [{"type": 0, "lit": "a"}],
            "Seq": [{"type": 0, "lit": "b"}]
        });
        normalize_template_token(&mut token).unwrap();
        assert_eq!(token["seq"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn display_name_template_prefers_plain_then_token() {
        // Broker reality (jackin-agent-brown dump 2026-06-11): DisplayName is
        // null and the explicit `name:` arrives only as DisplayNameToken.
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "id": "s1",
            "name": "__docker_login-action",
            "displayName": null,
            "displayNameToken": { "type": 0, "lit": "Login to Docker Hub for base image pulls" },
            "reference": { "type": "Repository", "name": "docker/login-action" }
        }))
        .unwrap();
        assert_eq!(
            step.display_name_template().as_deref(),
            Some("Login to Docker Hub for base image pulls")
        );

        // Expression tokens render back to ${{ }} for runtime evaluation.
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "id": "s2",
            "name": "__run",
            "displayNameToken": { "type": 3, "expr": "format('Deploy {0}', inputs.env)" },
            "reference": { "type": "Script" }
        }))
        .unwrap();
        assert_eq!(
            step.display_name_template().as_deref(),
            Some("${{ format('Deploy {0}', inputs.env) }}")
        );

        // Plain DisplayName (legacy servers) wins over the token.
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "id": "s3",
            "displayName": "Plain name",
            "displayNameToken": { "type": 0, "lit": "Token name" },
            "reference": { "type": "Script" }
        }))
        .unwrap();
        assert_eq!(step.display_name_template().as_deref(), Some("Plain name"));

        // A nonempty prepopulated Action.DisplayName is authoritative, even
        // when its text resembles an internal placeholder.
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "id": "s4",
            "displayName": "__run_2",
            "reference": { "type": "Script" }
        }))
        .unwrap();
        assert_eq!(step.display_name_template().as_deref(), Some("__run_2"));
    }

    #[test]
    fn explicit_display_name_provenance_survives_wire_materialization_and_recovery_stays_generated()
    {
        let wire = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Steps": [{
                "Type": 4,
                "Id": STEP_ID,
                "DisplayName": "${{ secrets.token }}"
            }]
        }))
        .unwrap();
        let explicit = wire.steps[0].as_ref().unwrap();
        assert!(explicit.display_name_is_explicit);
        assert_eq!(
            explicit.display_name_template().as_deref(),
            Some("${{ secrets.token }}")
        );

        let mut recovered = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Steps": [{ "Type": 4, "Id": STEP_ID }]
        }))
        .unwrap();
        let recovered_step = recovered.steps[0].as_mut().unwrap();
        recovered_step.display_name = Some("${{ secrets.token }}".to_owned());
        assert!(!recovered_step.display_name_is_explicit);
    }

    const JOB_ID: &str = "11111111-1111-1111-1111-111111111111";
    const PLAN_ID: &str = "22222222-2222-2222-2222-222222222222";
    const TIMELINE_ID: &str = "33333333-3333-3333-3333-333333333333";
    const STEP_ID: &str = "44444444-4444-4444-4444-444444444444";

    #[test]
    fn resource_property_double_strings_match_invariant_dotnet_general_format() {
        for (value, expected) in [
            (1.0, "1"),
            (100.0, "100"),
            (1e-4, "0.0001"),
            (1e-5, "1E-05"),
            (1e-7, "1E-07"),
            (1e16, "10000000000000000"),
            (1e17, "1E+17"),
            (-0.0, "-0"),
            (0.0, "0"),
        ] {
            assert_eq!(dotnet_double_general(value), expected, "{value:?}");
        }
    }

    #[test]
    fn ordered_resource_property_numbers_keep_newtonsoft_integer_and_double_types() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{"Resources":{"Containers":[{"Properties":{
                "integer":1,
                "floating":1.0,
                "exponent":1e2,
                "negativeZeroInteger":-0,
                "negativeZeroFloat":-0.0,
                "smallExponent":1e-7,
                "overflowFloat":1e309,
                "signedLargeInteger":9223372036854775808,
                "bigInteger":123456789012345678901234567890,
                "string":"1.0",
                "boolean":true,
                "null":null
            }}]}}"#,
        )
        .unwrap();
        let container = message.resources.as_ref().unwrap().containers[0]
            .as_ref()
            .unwrap();

        for (name, expected) in [
            ("integer", Some("1")),
            ("floating", Some("1")),
            ("exponent", Some("100")),
            ("negativeZeroInteger", Some("0")),
            ("negativeZeroFloat", Some("-0")),
            ("smallExponent", Some("1E-07")),
            ("overflowFloat", Some("Infinity")),
            ("signedLargeInteger", Some("9223372036854775808")),
            ("bigInteger", Some("123456789012345678901234567890")),
            ("string", Some("1.0")),
            ("boolean", Some("True")),
            ("null", None),
        ] {
            assert_eq!(
                container.property_string(name).as_deref(),
                expected,
                "{name}"
            );
        }
        assert!(matches!(
            container.property_value("negativeZeroFloat"),
            Some(ContextValue::Number(value))
                if value.is_f64() && value.as_f64().is_some_and(|value| value.is_sign_negative())
        ));
        assert!(matches!(
            container.property_value("signedLargeInteger"),
            Some(ContextValue::BigInteger(value)) if value == "9223372036854775808"
        ));
        assert!(matches!(
            container.property_value("bigInteger"),
            Some(ContextValue::BigInteger(value)) if value == "123456789012345678901234567890"
        ));
        assert!(matches!(
            container.property_value("overflowFloat"),
            Some(ContextValue::NonFinite(NonFinite::PositiveInfinity))
        ));
    }

    #[test]
    fn direct_text_reader_strings_keep_number_lexemes_while_jtoken_strings_format_values() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{
                "MessageType":-0.0,
                "JobDisplayName":1.0,
                "JobName":1e2,
                "BillingOwnerId":1e-7,
                "Resources":{"Containers":[{"Properties":{
                    "integerFloat":1.0,
                    "exponent":1e2,
                    "negativeZeroFloat":-0.0,
                    "smallExponent":1e-7
                }}]}
            }"#,
        )
        .unwrap();

        assert_eq!(message.message_type.as_deref(), Some("-0.0"));
        assert_eq!(message.job_display_name.as_deref(), Some("1.0"));
        assert_eq!(message.job_name.as_deref(), Some("1e2"));
        assert_eq!(message.billing_owner_id.as_deref(), Some("1e-7"));
        let container = message.resources.as_ref().unwrap().containers[0]
            .as_ref()
            .unwrap();
        for (name, expected) in [
            ("integerFloat", "1"),
            ("exponent", "100"),
            ("negativeZeroFloat", "-0"),
            ("smallExponent", "1E-07"),
        ] {
            assert_eq!(container.property_string(name).as_deref(), Some(expected));
        }
    }

    #[test]
    fn text_reader_preserves_nonstandard_and_nonfinite_number_lexemes() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{
                "MessageType":0x10,
                "JobDisplayName":0XFF,
                "JobName":010,
                "BillingOwnerId":01,
                "QueueTime":1e309,
                "LockedUntil":-1e309,
                "Workspace":{"Clean":1e-5000},
                "Resources":{"Containers":[{"Properties":{
                    "hex":0x10,
                    "octal":010,
                    "one":01,
                    "trailingDecimal":1.,
                    "leadingDecimal":.5,
                    "positiveOverflow":1e309,
                    "negativeOverflow":-1e309,
                    "underflow":1e-5000,
                    "negativeUnderflow":-1e-5000
                }}]}
            }"#,
        )
        .unwrap();

        assert_eq!(message.message_type.as_deref(), Some("0x10"));
        assert_eq!(message.job_display_name.as_deref(), Some("0XFF"));
        assert_eq!(message.job_name.as_deref(), Some("010"));
        assert_eq!(message.billing_owner_id.as_deref(), Some("01"));
        assert_eq!(message.queue_time.as_deref(), Some("1e309"));
        assert_eq!(message.locked_until.as_deref(), Some("-1e309"));
        assert_eq!(
            message
                .workspace
                .as_ref()
                .and_then(|value| value.get("Clean"))
                .and_then(Value::as_str),
            Some("1e-5000")
        );

        let container = message.resources.as_ref().unwrap().containers[0]
            .as_ref()
            .unwrap();
        for (name, expected) in [
            ("hex", "16"),
            ("octal", "8"),
            ("one", "1"),
            ("trailingDecimal", "1"),
            ("leadingDecimal", "0.5"),
            ("positiveOverflow", "Infinity"),
            ("negativeOverflow", "-Infinity"),
            ("underflow", "0"),
            ("negativeUnderflow", "-0"),
        ] {
            assert_eq!(
                container.property_string(name).as_deref(),
                Some(expected),
                "{name}"
            );
        }
    }

    #[test]
    fn template_token_integer_literals_follow_int64_to_double_conversion() {
        let bare =
            WireAgentJobRequestMessage::parse_json(r#"{"JobContainer":9007199254740993}"#).unwrap();
        assert!(matches!(
            bare.job_container.as_ref(),
            Some(Value::Number(number))
                if number.is_f64() && number.as_f64() == Some(9_007_199_254_740_992.0)
        ));

        let number_token = WireAgentJobRequestMessage::parse_json(
            r#"{"JobContainer":{"type":6,"num":9007199254740993}}"#,
        )
        .unwrap();
        assert!(matches!(
            number_token.job_container.as_ref(),
            Some(Value::Object(object))
                if object.get("num").and_then(Value::as_f64) == Some(9_007_199_254_740_992.0)
        ));

        let bigint_number_token = WireAgentJobRequestMessage::parse_json(
            r#"{"JobContainer":{"type":6,"num":9007199254740995}}"#,
        )
        .unwrap();
        assert!(matches!(
            bigint_number_token.job_container.as_ref(),
            Some(Value::Object(object))
                if object.get("num").and_then(Value::as_f64) == Some(9_007_199_254_740_994.0)
        ));

        let normalized = AgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": 9007199254740993_u64
        }))
        .unwrap();
        assert!(matches!(
            normalized.job_container.as_ref(),
            Some(Value::Number(number))
                if number.is_f64() && number.as_f64() == Some(9_007_199_254_740_992.0)
        ));

        for integer in [i64::MIN, i64::MAX] {
            let body = format!(r#"{{"JobContainer":{integer}}}"#);
            let message = WireAgentJobRequestMessage::parse_json(&body).unwrap();
            assert!(matches!(
                message.job_container.as_ref(),
                Some(Value::Number(number)) if number.is_f64()
            ));
        }

        for integer in ["9223372036854775808", "-9223372036854775809"] {
            let body = format!(r#"{{"JobContainer":{integer}}}"#);
            assert!(WireAgentJobRequestMessage::parse_json(&body).is_err());
        }

        let overflow = WireAgentJobRequestMessage::parse_json(r#"{"JobContainer":1e309}"#).unwrap();
        assert!(matches!(
            template_token_context_value(overflow.job_container.as_ref().unwrap()).unwrap(),
            ContextValue::NonFinite(NonFinite::PositiveInfinity)
        ));

        let context = WireAgentJobRequestMessage::parse_json(
            r#"{"ContextData":{"number":{"t":4,"n":9223372036854775808}}}"#,
        )
        .unwrap();
        let number = context
            .context_data
            .as_ref()
            .unwrap()
            .get("number")
            .unwrap();
        assert!(matches!(
            pipeline_context_context_value(number).unwrap(),
            ContextValue::Number(value) if value.is_f64() && value.as_f64().is_some()
        ));

        let bigint_context = WireAgentJobRequestMessage::parse_json(
            r#"{"ContextData":{"number":{"t":4,"n":9223372036854776833}}}"#,
        )
        .unwrap();
        let number = bigint_context
            .context_data
            .as_ref()
            .unwrap()
            .get("number")
            .unwrap();
        assert!(matches!(
            pipeline_context_context_value(number).unwrap(),
            ContextValue::Number(value)
                if value.as_f64() == Some(9_223_372_036_854_775_808.0)
        ));

        let overflow_context = WireAgentJobRequestMessage::parse_json(
            r#"{"ContextData":{"number":{"t":4,"n":1e309}}}"#,
        )
        .unwrap();
        let number = overflow_context
            .context_data
            .as_ref()
            .unwrap()
            .get("number")
            .unwrap();
        assert!(matches!(
            pipeline_context_context_value(number).unwrap(),
            ContextValue::NonFinite(NonFinite::PositiveInfinity)
        ));
    }

    #[test]
    fn pipeline_context_root_scalars_match_converter_projection() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{"ContextData":{
                "integer":9007199254740993,
                "float":1.25,
                "overflow":1e309,
                "array":[],
                "null":null,
                "boolean":true,
                "string":"value"
            }}"#,
        )
        .unwrap();
        let values = message.materialize_context_values().unwrap();
        assert!(matches!(
            values.get("integer"),
            Some(ContextValue::Number(number))
                if number.is_f64() && number.as_f64() == Some(9_007_199_254_740_992.0)
        ));
        assert!(matches!(
            values.get("float"),
            Some(ContextValue::Number(number))
                if number.is_f64() && number.as_f64() == Some(1.25)
        ));
        assert!(matches!(
            values.get("overflow"),
            Some(ContextValue::NonFinite(NonFinite::PositiveInfinity))
        ));
        assert!(matches!(values.get("array"), Some(ContextValue::Null)));
        assert!(matches!(values.get("null"), Some(ContextValue::Null)));
        assert!(matches!(
            values.get("boolean"),
            Some(ContextValue::Bool(true))
        ));
        assert!(matches!(
            values.get("string"),
            Some(ContextValue::String(value)) if value == "value"
        ));

        let direct = AgentJobRequestMessage::from_value(serde_json::json!({
            "ContextData": {
                "integer": 9007199254740993_u64,
                "array": []
            }
        }))
        .unwrap();
        let values = direct.materialize_context_values().unwrap();
        assert!(matches!(
            values.get("integer"),
            Some(ContextValue::Number(number))
                if number.is_f64() && number.as_f64() == Some(9_007_199_254_740_992.0)
        ));
        assert!(matches!(values.get("array"), Some(ContextValue::Null)));

        let normalized = WireAgentJobRequestMessage::from_normalized_value(serde_json::json!({
            "ContextData": {
                "integer": 9007199254740993_u64,
                "array": []
            }
        }))
        .unwrap();
        let values = normalized.materialize_context_values().unwrap();
        assert!(matches!(
            values.get("integer"),
            Some(ContextValue::Number(number))
                if number.is_f64() && number.as_f64() == Some(9_007_199_254_740_992.0)
        ));
        assert!(matches!(values.get("array"), Some(ContextValue::Null)));

        for integer in ["9223372036854775808", "-9223372036854775809"] {
            let body = format!(r#"{{"ContextData":{{"integer":{integer}}}}}"#);
            assert!(WireAgentJobRequestMessage::parse_json(&body).is_err());
        }
        let overflow_normalized =
            WireAgentJobRequestMessage::from_normalized_value(serde_json::json!({
                "ContextData": { "integer": 9223372036854775808_u64 }
            }))
            .unwrap();
        assert!(overflow_normalized.materialize_context_values().is_err());
    }

    #[test]
    fn typed_double_fields_reject_boolean_null_and_invalid_strings() {
        for body in [
            r#"{"JobContainer":{"type":6,"num":true}}"#,
            r#"{"JobContainer":{"type":6,"num":null}}"#,
            r#"{"JobContainer":{"type":6,"num":"inf"}}"#,
            r#"{"ContextData":{"number":{"t":4,"n":true}}}"#,
            r#"{"ContextData":{"number":{"t":4,"n":null}}}"#,
            r#"{"ContextData":{"number":{"t":4,"n":"inf"}}}"#,
        ] {
            assert!(
                WireAgentJobRequestMessage::parse_json(body).is_err(),
                "{body}"
            );
        }

        for (value, expected) in [
            ("Infinity", f64::INFINITY),
            ("+Infinity", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("infinity", f64::INFINITY),
            ("NaN", f64::NAN),
            ("+NaN", f64::NAN),
            ("-NaN", f64::NAN),
            ("1,", 1.0),
            ("1,,2", 12.0),
            ("1,.2", 1.2),
            ("1,2.3", 12.3),
            ("1,2e3", 12000.0),
        ] {
            let value = parse_clr_double_text(value).unwrap();
            assert!(
                (value.is_nan() && expected.is_nan()) || value == expected,
                "{value:?} != {expected:?}"
            );
        }
        for (value, expected) in [
            ("\u{00a0}Infinity\u{00a0}", f64::INFINITY),
            ("\u{202f}-Infinity\u{202f}", f64::NEG_INFINITY),
            ("\u{00a0}NaN\u{00a0}", f64::NAN),
            ("\u{202f}+NaN\u{202f}", f64::NAN),
        ] {
            let value = parse_clr_double_text(value).unwrap();
            assert!(
                (value.is_nan() && expected.is_nan()) || value == expected,
                "{value:?} != {expected:?}"
            );
        }
        for value in [
            "inf",
            "-inf",
            ",1",
            "1.2,3",
            "1e1,2",
            "1e,3",
            "1.,2",
            "\u{00a0}1.5\u{00a0}",
            "\u{202f}1.5\u{202f}",
        ] {
            assert!(parse_clr_double_text(value).is_none(), "{value:?}");
        }
        assert_eq!(parse_clr_double_text(""), None);
    }

    #[test]
    fn undefined_field_nulls_differ_from_typed_array_holes() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{
                "JobContainer":undefined,
                "JobServiceContainers":undefined,
                "dependencies":undefined,
                "Steps":undefined,
                "Resources":{"Endpoints":undefined},
                "ContextData":{
                    "undefined":undefined,
                    "array":{"t":1,"a":[,]}
                }
            }"#,
        )
        .unwrap();
        assert!(wire.job_container.is_none());
        assert!(wire.job_service_containers.is_none());
        assert!(wire.actions_dependencies.is_empty());
        assert!(wire.steps.is_empty());
        assert!(wire.resources.as_ref().unwrap().endpoints.is_empty());
        assert!(matches!(
            wire.materialize_context_values().unwrap().get("undefined"),
            Some(ContextValue::Null)
        ));
        assert!(matches!(
            wire.materialize_context_values().unwrap().get("array"),
            Some(ContextValue::Array(values)) if matches!(values.as_slice(), [ContextValue::Null])
        ));

        // TemplateToken elements are object references, so an array hole
        // materializes as null. String[] elements use ReadAsString and reject
        // an undefined token.
        let object_array =
            WireAgentJobRequestMessage::parse_json(r#"{"JobContainer":{"type":1,"seq":[,]}}"#)
                .unwrap();
        assert_eq!(
            object_array.job_container.as_ref().unwrap()["seq"][0],
            Value::Null
        );
        for body in [
            r#"{"dependencies":[undefined]}"#,
            r#"{"dependencies":["first",,]}"#,
            r#"{"JobName":undefined}"#,
            r#"{"JobName":new Foo(0)}"#,
            r#"{"JobContainer":{"type":6,"num":undefined}}"#,
            r#"{"JobContainer":{"type":6,"num":new Foo(0)}}"#,
            r#"{"ContextData":{"number":{"t":4,"n":undefined}}}"#,
            r#"{"ContextData":{"number":{"t":4,"n":new Foo(0)}}}"#,
            r#"{"ContextData":{"boolean":{"t":3,"b":undefined}}}"#,
        ] {
            assert!(
                WireAgentJobRequestMessage::parse_json(body).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn typed_double_empty_strings_project_to_null() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{"JobContainer":{"type":6,"num":""},"ContextData":{"number":{"t":4,"n":""}}}"#,
        )
        .unwrap();
        assert!(wire.job_container.as_ref().unwrap()["num"].is_null());
        assert!(wire.context_data.as_ref().unwrap()["number"]["n"].is_null());
        assert!(matches!(
            wire.materialize_context_values().unwrap().get("number"),
            Some(ContextValue::Null)
        ));
        assert!(matches!(
            template_token_context_value(wire.job_container.as_ref().unwrap()).unwrap(),
            ContextValue::Null
        ));

        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": { "type": 6, "num": "" },
            "ContextData": { "number": { "t": 4, "n": "" } }
        }))
        .unwrap();
        assert!(matches!(
            message.materialize_context_values().unwrap().get("number"),
            Some(ContextValue::Null)
        ));
        assert!(matches!(
            template_token_context_value(message.job_container.as_ref().unwrap()).unwrap(),
            ContextValue::Null
        ));
    }

    #[test]
    fn uri_fields_are_nullable_and_validated_at_wire_ingress() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{
                "Plan":{"ArtifactUri":undefined,"ArtifactLocation":""},
                "Timeline":{"Location":"/timeline/1"},
                "Resources":{"Endpoints":[{"Url":"https://example.invalid/service"}]}
            }"#,
        )
        .unwrap();
        let plan = wire.plan.as_ref().unwrap();
        assert!(plan.artifact_uri.is_none());
        assert!(plan.artifact_location.is_none());
        assert_eq!(
            wire.timeline.as_ref().unwrap().location.as_deref(),
            Some("/timeline/1")
        );
        assert_eq!(
            wire.resources.as_ref().unwrap().endpoints[0]
                .as_ref()
                .unwrap()
                .url
                .as_deref(),
            Some("https://example.invalid/service")
        );

        let empty_wire = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Plan": { "ArtifactUri": "", "ArtifactLocation": "" },
            "Timeline": { "Location": "" },
            "Resources": { "Endpoints": [{ "Url": "" }] }
        }))
        .unwrap();
        assert!(empty_wire.plan.unwrap().artifact_uri.is_none());
        assert!(empty_wire.timeline.unwrap().location.is_none());
        assert!(empty_wire.resources.unwrap().endpoints[0]
            .as_ref()
            .unwrap()
            .url
            .is_none());

        for body in [
            r#"{"Plan":{"ArtifactUri":"http://["}}"#,
            r#"{"Plan":{"ArtifactLocation":"http://["}}"#,
            r#"{"Timeline":{"Location":"http://["}}"#,
            r#"{"Resources":{"Endpoints":[{"Url":"http://["}]}}"#,
            r#"{"Plan":{"ArtifactUri":42}}"#,
            r#"{"Plan":{"ArtifactLocation":42}}"#,
            r#"{"Timeline":{"Location":42}}"#,
            r#"{"Resources":{"Endpoints":[{"Url":42}]}}"#,
        ] {
            assert!(
                WireAgentJobRequestMessage::parse_json(body).is_err(),
                "invalid CLR Uri value accepted: {body}"
            );
        }

        for value in [
            serde_json::json!({ "Plan": { "ArtifactUri": "http://[" } }),
            serde_json::json!({ "Plan": { "ArtifactLocation": 42 } }),
            serde_json::json!({ "Timeline": { "Location": "http://[" } }),
            serde_json::json!({ "Resources": { "Endpoints": [{ "Url": 42 }] } }),
        ] {
            assert!(AgentJobRequestMessage::from_value(value).is_err());
        }
    }

    #[test]
    fn constructors_fail_at_known_context_dictionary_values_but_unknown_fields_skip() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{
                "Unknown":new Foo(0),
                "Plan":{"PlanGroup":"known","Unknown":new Foo(0)}
            }"#,
        )
        .unwrap();
        assert_eq!(
            wire.plan.as_ref().unwrap().plan_group.as_deref(),
            Some("known")
        );

        for body in [
            r#"{"ContextData":{"known":new Foo(0)}}"#,
            r#"{"ContextData":{"dictionary":{"t":2,"d":[{"k":"known","v":new Foo(0)}]}}}"#,
        ] {
            assert!(
                WireAgentJobRequestMessage::parse_json(body).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn ordered_clr_objects_skip_unknown_big_integer_and_nonfinite_subtrees() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{
                "UnknownBigInteger":9223372036854775808,
                "UnknownNonFinite":1e309,
                "Plan": {
                    "PlanGroup":"known",
                    "UnknownBigInteger":123456789012345678901234567890,
                    "UnknownNonFinite":-1e309
                }
            }"#,
        )
        .unwrap();
        assert_eq!(
            message.plan.as_ref().unwrap().plan_group.as_deref(),
            Some("known")
        );
    }

    #[test]
    fn from_value_matches_clr_case_insensitive_nested_member_lookup() {
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "mEsSaGeTyPe": "PipelineAgentJobRequest",
            "jObId": JOB_ID,
            "pLaN": { "pLaNiD": PLAN_ID },
            "tImElInE": { "iD": TIMELINE_ID },
            "vArIaBlEs": {
                "system.github.token": { "vAlUe": "secret", "iSsEcReT": true }
            },
            "sTePs": [{
                "tYpE": 4,
                "iD": STEP_ID,
                "rEfErEnCe": { "tYpE": "sCrIpT" }
            }]
        }))
        .unwrap();

        assert_eq!(message.message_type, PIPELINE_AGENT_JOB_REQUEST);
        assert_eq!(message.job_id, JOB_ID);
        assert_eq!(message.plan.plan_id, PLAN_ID);
        assert_eq!(message.timeline.id, TIMELINE_ID);
        assert!(message.variables["system.github.token"].is_secret);
        assert_eq!(
            message.steps[0].as_ref().unwrap().id.as_deref(),
            Some(STEP_ID)
        );
        assert_eq!(
            message.steps[0].as_ref().unwrap().reference_type(),
            Some(ActionReferenceType::Script)
        );
    }

    #[test]
    fn from_value_coerces_step_enabled_string_booleans() {
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "Steps": [
                { "Type": "Action", "Enabled": " TRUE " },
                { "Type": "4", "Enabled": "false" }
            ]
        }))
        .unwrap();

        assert!(message.steps[0].as_ref().unwrap().enabled);
        assert!(!message.steps[1].as_ref().unwrap().enabled);
    }

    #[test]
    fn from_value_retains_null_step_slots() {
        let message = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Steps": [null, { "Type": "Action" }]
        }))
        .unwrap();

        assert_eq!(message.steps.len(), 2);
        assert!(message.steps[0].is_none());
        assert!(message.steps[1].is_some());
    }

    #[test]
    fn wire_dto_preserves_nullable_reference_properties_until_runtime_admission() {
        for (name, value) in [
            ("Plan", serde_json::json!({ "Plan": null })),
            ("Timeline", serde_json::json!({ "Timeline": null })),
            ("Resources", serde_json::json!({ "Resources": null })),
        ] {
            let wire = WireAgentJobRequestMessage::from_value(value).unwrap();
            match name {
                "Plan" => assert!(wire.plan.is_none()),
                "Timeline" => assert!(wire.timeline.is_none()),
                "Resources" => assert!(wire.resources.is_none()),
                _ => unreachable!(),
            }
            assert!(
                wire.materialize_runtime().is_err(),
                "a null {name} must fail local admission instead of becoming a default object"
            );
        }
    }

    #[test]
    fn wire_null_collection_slots_survive_until_stable_runtime_materialization() {
        let wire = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Plan": {},
            "Timeline": {},
            "Variables": { "nullable": null },
            "Mask": [null],
            "Resources": {
                "Endpoints": [null],
                "Repositories": [null],
                "Containers": [null]
            },
            "Steps": [null, { "Type": "Action" }]
        }))
        .unwrap();

        assert!(wire.variables["nullable"].is_none());
        assert!(wire.mask[0].is_none());
        assert!(wire.resources.as_ref().unwrap().endpoints[0].is_none());
        assert!(wire.resources.as_ref().unwrap().repositories[0].is_none());
        assert!(wire.resources.as_ref().unwrap().containers[0].is_none());
        assert!(wire.steps[0].is_none());
        // Variables, masks, and endpoints have unconditional runtime
        // consumers, so null entries fail admission. Container/repository
        // slots remain conditional and are covered below.
        for value in [
            serde_json::json!({ "Variables": { "nullable": null } }),
            serde_json::json!({ "Mask": [null] }),
            serde_json::json!({ "Resources": { "Endpoints": [null] } }),
        ] {
            assert!(WireAgentJobRequestMessage::from_value(value)
                .unwrap()
                .materialize_runtime()
                .is_err());
        }

        let wire = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Plan": {},
            "Timeline": {},
            "Resources": {
                "Repositories": [null],
                "Containers": [null]
            }
        }))
        .unwrap();
        let runtime = wire.materialize_runtime().unwrap();
        assert!(runtime.resources.repositories[0].is_none());
        assert!(runtime.resources.containers[0].is_none());

        let wire = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Plan": {},
            "Timeline": {},
            "Resources": {},
            "Steps": [null, { "Type": "Action" }]
        }))
        .unwrap();
        let runtime = wire.materialize_runtime().unwrap();
        assert_eq!(runtime.steps.len(), 2);
        assert!(runtime.steps[0].is_none());
        assert!(runtime.steps[1].is_some());
    }

    #[test]
    fn wire_null_lazy_collections_default_but_non_lazy_maps_and_tokens_preserve_nulls() {
        let wire = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Plan": {},
            "Timeline": {},
            "Resources": {
                "Endpoints": null,
                "Repositories": null,
                "Containers": null
            },
            "Variables": null,
            "Mask": null,
            "Steps": null,
            "EnvironmentVariables": [null],
            "Defaults": null,
            "ContextData": null,
            "JobSidecarContainers": null,
            "JobServiceContainers": { "type": 1, "seq": [] }
        }))
        .unwrap();

        assert!(wire.variables.is_empty());
        assert!(wire.mask.is_empty());
        assert!(wire.steps.is_empty());
        assert!(wire.environment_variables[0].is_null());
        assert!(wire.defaults.is_empty());
        assert!(wire.context_data.is_none());
        assert!(wire.job_sidecar_containers.is_none());
        let resources = wire.resources.as_ref().unwrap();
        assert!(resources.endpoints.is_empty());
        assert!(resources.repositories.is_empty());
        assert!(resources.containers.is_empty());
    }

    #[test]
    fn container_callbacks_match_upstream_selection_behavior() {
        // No callback runs for absent, null, or non-string-token job
        // containers, so unrelated null container slots are not visited.
        for value in [
            serde_json::json!({ "Resources": { "Containers": [null] } }),
            serde_json::json!({
                "JobContainer": null,
                "Resources": { "Containers": [null] }
            }),
            serde_json::json!({
                "JobContainer": { "type": 1, "seq": [] },
                "Resources": { "Containers": [null] }
            }),
        ] {
            let wire = WireAgentJobRequestMessage::from_value(value).unwrap();
            let runtime = wire.materialize_runtime().unwrap();
            assert!(runtime.resources.containers[0].is_none());
        }

        // A StringToken activates SingleOrDefault: every visited slot must be
        // non-null, and duplicate aliases are invalid.
        assert!(WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": { "type": 0, "lit": "main" },
            "Resources": { "Containers": [null] }
        }))
        .is_err());
        assert!(WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": "main",
            "Resources": { "Containers": [
                { "Alias": "main" },
                { "Alias": "MAIN" }
            ] }
        }))
        .is_err());

        let resolved = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": "main",
            "JobSidecarContainers": { "network": "sidecar" },
            "Resources": { "Containers": [
                { "Alias": "main", "Properties": { "image": "alpine" } },
                { "Alias": "sidecar", "Properties": { "image": "redis" } }
            ] }
        }))
        .unwrap();
        assert_eq!(
            resolved.job_container_resource_alias,
            Some("main".to_owned())
        );
        assert_eq!(
            template_token_type(resolved.job_container.as_ref().unwrap()).unwrap(),
            Some(2)
        );
        assert_eq!(
            template_token_type(resolved.job_service_containers.as_ref().unwrap()).unwrap(),
            Some(2)
        );

        let explicit_token = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobServiceContainers": { "type": 0, "lit": null },
            "JobSidecarContainers": { "network": "sidecar" },
            "Resources": { "Containers": [{ "Alias": "sidecar" }] }
        }))
        .unwrap();
        let services = explicit_token.job_service_containers.as_ref().unwrap();
        assert_eq!(template_token_type(services).unwrap(), Some(0));
        assert_eq!(services["lit"], Value::Null);
        assert!(services.get("map").is_none());

        // Legacy sidecars use Single: zero matches, duplicates, and visited
        // null slots all fail conversion.
        for containers in [
            serde_json::json!([]),
            serde_json::json!([null]),
            serde_json::json!([{ "Alias": "sidecar" }, { "Alias": "SIDECAR" }]),
        ] {
            assert!(WireAgentJobRequestMessage::from_value(serde_json::json!({
                "JobSidecarContainers": { "network": "sidecar" },
                "Resources": { "Containers": containers }
            }))
            .is_err());
        }
    }

    #[test]
    fn clr_container_alias_selection_uses_unicode_ordinal_ignore_case() {
        let message = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": "Å",
            "JobSidecarContainers": { "network": "å" },
            "Resources": {
                "Containers": [{ "Alias": "å", "Properties": { "image": "alpine" } }]
            }
        }))
        .unwrap();

        assert_eq!(message.job_container_resource_alias, Some("Å".to_owned()));
        assert_eq!(
            template_token_type(message.job_service_containers.as_ref().unwrap()).unwrap(),
            Some(2)
        );

        let null_alias = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": { "type": 0, "lit": null },
            "Resources": { "Containers": [{ "Alias": "" }] }
        }))
        .unwrap();
        assert_eq!(null_alias.job_container_resource_alias.as_deref(), Some(""));
        assert_eq!(null_alias.job_container.as_ref().unwrap()["type"], 2);

        let empty_alias = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": { "type": 0, "lit": null },
            "Resources": { "Containers": [{ "Alias": "" }] }
        }))
        .unwrap();
        assert_eq!(
            empty_alias.job_container_resource_alias.as_deref(),
            Some("")
        );

        let duplicate_unicode_aliases = serde_json::json!({
            "JobContainer": "É",
            "Resources": { "Containers": [
                { "Alias": "é" },
                { "Alias": "É" }
            ] }
        });
        assert!(WireAgentJobRequestMessage::from_value(duplicate_unicode_aliases).is_err());
    }

    #[test]
    fn clr_ordinal_ignore_case_uses_unicode_simple_uppercase() {
        assert!(clr_ordinal_ignore_case_eq("é", "É"));
        assert!(!clr_ordinal_ignore_case_eq("ı", "I"));
        assert!(!clr_ordinal_ignore_case_eq("ſ", "S"));
        assert!(clr_ordinal_ignore_case_eq("ᾀ", "ᾈ"));
        assert!(clr_ordinal_ignore_case_eq("𐐨", "𐐀"));
        assert!(!clr_ordinal_ignore_case_eq("ß", "SS"));
        // Full Greek uppercase expands U+0390 to multiple scalars. Ordinal
        // case matching uses the scalar's simple mapping and must not expand.
        assert!(!clr_ordinal_ignore_case_eq("ΐ", "Ϊ́"));

        for (left, right, expected) in [
            ("ß", "ẞ", false),
            ("K", "K", false),
            ("Ω", "Ω", false),
            ("Å", "Å", false),
            ("ϴ", "Θ", false),
            ("Σ", "ς", true),
            ("ͅ", "Ι", true),
            ("𐐨", "𐐀", true),
        ] {
            assert_eq!(
                clr_ordinal_ignore_case_eq(left, right),
                expected,
                "{left:?}/{right:?}"
            );
        }
    }

    #[test]
    fn system_connection_selectors_match_single_and_single_or_default() {
        let no_match = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Resources": { "Endpoints": [{ "Name": "Other" }] }
        }))
        .unwrap();
        assert!(no_match.system_connection_single().is_err());
        assert!(no_match
            .system_connection_single_or_default()
            .unwrap()
            .is_none());

        let one_match = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Resources": { "Endpoints": [{ "Name": "systemvssconnection" }] }
        }))
        .unwrap();
        assert!(one_match.system_connection_single().is_ok());
        assert!(one_match
            .system_connection_single_or_default()
            .unwrap()
            .is_some());

        let unicode_match = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Resources": { "Endpoints": [{ "Name": "ſystemVssConnection" }] }
        }))
        .unwrap();
        assert!(unicode_match
            .system_connection_single_or_default()
            .unwrap()
            .is_none());

        for endpoints in [
            serde_json::json!([null, { "Name": "Other" }]),
            serde_json::json!([
                { "Name": "SystemVssConnection" },
                { "Name": "systemvssconnection" }
            ]),
        ] {
            let invalid = WireAgentJobRequestMessage::from_value(serde_json::json!({
                "Resources": { "Endpoints": endpoints }
            }))
            .unwrap();
            assert!(invalid.system_connection_single().is_err());
            assert!(invalid.system_connection_single_or_default().is_err());
        }
    }

    #[test]
    fn from_value_preserves_arbitrary_repository_property_values_and_keys() {
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "resources": {
                "repositories": [{
                    "properties": {
                        "CloneURL": "https://github.com/acme/repo.git",
                        "id": 42,
                        "opaque": { "Items": [null, false, { "MixedCase": "kept" }] }
                    }
                }],
                "containers": [{
                    "properties": { "image": "alpine", "arbitrary": [1, { "x": false }] }
                }]
            }
        }))
        .unwrap();
        let properties = &message.resources.repositories[0]
            .as_ref()
            .unwrap()
            .properties;
        let container_properties = &message.resources.containers[0].as_ref().unwrap().properties;

        assert_eq!(
            properties.get("CloneURL"),
            Some(&ContextValue::String(
                "https://github.com/acme/repo.git".to_owned()
            ))
        );
        assert_eq!(
            properties.get("id"),
            Some(&test_context_value(serde_json::json!(42)))
        );
        assert_eq!(
            properties.get("opaque"),
            Some(&test_context_value(serde_json::json!({
                "Items": [null, false, { "MixedCase": "kept" }]
            })))
        );
        assert_eq!(
            container_properties.get("arbitrary"),
            Some(&test_context_value(serde_json::json!([1, { "x": false }])))
        );
    }

    #[test]
    fn raw_jtoken_values_keep_case_rules_and_do_not_read_user_context_tags() {
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "Resources": {
                "Containers": [{
                    "Properties": {
                        "nested": { "A": 1, "a": 2 },
                        "marker": {
                            "$velnor_context_value": "non_finite",
                            "value": "NaN"
                        }
                    }
                }],
                "Endpoints": [{
                    "OperationStatus": {
                        "literal": "Infinity",
                        "nested": { "A": 1, "a": 2 }
                    }
                }]
            }
        }))
        .unwrap();

        let container = message.resources.containers[0].as_ref().unwrap();
        assert!(matches!(
            container
                .property_value("nested")
                .and_then(|value| value.get("A")),
            Some(ContextValue::Number(value)) if value.as_i64() == Some(1)
        ));
        assert!(matches!(
            container.property_value("nested").and_then(|value| value.get("a")),
            Some(ContextValue::Number(value)) if value.as_i64() == Some(2)
        ));
        assert!(container
            .property_value("nested")
            .is_some_and(|value| value.get("A").is_some() && value.get("a").is_some()));
        assert!(matches!(
            container.property_value("marker"),
            Some(ContextValue::Object { case_sensitive: true, entries })
                if entries.len() == 2
                    && matches!(&entries[0].1, ContextValue::String(value) if value == "non_finite")
        ));

        let status = message.resources.endpoints[0]
            .operation_status
            .as_ref()
            .unwrap();
        assert!(matches!(
            status.get("literal"),
            Some(ContextValue::String(value)) if value == "Infinity"
        ));
        assert!(status
            .get("nested")
            .is_some_and(|nested| { nested.get("A").is_some() && nested.get("a").is_some() }));
    }

    #[test]
    fn raw_jtoken_fields_preserve_undefined_and_constructor_values() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{
                "Resources": {
                    "Containers": [{
                        "Properties": {
                            "undefined": undefined,
                            "constructor": new Foo(undefined, 1),
                            "nested": { "hole": [,], "constructor": new Foo(new Bar(undefined)) }
                        }
                    }],
                    "Endpoints": [{
                        "OperationStatus": {
                            "undefined": undefined,
                            "constructor": new Foo(0)
                        }
                    }]
                }
            }"#,
        )
        .unwrap();

        let properties = &message.resources.as_ref().unwrap().containers[0]
            .as_ref()
            .unwrap()
            .properties;
        assert!(matches!(
            properties.get("undefined"),
            Some(ContextValue::Undefined)
        ));
        assert!(matches!(
            properties.get("constructor"),
            Some(ContextValue::Constructor { name, arguments })
                if name == "Foo" && matches!(arguments.as_slice(), [
                    ContextValue::Undefined,
                    ContextValue::Number(number)
                ] if number.as_i64() == Some(1))
        ));
        assert!(matches!(
            properties.get("nested").and_then(|value| value.get("hole")),
            Some(ContextValue::Array(values))
                if matches!(values.as_slice(), [ContextValue::Undefined, ContextValue::Undefined])
        ));
        assert!(matches!(
            properties.get("nested").and_then(|value| value.get("constructor")),
            Some(ContextValue::Constructor { name, arguments })
                if name == "Foo" && matches!(arguments.as_slice(), [
                    ContextValue::Constructor { name, arguments }
                ] if name == "Bar" && matches!(arguments.as_slice(), [ContextValue::Undefined]))
        ));

        let status = message.resources.as_ref().unwrap().endpoints[0]
            .as_ref()
            .unwrap()
            .operation_status
            .as_ref()
            .unwrap();
        assert_eq!(status.is_case_sensitive(), Some(true));
        assert!(matches!(
            status.get("undefined"),
            Some(ContextValue::Undefined)
        ));
        assert!(matches!(
            status.get("constructor"),
            Some(ContextValue::Constructor { name, arguments })
                if name == "Foo" && matches!(arguments.as_slice(), [ContextValue::Number(number)]
                    if number.as_i64() == Some(0))
        ));
    }

    #[test]
    fn typed_jobject_and_resource_properties_roots_reject_undefined() {
        for body in [
            r#"{"Resources":{"Endpoints":[{"OperationStatus":undefined}]}}"#,
            r#"{"Resources":{"Endpoints":[{"OperationStatus":new Foo()}]}}"#,
            r#"{"Resources":{"Repositories":[{"Properties":undefined}]}}"#,
            r#"{"Resources":{"Repositories":[{"Properties":new Foo()}]}}"#,
            r#"{"Resources":{"Containers":[{"Properties":undefined}]}}"#,
            r#"{"Resources":{"Containers":[{"Properties":new Foo()}]}}"#,
        ] {
            assert!(
                WireAgentJobRequestMessage::parse_json(body).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn normalized_raw_jtoken_fields_preserve_nonfinite_numbers() {
        let resource_properties = ContextValue::object(vec![
            (
                "measurement".to_owned(),
                ContextValue::non_finite(NonFinite::PositiveInfinity),
            ),
            (
                "nested".to_owned(),
                ContextValue::case_sensitive_object(vec![
                    ("NaN".to_owned(), ContextValue::non_finite(NonFinite::NaN)),
                    ("marker".to_owned(), ContextValue::String("NaN".to_owned())),
                ])
                .unwrap(),
            ),
        ])
        .unwrap();
        let operation_status = ContextValue::case_sensitive_object(vec![(
            "operation".to_owned(),
            ContextValue::non_finite(NonFinite::NegativeInfinity),
        )])
        .unwrap();
        let normalized = serde_json::json!({
            "Resources": {
                "Containers": [{
                    "Properties": serde_json::to_value(resource_properties).unwrap()
                }],
                "Endpoints": [{
                    "OperationStatus": serde_json::to_value(operation_status).unwrap()
                }]
            }
        });

        let message = WireAgentJobRequestMessage::from_normalized_value(normalized).unwrap();
        let properties = &message.resources.as_ref().unwrap().containers[0]
            .as_ref()
            .unwrap()
            .properties;
        assert!(matches!(
            properties.get("measurement"),
            Some(ContextValue::NonFinite(NonFinite::PositiveInfinity))
        ));
        assert!(matches!(
            properties.get("nested").and_then(|value| value.get("NaN")),
            Some(ContextValue::NonFinite(NonFinite::NaN))
        ));
        assert!(matches!(
            properties.get("nested").and_then(|value| value.get("marker")),
            Some(ContextValue::String(value)) if value == "NaN"
        ));
        assert!(matches!(
            message.resources.as_ref().unwrap().endpoints[0]
                .as_ref()
                .unwrap()
                .operation_status
                .as_ref()
                .and_then(|value| value.get("operation")),
            Some(ContextValue::NonFinite(NonFinite::NegativeInfinity))
        ));
    }

    #[test]
    fn normalized_jtoken_fields_require_jobject_comparers_at_every_depth() {
        let nested_ci = ContextValue::object(vec![(
            "nested".to_owned(),
            ContextValue::object(vec![("A".to_owned(), ContextValue::Null)]).unwrap(),
        )])
        .unwrap();
        let bad_properties = serde_json::json!({
            "Resources": {
                "Containers": [{
                    "Properties": serde_json::to_value(nested_ci).unwrap()
                }]
            }
        });
        assert!(WireAgentJobRequestMessage::from_normalized_value(bad_properties).is_err());

        let nested_ci = ContextValue::case_sensitive_object(vec![(
            "nested".to_owned(),
            ContextValue::object(vec![("A".to_owned(), ContextValue::Null)]).unwrap(),
        )])
        .unwrap();
        let bad_operation_status = serde_json::json!({
            "Resources": {
                "Endpoints": [{
                    "OperationStatus": serde_json::to_value(nested_ci).unwrap()
                }]
            }
        });
        assert!(WireAgentJobRequestMessage::from_normalized_value(bad_operation_status).is_err());
    }

    #[test]
    fn resource_property_bags_reject_null_and_case_insensitive_duplicates() {
        for properties in [
            serde_json::json!(null),
            serde_json::json!({ "A": 1, "a": 2 }),
        ] {
            assert!(AgentJobRequestMessage::from_value(serde_json::json!({
                "Resources": { "Containers": [{ "Properties": properties }] }
            }))
            .is_err());
        }
        let missing = AgentJobRequestMessage::from_value(serde_json::json!({
            "Resources": { "Containers": [{}] }
        }))
        .unwrap();
        assert!(matches!(
            &missing.resources.containers[0].as_ref().unwrap().properties,
            ContextValue::Object { case_sensitive: false, entries } if entries.is_empty()
        ));
    }

    #[test]
    fn parses_pipeline_agent_job_request_subset() {
        let body = format!(
            r#"{{
                "MessageType": "PipelineAgentJobRequest",
                "Plan": {{
                    "PlanId": "{PLAN_ID}",
                    "PlanType": "Build",
                    "ScopeIdentifier": "55555555-5555-5555-5555-555555555555",
                    "Version": 8
                }},
                "Timeline": {{
                    "Id": "{TIMELINE_ID}",
                    "ChangeId": 1
                }},
                "JobId": "{JOB_ID}",
                "JobDisplayName": "Check",
                "JobName": "check",
                "RequestId": 123,
                "LockedUntil": "2026-05-31T12:10:00Z",
                "Variables": {{
                    "system.github.token": {{
                        "Value": "secret-token",
                        "IsSecret": true
                    }},
                    "github.repository": {{
                        "Value": "ChainArgos/java-monorepo"
                    }}
                }},
                "Resources": {{
                    "Endpoints": [{{
                        "Name": "SystemVssConnection",
                        "Url": "https://pipelines.actions.githubusercontent.com/abc",
                        "Authorization": {{
                            "Scheme": "OAuth",
                            "Parameters": {{
                                "AccessToken": "job-token"
                            }}
                        }},
                        "Data": {{
                            "GenerateIdTokenUrl": "https://token.actions.githubusercontent.com"
                        }}
                    }}],
                    "Repositories": [{{
                        "Alias": "self",
                        "Name": "ChainArgos/java-monorepo",
                        "Ref": "refs/heads/main",
                        "Version": "abc123",
                        "Properties": {{
                            "cloneUrl": "https://github.com/ChainArgos/java-monorepo.git"
                        }}
                    }}]
                }},
                "Steps": [{{
                    "Type": "Action",
                    "Id": "{STEP_ID}",
                    "Name": "__run",
                    "DisplayName": "Run tests",
                    "Enabled": true,
                    "Condition": "success()",
                    "Reference": {{
                        "Type": "Script"
                    }},
                    "Inputs": {{
                        "script": "cargo test",
                        "shell": "bash",
                        "workingDirectory": "./crates"
                    }}
                }}]
            }}"#
        );

        let message = AgentJobRequestMessage::parse_json(&body).unwrap();

        assert_eq!(message.message_type, PIPELINE_AGENT_JOB_REQUEST);
        assert_eq!(message.request_id, 123);
        assert_eq!(message.job_id, JOB_ID);
        assert_eq!(message.plan.plan_id, PLAN_ID);
        assert_eq!(message.timeline.id, TIMELINE_ID);
        assert!(message.variables["system.github.token"].is_secret);
        assert_eq!(
            message
                .system_connection_single_or_default()
                .unwrap()
                .unwrap()
                .authorization
                .as_ref()
                .unwrap()
                .parameter_string("AccessToken"),
            Some("job-token")
        );
        assert_eq!(message.steps.len(), 1);
        assert_eq!(
            message.steps[0].as_ref().unwrap().reference_type(),
            Some(ActionReferenceType::Script)
        );
        assert_eq!(
            message.steps[0].as_ref().unwrap().display_name.as_deref(),
            Some("Run tests")
        );
    }

    #[test]
    fn accepts_lower_camel_case_message_fields() {
        let body = format!(
            r#"{{
                "messageType": "PipelineAgentJobRequest",
                "plan": {{ "planId": "{PLAN_ID}" }},
                "timeline": {{ "id": "{TIMELINE_ID}" }},
                "jobId": "{JOB_ID}",
                "jobDisplayName": "Check",
                "requestId": 123,
                "steps": [{{
                    "type": 4,
                    "reference": {{ "type": "Repository", "name": "actions/checkout", "ref": "v4" }}
                }}]
            }}"#
        );

        let message = AgentJobRequestMessage::parse_json(&body).unwrap();

        assert_eq!(message.message_type, PIPELINE_AGENT_JOB_REQUEST);
        assert_eq!(
            message.steps[0]
                .as_ref()
                .unwrap()
                .reference
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("actions/checkout")
        );
        assert_eq!(
            message.steps[0].as_ref().unwrap().reference_type(),
            Some(ActionReferenceType::Repository)
        );
    }

    #[test]
    fn accepts_snake_case_step_name_fields() {
        let body = format!(
            r#"{{
                "messageType": "PipelineAgentJobRequest",
                "plan": {{ "planId": "{PLAN_ID}" }},
                "timeline": {{ "id": "{TIMELINE_ID}" }},
                "jobId": "{JOB_ID}",
                "jobDisplayName": "Check",
                "requestId": 123,
                "steps": [{{
                    "type": "Action",
                    "name": "__run",
                    "display_name": "Install Ansible",
                    "context_name": "install",
                    "reference": {{ "type": "Script" }},
                    "inputs": {{ "script": "pip install ansible-core" }}
                }}]
            }}"#
        );

        let message = AgentJobRequestMessage::parse_json(&body).unwrap();

        assert_eq!(
            message.steps[0].as_ref().unwrap().display_name.as_deref(),
            Some("Install Ansible")
        );
        assert_eq!(
            message.steps[0].as_ref().unwrap().context_name.as_deref(),
            Some("install")
        );
    }

    #[test]
    fn accepts_numeric_action_reference_type() {
        let body = format!(
            r#"{{
                "messageType": "PipelineAgentJobRequest",
                "plan": {{ "planId": "{PLAN_ID}" }},
                "timeline": {{ "id": "{TIMELINE_ID}" }},
                "jobId": "{JOB_ID}",
                "jobDisplayName": "Check",
                "requestId": 123,
                "steps": [{{
                    "type": "4",
                    "reference": {{ "type": "3" }}
                }}]
            }}"#
        );

        let message = AgentJobRequestMessage::parse_json(&body).unwrap();

        assert_eq!(
            message.steps[0].as_ref().unwrap().reference_type(),
            Some(ActionReferenceType::Script)
        );
    }

    #[test]
    fn from_value_normalizes_clr_guid_forms_and_numeric_values() {
        let plan_id = PLAN_ID.replace('-', "");
        let timeline_id = format!("({TIMELINE_ID})");
        let job_id = format!("  {{{JOB_ID}}}  ");
        let step_id = "{0x44444444,0x4444,0x4444,{0x44,0x44,0x44,0x44,0x44,0x44,0x44,0x44}}";
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "JobId": job_id,
            "Plan": { "PlanId": plan_id, "Version": 4.5 },
            "Timeline": { "Id": timeline_id },
            "RequestId": 4.5,
            "Variables": {
                "secret.true": { "Value": "secret", "IsSecret": 1 },
                "secret.false": { "Value": "public", "IsSecret": 0 }
            },
            "Steps": [{
                "Type": 4,
                "Id": step_id,
                "Reference": { "Type": "3" }
            }]
        }))
        .unwrap();

        assert_eq!(message.job_id, JOB_ID);
        assert_eq!(message.plan.plan_id, PLAN_ID);
        assert_eq!(message.plan.version, Some(4));
        assert_eq!(message.timeline.id, TIMELINE_ID);
        assert_eq!(message.request_id, 4);
        assert!(message.variables["secret.true"].is_secret);
        assert!(!message.variables["secret.false"].is_secret);
        let step = message.steps[0].as_ref().unwrap();
        assert_eq!(step.id.as_deref(), Some(STEP_ID));
        assert_eq!(step.reference_type(), Some(ActionReferenceType::Script));

        let string_values = AgentJobRequestMessage::from_value(serde_json::json!({
            "RequestId": "19",
            "Plan": { "Version": "8" }
        }))
        .unwrap();
        assert_eq!(string_values.request_id, 19);
        assert_eq!(string_values.plan.version, Some(8));
    }

    #[test]
    fn from_value_preserves_nullable_collection_slots_and_endpoint_key_casing() {
        let message = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Variables": { "null-variable": null },
            "Mask": [null, { "Value": "secret" }],
            "Steps": [null, {}, { "Type": 4 }],
            "Resources": {
                "Endpoints": [null, {
                    "Name": "SystemVssConnection",
                    "Data": { "ResultsServiceUrl": null, "MixedCase": 3 },
                    "Authorization": { "Parameters": { "aCcEsStOkEn": null } }
                }],
                "Repositories": [null, { "Properties": { "id": 7 } }],
                "Containers": [null, { "Properties": { "image": "alpine" } }]
            }
        }))
        .unwrap();

        assert!(message.variables["null-variable"].is_none());
        assert_eq!(message.mask.len(), 2);
        assert!(message.mask[0].is_none());
        assert_eq!(message.steps.len(), 3);
        assert!(message.steps[0].is_none());
        assert!(message.steps[1].is_none());
        assert!(message.steps[2].is_some());
        let resources = message.resources.as_ref().unwrap();
        assert!(resources.endpoints[0].is_none());
        assert!(resources.repositories[0].is_none());
        assert!(resources.containers[0].is_none());

        assert!(message.system_connection_single().is_err());
        assert!(message.system_connection_single_or_default().is_err());

        let endpoint_message = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "Resources": {
                "Endpoints": [{
                    "Name": "SystemVssConnection",
                    "Data": { "ResultsServiceUrl": null, "MixedCase": 3 },
                    "Authorization": { "Parameters": { "aCcEsStOkEn": null } }
                }]
            }
        }))
        .unwrap();
        let endpoint = endpoint_message.system_connection_single().unwrap();
        assert_eq!(endpoint.data_string("mixedcase"), Some("3"));
        assert!(endpoint.data_value("resultsserviceurl").unwrap().is_none());
        assert!(endpoint
            .authorization
            .as_ref()
            .unwrap()
            .parameter_value("AccessToken")
            .unwrap()
            .is_none());
        assert!(endpoint.data.contains_key("MixedCase"));
        assert!(endpoint.data.contains_key("ResultsServiceUrl"));
    }

    #[test]
    fn from_value_rejects_case_aliases_for_ci_copy_maps() {
        let authorization = serde_json::json!({
            "Resources": { "Endpoints": [{
                "Authorization": {
                    "Parameters": { "AccessToken": "first", "accesstoken": "last" }
                }
            }] }
        });
        assert!(AgentJobRequestMessage::from_value(authorization).is_err());

        let properties = serde_json::json!({
            "Resources": { "Containers": [{
                "Properties": { "Image": "first", "image": "last" }
            }] }
        });
        assert!(AgentJobRequestMessage::from_value(properties).is_err());
    }

    #[test]
    fn parse_json_folds_endpoint_data_aliases_in_wire_order() {
        let message = AgentJobRequestMessage::parse_json(
            r#"{"Resources":{"Endpoints":[{"Data":{"FirstCase":"first","firstcase":"last"}}]}}"#,
        )
        .unwrap();
        let endpoint = &message.resources.endpoints[0];
        assert_eq!(endpoint.data.len(), 1);
        assert!(endpoint.data.contains_key("FirstCase"));
        assert_eq!(endpoint.data_string("FIRSTCASE"), Some("last"));

        let acquired_value = serde_json::json!({
            "Resources": { "Endpoints": [{ "Data": {
                "FirstCase": "first", "firstcase": "last"
            } }] }
        });
        assert!(AgentJobRequestMessage::from_value(acquired_value).is_err());
    }

    #[test]
    fn parse_json_coerces_clr_string_members_and_map_values() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{
                "JobSidecarContainers":{"node":17},
                "Resources":{"Endpoints":[{
                    "Name":42,
                    "Data":{"ResultServiceUrl":73},
                    "Authorization":{"Parameters":{"AccessToken":false}}
                }]}
            }"#,
        )
        .unwrap();
        assert_eq!(
            message.job_sidecar_containers.as_ref().unwrap().get("node"),
            Some(&Some("17".to_owned()))
        );
        let endpoint = message.resources.as_ref().unwrap().endpoints[0]
            .as_ref()
            .unwrap();
        assert_eq!(endpoint.name.as_deref(), Some("42"));
        assert_eq!(endpoint.data_string("resultserviceurl"), Some("73"));
        assert_eq!(
            endpoint
                .authorization
                .as_ref()
                .unwrap()
                .parameter_string("accesstoken"),
            Some("False")
        );
    }

    #[test]
    fn parse_json_authorization_parameters_use_runner_ci_copy_semantics() {
        let case_aliases = WireAgentJobRequestMessage::parse_json(
            r#"{"Resources":{"Endpoints":[{"Authorization":{"Parameters":{
                "AccessToken":"first","accesstoken":"last"
            }}}]}}"#,
        );
        assert!(case_aliases.is_err());

        let exact_duplicate = WireAgentJobRequestMessage::parse_json(
            r#"{"Resources":{"Endpoints":[{"Authorization":{"Parameters":{
                "AccessToken":"first","AccessToken":"last"
            }}}]}}"#,
        )
        .unwrap();
        assert_eq!(
            exact_duplicate.resources.as_ref().unwrap().endpoints[0]
                .as_ref()
                .unwrap()
                .authorization
                .as_ref()
                .unwrap()
                .parameter_string("accesstoken"),
            Some("last")
        );
    }

    #[test]
    fn parse_json_merges_writable_members_in_wire_order_and_replaces_property_bags() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{
                "Resources": {"Containers": [{"Alias":"first","Properties":{"old":1}}]},
                "resources": {"containers": [{"Alias":"second","Properties":{"old":2,"kept":true}}]},
                "Steps": [{"Type":4,"Id":"11111111-1111-1111-1111-111111111111"}],
                "steps": null,
                "Steps": [null,{"Type":5,"Id":"22222222-2222-2222-2222-222222222222"}],
                "Variables": {"first":{"Value":"one"}},
                "variables": null,
                "Variables": {"second":{"Value":"two"}},
                "Resources": {"Endpoints": [{
                    "Authorization": {"Parameters":{"old":"one"}},
                    "authorization": {"parameters":{"new":"two"}}
                }]}
            }"#,
        )
        .unwrap();

        let resources = message.resources.as_ref().unwrap();
        assert_eq!(resources.containers.len(), 2);
        assert_eq!(
            resources.containers[0].as_ref().unwrap().alias.as_deref(),
            Some("first")
        );
        assert_eq!(
            resources.containers[1].as_ref().unwrap().alias.as_deref(),
            Some("second")
        );
        assert_eq!(
            resources.containers[1].as_ref().unwrap().properties,
            test_resource_properties(serde_json::json!({ "old": 2, "kept": true }))
        );
        assert_eq!(message.steps.len(), 2);
        assert!(message.steps[0].is_none());
        assert_eq!(
            message.steps[1].as_ref().unwrap().kind,
            Some(ActionStepKind::BackgroundStepControl)
        );
        assert_eq!(message.variables.len(), 1);
        assert!(message.variables.contains_key("second"));
        let endpoint = resources.endpoints[0].as_ref().unwrap();
        let parameters = &endpoint.authorization.as_ref().unwrap().parameters;
        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters.get("new"), Some(&Some("two".to_owned())));
    }

    #[test]
    fn parse_json_repeated_resource_properties_replaces_the_converter_bag() {
        let message = WireAgentJobRequestMessage::parse_json(
            r#"{"Resources":{"Containers":[{
                "Properties":{"image":"old","old-only":true},
                "properties":{"ports":["8080"],"image":"new"}
            }]}}"#,
        )
        .unwrap();
        let properties = &message.resources.as_ref().unwrap().containers[0]
            .as_ref()
            .unwrap()
            .properties;
        assert_eq!(properties.entries().unwrap().len(), 2);
        assert!(properties.get("old-only").is_none());
        assert_eq!(
            properties.get("image"),
            Some(&ContextValue::String("new".to_owned()))
        );
        assert_eq!(
            properties.get("ports"),
            Some(&test_context_value(serde_json::json!(["8080"])))
        );
    }

    #[test]
    fn parse_json_null_or_empty_later_authorization_parameters_keep_prior_map() {
        for body in [
            r#"{"Resources":{"Endpoints":[{"Authorization":{"Parameters":{"keep":"value"},"parameters":null}}]}}"#,
            r#"{"Resources":{"Endpoints":[{"Authorization":{"Parameters":{"keep":"value"},"parameters":{}}}]}}"#,
        ] {
            let message = WireAgentJobRequestMessage::parse_json(body).unwrap();
            let parameters = &message.resources.as_ref().unwrap().endpoints[0]
                .as_ref()
                .unwrap()
                .authorization
                .as_ref()
                .unwrap()
                .parameters;
            assert_eq!(parameters.get("keep"), Some(&Some("value".to_owned())));
        }
    }

    #[test]
    fn callback_only_validator_ignores_unrelated_local_model_failures() {
        let value = serde_json::json!({
            "Plan": { "Version": "not-an-integer" },
            "JobContainer": "absent",
            "Resources": { "Containers": [{ "Alias": "present" }] }
        });
        assert!(
            WireAgentJobRequestMessage::validate_deserialization_callback_from_value(&value)
                .is_ok()
        );

        let callback_error = serde_json::json!({
            "JobContainer": "main",
            "Resources": { "Containers": [null] }
        });
        assert!(
            WireAgentJobRequestMessage::validate_deserialization_callback_from_value(
                &callback_error,
            )
            .is_err()
        );
    }

    #[test]
    fn from_value_keeps_action_and_background_control_steps_distinct() {
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "Steps": [
                null,
                { "Name": "missing discriminator" },
                { "Type": "not-a-step" },
                {
                    "Type": "aCtIoN",
                    "Background": "true",
                    "ParallelGroupId": "group-a",
                    "ContextName": "build"
                },
                {
                    "Type": 5,
                    "ControlType": "wait-all",
                    "StepIds": ["first", null, 17],
                    "Reference": { "Type": { "unexpected": true } }
                }
            ]
        }))
        .unwrap();

        assert_eq!(message.steps.len(), 5);
        assert!(message.steps[0].is_none());
        assert!(message.steps[1].is_none());
        assert!(message.steps[2].is_none());
        let action = message.steps[3].as_ref().unwrap();
        assert_eq!(action.step_kind(), Some(ActionStepKind::Action));
        assert!(action.background);
        assert_eq!(action.parallel_group_id.as_deref(), Some("group-a"));
        assert_eq!(action.context_name.as_deref(), Some("build"));
        let background = message.steps[4].as_ref().unwrap();
        assert_eq!(
            background.step_kind(),
            Some(ActionStepKind::BackgroundStepControl)
        );
        assert_eq!(background.control_type.as_deref(), Some("wait-all"));
        assert_eq!(
            background.step_ids,
            vec![Some("first".to_owned()), None, Some("17".to_owned())]
        );
        assert!(background.reference.is_none());
    }

    #[test]
    fn from_value_applies_job_container_and_sidecar_callbacks_from_properties() {
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": "DB",
            "JobServiceContainers": { "type": 7 },
            "JobSidecarContainers": { "service-network": "db" },
            "Resources": {
                "Containers": [{
                    "Alias": "db",
                    "Properties": {
                        "IMAGE": "postgres:16",
                        "env": { "MixedKey": false, "NullValue": null },
                        "ports": ["5432:5432", null, 5433],
                        "volumes": ["${{ secrets.volume }}"],
                        "credentials": {
                            "username": "private-user",
                            "password": "private-password"
                        },
                        "opaque": { "KeepCase": true }
                    }
                }]
            }
        }))
        .unwrap();

        assert_eq!(message.job_container_resource_alias, Some("DB".to_owned()));
        let container = message.resources.containers[0].as_ref().unwrap();
        assert_eq!(
            container.property_string("image").as_deref(),
            Some("postgres:16")
        );
        assert_eq!(
            container.property_string_list("ports").unwrap(),
            Some(vec![
                Some("5432:5432".to_owned()),
                None,
                Some("5433".to_owned())
            ])
        );
        assert_eq!(
            container.property_string_map("ENV").unwrap().unwrap()["MixedKey"],
            Some("False".to_owned())
        );
        assert_eq!(
            container.properties.get("opaque"),
            Some(&test_context_value(serde_json::json!({ "KeepCase": true })))
        );

        let job_container = message.job_container.as_ref().unwrap();
        assert_eq!(job_container["type"], 2);
        assert_eq!(
            find_token_mapping_value(job_container, "image").unwrap()["lit"],
            "postgres:16"
        );
        assert_eq!(
            find_token_mapping_value(job_container, "env").unwrap()["type"],
            2
        );
        assert_eq!(
            find_token_mapping_value(job_container, "ports").unwrap()["type"],
            1
        );
        let credentials = find_token_mapping_value(job_container, "credentials").unwrap();
        assert_eq!(credentials["type"], 7);
        assert_eq!(
            find_token_mapping_value(job_container, "volumes").unwrap()["type"],
            7
        );
        let serialized_job_container = serde_json::to_string(job_container).unwrap();
        assert!(!serialized_job_container.contains("private-user"));
        assert!(!serialized_job_container.contains("private-password"));
        assert!(!serialized_job_container.contains("secrets.volume"));
        let services = message.job_service_containers.as_ref().unwrap();
        assert_eq!(services["type"], 2);
        let sidecar = find_token_mapping_value(services, "service-network").unwrap();
        assert_eq!(
            find_token_mapping_value(sidecar, "credentials").unwrap()["type"],
            7
        );
        assert_eq!(
            find_token_mapping_value(sidecar, "volumes").unwrap()["type"],
            7
        );
        let serialized_services = serde_json::to_string(services).unwrap();
        assert!(!serialized_services.contains("private-user"));
        assert!(!serialized_services.contains("private-password"));
        assert!(!serialized_services.contains("secrets.volume"));
    }

    #[test]
    fn container_callback_projects_null_string_values_as_empty_string_tokens() {
        let wire = WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": "main",
            "Resources": { "Containers": [{
                "Alias": "main",
                "Properties": {
                    "env": { "NULL_ENV": null },
                    "ports": [null],
                    "volumes": [null]
                }
            }] }
        }))
        .unwrap();

        let container = wire.resources.as_ref().unwrap().containers[0]
            .as_ref()
            .unwrap();
        assert_eq!(
            container.properties.get("env"),
            Some(&test_context_value(serde_json::json!({ "NULL_ENV": null })))
        );
        assert_eq!(
            container.properties.get("ports"),
            Some(&test_context_value(serde_json::json!([null])))
        );
        assert_eq!(
            container.properties.get("volumes"),
            Some(&test_context_value(serde_json::json!([null])))
        );

        let projected = wire.job_container.as_ref().unwrap();
        let env = find_token_mapping_value(projected, "env").unwrap();
        assert_eq!(
            find_token_mapping_value(env, "NULL_ENV"),
            Some(&serde_json::json!({ "type": 0, "lit": "" }))
        );
        let sequence = find_token_mapping_value(projected, "ports").unwrap();
        assert_eq!(
            sequence["seq"][0],
            serde_json::json!({ "type": 0, "lit": "" })
        );
        assert_eq!(
            find_token_mapping_value(projected, "volumes").unwrap()["type"],
            7
        );
    }

    #[test]
    fn volume_callback_projects_presence_without_reading_source_values() {
        for volumes in [
            serde_json::json!([]),
            Value::Null,
            serde_json::json!(["${{ secrets.volume }}"]),
        ] {
            let message = WireAgentJobRequestMessage::from_value(serde_json::json!({
                "JobContainer": "db",
                "JobSidecarContainers": { "service-network": "db" },
                "Resources": { "Containers": [{
                    "Alias": "db",
                    "Properties": { "volumes": volumes }
                }] }
            }))
            .unwrap();
            let job = message.job_container.as_ref().unwrap();
            assert_eq!(find_token_mapping_value(job, "volumes").unwrap()["type"], 7);
            let services = message.job_service_containers.as_ref().unwrap();
            let sidecar = find_token_mapping_value(services, "service-network").unwrap();
            assert_eq!(
                find_token_mapping_value(sidecar, "volumes").unwrap()["type"],
                7
            );
            let projected = format!("{}{}", job, services);
            assert!(!projected.contains("secrets.volume"));
        }
    }

    #[test]
    fn from_value_materializes_typed_pipeline_context_envelopes_without_rekeying() {
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "ContextData": {
                "GitHub": {
                    "T": 2,
                    "D": [
                        { "K": "Repo", "V": { "T": 0, "S": true } },
                        { "K": "Owner", "V": { "t": 0, "s": "owner" } }
                    ]
                },
                "caseSensitive": {
                    "t": 5,
                    "d": [
                        { "k": "CamelCase", "v": { "t": 4, "n": "1.5" } },
                        { "k": "camelcase", "v": { "t": 0, "s": "kept separate" } }
                    ]
                },
                "explicit-null": null,
                "raw": { "KeepCase": { "NestedKey": true } },
                "array-null": []
            },
            "EnvironmentVariables": [
                { "TYPE": 0, "LIT": 42 },
                { "type": 5, "bool": 1 },
                null,
                "plain",
                []
            ],
            "ActionsEnvironment": { "NAME": "gh", "URL": { "TYPE": 0, "LIT": true } }
        }))
        .unwrap();

        let github = &message.context_data["GitHub"];
        assert!(github.get("t").is_some());
        assert!(github.get("T").is_none());
        assert_eq!(github["d"][0]["k"], "Repo");
        assert_eq!(github["d"][0]["v"]["s"], "True");
        assert_eq!(github["d"][1]["v"]["s"], "owner");
        assert_eq!(message.context_data["explicit-null"], Value::Null);
        assert_eq!(message.context_data["raw"]["t"], 0);
        assert!(message.context_data["array-null"].is_null());

        let materialized = message.materialize_context_data().unwrap();
        assert_eq!(materialized["GitHub"].as_object().unwrap().len(), 2);
        assert_eq!(materialized["GitHub"]["Repo"], "True");
        assert_eq!(materialized["GitHub"]["Owner"], "owner");
        assert_eq!(materialized["caseSensitive"].as_object().unwrap().len(), 2);
        assert_eq!(materialized["caseSensitive"]["CamelCase"], 1.5);
        assert_eq!(materialized["caseSensitive"]["camelcase"], "kept separate");
        assert_eq!(materialized["explicit-null"], Value::Null);
        assert_eq!(materialized["raw"], "");
        assert_eq!(materialized["array-null"], Value::Null);
        assert_eq!(message.environment_variables[0]["lit"], "42");
        assert_eq!(message.environment_variables[1]["bool"], true);
        assert!(message.environment_variables[2].is_null());
        assert_eq!(message.environment_variables[3], "plain");
        assert!(message.environment_variables[4].is_null());
        assert_eq!(
            message.actions_environment.as_ref().unwrap()["url"]["lit"],
            "True"
        );

        let duplicate_ci_key = AgentJobRequestMessage::from_value(serde_json::json!({
            "ContextData": { "github": { "t": 2, "d": [
                { "k": "Repo", "v": { "t": 0, "s": "first" } },
                { "k": "repo", "v": { "t": 0, "s": "second" } }
            ] } }
        }))
        .unwrap();
        assert!(duplicate_ci_key.materialize_context_data().is_err());

        let null_pair = AgentJobRequestMessage::from_value(serde_json::json!({
            "ContextData": { "github": { "t": 2, "d": [null] } }
        }))
        .unwrap();
        assert!(null_pair.materialize_context_data().is_err());
    }

    #[test]
    fn wire_context_strings_default_null_and_missing_values_to_empty() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{"ContextData":{"missing":{"Noise":true},"null-literal":{"t":0,"s":null},"absent-literal":{"t":0}}}"#,
        )
        .unwrap();

        assert_eq!(wire.context_data.as_ref().unwrap()["missing"]["t"], 0);
        assert!(!wire.context_data.as_ref().unwrap()["missing"]
            .as_object()
            .unwrap()
            .contains_key("Noise"));
        let runtime = wire.materialize_runtime().unwrap();
        let context = runtime.materialize_context_data().unwrap();
        assert_eq!(context["missing"], "");
        assert_eq!(context["null-literal"], "");
        assert_eq!(context["absent-literal"], "");
    }

    #[test]
    fn wire_context_map_keeps_source_order_and_exact_duplicate_slots() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{"ContextData":{"root":{"t":0,"s":"first"},"Root":{"t":0,"s":"second"},"root":{"t":0,"s":"last exact"}}}"#,
        )
        .unwrap();
        let entries = wire.materialize_context_values_ordered().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "root");
        assert_eq!(entries[0].1, ContextValue::String("last exact".to_owned()));
        assert_eq!(entries[1].0, "Root");
        assert_eq!(entries[1].1, ContextValue::String("second".to_owned()));
    }

    #[test]
    fn public_normalized_context_data_does_not_accept_internal_pair_arrays() {
        let array_context = serde_json::json!({
            "ContextData": [["root", {"t": 0, "s": "value"}]]
        });
        assert!(WireAgentJobRequestMessage::from_normalized_value(array_context.clone()).is_err());
        assert!(WireAgentJobRequestMessage::from_value(array_context.clone()).is_err());
        assert!(serde_json::from_value::<AgentJobRequestMessage>(array_context).is_err());
    }

    #[test]
    fn wire_double_values_use_clr_double_precision_and_reject_null() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{"JobContainer":{"type":6,"num":9007199254740993},"ContextData":{"number":{"t":4,"n":9007199254740993},"unknown-field":{"t":4,"n":1,"b":[]}}}"#,
        )
        .unwrap();

        let token_number = &wire.job_container.as_ref().unwrap()["num"];
        assert!(token_number.as_number().unwrap().is_f64());
        assert_eq!(token_number.as_f64(), Some(9007199254740992.0));
        let context = wire
            .materialize_runtime()
            .unwrap()
            .materialize_context_data()
            .unwrap();
        let context_number = context["number"].as_number().unwrap();
        assert!(context_number.is_f64());
        assert_eq!(context_number.as_f64(), Some(9007199254740992.0));
        assert_eq!(context["unknown-field"], 1.0);

        assert!(WireAgentJobRequestMessage::from_value(serde_json::json!({
            "ContextData": { "number": { "t": 4, "n": null } }
        }))
        .is_err());
        assert!(WireAgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": { "type": 6, "num": null }
        }))
        .is_err());
    }

    #[test]
    fn ordered_wire_converters_choose_exact_discriminators_before_aliases() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{
                "JobContainer":{"Type":1,"type":0,"lit":"job-token","seq":17},
                "ContextData":{
                    "missing":{"Noise":true},
                    "collision":{"T":1,"t":0,"S":"selected","A":[{"t":99}]},
                    "invalid":{"t":"not-an-integer","a":[{"t":99}]}
                },
                "Steps":[{
                    "type":5,"Type":4,"StepIds":17,
                    "Reference":{"type":2,"Type":1,"Name":"owner/action","Image":[]}
                }]
            }"#,
        )
        .unwrap();

        let container = wire.job_container.as_ref().unwrap();
        assert_eq!(container["type"], 0);
        assert_eq!(container["lit"], "job-token");
        assert!(container.get("seq").is_none());

        let context = wire.context_data.as_ref().unwrap();
        assert_eq!(context["missing"]["t"], 0);
        assert_eq!(context["missing"].as_object().unwrap().len(), 1);
        assert_eq!(context["collision"]["t"], 0);
        assert_eq!(context["collision"]["s"], "selected");
        assert!(context["collision"].get("a").is_none());
        assert!(context["invalid"].is_null());

        let step = wire.steps[0].as_ref().unwrap();
        assert_eq!(step.kind, Some(ActionStepKind::Action));
        let reference = step.reference.as_ref().unwrap();
        assert_eq!(reference.r#type, Some(ActionReferenceType::Repository));
        assert_eq!(reference.name.as_deref(), Some("owner/action"));
        assert!(reference.image.is_none());
        assert!(step.step_ids.is_empty());
    }

    #[test]
    fn ordered_template_converter_preserves_existing_value_for_invalid_repeated_token() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{"JobServiceContainers":{"type":2,"map":[]},"JobServiceContainers":{"type":"invalid"},"JobSidecarContainers":{"network":"sidecar"}}"#,
        )
        .unwrap();

        let services = wire.job_service_containers.as_ref().unwrap();
        assert_eq!(services["type"], 2);
        assert_eq!(services["map"], serde_json::json!([]));
    }

    #[test]
    fn ordered_context_dictionary_invalid_duplicate_replaces_value_and_step_token_keeps_existing() {
        let wire = WireAgentJobRequestMessage::parse_json(
            r#"{"ContextData":{"x":"kept","x":{"t":"invalid"}},"Steps":[{"Type":4,"Environment":{"type":2},"environment":{"type":"invalid"}}]}"#,
        )
        .unwrap();

        assert_eq!(wire.context_data.as_ref().unwrap()["x"], Value::Null);
        assert_eq!(
            wire.steps[0].as_ref().unwrap().environment,
            Some(serde_json::json!({ "type": 2 }))
        );
    }

    #[test]
    fn ordered_converter_rejects_discriminator_overflow_and_bad_first_alias_value() {
        for body in [
            r#"{"JobContainer":{"type":2147483648}}"#,
            r#"{"ContextData":{"x":{"t":2147483648}}}"#,
            r#"{"JobContainer":{"type":6,"num":null,"Num":1}}"#,
            r#"{"ContextData":{"x":{"t":4,"n":null,"N":1}}}"#,
        ] {
            assert!(
                WireAgentJobRequestMessage::parse_json(body).is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn context_value_materialization_preserves_typed_comparers_and_nonfinite_numbers() {
        let message = AgentJobRequestMessage::from_value(serde_json::json!({
            "ContextData": {
                "dictionary": {"t": 2, "d": [
                    {"k": "Repo", "v": {"t": 0, "s": "owner/repo"}},
                    {"k": "event", "v": {"t": 0, "s": "push"}}
                ]},
                "case_sensitive": {"t": 5, "d": [
                    {"k": "CamelCase", "v": {"t": 4, "n": 1.5}},
                    {"k": "camelcase", "v": {"t": 0, "s": "kept separate"}}
                ]},
                "not_a_number": {"t": 4, "n": "NaN"},
                "positive_infinity": {"t": 4, "n": "Infinity"},
                "negative_infinity": {"t": 4, "n": "-Infinity"},
                "literal_marker": {"t": 0, "s": "NaN"},
                "plain_marker": "NaN"
            }
        }))
        .unwrap();

        let contexts = message.materialize_context_values().unwrap();
        assert!(matches!(
            contexts["dictionary"].get("rEpO"),
            Some(ContextValue::String(value)) if value == "owner/repo"
        ));
        assert!(contexts["dictionary"].get("missing").is_none());
        assert!(matches!(
            contexts["case_sensitive"].get("CamelCase"),
            Some(ContextValue::Number(value)) if value.as_f64() == Some(1.5)
        ));
        assert!(contexts["case_sensitive"].get("CAMELCASE").is_none());
        assert!(matches!(
            contexts.get("not_a_number"),
            Some(ContextValue::NonFinite(NonFinite::NaN))
        ));
        assert!(matches!(
            contexts.get("positive_infinity"),
            Some(ContextValue::NonFinite(NonFinite::PositiveInfinity))
        ));
        assert!(matches!(
            contexts.get("negative_infinity"),
            Some(ContextValue::NonFinite(NonFinite::NegativeInfinity))
        ));
        assert!(matches!(
            contexts.get("literal_marker"),
            Some(ContextValue::String(value)) if value == "NaN"
        ));
        assert!(matches!(
            contexts.get("plain_marker"),
            Some(ContextValue::String(value)) if value == "NaN"
        ));
        assert!(message.materialize_context_data().is_err());

        assert!(matches!(
            template_token_context_value(&serde_json::json!({"type": 6, "num": "NaN"})).unwrap(),
            ContextValue::NonFinite(NonFinite::NaN)
        ));
        assert!(matches!(
            template_token_context_value(&serde_json::json!({"type": 0, "lit": "NaN"}))
                .unwrap(),
            ContextValue::String(value) if value == "NaN"
        ));
    }

    #[test]
    fn from_value_callback_errors_when_active_alias_search_hits_null_resource() {
        let error = AgentJobRequestMessage::from_value(serde_json::json!({
            "JobContainer": "db",
            "Resources": { "Containers": [null] }
        }))
        .unwrap_err();
        assert!(error.to_string().contains("null container resource"));
    }

    fn find_token_mapping_value<'a>(token: &'a Value, name: &str) -> Option<&'a Value> {
        token.get("map")?.as_array()?.iter().find_map(|pair| {
            let key = pair.get("key")?;
            let literal = key.get("lit")?.as_str()?;
            if clr_ordinal_ignore_case_eq(literal, name) {
                pair.get("value")
            } else {
                None
            }
        })
    }

    fn test_context_value(value: Value) -> ContextValue {
        ContextValue::from_json_case_sensitive(value).unwrap()
    }

    fn test_resource_properties(value: Value) -> ContextValue {
        ContextValue::from_json_root_case_insensitive(value).unwrap()
    }
}

//! Generic GitHub Action metadata detector.
//!
//! An action is a repository product, not a project-language unit.  The scan
//! therefore proves only the metadata runtime and local files the metadata
//! names; it never executes an action, shell, JavaScript, or Docker entrypoint.
//! Repository-owned consumer fixtures are attached later through the generic
//! `github-action-fixtures` unit contract.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use velnor_model::action_reference::{
    is_runner_local_action_reference, resolve_action_path, ActionImageReference,
    RepositoryActionReference,
};

use super::file_walk::is_test_support_path;
use super::{unit, RepositoryShape, ScanContext};
use crate::s2::{parent_path, shell_quote, UnitKind};

const MAX_ACTION_METADATA_BYTES: usize = 1024 * 1024;
const MAX_ACTION_METADATA_BYTES_U64: u64 = 1024 * 1024;
const MAX_ACTION_METADATA_DEPTH: usize = 64;
const MAX_ACTION_METADATA_NODES: usize = 50_000;
const MAX_ACTION_METADATA_EVENTS: usize = 1_000_000;
const MAX_ACTION_METADATA_ESTIMATED_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionSourceKind {
    Metadata,
    Dockerfile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActionSource {
    root: String,
    path: String,
    kind: ActionSourceKind,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ActionMetadata {
    #[serde(default)]
    inputs: serde_yaml::Value,
    #[serde(default)]
    outputs: serde_yaml::Value,
    runs: ActionRuns,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ActionRuns {
    using: String,
    #[serde(default)]
    main: Option<String>,
    #[serde(default)]
    pre: Option<String>,
    #[serde(default)]
    post: Option<String>,
    #[serde(default, rename = "pre-if", alias = "preIf")]
    pre_if: Option<String>,
    #[serde(default, rename = "post-if", alias = "postIf")]
    post_if: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default, rename = "pre-entrypoint", alias = "preEntrypoint")]
    pre_entrypoint: Option<String>,
    #[serde(default, rename = "post-entrypoint", alias = "postEntrypoint")]
    post_entrypoint: Option<String>,
    #[serde(default)]
    args: serde_yaml::Value,
    #[serde(default)]
    env: serde_yaml::Value,
    #[serde(default)]
    steps: Option<Vec<ActionStep>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ActionStep {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    uses: Option<String>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default, rename = "if")]
    condition: Option<String>,
    #[serde(default, rename = "working-directory", alias = "workingDirectory")]
    working_directory: Option<String>,
    #[serde(default, rename = "continue-on-error", alias = "continueOnError")]
    continue_on_error: Option<serde_yaml::Value>,
    #[serde(default)]
    with: serde_yaml::Value,
    #[serde(default)]
    env: serde_yaml::Value,
}

/// Decode scalar keys and schema String values into Runner-compatible text
/// before building the scanner's string-keyed `Value` tree. The decoder keeps
/// map roles so schema keys get non-empty checks while loose nested maps remain
/// permissive.
#[derive(Debug)]
struct RunnerYamlValue(serde_yaml::Value);

impl<'de> Deserialize<'de> for RunnerYamlValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RunnerYamlValueSeed {
            role: RunnerYamlMapRole::Manifest,
        }
        .deserialize(deserializer)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunnerYamlMapRole {
    Manifest,
    Inputs,
    InputDefinition,
    Outputs,
    OutputDefinition,
    Runs,
    RunEnvironment,
    StringSequence,
    CompositeSteps,
    CompositeStep,
    StepWith,
    StepEnvironment,
    ScalarString,
    Any,
}

impl RunnerYamlMapRole {
    fn requires_non_empty_keys(self) -> bool {
        !matches!(self, Self::Any)
    }

    fn value_role(self, key: &str) -> Self {
        match self {
            Self::RunEnvironment | Self::StepWith | Self::StepEnvironment => Self::ScalarString,
            Self::Manifest if key == "inputs" => Self::Inputs,
            Self::Manifest if key == "outputs" => Self::Outputs,
            Self::Manifest if key == "runs" => Self::Runs,
            Self::Manifest if matches!(key, "name" | "description") => Self::ScalarString,
            Self::Inputs => Self::InputDefinition,
            Self::Outputs => Self::OutputDefinition,
            Self::InputDefinition if runner_ordinal_ignore_case_eq(key, "default") => {
                Self::ScalarString
            }
            Self::OutputDefinition if matches!(key, "description" | "value") => Self::ScalarString,
            Self::Runs if key == "env" => Self::RunEnvironment,
            Self::Runs if key == "steps" => Self::CompositeSteps,
            Self::Runs if key == "args" => Self::StringSequence,
            Self::Runs
                if matches!(
                    key,
                    "using"
                        | "image"
                        | "entrypoint"
                        | "pre-entrypoint"
                        | "pre-if"
                        | "post-entrypoint"
                        | "post-if"
                        | "main"
                        | "pre"
                        | "post"
                ) =>
            {
                Self::ScalarString
            }
            Self::CompositeStep if key == "with" => Self::StepWith,
            Self::CompositeStep if key == "env" => Self::StepEnvironment,
            Self::CompositeStep
                if matches!(
                    key,
                    "id" | "name" | "if" | "run" | "shell" | "working-directory" | "uses"
                ) =>
            {
                Self::ScalarString
            }
            _ => Self::Any,
        }
    }

    fn sequence_element_role(self) -> Self {
        match self {
            Self::CompositeSteps => Self::CompositeStep,
            Self::StringSequence => Self::ScalarString,
            _ => Self::Any,
        }
    }
}

struct RunnerYamlValueSeed {
    role: RunnerYamlMapRole,
}

impl<'de> DeserializeSeed<'de> for RunnerYamlValueSeed {
    type Value = RunnerYamlValue;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(RunnerYamlValueVisitor { role: self.role })
    }
}

struct RunnerYamlValueVisitor {
    role: RunnerYamlMapRole,
}

impl<'de> Visitor<'de> for RunnerYamlValueVisitor {
    type Value = RunnerYamlValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a YAML value with Runner-compatible mapping keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.scalar(serde_yaml::Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.scalar(serde_yaml::Value::from(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.scalar(serde_yaml::Value::from(value))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.scalar(serde_yaml::Value::from(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.scalar(serde_yaml::Value::from(value))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.scalar(serde_yaml::Value::from(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.scalar(serde_yaml::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.scalar(serde_yaml::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RunnerYamlValueSeed { role: self.role }.deserialize(deserializer)
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RunnerYamlValueSeed { role: self.role }.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let role = self.role.sequence_element_role();
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(RunnerYamlValueSeed { role })? {
            values.push(value.0);
        }
        Ok(RunnerYamlValue(serde_yaml::Value::Sequence(values)))
    }

    fn visit_map<A>(self, mut mapping: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let role = self.role;
        let mut keys = BTreeSet::new();
        let mut values = serde_yaml::Mapping::new();
        while let Some(key) = mapping.next_key::<RunnerYamlMapKey>()? {
            if role.requires_non_empty_keys() && key.0.is_empty() {
                return Err(A::Error::custom(
                    "actions/runner requires non-empty strings for action schema mapping keys",
                ));
            }
            if !keys.insert(runner_ordinal_ignore_case_key(&key.0)) {
                return Err(A::Error::custom(format!(
                    "duplicate YAML mapping key after Runner case-insensitive matching: `{}`",
                    key.0
                )));
            }
            let value_role = role.value_role(&key.0);
            let value: RunnerYamlValue =
                mapping.next_value_seed(RunnerYamlValueSeed { role: value_role })?;
            values.insert(key.0, value.0);
        }
        Ok(RunnerYamlValue(serde_yaml::Value::Mapping(values)))
    }
}

impl RunnerYamlValueVisitor {
    fn scalar<E>(self, value: serde_yaml::Value) -> Result<RunnerYamlValue, E>
    where
        E: serde::de::Error,
    {
        if self.role == RunnerYamlMapRole::ScalarString {
            runner_scalar_to_string(value)
                .map(|value| RunnerYamlValue(serde_yaml::Value::String(value)))
                .map_err(E::custom)
        } else {
            Ok(RunnerYamlValue(value))
        }
    }
}

/// Runner's ordinal-ignore-case matching folds one Unicode scalar at a time.
/// Dotless i and long s remain distinct in the runner's invariant comparison.
fn runner_ordinal_ignore_case_key(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if matches!(character, '\u{0131}' | '\u{017f}') {
                return character;
            }
            let mut uppercase = character.to_uppercase();
            match (uppercase.next(), uppercase.next()) {
                (Some(single), None) => single,
                _ => character,
            }
        })
        .collect()
}

fn runner_ordinal_ignore_case_eq(left: &str, right: &str) -> bool {
    runner_ordinal_ignore_case_key(left) == runner_ordinal_ignore_case_key(right)
}

struct RunnerYamlMapKey(String);

impl<'de> Deserialize<'de> for RunnerYamlMapKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(RunnerYamlMapKeyVisitor)
    }
}

struct RunnerYamlMapKeyVisitor;

impl<'de> Visitor<'de> for RunnerYamlMapKeyVisitor {
    type Value = RunnerYamlMapKey;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a scalar YAML mapping key")
    }

    fn visit_str<E>(self, key: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(RunnerYamlMapKey(key.to_owned()))
    }

    fn visit_string<E>(self, key: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(RunnerYamlMapKey(key))
    }

    fn visit_bool<E>(self, key: bool) -> Result<Self::Value, E> {
        Ok(RunnerYamlMapKey(key.to_string()))
    }

    fn visit_i64<E>(self, key: i64) -> Result<Self::Value, E> {
        Ok(RunnerYamlMapKey(runner_number_to_string(
            serde_yaml::Number::from(key).as_f64(),
        )))
    }

    fn visit_u64<E>(self, key: u64) -> Result<Self::Value, E> {
        Ok(RunnerYamlMapKey(runner_number_to_string(
            serde_yaml::Number::from(key).as_f64(),
        )))
    }

    fn visit_f64<E>(self, key: f64) -> Result<Self::Value, E> {
        Ok(RunnerYamlMapKey(runner_number_to_string(key)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(RunnerYamlMapKey(String::new()))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(RunnerYamlMapKey(String::new()))
    }

    fn visit_seq<A>(self, _: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        Err(A::Error::custom(
            "actions/runner does not accept collection mapping keys",
        ))
    }

    fn visit_map<A>(self, _: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        Err(A::Error::custom(
            "actions/runner does not accept collection mapping keys",
        ))
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RunnerYamlMapKey::deserialize(deserializer)
    }
}

fn runner_scalar_to_string(value: serde_yaml::Value) -> Result<String, &'static str> {
    match value {
        serde_yaml::Value::Null => Ok(String::new()),
        serde_yaml::Value::Bool(value) => Ok(value.to_string()),
        serde_yaml::Value::Number(value) => Ok(runner_number_to_string(value.as_f64())),
        serde_yaml::Value::String(value) => Ok(value),
        serde_yaml::Value::Sequence(_)
        | serde_yaml::Value::Mapping(_)
        | serde_yaml::Value::Tagged(_) => Err("actions/runner requires a scalar string value"),
    }
}

fn runner_yaml_scalar_source(value: &serde_yaml::Value) -> Option<String> {
    if let serde_yaml::Value::String(value) = value {
        let mut quoted = String::with_capacity(value.len().saturating_add(2));
        quoted.push('"');
        for character in value.chars() {
            match character {
                '"' => quoted.push_str("\\\""),
                '\\' => quoted.push_str("\\\\"),
                '\0' => quoted.push_str("\\0"),
                '\u{0008}' => quoted.push_str("\\b"),
                '\t' => quoted.push_str("\\t"),
                '\n' => quoted.push_str("\\n"),
                '\u{000c}' => quoted.push_str("\\f"),
                '\r' => quoted.push_str("\\r"),
                '\u{001b}' => quoted.push_str("\\e"),
                '\u{0085}' => quoted.push_str("\\N"),
                '\u{2028}' => quoted.push_str("\\L"),
                '\u{2029}' => quoted.push_str("\\P"),
                character if character.is_control() => {
                    use std::fmt::Write as _;
                    let _ = write!(quoted, "\\u{:04X}", character as u32);
                }
                character => quoted.push(character),
            }
        }
        quoted.push('"');
        return Some(quoted);
    }
    serde_yaml::to_string(value)
        .ok()
        .map(|source| source.trim_end_matches(['\r', '\n']).to_owned())
}

fn runner_yaml_scalar_source_end(
    kind: serde_yaml::cst::SyntaxKind,
    raw: &str,
    end: usize,
) -> usize {
    use serde_yaml::cst::SyntaxKind;

    if matches!(kind, SyntaxKind::LiteralScalar | SyntaxKind::FoldedScalar) {
        return end;
    }
    end.saturating_sub(
        raw.len()
            .saturating_sub(raw.trim_end_matches([' ', '\t', '\r', '\n']).len()),
    )
}

/// Match actions/runner's `NumberToken.ToString()` (`G15`, invariant culture).
fn runner_number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == f64::INFINITY {
        return "Infinity".to_owned();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    let sign = if value.is_sign_negative() { "-" } else { "" };
    let scientific = format!("{:.14e}", value.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return value.to_string();
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return value.to_string();
    };
    let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');

    if !(-4..15).contains(&exponent) {
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{mantissa}E{exponent_sign}{:02}", exponent.abs());
    }

    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let decimal_index = exponent + 1;
    let mut result = if decimal_index <= 0 {
        let Some(leading_zeroes) = usize::try_from(decimal_index.unsigned_abs()).ok() else {
            return value.to_string();
        };
        format!("0.{}{}", "0".repeat(leading_zeroes), digits)
    } else {
        let Some(decimal_index) = usize::try_from(decimal_index).ok() else {
            return value.to_string();
        };
        if decimal_index >= digits.len() {
            format!("{}{}", digits, "0".repeat(decimal_index - digits.len()))
        } else {
            format!("{}.{}", &digits[..decimal_index], &digits[decimal_index..])
        }
    };
    if result.contains('.') {
        while result.ends_with('0') {
            result.pop();
        }
        if result.ends_with('.') {
            result.pop();
        }
    }
    format!("{sign}{result}")
}

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the source preflight records independent runner syntax violations"
)]
struct RunnerYamlSyntax {
    has_anchor_or_alias: bool,
    has_complex_mapping_key: bool,
    has_duplicate_mapping_key: bool,
    exceeded_budget: bool,
    exceeded_depth: bool,
    event_count: usize,
    node_count: usize,
    estimated_bytes: usize,
    pending_tag: Option<String>,
    pending_tag_start: Option<usize>,
    pending_tag_end: Option<usize>,
    tag_error: Option<String>,
    tag_directives: BTreeMap<String, String>,
    source_replacements: Vec<(usize, usize, String)>,
}

impl RunnerYamlSyntax {
    fn count_event(&mut self) -> bool {
        self.event_count = match self.event_count.checked_add(1) {
            Some(count) if count <= MAX_ACTION_METADATA_EVENTS => count,
            _ => {
                self.exceeded_budget = true;
                return false;
            }
        };
        true
    }

    fn count_node(&mut self) -> bool {
        self.node_count = match self.node_count.checked_add(1) {
            Some(count) if count <= MAX_ACTION_METADATA_NODES => count,
            _ => {
                self.exceeded_budget = true;
                return false;
            }
        };
        true
    }
}

fn runner_yaml_collection_kind(kind: serde_yaml::cst::SyntaxKind) -> bool {
    use serde_yaml::cst::SyntaxKind;

    matches!(
        kind,
        SyntaxKind::BlockMapping
            | SyntaxKind::BlockSequence
            | SyntaxKind::FlowMapping
            | SyntaxKind::FlowSequence
    )
}

fn runner_action_parser_config() -> serde_yaml::ParserConfig {
    serde_yaml::ParserConfig::new()
        .max_document_length(MAX_ACTION_METADATA_BYTES)
        .max_depth(MAX_ACTION_METADATA_DEPTH)
        .max_nodes(MAX_ACTION_METADATA_NODES)
        .max_events(MAX_ACTION_METADATA_EVENTS)
        .max_total_scalar_bytes(MAX_ACTION_METADATA_BYTES)
        .max_mapping_keys(MAX_ACTION_METADATA_NODES)
        .max_sequence_length(MAX_ACTION_METADATA_NODES)
        .max_alias_expansions(0)
        .max_documents(1)
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Last)
        .merge_key_policy(serde_yaml::MergeKeyPolicy::AsOrdinary)
        .with_policy(serde_yaml::policy::DenyAnchors)
}

fn runner_tag_directives(contents: &str) -> Result<BTreeMap<String, String>, &'static str> {
    let mut directives = BTreeMap::new();
    for line in contents.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if is_runner_document_start(line) {
            break;
        }
        // YAML directives are only valid in the document prolog and must
        // start at column zero. In particular, never treat `%TAG` text inside
        // an indented block scalar as a directive.
        if !line.starts_with('%') {
            break;
        }
        let mut fields = line.split_whitespace();
        if fields.next() != Some("%TAG") {
            continue;
        }
        let (Some(handle), Some(prefix)) = (fields.next(), fields.next()) else {
            continue;
        };
        if fields.next().is_some_and(|field| !field.starts_with('#')) {
            continue;
        }
        if !handle.starts_with('!') {
            continue;
        }
        if directives
            .insert(handle.to_owned(), prefix.to_owned())
            .is_some()
        {
            return Err("duplicate YAML %TAG directive handle");
        }
    }
    Ok(directives)
}

fn is_runner_document_start(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("---") else {
        return false;
    };
    rest.is_empty()
        || rest
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_whitespace() || character == '#')
}

fn decode_runner_tag_uri(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = bytes
            .get(index + 1)
            .and_then(|byte| (*byte as char).to_digit(16))?;
        let low = bytes
            .get(index + 2)
            .and_then(|byte| (*byte as char).to_digit(16))?;
        decoded.push(u8::try_from(high * 16 + low).ok()?);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

fn expand_runner_tag(raw_tag: &str, directives: &BTreeMap<String, String>) -> Option<String> {
    if let Some(uri) = raw_tag
        .strip_prefix("!<")
        .and_then(|tag| tag.strip_suffix('>'))
    {
        return decode_runner_tag_uri(uri);
    }
    if let Some(suffix) = raw_tag.strip_prefix("!!") {
        // A document may override the default secondary handle. Match
        // Runner's expanded tag URI before applying its built-in default.
        let prefix = directives
            .get("!!")
            .map_or("tag:yaml.org,2002:", String::as_str);
        return decode_runner_tag_uri(&format!("{prefix}{suffix}"));
    }

    let separator = raw_tag[1..].find('!').map(|index| index + 1);
    let (handle, suffix) = separator.map_or(("!", &raw_tag[1..]), |index| {
        (&raw_tag[..=index], &raw_tag[index + 1..])
    });
    directives
        .get(handle)
        .and_then(|prefix| decode_runner_tag_uri(&format!("{prefix}{suffix}")))
}

fn runner_tagged_scalar(
    tag: &str,
    kind: serde_yaml::cst::SyntaxKind,
    raw: &str,
) -> Option<serde_yaml::Value> {
    use serde_yaml::cst::SyntaxKind;

    const YAML_TAG_PREFIX: &str = "tag:yaml.org,2002:";
    let plain = kind == SyntaxKind::PlainScalar;
    let lexical = raw.trim();
    match tag.strip_prefix(YAML_TAG_PREFIX)? {
        "str" => runner_tagged_string(raw, kind).map(serde_yaml::Value::String),
        "bool" if plain => match lexical {
            "true" | "True" | "TRUE" => Some(serde_yaml::Value::Bool(true)),
            "false" | "False" | "FALSE" => Some(serde_yaml::Value::Bool(false)),
            _ => None,
        },
        "int" if plain => runner_tagged_integer(lexical),
        "float" if plain => runner_tagged_float(lexical),
        "null" if plain && runner_null_lexeme(lexical) => Some(serde_yaml::Value::Null),
        _ => None,
    }
}

fn runner_tagged_string(raw: &str, kind: serde_yaml::cst::SyntaxKind) -> Option<String> {
    use serde_yaml::cst::SyntaxKind;

    if kind == SyntaxKind::PlainScalar {
        // Plain scalars such as `false` are implicitly typed by YAML; an
        // explicit Runner string tag must preserve their source spelling.
        return Some(match serde_yaml::from_str::<serde_yaml::Value>(raw).ok() {
            Some(serde_yaml::Value::String(value)) => value,
            _ => raw.trim_end_matches([' ', '\t', '\r', '\n']).to_owned(),
        });
    }
    serde_yaml::from_str::<String>(raw).ok()
}

fn runner_null_lexeme(raw: &str) -> bool {
    matches!(raw, "" | "~" | "null" | "Null" | "NULL")
}

fn runner_tagged_integer(raw: &str) -> Option<serde_yaml::Value> {
    let number = if let Some(hex) = raw.strip_prefix("0x") {
        if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let bits = u32::from_str_radix(hex, 16).ok()?;
        f64::from(i32::from_ne_bytes(bits.to_ne_bytes()))
    } else if let Some(octal) = raw.strip_prefix("0o") {
        if octal.is_empty() || !octal.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
            return None;
        }
        f64::from(i32::from_str_radix(octal, 8).ok()?)
    } else {
        if !runner_float_lexeme(raw) {
            return None;
        }
        raw.parse::<f64>().ok()?
    };
    Some(serde_yaml::Value::Number(number.into()))
}

fn runner_radix_integer(raw: &str) -> Option<serde_yaml::Value> {
    let raw = raw.trim();
    (raw.starts_with("0x") || raw.starts_with("0o")).then(|| runner_tagged_integer(raw))?
}

fn runner_tagged_float(raw: &str) -> Option<serde_yaml::Value> {
    let number = match raw {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => f64::INFINITY,
        "-.inf" | "-.Inf" | "-.INF" => f64::NEG_INFINITY,
        ".nan" | ".NaN" | ".NAN" => f64::NAN,
        _ => {
            if !runner_float_lexeme(raw) {
                return None;
            }
            raw.parse::<f64>().ok()?
        }
    };
    Some(serde_yaml::Value::Number(number.into()))
}

fn runner_float_lexeme(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut index = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let integer_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let has_integer = index > integer_start;
    let has_dot = bytes.get(index) == Some(&b'.');
    let mut has_fraction = false;
    if has_dot {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        has_fraction = index > fraction_start;
    }
    if !has_integer && !has_fraction {
        return false;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    index == bytes.len()
}

fn accept_empty_runner_tag(syntax: &mut RunnerYamlSyntax) {
    let Some(raw_tag) = syntax.pending_tag.take() else {
        return;
    };
    let Some(tag_start) = syntax.pending_tag_start.take() else {
        syntax.tag_error = Some("YAML tag has no source position".into());
        return;
    };
    let Some(tag_end) = syntax.pending_tag_end.take() else {
        syntax.tag_error = Some("YAML tag has no source range".into());
        return;
    };
    match expand_runner_tag(&raw_tag, &syntax.tag_directives).as_deref() {
        Some("tag:yaml.org,2002:null") => {
            // serde_yaml's value decoder does not accept explicit tags. Runner
            // treats an empty !!null scalar as an ordinary null value, so
            // remove only the tag token and preserve the empty source value.
            syntax
                .source_replacements
                .push((tag_start, tag_end, String::new()));
        }
        Some("tag:yaml.org,2002:str") => {
            // ActionManifestManager's string fast path accepts a tag-only
            // scalar as an empty string. Strip the tag and insert a quoted
            // empty scalar so the scanner's second parse carries that value.
            syntax
                .source_replacements
                .push((tag_start, tag_end, "\"\"".to_owned()));
        }
        _ => {
            syntax.tag_error = Some(format!(
                "YAML scalar tag `{raw_tag}` is unsupported or has an invalid empty scalar"
            ));
        }
    }
}

fn inspect_runner_yaml_syntax(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    base: usize,
    depth: usize,
    syntax: &mut RunnerYamlSyntax,
) {
    use serde_yaml::cst::SyntaxKind;

    if syntax.exceeded_budget || syntax.tag_error.is_some() {
        return;
    }

    if !syntax.count_event() {
        return;
    }
    let yaml_depth = if runner_yaml_collection_kind(node.kind()) {
        let depth = depth.saturating_add(1);
        if depth > MAX_ACTION_METADATA_DEPTH {
            syntax.exceeded_depth = true;
            return;
        }
        if !syntax.count_node() {
            return;
        }
        depth
    } else {
        depth
    };

    match node.kind() {
        SyntaxKind::BlockMapping => {
            inspect_runner_block_mapping_keys(node, source, base, syntax);
        }
        SyntaxKind::MappingEntry => {
            inspect_runner_mapping_entry_key(node, syntax);
        }
        SyntaxKind::FlowMapping => {
            inspect_runner_flow_mapping_keys(node, source, base, syntax);
        }
        _ => {}
    }

    let mut offset = base;
    for child in node.children() {
        inspect_runner_yaml_child(child, source, offset, yaml_depth, syntax);
        offset += child.text_len();
    }
}

fn inspect_runner_yaml_child(
    child: &serde_yaml::cst::GreenChild,
    source: &str,
    offset: usize,
    depth: usize,
    syntax: &mut RunnerYamlSyntax,
) {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    match child {
        GreenChild::Node(child_node) => {
            if syntax.pending_tag.is_some() && child_node.kind() == SyntaxKind::MappingEntry {
                // A tag at the end of a mapping entry with no scalar token
                // belongs to that entry's empty value.
                accept_empty_runner_tag(syntax);
            }
            if syntax.pending_tag.is_some() && runner_yaml_collection_kind(child_node.kind()) {
                discard_runner_collection_tag(syntax);
            }
            inspect_runner_yaml_syntax(child_node, source, offset, depth, syntax);
        }
        GreenChild::Token { kind, len } => {
            if !syntax.count_event() {
                return;
            }
            let end = offset.saturating_add(*len as usize);
            let token = source.get(offset..end).unwrap_or_default();
            inspect_runner_yaml_token(*kind, token, offset, end, syntax);
        }
    }
}

fn discard_runner_collection_tag(syntax: &mut RunnerYamlSyntax) {
    // ActionManifestManager ignores tags when the tagged node is a
    // collection. Remove the tag before the second, Runner-compatible parse.
    let (Some(_raw_tag), Some(tag_start), Some(tag_end)) = (
        syntax.pending_tag.take(),
        syntax.pending_tag_start.take(),
        syntax.pending_tag_end.take(),
    ) else {
        return;
    };
    syntax
        .source_replacements
        .push((tag_start, tag_end, String::new()));
}

fn inspect_runner_yaml_token(
    kind: serde_yaml::cst::SyntaxKind,
    token: &str,
    offset: usize,
    token_end: usize,
    syntax: &mut RunnerYamlSyntax,
) {
    use serde_yaml::cst::SyntaxKind;

    match kind {
        SyntaxKind::AnchorMark | SyntaxKind::AliasMark => {
            syntax.has_anchor_or_alias = true;
        }
        SyntaxKind::TagMark => {
            if syntax.pending_tag.is_some() {
                syntax.tag_error = Some("nested YAML tags are not supported".into());
                return;
            }
            syntax.pending_tag = Some(token.to_owned());
            syntax.pending_tag_start = Some(offset);
            syntax.pending_tag_end = Some(token_end);
        }
        kind if runner_yaml_scalar_kind(kind) => {
            if !syntax.count_node() {
                return;
            }
            inspect_runner_yaml_scalar(kind, token, offset, token_end, syntax);
        }
        _ => inspect_runner_yaml_pending_tag(kind, syntax),
    }
}

fn runner_yaml_scalar_kind(kind: serde_yaml::cst::SyntaxKind) -> bool {
    use serde_yaml::cst::SyntaxKind;

    matches!(
        kind,
        SyntaxKind::PlainScalar
            | SyntaxKind::SingleQuotedScalar
            | SyntaxKind::DoubleQuotedScalar
            | SyntaxKind::LiteralScalar
            | SyntaxKind::FoldedScalar
    )
}

fn inspect_runner_yaml_scalar(
    kind: serde_yaml::cst::SyntaxKind,
    token: &str,
    offset: usize,
    token_end: usize,
    syntax: &mut RunnerYamlSyntax,
) {
    if let Some(raw_tag) = syntax.pending_tag.take() {
        let Some(replacement_start) = syntax.pending_tag_start.take() else {
            syntax.tag_error = Some("YAML tag has no source position".into());
            return;
        };
        let Some(_tag_end) = syntax.pending_tag_end.take() else {
            syntax.tag_error = Some("YAML tag has no source range".into());
            return;
        };
        let Some(tag) = expand_runner_tag(&raw_tag, &syntax.tag_directives) else {
            syntax.tag_error = Some(format!(
                "YAML scalar tag `{raw_tag}` is unsupported or has an invalid scalar"
            ));
            return;
        };
        let Some(value) = runner_tagged_scalar(&tag, kind, token) else {
            syntax.tag_error = Some(format!(
                "YAML scalar tag `{raw_tag}` is unsupported or has an invalid scalar"
            ));
            return;
        };
        let Some(replacement) = runner_yaml_scalar_source(&value) else {
            syntax.tag_error = Some(format!("YAML scalar tag `{raw_tag}` cannot be normalized"));
            return;
        };
        let replacement_end = runner_yaml_scalar_source_end(kind, token, token_end);
        syntax
            .source_replacements
            .push((replacement_start, replacement_end, replacement));
        return;
    }

    if kind == serde_yaml::cst::SyntaxKind::PlainScalar {
        inspect_runner_yaml_plain_scalar(token, offset, token_end, syntax);
    }
}

fn inspect_runner_yaml_plain_scalar(
    token: &str,
    offset: usize,
    token_end: usize,
    syntax: &mut RunnerYamlSyntax,
) {
    let trimmed = token.trim();
    let value = if trimmed.starts_with("0X") || trimmed.starts_with("0O") {
        Some(serde_yaml::Value::String(trimmed.to_owned()))
    } else if trimmed.starts_with("0x") || trimmed.starts_with("0o") {
        let Some(value) = runner_radix_integer(token) else {
            syntax.tag_error = Some(format!(
                "YAML radix integer `{token}` is outside actions/runner's supported range"
            ));
            return;
        };
        Some(value)
    } else if runner_float_lexeme(trimmed) {
        let Ok(number) = trimmed.parse::<f64>() else {
            syntax.tag_error = Some(format!(
                "YAML decimal number `{trimmed}` cannot be parsed by actions/runner"
            ));
            return;
        };
        Some(serde_yaml::Value::Number(number.into()))
    } else {
        None
    };

    let Some(value) = value else {
        return;
    };
    let Some(replacement) = runner_yaml_scalar_source(&value) else {
        return;
    };
    syntax.source_replacements.push((
        offset,
        runner_yaml_scalar_source_end(serde_yaml::cst::SyntaxKind::PlainScalar, token, token_end),
        replacement,
    ));
}

fn inspect_runner_yaml_pending_tag(
    kind: serde_yaml::cst::SyntaxKind,
    syntax: &mut RunnerYamlSyntax,
) {
    use serde_yaml::cst::SyntaxKind;

    if syntax.pending_tag.is_none() {
        return;
    }
    if matches!(
        kind,
        SyntaxKind::Comma | SyntaxKind::CloseBracket | SyntaxKind::CloseBrace
    ) {
        accept_empty_runner_tag(syntax);
    } else if !matches!(
        kind,
        SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::Comment
    ) {
        syntax.tag_error =
            Some("actions/runner accepts standard scalar YAML tags only on scalars".into());
    }
}

fn inspect_runner_block_mapping_keys(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    base: usize,
    syntax: &mut RunnerYamlSyntax,
) {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let mut keys = BTreeSet::new();
    let mut offset = base;
    for child in node.children() {
        if let GreenChild::Node(entry) = child
            && entry.kind() == SyntaxKind::MappingEntry
        {
            let raw_key = runner_mapping_entry_key_source(entry, source, offset);
            if let Some(raw_key) = raw_key {
                let normalized = runner_source_mapping_key(&raw_key, &syntax.tag_directives);
                if let Some(key) = normalized.as_deref() {
                    inspect_runner_mapping_key(key, &mut keys, syntax);
                } else {
                    syntax.has_complex_mapping_key = true;
                }
            }
        }
        offset += child.text_len();
    }
}

fn runner_mapping_entry_key_source(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    base: usize,
) -> Option<String> {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let mut raw_key = String::new();
    let mut offset = base;
    for child in node.children() {
        match child {
            GreenChild::Token {
                kind: SyntaxKind::ColonIndicator,
                ..
            } => break,
            GreenChild::Node(_) => return None,
            GreenChild::Token { kind, len } => {
                if !runner_mapping_key_trivia(*kind) {
                    let end = offset + *len as usize;
                    let token = source.get(offset..end)?;
                    if !raw_key.is_empty() {
                        raw_key.push(' ');
                    }
                    raw_key.push_str(token);
                }
            }
        }
        offset += child.text_len();
    }
    Some(raw_key)
}

fn inspect_runner_flow_mapping_keys(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    base: usize,
    syntax: &mut RunnerYamlSyntax,
) {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let mut keys = BTreeSet::new();
    let mut in_key = false;
    let mut key_has_collection = false;
    let mut raw_key = String::new();
    let mut offset = base;
    for child in node.children() {
        match child {
            GreenChild::Token { kind, len } => match kind {
                SyntaxKind::OpenBrace => {
                    in_key = true;
                    key_has_collection = false;
                    raw_key.clear();
                }
                SyntaxKind::ColonIndicator if in_key => {
                    if key_has_collection {
                        syntax.has_complex_mapping_key = true;
                    } else {
                        let normalized =
                            runner_source_mapping_key(&raw_key, &syntax.tag_directives);
                        if let Some(key) = normalized.as_deref() {
                            inspect_runner_mapping_key(key, &mut keys, syntax);
                        } else {
                            syntax.has_complex_mapping_key = true;
                        }
                    }
                    in_key = false;
                }
                SyntaxKind::Comma => {
                    if in_key && !raw_key.is_empty() {
                        if key_has_collection {
                            syntax.has_complex_mapping_key = true;
                        } else {
                            let normalized =
                                runner_source_mapping_key(&raw_key, &syntax.tag_directives);
                            if let Some(key) = normalized.as_deref() {
                                inspect_runner_mapping_key(key, &mut keys, syntax);
                            } else {
                                syntax.has_complex_mapping_key = true;
                            }
                        }
                    }
                    in_key = true;
                    key_has_collection = false;
                    raw_key.clear();
                }
                SyntaxKind::CloseBrace => {
                    if in_key && !raw_key.is_empty() {
                        if key_has_collection {
                            syntax.has_complex_mapping_key = true;
                        } else {
                            let normalized =
                                runner_source_mapping_key(&raw_key, &syntax.tag_directives);
                            if let Some(key) = normalized.as_deref() {
                                inspect_runner_mapping_key(key, &mut keys, syntax);
                            } else {
                                syntax.has_complex_mapping_key = true;
                            }
                        }
                    }
                    break;
                }
                _ if in_key && !runner_mapping_key_trivia(*kind) => {
                    let end = offset + *len as usize;
                    if let Some(token) = source.get(offset..end) {
                        if !raw_key.is_empty() {
                            raw_key.push(' ');
                        }
                        raw_key.push_str(token);
                    }
                }
                _ => {}
            },
            GreenChild::Node(_) if in_key => {
                key_has_collection = true;
                syntax.has_complex_mapping_key = true;
            }
            GreenChild::Node(_) => {}
        }
        offset += child.text_len();
    }
}

fn runner_mapping_key_trivia(kind: serde_yaml::cst::SyntaxKind) -> bool {
    matches!(
        kind,
        serde_yaml::cst::SyntaxKind::Whitespace
            | serde_yaml::cst::SyntaxKind::Newline
            | serde_yaml::cst::SyntaxKind::Comment
            | serde_yaml::cst::SyntaxKind::QuestionIndicator
    )
}

fn inspect_runner_mapping_key(
    key: &str,
    keys: &mut BTreeSet<String>,
    syntax: &mut RunnerYamlSyntax,
) {
    if !keys.insert(runner_ordinal_ignore_case_key(key)) {
        syntax.has_duplicate_mapping_key = true;
    }
}

fn runner_source_mapping_key(
    raw_key: &str,
    tag_directives: &BTreeMap<String, String>,
) -> Option<String> {
    let raw_key = raw_key.trim();
    if raw_key.is_empty() {
        return Some(String::new());
    }

    if raw_key.starts_with('!') {
        let tag_end = if raw_key.starts_with("!<") {
            raw_key.find('>')? + 1
        } else {
            raw_key.find(char::is_whitespace).unwrap_or(raw_key.len())
        };
        let raw_tag = &raw_key[..tag_end];
        let tag = expand_runner_tag(raw_tag, tag_directives)?;
        let scalar = raw_key[tag_end..].trim_start();
        let kind = match scalar.as_bytes().first() {
            Some(b'\'') => serde_yaml::cst::SyntaxKind::SingleQuotedScalar,
            Some(b'"') => serde_yaml::cst::SyntaxKind::DoubleQuotedScalar,
            _ => serde_yaml::cst::SyntaxKind::PlainScalar,
        };
        let value = runner_tagged_scalar(&tag, kind, scalar)?;
        return runner_scalar_to_string(value).ok();
    }

    if raw_key.starts_with("0x") || raw_key.starts_with("0o") {
        return runner_scalar_to_string(runner_radix_integer(raw_key)?).ok();
    }
    if raw_key.starts_with("0X") || raw_key.starts_with("0O") {
        return Some(raw_key.to_owned());
    }

    let parser_config =
        serde_yaml::ParserConfig::new().merge_key_policy(serde_yaml::MergeKeyPolicy::AsOrdinary);
    serde_yaml::from_str_with_config::<RunnerYamlMapKey>(raw_key, &parser_config)
        .ok()
        .map(|key| key.0)
}

fn inspect_runner_mapping_entry_key(
    node: &serde_yaml::cst::GreenNode,
    syntax: &mut RunnerYamlSyntax,
) {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    for child in node.children() {
        match child {
            GreenChild::Token {
                kind: SyntaxKind::ColonIndicator,
                ..
            } => break,
            GreenChild::Node(_) => {
                syntax.has_complex_mapping_key = true;
                return;
            }
            GreenChild::Token { .. } => {}
        }
    }
}

fn normalize_runner_yaml_source(
    contents: &str,
    replacements: &[(usize, usize, String)],
) -> Result<Option<String>, crate::s2::GeneratorError> {
    if replacements.is_empty() {
        return Ok(None);
    }
    let mut normalized = String::with_capacity(contents.len());
    let mut cursor = 0;
    for (start, end, replacement) in replacements {
        if *start < cursor || *end < *start || *end > contents.len() {
            return Err(crate::s2::GeneratorError::usage(
                "GitHub Action metadata YAML scalar source ranges overlap or exceed the document",
            ));
        }
        normalized.push_str(&contents[cursor..*start]);
        normalized.push_str(replacement);
        cursor = *end;
        if normalized.len() > MAX_ACTION_METADATA_BYTES {
            return Err(crate::s2::GeneratorError::usage(format!(
                "normalized GitHub Action metadata exceeds {MAX_ACTION_METADATA_BYTES} bytes"
            )));
        }
    }
    normalized.push_str(&contents[cursor..]);
    if normalized.len() > MAX_ACTION_METADATA_BYTES {
        return Err(crate::s2::GeneratorError::usage(format!(
            "normalized GitHub Action metadata exceeds {MAX_ACTION_METADATA_BYTES} bytes"
        )));
    }
    Ok(Some(normalized))
}

fn validate_runner_yaml_syntax(
    contents: &str,
    metadata_file: &Path,
) -> Result<RunnerYamlSyntax, crate::s2::GeneratorError> {
    // Noyalib's CST parser materializes a second tree and has no per-parse
    // budget parameter. Validate the source through its bounded Value parser
    // first, then drop that tree before building the CST for source details.
    serde_yaml::from_str_with_config::<serde_yaml::Value>(contents, &runner_action_parser_config())
        .map_err(|error| {
            let detail = if error.to_string().contains("DenyAnchors") {
                "actions/runner does not support YAML anchors or aliases".to_owned()
            } else {
                format!("within scanner limits: {error}")
            };
            crate::s2::GeneratorError::usage(format!(
                "parse GitHub Action metadata {}: {detail}",
                metadata_file.display()
            ))
        })?;
    let document = serde_yaml::cst::parse_document(contents).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    let tag_directives = runner_tag_directives(contents).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    let mut syntax = RunnerYamlSyntax {
        tag_directives,
        ..RunnerYamlSyntax::default()
    };
    inspect_runner_yaml_syntax(document.syntax(), contents, 0, 0, &mut syntax);
    if syntax.exceeded_depth {
        return Err(crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: YAML nesting exceeds {MAX_ACTION_METADATA_DEPTH} levels",
            metadata_file.display()
        )));
    }
    if syntax.exceeded_budget {
        return Err(crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: YAML exceeds scanner node/event limits",
            metadata_file.display()
        )));
    }
    syntax.estimated_bytes = contents
        .len()
        .checked_add(syntax.node_count.saturating_mul(64))
        .and_then(|bytes| bytes.checked_add(syntax.event_count.saturating_mul(8)))
        .unwrap_or(usize::MAX);
    if syntax.estimated_bytes > MAX_ACTION_METADATA_ESTIMATED_BYTES {
        return Err(crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: estimated YAML parser memory exceeds {MAX_ACTION_METADATA_ESTIMATED_BYTES} bytes",
            metadata_file.display()
        )));
    }
    if let Some(error) = &syntax.tag_error {
        return Err(crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        )));
    }
    if syntax.pending_tag.is_some() {
        accept_empty_runner_tag(&mut syntax);
        if let Some(error) = &syntax.tag_error {
            return Err(crate::s2::GeneratorError::usage(format!(
                "parse GitHub Action metadata {}: {error}",
                metadata_file.display()
            )));
        }
    }
    if syntax.has_anchor_or_alias {
        return Err(crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: actions/runner does not support YAML anchors or aliases",
            metadata_file.display()
        )));
    }
    if syntax.has_complex_mapping_key {
        return Err(crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: actions/runner does not support collection mapping keys",
            metadata_file.display()
        )));
    }
    if syntax.has_duplicate_mapping_key {
        return Err(crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: duplicate YAML mapping key after Runner case-insensitive matching",
            metadata_file.display()
        )));
    }
    Ok(syntax)
}

fn read_action_metadata(metadata_file: &Path) -> Result<String, crate::s2::GeneratorError> {
    let file = std::fs::File::open(metadata_file).map_err(|error| {
        crate::s2::GeneratorError::io("read GitHub Action metadata", metadata_file, &error)
    })?;
    let length = file
        .metadata()
        .map_err(|error| {
            crate::s2::GeneratorError::io("inspect GitHub Action metadata", metadata_file, &error)
        })?
        .len();
    if length > MAX_ACTION_METADATA_BYTES_U64 {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata {} exceeds {MAX_ACTION_METADATA_BYTES} bytes",
            metadata_file.display()
        )));
    }

    let capacity = usize::try_from(length).unwrap_or(MAX_ACTION_METADATA_BYTES);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(MAX_ACTION_METADATA_BYTES_U64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            crate::s2::GeneratorError::io("read GitHub Action metadata", metadata_file, &error)
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ACTION_METADATA_BYTES_U64 {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata {} exceeds {MAX_ACTION_METADATA_BYTES} bytes",
            metadata_file.display()
        )));
    }
    String::from_utf8(bytes).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata {} is not UTF-8: {error}",
            metadata_file.display()
        ))
    })
}

/// Detect every tracked action metadata file outside test support trees.
pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), crate::s2::GeneratorError> {
    for source in discover_action_sources(context.root, context.files)? {
        let (metadata, references) = inspect_action_source(
            &source,
            context.root,
            context.files,
            &context.renderer_output_paths,
        )?;
        let dependencies = if let Some(metadata) = metadata.as_ref() {
            local_action_dependency_closure(
                context.root,
                &metadata.runs,
                references,
                context.files,
                &context.renderer_output_paths,
            )?
        } else {
            LocalActionDependencyClosure {
                references: references.into_iter().collect(),
                action_roots: BTreeSet::new(),
            }
        };
        let mut commands = vec![format!(
            "velnor-workflow verify-action --path {}",
            shell_quote(&source.path)
        )];
        commands.extend(
            dependencies
                .references
                .iter()
                .map(|path| format!("test -f {}", shell_quote(path))),
        );
        let mut watch = vec![if source.root == "." {
            "**".to_owned()
        } else {
            format!("{}/**", source.root)
        }];
        watch.extend(dependencies.references.iter().cloned());
        watch.extend(
            dependencies
                .action_roots
                .into_iter()
                .map(|path| format!("{path}/**")),
        );
        let mut action = unit(UnitKind::GithubAction, &source.root, watch, commands, None);
        if source.kind == ActionSourceKind::Dockerfile
            || metadata
                .as_ref()
                .is_some_and(|metadata| metadata.runs.using.eq_ignore_ascii_case("docker"))
        {
            action.capabilities.docker = true;
        }
        shape.units.push(action);
    }
    Ok(())
}

struct LocalActionDependencyClosure {
    references: BTreeSet<String>,
    action_roots: BTreeSet<String>,
}

fn local_action_dependency_closure(
    root: &Path,
    runs: &ActionRuns,
    references: Vec<String>,
    files: &[String],
    renderer_output_paths: &BTreeSet<PathBuf>,
) -> Result<LocalActionDependencyClosure, crate::s2::GeneratorError> {
    let candidates = discover_action_source_candidates(files);
    let mut visited = BTreeSet::new();
    let mut closure = LocalActionDependencyClosure {
        references: references.into_iter().collect(),
        action_roots: BTreeSet::new(),
    };
    collect_local_action_dependency_closure(
        root,
        runs,
        files,
        renderer_output_paths,
        &candidates,
        &mut visited,
        &mut closure,
    )?;
    Ok(closure)
}

fn collect_local_action_dependency_closure(
    root: &Path,
    runs: &ActionRuns,
    files: &[String],
    renderer_output_paths: &BTreeSet<PathBuf>,
    candidates: &BTreeMap<String, ActionSource>,
    visited: &mut BTreeSet<String>,
    closure: &mut LocalActionDependencyClosure,
) -> Result<(), crate::s2::GeneratorError> {
    if !runs.using.eq_ignore_ascii_case("composite") {
        return Ok(());
    }
    for step in runs.steps.as_deref().unwrap_or_default() {
        let Some(reference) = step.uses.as_deref() else {
            continue;
        };
        let Some(dependency_root) = discovered_local_action_root(root, reference)? else {
            continue;
        };
        if !visited.insert(dependency_root.clone()) {
            continue;
        }
        closure.action_roots.insert(dependency_root.clone());
        if let Some(source) = candidates.get(&dependency_root)
            && source.kind == ActionSourceKind::Metadata
        {
            let (metadata, references) =
                inspect_action_source(source, root, files, renderer_output_paths)?;
            closure.references.extend(references);
            if let Some(metadata) = metadata {
                collect_local_action_dependency_closure(
                    root,
                    &metadata.runs,
                    files,
                    renderer_output_paths,
                    candidates,
                    visited,
                    closure,
                )?;
            }
        }
    }
    Ok(())
}

/// Re-validate one action metadata file at execution time. Generation proves
/// the checked-in shape; this command makes the generated unit fail if the
/// metadata or any local entrypoint changes between scan and execution.
pub(crate) fn verify_action(
    root: &Path,
    source_path: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let source = Path::new(source_path);
    if source.is_absolute()
        || source
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action source path `{source_path}` must be repository-relative"
        )));
    }
    let files = super::file_walk::repository_files(root, &[])?;
    let renderer_output_paths = BTreeSet::new();
    let Some(canonical) = discover_action_sources(root, &files)?
        .into_iter()
        .find(|candidate| candidate.path == source_path)
    else {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action source `{source_path}` is not the canonical action entrypoint"
        )));
    };
    inspect_action_source(&canonical, root, &files, &renderer_output_paths).map(|_| ())
}

/// Discover the entrypoint that actions/runner would prepare for every action
/// directory.  `action.yml` wins over `action.yaml`; a bare Dockerfile is the
/// fallback only when neither metadata file exists.  Keeping this decision in
/// one function prevents generation and runtime verification from auditing
/// different files.
fn discover_action_sources(
    root: &Path,
    files: &[String],
) -> Result<Vec<ActionSource>, crate::s2::GeneratorError> {
    let candidates = discover_action_source_candidates(files);
    let mut referenced_roots = BTreeSet::new();
    referenced_roots.extend(workflow_action_roots(root, files)?);
    for source in candidates.values() {
        if source.kind != ActionSourceKind::Metadata {
            continue;
        }
        let metadata = parse_metadata(root, &source.path)?;
        for step in metadata.runs.steps.as_deref().unwrap_or_default() {
            let Some(uses) = step.uses.as_deref() else {
                continue;
            };
            let Some(directory) = discovered_local_action_root(root, uses)? else {
                continue;
            };
            if candidates.contains_key(&directory) {
                referenced_roots.insert(directory);
            }
        }
    }
    Ok(candidates
        .into_iter()
        .filter_map(|(root, source)| match source.kind {
            ActionSourceKind::Metadata => Some(source),
            ActionSourceKind::Dockerfile if referenced_roots.contains(&root) => Some(source),
            ActionSourceKind::Dockerfile => None,
        })
        .collect())
}

/// Find every runner entrypoint candidate before role classification. Metadata
/// is authoritative wherever present; a Dockerfile is only a fallback after
/// a caller proves that its directory is an action root.
fn discover_action_source_candidates(files: &[String]) -> BTreeMap<String, ActionSource> {
    let mut preferred_metadata = BTreeMap::new();
    let mut alternate_metadata = BTreeMap::new();
    let mut dockerfiles = BTreeMap::new();
    for file in files
        .iter()
        .filter(|file| !is_test_support_path(file) && !is_generator_source_path(file))
    {
        let root = parent_path(file);
        match file.rsplit('/').next() {
            Some("action.yml") => {
                preferred_metadata.insert(root, file.clone());
            }
            Some("action.yaml") => {
                alternate_metadata.insert(root, file.clone());
            }
            Some("Dockerfile") => {
                dockerfiles.insert(root, file.clone());
            }
            Some("dockerfile") => {
                dockerfiles.entry(root).or_insert_with(|| file.clone());
            }
            _ => {}
        }
    }
    let roots = preferred_metadata
        .keys()
        .chain(alternate_metadata.keys())
        .chain(dockerfiles.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    roots
        .into_iter()
        .filter_map(|root| {
            if let Some(path) = preferred_metadata
                .get(&root)
                .or_else(|| alternate_metadata.get(&root))
            {
                return Some((
                    root.clone(),
                    ActionSource {
                        root,
                        path: path.clone(),
                        kind: ActionSourceKind::Metadata,
                    },
                ));
            }
            dockerfiles.get(&root).map(|path| {
                (
                    root.clone(),
                    ActionSource {
                        root,
                        path: path.clone(),
                        kind: ActionSourceKind::Dockerfile,
                    },
                )
            })
        })
        .collect()
}

fn is_generator_source_path(path: &str) -> bool {
    path == ".github-gen" || path.starts_with(".github-gen/")
}

fn discovered_local_action_root(
    root: &Path,
    reference: &str,
) -> Result<Option<String>, crate::s2::GeneratorError> {
    let Some(relative) = reference
        .strip_prefix("./")
        .or_else(|| reference.strip_prefix(".\\"))
    else {
        if is_unsafe_local_action_reference(reference) {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub local action reference `{reference}` must stay inside the repository workspace"
            )));
        }
        return Ok(None);
    };
    if reference.contains('$') || reference.contains("{{") || reference.contains("}}") {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub local action reference `{reference}` must be static"
        )));
    }
    let normalized = relative.replace('\\', "/");
    if normalized.starts_with('/') || has_windows_drive_prefix(&normalized) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub local action reference `{reference}` must stay inside the repository workspace"
        )));
    }
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub local action reference `{reference}` escapes the repository workspace"
                )));
            }
            component if has_windows_drive_prefix(component) => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub local action reference `{reference}` must stay inside the repository workspace"
                )));
            }
            component => components.push(component),
        }
    }
    let repository_path = if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    };
    reject_symlink_components(root, &components, reference)?;
    Ok(Some(repository_path))
}

fn is_unsafe_local_action_reference(reference: &str) -> bool {
    let normalized = reference.replace('\\', "/");
    normalized.starts_with('/')
        || has_windows_drive_prefix(&normalized)
        || normalized == ".."
        || normalized.starts_with("../")
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn reject_symlink_components(
    root: &Path,
    components: &[&str],
    reference: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let mut path = root.to_path_buf();
    for component in components {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub local action reference `{reference}` traverses symlink `{}`",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                break;
            }
            Err(error) => {
                return Err(crate::s2::GeneratorError::io(
                    "inspect GitHub local action path",
                    &path,
                    &error,
                ));
            }
        }
    }
    Ok(())
}

fn workflow_action_roots(
    root: &Path,
    files: &[String],
) -> Result<BTreeSet<String>, crate::s2::GeneratorError> {
    let mut roots = BTreeSet::new();
    for path in files.iter().filter(|path| {
        let path = Path::new(path);
        path.parent() == Some(Path::new(".github/workflows"))
            && matches!(
                path.extension().and_then(std::ffi::OsStr::to_str),
                Some("yml" | "yaml")
            )
    }) {
        let workflow_path = root.join(path);
        let contents = std::fs::read_to_string(&workflow_path).map_err(|error| {
            crate::s2::GeneratorError::io("read workflow action references", &workflow_path, &error)
        })?;
        let document: serde_yaml::Value = serde_yaml::from_str(&contents).map_err(|error| {
            crate::s2::GeneratorError::usage(format!(
                "parse workflow action references {}: {error}",
                workflow_path.display()
            ))
        })?;
        collect_workflow_action_roots(&document, root, &mut roots)?;
    }
    Ok(roots)
}

fn collect_workflow_action_roots(
    document: &serde_yaml::Value,
    root: &Path,
    roots: &mut BTreeSet<String>,
) -> Result<(), crate::s2::GeneratorError> {
    let Some(jobs) = document
        .as_mapping()
        .and_then(|mapping| mapping_value(mapping, "jobs"))
        .and_then(serde_yaml::Value::as_mapping)
    else {
        return Ok(());
    };
    for job in jobs.values() {
        let Some(steps) = job
            .as_mapping()
            .and_then(|mapping| mapping_value(mapping, "steps"))
            .and_then(serde_yaml::Value::as_sequence)
        else {
            continue;
        };
        for step in steps {
            let Some(reference) = step
                .as_mapping()
                .and_then(|mapping| mapping_value(mapping, "uses"))
                .and_then(serde_yaml::Value::as_str)
            else {
                continue;
            };
            if let Some(action_root) = discovered_local_action_root(root, reference)? {
                roots.insert(action_root);
            }
        }
    }
    Ok(())
}

fn inspect_action_source(
    source: &ActionSource,
    root: &Path,
    files: &[String],
    renderer_output_paths: &BTreeSet<PathBuf>,
) -> Result<(Option<ActionMetadata>, Vec<String>), crate::s2::GeneratorError> {
    match source.kind {
        ActionSourceKind::Dockerfile => Ok((None, Vec::new())),
        ActionSourceKind::Metadata => {
            let metadata = parse_metadata(root, &source.path)?;
            let references = local_references(
                root,
                &metadata.runs,
                &source.root,
                files,
                renderer_output_paths,
            )?;
            Ok((Some(metadata), references))
        }
    }
}

fn parse_metadata(
    root: &Path,
    metadata_path: &str,
) -> Result<ActionMetadata, crate::s2::GeneratorError> {
    let normalized_metadata_path = metadata_path.replace('\\', "/");
    let metadata_components = normalized_metadata_path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>();
    reject_symlink_components(root, &metadata_components, metadata_path)?;
    let metadata_file = root.join(metadata_path);
    let contents = read_action_metadata(&metadata_file)?;
    let syntax = validate_runner_yaml_syntax(&contents, &metadata_file)?;
    let parser_config = runner_action_parser_config();
    // Decode the original mapping structure so implicit empty keys and values
    // survive. Replace only tagged or radix scalars whose source spelling
    // differs from Runner's scalar semantics; offsets avoid AST key loss.
    let normalized = normalize_runner_yaml_source(&contents, &syntax.source_replacements)?;
    let parse_contents = normalized.as_deref().unwrap_or(&contents);
    let mut deserializer =
        serde_yaml::StreamingDeserializer::with_config(parse_contents, &parser_config);
    let decoded = RunnerYamlValue::deserialize(&mut deserializer).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {} with Runner-compatible mapping keys: {error}",
            metadata_file.display()
        ))
    })?;
    let RunnerYamlValue(document) = decoded;
    validate_metadata_shape(&document)?;
    let metadata: ActionMetadata = serde_yaml::from_value(&document).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    Ok(metadata)
}

/// Validate the typed fields that actions/runner's action manifest schema
/// asserts before converting the manifest. Unknown keys stay loose like the
/// runner; known keys never silently fall through a `serde_yaml::Value`.
fn validate_metadata_shape(document: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let root = require_mapping(document, "action manifest")?;
    for name in root.keys() {
        mapping_key(name, "action manifest")?;
    }
    if let Some(value) = mapping_value(root, "name") {
        require_string(value, "name", false)?;
    }
    if let Some(value) = mapping_value(root, "description") {
        require_string(value, "description", false)?;
    }
    if let Some(value) = mapping_value(root, "inputs") {
        validate_inputs(value)?;
    }
    if let Some(value) = mapping_value(root, "outputs") {
        validate_outputs(value)?;
    }
    validate_runs(mapping_value(root, "runs").ok_or_else(|| {
        crate::s2::GeneratorError::usage(
            "GitHub Action metadata `runs` must be a mapping, but it is missing".to_owned(),
        )
    })?)
}

fn require_mapping<'a>(
    value: &'a serde_yaml::Value,
    field: &str,
) -> Result<&'a serde_yaml::Mapping, crate::s2::GeneratorError> {
    value.as_mapping().ok_or_else(|| {
        crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` must be a mapping"
        ))
    })
}

fn mapping_value<'a>(
    mapping: &'a serde_yaml::Mapping,
    name: &str,
) -> Option<&'a serde_yaml::Value> {
    mapping.get(name)
}

fn mapping_key<'a>(key: &'a str, field: &str) -> Result<&'a str, crate::s2::GeneratorError> {
    if key.is_empty() {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` keys must not be empty"
        )));
    }
    Ok(key)
}

fn require_string(
    value: &serde_yaml::Value,
    field: &str,
    non_empty: bool,
) -> Result<(), crate::s2::GeneratorError> {
    let text = value.as_str().ok_or_else(|| {
        crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` must be a string"
        ))
    })?;
    if non_empty && text.is_empty() {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` must not be empty"
        )));
    }
    Ok(())
}

fn validate_inputs(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let inputs = require_mapping(value, "inputs")?;
    for (name, definition) in inputs {
        mapping_key(name, "inputs")?;
        let definition = require_mapping(definition, "input definition")?;
        for (field, value) in definition {
            let field = mapping_key(field, "input definition")?;
            if runner_ordinal_ignore_case_eq(field, "default")
                || runner_ordinal_ignore_case_eq(field, "deprecationMessage")
            {
                require_string(value, &format!("inputs.{field}"), false)?;
            }
        }
    }
    Ok(())
}

fn validate_outputs(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let outputs = require_mapping(value, "outputs")?;
    for (name, definition) in outputs {
        mapping_key(name, "outputs")?;
        let definition = require_mapping(definition, "output definition")?;
        for (field, value) in definition {
            let field = mapping_key(field, "output definition")?;
            if field == "description" || field == "value" {
                require_string(value, &format!("outputs.{field}"), false)?;
            } else {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub Action metadata has unexpected output definition key `{field}`"
                )));
            }
        }
    }
    Ok(())
}

fn validate_runs(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let runs = require_mapping(value, "runs")?;
    if let Some(plugin) = mapping_value(runs, "plugin") {
        require_string(plugin, "runs.plugin", true)?;
        return Err(crate::s2::GeneratorError::usage(
            "unsupported GitHub Action runtime `plugin`; Velnor does not execute plugin actions"
                .to_owned(),
        ));
    }
    let using = mapping_value(runs, "using").ok_or_else(|| {
        crate::s2::GeneratorError::usage(
            "GitHub Action metadata `runs.using` must be a non-empty string".to_owned(),
        )
    })?;
    require_string(using, "runs.using", true)?;
    let using = using.as_str().unwrap_or_default().to_ascii_lowercase();
    if using == "composite" && mapping_value(runs, "steps").is_none() {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub composite action metadata must declare runs.steps",
        ));
    }
    let allowed_fields: &[&str] = match using.as_str() {
        "composite" => &["using", "steps"],
        "docker" => &[
            "using",
            "image",
            "entrypoint",
            "pre-entrypoint",
            "pre-if",
            "post-entrypoint",
            "post-if",
            "args",
            "env",
        ],
        "node12" | "node16" | "node20" | "node24" => {
            &["using", "main", "pre", "pre-if", "post", "post-if"]
        }
        // The runtime is rejected later, but retain typed validation so its
        // malformed fields do not disappear behind the unsupported-runtime
        // diagnostic.
        _ => &[
            "using",
            "image",
            "entrypoint",
            "pre-entrypoint",
            "pre-if",
            "post-entrypoint",
            "post-if",
            "main",
            "pre",
            "post",
            "args",
            "env",
            "steps",
        ],
    };
    for (field, value) in runs {
        let field = mapping_key(field, "runs")?;
        if !allowed_fields.contains(&field) {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub Action metadata `runs.{field}` is not allowed for runtime `{using}`"
            )));
        }
        match field {
            "using" | "image" | "entrypoint" | "pre-entrypoint" | "pre-if" | "post-entrypoint"
            | "post-if" | "main" | "pre" | "post" => {
                require_string(value, &format!("runs.{field}"), true)?;
            }
            "args" => validate_string_sequence(value, "runs.args")?,
            "env" => validate_string_mapping(value, "runs.env")?,
            "steps" => validate_composite_steps(value)?,
            _ => {}
        }
    }
    Ok(())
}

fn validate_string_sequence(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let sequence = value.as_sequence().ok_or_else(|| {
        crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` must be a sequence"
        ))
    })?;
    for (index, item) in sequence.iter().enumerate() {
        require_string(item, &format!("{field}[{index}]"), false)?;
    }
    Ok(())
}

fn validate_string_mapping(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let mapping = require_mapping(value, field)?;
    for (name, value) in mapping {
        mapping_key(name, field)?;
        require_string(value, &format!("{field} entry"), false)?;
    }
    Ok(())
}

fn validate_composite_steps(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let steps = value.as_sequence().ok_or_else(|| {
        crate::s2::GeneratorError::usage(
            "GitHub Action metadata `runs.steps` must be a sequence".to_owned(),
        )
    })?;
    for (index, step) in steps.iter().enumerate() {
        let step = require_mapping(step, &format!("runs.steps[{index}]"))?;
        for (field, value) in step {
            let field = mapping_key(field, &format!("runs.steps[{index}]"))?;
            match field {
                "id" => require_string(value, "composite step id", true)?,
                "name" | "if" | "run" | "shell" | "working-directory" | "uses" => {
                    require_string(value, &format!("composite step {field}"), field == "uses")?;
                }
                "with" | "env" => validate_string_mapping(value, &format!("step.{field}"))?,
                "continue-on-error" => match value {
                    serde_yaml::Value::Bool(_) => {}
                    _ => require_string(value, "composite step continue-on-error", false)?,
                },
                _ => {
                    return Err(crate::s2::GeneratorError::usage(format!(
                        "GitHub Action metadata has unexpected composite step key `{field}`"
                    )));
                }
            }
        }
        let has_run = mapping_value(step, "run").is_some();
        let has_uses = mapping_value(step, "uses").is_some();
        if has_run == has_uses {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub composite action step {index} must declare exactly one of `run` or `uses`"
            )));
        }
        if has_run && mapping_value(step, "with").is_some() {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub composite run step {index} must not declare `with`"
            )));
        }
        if has_uses
            && (mapping_value(step, "shell").is_some()
                || mapping_value(step, "working-directory").is_some())
        {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub composite uses step {index} must not declare `shell` or `working-directory`"
            )));
        }
        if has_run && mapping_value(step, "shell").is_none() {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub composite action step {index} must declare `shell`"
            )));
        }
    }
    Ok(())
}

fn validate_mapping(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::s2::GeneratorError> {
    if !value.is_mapping() {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` must be a mapping"
        )));
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "runner metadata validation keeps composite, JavaScript, and Docker branches together"
)]
fn local_references(
    root: &Path,
    runs: &ActionRuns,
    action_root: &str,
    files: &[String],
    renderer_output_paths: &BTreeSet<PathBuf>,
) -> Result<Vec<String>, crate::s2::GeneratorError> {
    let using = runs.using.to_ascii_lowercase();
    let mut references = BTreeSet::new();
    match using.as_str() {
        "composite" => {
            for step in runs.steps.as_deref().unwrap_or_default() {
                validate_composite_step(step)?;
                if step.run.is_none() && step.uses.is_none() {
                    return Err(crate::s2::GeneratorError::usage(
                        "GitHub composite action step must declare run or uses",
                    ));
                }
                if step.run.is_some() && step.uses.is_some() {
                    return Err(crate::s2::GeneratorError::usage(
                        "GitHub composite action step must not declare both run and uses",
                    ));
                }
                if let Some(run) = &step.run {
                    if step.shell.is_none() {
                        return Err(crate::s2::GeneratorError::usage(
                            "GitHub composite action run step must declare shell",
                        ));
                    }
                    let run = resolve_action_path_expression(run, action_root);
                    for reference in shell_references(&run) {
                        if let Some(reference) = strip_action_path_marker(&reference) {
                            add_local_reference(
                                &mut references,
                                reference,
                                ".",
                                root,
                                files,
                                renderer_output_paths,
                            )?;
                        } else {
                            let working_directory = normalize_working_directory(
                                root,
                                step.working_directory.as_deref().unwrap_or_default(),
                            )?;
                            add_local_reference(
                                &mut references,
                                &reference,
                                &working_directory,
                                root,
                                files,
                                renderer_output_paths,
                            )?;
                        }
                    }
                }
                if let Some(uses) = &step.uses
                    && uses.trim().is_empty()
                {
                    return Err(crate::s2::GeneratorError::usage(
                        "GitHub composite action uses value must not be empty",
                    ));
                }
                if let Some(uses) = &step.uses {
                    let uses = uses.as_str();
                    if is_runner_local_action_reference(uses) {
                        add_local_action_reference(&mut references, uses, root, files)?;
                    } else if is_unsafe_local_action_reference(uses) {
                        return Err(crate::s2::GeneratorError::usage(format!(
                            "GitHub composite local action `{uses}` must stay inside the repository workspace"
                        )));
                    } else if !is_full_sha_action_reference(uses) {
                        return Err(crate::s2::GeneratorError::usage(format!(
                            "GitHub composite external action `{uses}` must use a full 40-character SHA pin"
                        )));
                    }
                }
            }
        }
        "node12" | "node16" | "node20" | "node24" => {
            let main = runs.main.as_deref().ok_or_else(|| {
                crate::s2::GeneratorError::usage(format!(
                    "GitHub JavaScript action ({using}) metadata must declare runs.main"
                ))
            })?;
            add_action_path_reference(
                &mut references,
                main,
                action_root,
                root,
                files,
                renderer_output_paths,
            )?;
            for reference in [&runs.pre, &runs.post] {
                if let Some(reference) = reference.as_deref() {
                    add_action_path_reference(
                        &mut references,
                        reference,
                        action_root,
                        root,
                        files,
                        renderer_output_paths,
                    )?;
                }
            }
        }
        "docker" => {
            let image = runs.image.as_deref().ok_or_else(|| {
                crate::s2::GeneratorError::usage(
                    "GitHub Docker action metadata must declare runs.image",
                )
            })?;
            if image.trim().is_empty() {
                return Err(crate::s2::GeneratorError::usage(
                    "GitHub Docker action metadata `runs.image` must not be empty",
                ));
            }
            if is_dockerfile_reference(image) {
                add_action_path_reference(
                    &mut references,
                    image,
                    action_root,
                    root,
                    files,
                    renderer_output_paths,
                )?;
            } else if !is_docker_image_reference(image) {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub Docker action metadata `runs.image` must be a Dockerfile path or docker:// image reference, got `{image}`"
                )));
            }
            // Docker action entrypoints are resolved inside the image. They
            // are not host files and must not be mistaken for local scripts.
        }
        other => {
            return Err(crate::s2::GeneratorError::usage(format!(
                "unsupported GitHub Action runtime `{other}`; expected composite, node12/node16/node20/node24, or docker"
            )));
        }
    }
    Ok(references.into_iter().collect())
}

fn is_full_sha_action_reference(value: &str) -> bool {
    RepositoryActionReference::parse(value).is_ok()
}

/// Match actions/runner's Dockerfile test instead of guessing from an image
/// tag. Repository Docker actions admit only a Dockerfile path or a
/// `docker://` image; only a basename named `Dockerfile` or beginning
/// `Dockerfile.` is a host-side build source.
fn is_dockerfile_reference(value: &str) -> bool {
    matches!(
        ActionImageReference::parse(value),
        Ok(ActionImageReference::Dockerfile(_))
    )
}

fn is_docker_image_reference(value: &str) -> bool {
    matches!(
        ActionImageReference::parse(value),
        Ok(ActionImageReference::DockerImage(_))
    )
}

/// Resolve the one host-local expression actions/runner makes available to a
/// composite action.  Other expressions stay opaque and are rejected if they
/// would be used as a local entrypoint, so dynamic paths cannot become an
/// accidental host-file dependency.
const ACTION_PATH_MARKER: &str = "__velnor_action_path__/";

fn resolve_action_path_expression(value: &str, action_root: &str) -> String {
    let mut resolved = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_start) = value[cursor..].find("${{") {
        let start = cursor + relative_start;
        resolved.push_str(&value[cursor..start]);
        let expression_start = start + 3;
        let Some(relative_end) = value[expression_start..].find("}}") else {
            resolved.push_str(&value[start..]);
            return resolved;
        };
        let end = expression_start + relative_end + 2;
        let expression = value[expression_start..end - 2].trim();
        if expression.eq_ignore_ascii_case("github.action_path") {
            resolved.push_str(ACTION_PATH_MARKER);
            if action_root != "." {
                resolved.push_str(action_root);
            }
        } else {
            resolved.push_str(&value[start..end]);
        }
        cursor = end;
    }
    resolved.push_str(&value[cursor..]);
    resolved
}

fn strip_action_path_marker(reference: &str) -> Option<&str> {
    reference
        .strip_prefix(ACTION_PATH_MARKER)
        .or_else(|| {
            reference
                .strip_prefix("./")
                .and_then(|reference| reference.strip_prefix(ACTION_PATH_MARKER))
        })
        .map(|reference| reference.trim_start_matches('/'))
}

fn validate_composite_step(step: &ActionStep) -> Result<(), crate::s2::GeneratorError> {
    // Keep GitHub's expression-bearing fields opaque, but preserve their
    // mapping/value shapes instead of silently treating malformed metadata as
    // a valid action. `if`, `id`, `name`, and `working-directory` remain
    // untouched by the detector and therefore retain the action's semantics.
    if !step.with.is_null() {
        validate_mapping(&step.with, "step.with")?;
    }
    if !step.env.is_null() {
        validate_mapping(&step.env, "step.env")?;
    }
    if let Some(continue_on_error) = &step.continue_on_error
        && !continue_on_error.is_bool()
        && !continue_on_error
            .as_str()
            .is_some_and(is_runner_boolean_expression)
    {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub composite action step `continue-on-error` must be a boolean or expression string",
        ));
    }
    if step.id.as_deref().is_some_and(str::is_empty) {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub composite action step `id` must not be empty",
        ));
    }
    Ok(())
}

fn is_runner_boolean_expression(value: &str) -> bool {
    let value = value.trim();
    value
        .strip_prefix("${{")
        .and_then(|expression| expression.strip_suffix("}}"))
        .is_some_and(|expression| !expression.trim().is_empty())
}

fn normalize_working_directory(
    root: &Path,
    working_directory: &str,
) -> Result<String, crate::s2::GeneratorError> {
    let normalized = working_directory.replace('\\', "/");
    if normalized.contains("${{")
        || normalized.contains("{{")
        || normalized.contains("}}")
        || normalized.contains('$')
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite `working-directory` `{working_directory}` must be static for local script lookup"
        )));
    }
    if normalized.starts_with('/') || has_windows_drive_prefix(&normalized) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite `working-directory` `{working_directory}` must stay inside the repository workspace"
        )));
    }
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub composite `working-directory` `{working_directory}` must not traverse parent directories"
                )));
            }
            component if has_windows_drive_prefix(component) => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub composite `working-directory` `{working_directory}` must stay inside the repository workspace"
                )));
            }
            component => components.push(component),
        }
    }
    reject_symlink_components(root, &components, working_directory)?;
    if components.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(components.join("/"))
    }
}

fn add_action_path_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    action_root: &str,
    root: &Path,
    files: &[String],
    renderer_output_paths: &BTreeSet<PathBuf>,
) -> Result<(), crate::s2::GeneratorError> {
    let normalized_action_root = action_root.replace('\\', "/");
    let action_root_components = normalized_action_root
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>();
    reject_symlink_components(root, &action_root_components, action_root)?;
    let action_dir = root.join(action_root);
    let resolved = resolve_action_path(&action_dir, reference).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata path `{reference}` is unsafe below `{action_root}`: {error}"
        ))
    })?;
    let repository_path = resolved
        .strip_prefix(root)
        .map_err(|error| {
            crate::s2::GeneratorError::usage(format!(
                "GitHub Action metadata path `{reference}` escaped the repository root: {error}"
            ))
        })?
        .to_string_lossy()
        .replace('\\', "/");
    let path_components = repository_path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>();
    reject_symlink_components(root, &path_components, reference)?;
    if !files.iter().any(|file| file == &repository_path)
        && !is_excluded_action_file(root, &repository_path, renderer_output_paths)?
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action entrypoint `{reference}` resolves to missing file `{repository_path}`"
        )));
    }
    references.insert(repository_path);
    Ok(())
}

fn add_local_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    base_path: &str,
    root: &Path,
    files: &[String],
    renderer_output_paths: &BTreeSet<PathBuf>,
) -> Result<(), crate::s2::GeneratorError> {
    let reference = reference.trim().trim_matches(['"', '\'']);
    if reference.is_empty()
        || reference.contains("${{")
        || reference.contains("{{")
        || reference.contains("}}")
        || reference.contains('$')
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` must be a static relative path"
        )));
    }
    let normalized = reference.replace('\\', "/");
    if normalized.starts_with('/') || has_windows_drive_prefix(&normalized) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` must stay inside the repository workspace"
        )));
    }
    let mut components = Vec::new();
    for component in base_path.replace('\\', "/").split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub Action local entrypoint base `{base_path}` escapes the repository workspace"
                )));
            }
            component if has_windows_drive_prefix(component) => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub Action local entrypoint base `{base_path}` must stay inside the repository workspace"
                )));
            }
            component => components.push(component.to_owned()),
        }
    }
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub Action local entrypoint `{reference}` escapes the repository workspace"
                )));
            }
            component if has_windows_drive_prefix(component) => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub Action local entrypoint `{reference}` must stay inside the repository workspace"
                )));
            }
            component => components.push(component.to_owned()),
        }
    }
    let repository_path = if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    };
    let borrowed_components = components.iter().map(String::as_str).collect::<Vec<_>>();
    reject_symlink_components(root, &borrowed_components, reference)?;
    if !files.iter().any(|file| file == &repository_path)
        && !is_excluded_action_file(root, &repository_path, renderer_output_paths)?
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action entrypoint `{reference}` resolves to missing file `{repository_path}`"
        )));
    }
    references.insert(repository_path);
    Ok(())
}

/// W6 deliberately omits repository-root build output such as `dist/` from
/// the ordinary source walk. A checked-in root action may still use that
/// directory as its Runner entrypoint, so validate that one narrow path
/// directly without broadening the repository walk or admitting generator
/// outputs proven by the current render.
fn is_excluded_action_file(
    root: &Path,
    repository_path: &str,
    renderer_output_paths: &BTreeSet<PathBuf>,
) -> Result<bool, crate::s2::GeneratorError> {
    let first = repository_path.split('/').next();
    if first != Some("dist") {
        return Ok(false);
    }
    if renderer_output_paths.contains(Path::new(repository_path)) {
        return Ok(false);
    }
    let path = root.join(repository_path);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(crate::s2::GeneratorError::io(
            "inspect GitHub Action entrypoint",
            &path,
            &error,
        )),
    }
}

fn add_local_action_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    root: &Path,
    files: &[String],
) -> Result<(), crate::s2::GeneratorError> {
    let Some(directory) = discovered_local_action_root(root, reference)? else {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite local action `{reference}` must start with `./` or `.\\`"
        )));
    };
    if let Some(source) = discover_action_source_candidates(files)
        .into_values()
        .find(|source| source.root == directory)
    {
        references.insert(source.path);
        return Ok(());
    }
    Err(crate::s2::GeneratorError::usage(format!(
        "GitHub composite local action `{reference}` has no action metadata or Dockerfile under `{directory}`"
    )))
}

fn shell_references(command: &str) -> Vec<String> {
    let tokens = shell_tokens(command);
    let interpreters = [
        "bash",
        "sh",
        "dash",
        "zsh",
        "node",
        "deno",
        "bun",
        "python",
        "python3",
        "ruby",
        "perl",
        "pwsh",
        "powershell",
        "source",
    ];
    let mut references = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let previous_is_interpreter = index
            .checked_sub(1)
            .and_then(|previous| tokens.get(previous))
            .is_some_and(|previous| interpreters.contains(&previous.as_str()));
        let previous_is_boundary = index
            .checked_sub(1)
            .and_then(|previous| tokens.get(previous))
            .is_some_and(|previous| matches!(previous.as_str(), "&&" | "||" | ";"));
        let command_position = index == 0 || previous_is_boundary;
        let plain_path_token = token
            .chars()
            .all(|character| !character.is_whitespace() && !"(){}[]$><|;&=\"`".contains(character));
        let looks_like_path = token.starts_with("./")
            || token.starts_with("../")
            || token.contains('/') && has_script_suffix(token)
            || has_script_suffix(token)
            || command_position
                && plain_path_token
                && token.contains('/')
                && !token.starts_with('-');
        if (index == 0
            || previous_is_interpreter
            || previous_is_boundary
            || token.starts_with("./")
            || token.starts_with("../")
            || has_script_suffix(token))
            && looks_like_path
        {
            references.push(token.clone());
        }
    }
    references
}

fn has_script_suffix(value: &str) -> bool {
    [
        ".sh", ".bash", ".js", ".mjs", ".cjs", ".ts", ".py", ".rb", ".pl", ".ps1", ".cmd", ".bat",
    ]
    .iter()
    .any(|suffix| value.ends_with(suffix))
}

fn shell_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        if quote == Some('"') && character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            } else {
                token.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '\\' => escaped = true,
            ';' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push(";".to_owned());
            }
            '&' if characters.peek() == Some(&'&') => {
                let _ = characters.next();
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push("&&".to_owned());
            }
            '|' if characters.peek() == Some(&'|') => {
                let _ = characters.next();
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push("||".to_owned());
            }
            character if character.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            _ => token.push(character),
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "test assertions name missing fixture evidence"
    )]

    use std::collections::BTreeSet;
    use std::fmt::Write as _;
    use std::fs;
    use std::path::PathBuf;

    use super::{
        shell_references, shell_tokens, MAX_ACTION_METADATA_BYTES, MAX_ACTION_METADATA_DEPTH,
    };
    use crate::s2::provider::ProviderId;

    fn assert_change_selects_action_unit(action: &crate::s2::Unit, path: &str) {
        let watched = crate::s2::reuse::WatchedUnit {
            id: action.id.clone(),
            watch: action.watch.clone(),
            depends_on: Vec::new(),
            kind: action.kind.id_prefix().to_owned(),
            commands: action.pr_commands.clone(),
            reads_closed: false,
        };
        let selection = must(
            crate::s2::reuse::select_affected(
                &[watched],
                &[crate::s2::reuse::ChangedPath {
                    path: path.to_owned(),
                    previous: None,
                    status: crate::s2::reuse::ChangeKind::Modified,
                }],
                &[],
            ),
            "select changed action reference",
        );
        assert!(
            selection.required.contains(&action.id),
            "editing {path} must select action unit {}",
            action.id
        );
    }

    #[test]
    fn raw_sidecar_claim_does_not_hide_dist_action_entrypoint() {
        let root = fixture("forged-dist-action-claim");
        let relative = PathBuf::from("dist/index.js");
        let bytes = "process.exit(0)\n";
        must(
            fs::create_dir_all(root.join("dist")),
            "create dist directory",
        );
        must(
            fs::write(root.join(&relative), bytes),
            "write dist action entrypoint",
        );
        let files = std::collections::BTreeMap::from([(relative.clone(), bytes.to_owned())]);
        let sidecar = crate::s2::ownership_state_content(
            &files,
            &std::collections::BTreeMap::new(),
            &crate::s2::GenerationInputs::parts(0, 0),
        );
        let state_path = root.join(crate::s2::OWNERSHIP_STATE);
        must(
            fs::create_dir_all(state_path.parent().unwrap_or(&root)),
            "create ownership state directory",
        );
        must(
            fs::write(state_path, sidecar),
            "write exact-digest forged sidecar claim",
        );

        assert!(must(
            super::is_excluded_action_file(&root, "dist/index.js", &BTreeSet::new()),
            "classify unrendered dist action entrypoint",
        ));
        assert!(!must(
            super::is_excluded_action_file(&root, "dist/index.js", &BTreeSet::from([relative]),),
            "exclude current-renderer dist output",
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[expect(
        clippy::panic,
        reason = "test fixture setup failures must name their root cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}"),
        }
    }

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-github-action-scan-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        must(
            fs::create_dir_all(root.join("scripts")),
            "create action fixture",
        );
        root
    }

    fn providers() -> std::collections::BTreeSet<ProviderId> {
        ProviderId::ALL.into_iter().collect()
    }

    #[test]
    fn shell_references_find_interpreter_and_direct_script_paths() {
        assert_eq!(
            shell_references("bash scripts/check.sh && ./scripts/entrypoint && scripts/next"),
            ["scripts/check.sh", "./scripts/entrypoint", "scripts/next"]
        );
        assert_eq!(
            shell_references("scripts/check&&./scripts/entrypoint; scripts/next.sh"),
            ["scripts/check", "./scripts/entrypoint", "scripts/next.sh"]
        );
        assert_eq!(
            shell_references("bash -e scripts/flagged.sh"),
            ["scripts/flagged.sh"]
        );
    }

    #[test]
    fn shell_references_ignore_command_substitution_internals() {
        let command = r#"report_json=\"$(jq -nc --argjson value \"$(printf '%s' \"$value\" | jq -Rsc 'split(\"\\n\")' || true)\" '{value: $value}' 2>/dev/null || true)\""#;
        assert!(shell_references(command).is_empty());
    }

    #[test]
    fn shell_tokens_keep_quoted_script_paths_together() {
        assert_eq!(
            shell_tokens("node './dist/main.js' --flag"),
            ["node", "./dist/main.js", "--flag"]
        );
    }

    #[test]
    fn metadata_entrypoints_share_strict_path_validation() {
        let invalid = [
            (
                "main-traversal",
                "runs:\n  using: node20\n  main: ../main.js\n",
            ),
            (
                "pre-backslash",
                "runs:\n  using: node20\n  main: main.js\n  pre: nested\\\\pre.js\n",
            ),
            (
                "post-drive",
                "runs:\n  using: node20\n  main: main.js\n  post: C:/post.js\n",
            ),
            (
                "dockerfile-absolute",
                "runs:\n  using: docker\n  image: /Dockerfile\n",
            ),
            (
                "dockerfile-traversal",
                "runs:\n  using: docker\n  image: ../Dockerfile\n",
            ),
        ];
        for (name, metadata) in invalid {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write unsafe metadata",
            );
            let error = super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("unsafe metadata path passed scan: {name}"));
            assert!(
                error.to_string().contains("unsafe")
                    || error.to_string().contains("workspace")
                    || error.to_string().contains("missing file"),
                "unexpected unsafe metadata error for {name}: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn metadata_entrypoints_reject_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = fixture("metadata-symlink");
        let outside = root.join("outside");
        let action = root.join("actions/js");
        must(fs::create_dir_all(&outside), "create symlink target");
        must(fs::create_dir_all(&action), "create nested action");
        must(
            fs::write(outside.join("index.js"), "process.exit(0)\n"),
            "write symlink target",
        );
        must(
            symlink(&outside, action.join("linked")),
            "create metadata symlink",
        );
        must(
            fs::write(
                action.join("action.yml"),
                "runs:\n  using: node20\n  main: linked/index.js\n",
            ),
            "write symlinked action metadata",
        );
        let error = super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("symlinked metadata path passed scan"));
        assert!(error.to_string().contains("symlink"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scan_detects_composite_javascript_and_docker_entrypoints() {
        let root = fixture("runtimes");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: bash scripts/check.sh\n",
            ),
            "write composite metadata",
        );
        must(
            fs::write(root.join("scripts/check.sh"), "exit 0\n"),
            "write shell entrypoint",
        );
        must(
            fs::create_dir_all(root.join("actions/js/dist")),
            "create js action",
        );
        must(
            fs::write(
                root.join("actions/js/action.yaml"),
                "runs:\n  using: node20\n  main: dist/index.js\n",
            ),
            "write javascript metadata",
        );
        must(
            fs::write(root.join("actions/js/dist/index.js"), "process.exit(0)\n"),
            "write js entrypoint",
        );
        must(
            fs::create_dir_all(root.join("actions/docker")),
            "create docker action",
        );
        must(
            fs::write(
                root.join("actions/docker/action.yml"),
                "runs:\n  using: docker\n  image: Dockerfile\n  entrypoint: /entrypoint.sh\n",
            ),
            "write docker metadata",
        );
        must(
            fs::write(root.join("actions/docker/Dockerfile"), "FROM scratch\n"),
            "write Dockerfile",
        );

        let files = must(
            super::super::file_walk::repository_files(&root, &[]),
            "walk action fixture",
        );
        assert!(
            files.iter().any(|file| file == "actions/js/dist/index.js"),
            "walked files: {files:?}"
        );
        let shape = must(
            super::super::scan_shape_for_tests(&root, &providers(), "main", &[]),
            "scan action fixture",
        );
        assert_eq!(shape.units.len(), 4);
        let composite = shape
            .units
            .iter()
            .find(|unit| unit.root == ".")
            .unwrap_or_else(|| panic!("composite action unit missing"));
        assert_eq!(composite.kind, crate::s2::UnitKind::GithubAction);
        assert!(composite
            .pr_commands
            .iter()
            .any(|command| command == "velnor-workflow verify-action --path 'action.yml'"));
        assert!(composite
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'scripts/check.sh'"));
        let docker = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == "actions/docker" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("docker action unit missing"));
        assert!(docker.capabilities.docker);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scan_rejects_missing_entrypoint_and_unknown_runtime() {
        let missing = fixture("missing");
        must(
            fs::write(
                missing.join("action.yml"),
                "runs:\n  using: node20\n  main: dist/index.js\n",
            ),
            "write missing metadata",
        );
        let error = super::super::scan_shape_for_tests(&missing, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("missing entrypoint should fail scan"));
        assert!(error.to_string().contains("missing file"), "{error}");
        let _ = fs::remove_dir_all(missing);

        let unknown = fixture("unknown");
        must(
            fs::write(
                unknown.join("action.yml"),
                "runs:\n  using: wasm\n  main: action.wasm\n",
            ),
            "write unknown metadata",
        );
        must(
            fs::write(unknown.join("action.wasm"), "fixture\n"),
            "write wasm fixture",
        );
        let error = super::super::scan_shape_for_tests(&unknown, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("unknown runtime should fail scan"));
        assert!(
            error
                .to_string()
                .contains("unsupported GitHub Action runtime"),
            "{error}"
        );
        let _ = fs::remove_dir_all(unknown);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "runner metadata fixture keeps adversarial shapes in one audit"
    )]
    fn runner_typed_manifest_shapes_fail_closed() {
        let cases = [
            (
                "inputs-child-scalar",
                "inputs:\n  bad: scalar\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "input-deprecation-message-case-insensitive-wrong-type",
                "inputs:\n  good:\n    DEPRECATIONMESSAGE: []\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "inputs-empty-key",
                "inputs:\n  \"\":\n    description: empty key\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "input-definition-empty-key",
                "inputs:\n  good:\n    \"\": empty metadata key\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "outputs-child-scalar",
                "outputs:\n  bad: scalar\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "outputs-empty-key",
                "outputs:\n  \"\":\n    description: empty output name\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "output-definition-unknown-key",
                "outputs:\n  good:\n    false: unknown property\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "output-description-wrong-case",
                "outputs:\n  good:\n    DESCRIPTION: wrong-case property\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "output-value-wrong-case",
                "outputs:\n  good:\n    VALUE: wrong-case property\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "output-definition-empty-key",
                "outputs:\n  good:\n    \"\": empty property\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "root-empty-key",
                "\"\": empty key\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "explicit-null-fields",
                "inputs: null\noutputs: null\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "docker-args-shape",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  args: {}\n",
            ),
            (
                "docker-args-null",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  args: null\n",
            ),
            (
                "docker-env-shape",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env: []\n",
            ),
            (
                "docker-condition-shape",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  pre-if: []\n",
            ),
            (
                "runs-unknown-key",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  1: unexpected\n",
            ),
            (
                "node-runtime-disallows-container-args",
                "runs:\n  using: node20\n  main: index.js\n  args: []\n",
            ),
            (
                "node-runtime-pre-if-without-main",
                "runs:\n  using: node20\n  pre-if: always()\n",
            ),
            (
                "runs-env-empty-key",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    \"\": unexpected\n",
            ),
            (
                "composite-step-unknown-key",
                "runs:\n  using: composite\n  steps:\n    - 1: unexpected\n      shell: bash\n      run: echo ok\n",
            ),
            (
                "composite-uses-step-disallows-shell",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      shell: bash\n",
            ),
            (
                "composite-uses-step-disallows-working-directory",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      working-directory: scripts\n",
            ),
            (
                "composite-run-step-disallows-with",
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n      with:\n        input: value\n",
            ),
            (
                "composite-with-empty-key",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        \"\": empty key\n",
            ),
            (
                "composite-env-empty-key",
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n      env:\n        \"\": empty key\n",
            ),
            (
                "composite-step-empty-key",
                "runs:\n  using: composite\n  steps:\n    - \"\": empty key\n      shell: bash\n      run: echo ok\n",
            ),
            (
                "complex-mapping-key",
                "? [complex, key]\n: ignored\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "case-insensitive-key-collision",
                "name: Action\nNAME: Duplicate\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            (
                "merge-alias-is-unsupported",
                "numeric_key: &numeric_key\n  1:\n    description: numeric key\ninputs:\n  good:\n    description: ok\n  <<: *numeric_key\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
        ];
        for (name, metadata) in cases {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write malformed action metadata",
            );
            let error = super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("runner-invalid metadata passed scan: {name}"));
            assert!(
                error.to_string().contains("GitHub"),
                "unexpected malformed metadata error for {name}: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn runner_unknown_root_and_input_metadata_values_are_ignored() {
        let metadata = "mystery-root:\n  nested: [false, null, {arbitrary: [1, 2]}]\ninputs:\n  value:\n    custom: {nested: [false, null]}\nruns:\n  using: composite\n  steps: []\n";
        let root = fixture("unknown-schema-metadata");
        must(
            fs::write(root.join("action.yml"), metadata),
            "write unknown schema metadata",
        );
        must(
            super::parse_metadata(&root, "action.yml"),
            "scanner must ignore unknown root and input metadata values",
        );
        must(
            velnor_runner::action_contract::parse_action_metadata(metadata),
            "runtime must ignore unknown root and input metadata values",
        );
        let _ = fs::remove_dir_all(root);
    }

    fn runner_scalar_input_schema_matches_runtime_parser() {
        let input_source = "inputs:\n  boolean:\n    default: false\n  number:\n    DEFAULT: 1e15\n  nullable:\n    default: null\n  7:\n    default: 1e-5\n  large-decimal:\n    default: 2147483648\n  hexadecimal:\n    default: 0xFFFFFFFF\n  octal:\n    default: 0o10\n  uppercase-hex:\n    default: 0X10\n  tagged-string:\n    default: !!str false\n  tagged-number:\n    default: !!int 0xFFFFFFFF\n  tagged-null:\n    default: !!null null\nruns:\n  using: composite\n  steps: []\n";
        let input_root = fixture("input-scalar-coercion");
        must(
            fs::write(input_root.join("action.yml"), input_source),
            "write scalar input metadata",
        );
        let scanner = must(
            super::parse_metadata(&input_root, "action.yml"),
            "parse scanner scalar input metadata",
        );
        let runner = must(
            velnor_runner::action_contract::parse_action_metadata(input_source),
            "parse runtime scalar input metadata",
        );
        for (name, expected) in [
            ("boolean", "false"),
            ("number", "1E+15"),
            ("nullable", ""),
            ("7", "1E-05"),
            ("large-decimal", "2147483648"),
            ("hexadecimal", "-1"),
            ("octal", "8"),
            ("uppercase-hex", "0X10"),
            ("tagged-string", "false"),
            ("tagged-number", "-1"),
            ("tagged-null", ""),
        ] {
            let inputs = some(scanner.inputs.as_mapping(), "scanner inputs mapping");
            let definition = some(inputs.get(name), "scanner input definition");
            let definition = some(definition.as_mapping(), "scanner input definition mapping");
            let (_, default) = some(
                definition
                    .iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case("default")),
                "scanner input default",
            );
            assert_eq!(
                default.as_str(),
                Some(expected),
                "scanner default for {name}"
            );
            assert_eq!(
                some(runner.inputs.get(name), "runtime input definition")
                    .default_value
                    .as_deref(),
                Some(expected),
                "runtime default for {name}"
            );
        }
        let _ = fs::remove_dir_all(input_root);
    }

    fn runner_scalar_docker_schema_matches_runtime_parser() {
        let docker_source = "runs:\n  using: docker\n  image: docker://ubuntu\n  args: [false, 7, null, 1e-5]\n  env:\n    BOOL: false\n    COUNT: 7\n    EMPTY: null\n    1e15: key\n";
        let docker_root = fixture("docker-scalar-coercion");
        must(
            fs::write(docker_root.join("action.yml"), docker_source),
            "write scalar Docker action metadata",
        );
        let scanner = must(
            super::parse_metadata(&docker_root, "action.yml"),
            "parse scanner scalar Docker metadata",
        );
        let runner = must(
            velnor_runner::action_contract::parse_action_metadata(docker_source),
            "parse runtime scalar Docker metadata",
        );
        let scanner_env = some(scanner.runs.env.as_mapping(), "scanner Docker environment")
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    some(value.as_str(), "scanner Docker environment value").to_owned(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(scanner_env, runner.runs.env);
        let args = some(scanner.runs.args.as_sequence(), "scanner Docker arguments");
        assert_eq!(args.len(), 4);
        assert_eq!(
            args.iter()
                .map(|value| some(value.as_str(), "scanner Docker argument"))
                .collect::<Vec<_>>(),
            ["false", "7", "", "1E-05"]
        );
        assert_eq!(runner.runs.args, ["false", "7", "", "1E-05"]);
        let _ = fs::remove_dir_all(docker_root);
    }

    fn runner_scalar_composite_schema_matches_runtime_parser() {
        let composite_source = "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        BOOL: false\n        COUNT: 7\n        EMPTY: null\n        1e15: key\n    - shell: bash\n      run: echo ok\n      env:\n        BOOL: false\n        COUNT: 7\n        EMPTY: null\n";
        let composite_root = fixture("composite-scalar-coercion");
        must(
            fs::write(composite_root.join("action.yml"), composite_source),
            "write scalar composite action metadata",
        );
        let scanner = must(
            super::parse_metadata(&composite_root, "action.yml"),
            "parse scanner scalar composite metadata",
        );
        let runner = must(
            velnor_runner::action_contract::parse_action_metadata(composite_source),
            "parse runtime scalar composite metadata",
        );
        let scanner_steps = some(scanner.runs.steps.as_deref(), "scanner composite steps");
        let with = some(
            scanner_steps[0].with.as_mapping(),
            "scanner composite inputs",
        )
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                some(value.as_str(), "scanner composite input value").to_owned(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(with, runner.runs.steps[0].with);
        let env = some(
            scanner_steps[1].env.as_mapping(),
            "scanner composite environment",
        )
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                some(value.as_str(), "scanner composite environment value").to_owned(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(env, runner.runs.steps[1].env);
        let _ = fs::remove_dir_all(composite_root);
    }

    fn runner_scalar_schema_rejects_non_scalar_values() {
        for (name, source) in [
            (
                "input-unknown-scalar-tag",
                "inputs:\n  value:\n    default: !custom false\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-mismatched-explicit-bool-tag",
                "inputs:\n  value:\n    default: !!bool \"false\"\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-radix-overflow",
                "inputs:\n  value:\n    default: 0x100000000\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-default-collection",
                "inputs:\n  value:\n    default: [false]\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "docker-env-collection",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    VALUE: {nested: value}\n",
            ),
            (
                "composite-with-collection",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        VALUE: [nested]\n",
            ),
            (
                "docker-args-collection",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  args: [{nested: value}]\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), source),
                "write collection-valued action metadata",
            );
            assert!(
                super::parse_metadata(&root, "action.yml").is_err(),
                "scanner accepted non-scalar schema value: {name}"
            );
            assert!(
                velnor_runner::action_contract::parse_action_metadata(source).is_err(),
                "runtime accepted non-scalar schema value: {name}"
            );
            let _ = fs::remove_dir_all(root);
        }

        let source = "inputs:\n  value:\n    deprecationMessage: false\nruns:\n  using: composite\n  steps: []\n";
        let root = fixture("input-deprecation-message-scalar");
        must(
            fs::write(root.join("action.yml"), source),
            "write scalar deprecation message metadata",
        );
        assert!(
            super::parse_metadata(&root, "action.yml").is_err(),
            "scanner coerced a loose deprecationMessage value"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runner_scalar_string_schema_matches_runtime_parser() {
        runner_scalar_input_schema_matches_runtime_parser();
        runner_scalar_docker_schema_matches_runtime_parser();
        runner_scalar_composite_schema_matches_runtime_parser();
        runner_scalar_schema_rejects_non_scalar_values();
    }

    fn runner_yaml_accepts_collection_tags_and_empty_null() {
        for (name, metadata) in [
            (
                "tagged-flow-collections",
                "inputs: !custom {value: {default: false}}\nruns: !!map\n  using: composite\n  steps: []\n",
            ),
            (
                "tagged-block-collections",
                "inputs: !custom\n  value:\n    default: false\nruns: !!map\n  using: composite\n  steps: []\n",
            ),
            (
                "tagged-block-collections-with-comment",
                "inputs: !custom # ignored collection tag\n  value:\n    default: false\nruns: !!map # ignored collection tag\n  using: composite\n  steps: []\n",
            ),
            (
                "empty-null",
                "inputs:\n  value:\n    default: !!null\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "empty-null-comment",
                "inputs:\n  value:\n    default: !!null # empty\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "empty-string",
                "inputs:\n  value:\n    default: !!str\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "empty-string-directive",
                "%TAG !empty! tag:yaml.org,2002:str\n---\ninputs:\n  value:\n    default: !empty!\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "empty-string-directive-comment",
                "%TAG !empty! tag:yaml.org,2002:str # keep this handle\n---\ninputs:\n  value:\n    default: !empty!\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "directive-text-in-block-scalar",
                "name: !!str false\ndescription: |\n  %TAG !! tag:custom.example,2026:\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "percent-encoded-tag-directive",
                "%TAG !core! tag:yaml.org,2002:\n---\ninputs:\n  value:\n    default: !core!%73tr false\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "percent-encoded-tag-prefix",
                "%TAG !! tag:yaml.org,2002%3A\n---\nname: !!str false\nruns:\n  using: composite\n  steps: []\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write Runner tag action metadata",
            );
            if name == "percent-encoded-tag-directive" {
                let directives = super::runner_tag_directives(metadata)
                    .unwrap_or_else(|error| panic!("tag directive parse failed: {error}"));
                assert_eq!(
                    directives.get("!core!").map(String::as_str),
                    Some("tag:yaml.org,2002:")
                );
                assert_eq!(
                    super::expand_runner_tag("!core!str", &directives).as_deref(),
                    Some("tag:yaml.org,2002:str")
                );
            }
            let scanner = must(
                super::parse_metadata(&root, "action.yml"),
                "scanner must match Runner collection and empty-null tags",
            );
            must(
                velnor_runner::action_contract::parse_action_metadata(metadata),
                "runtime must accept Runner collection and empty-null tags",
            );
            if name.starts_with("empty-string") {
                let default = scanner
                    .inputs
                    .as_mapping()
                    .and_then(|inputs| inputs.get("value"))
                    .and_then(serde_yaml::Value::as_mapping)
                    .and_then(|definition| definition.get("default"))
                    .and_then(serde_yaml::Value::as_str);
                assert_eq!(default, Some(""), "scanner empty string value: {name}");
            }
            let _ = fs::remove_dir_all(root);
        }
    }

    fn runner_yaml_rejects_unknown_tags() {
        for (name, metadata) in [
            (
                "unknown-empty-tag",
                "inputs:\n  value:\n    default: !custom\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "unknown-scalar-tag",
                "inputs:\n  value:\n    default: !custom false\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "overridden-secondary-tag-handle",
                "%TAG !! tag:custom.example,2026:\n---\ninputs:\n  value:\n    default: !!str false\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "overridden-secondary-tag-handle-custom-prefix",
                "%TAG !! !custom-\n---\ninputs:\n  value:\n    default: !!str false\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "overridden-primary-tag-handle-custom-prefix",
                "%TAG ! !custom-\n---\ninputs:\n  value:\n    default: !str false\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "duplicate-tag-directive-handle",
                "%TAG !core! tag:yaml.org,2002:\n%TAG !core! tag:custom.example,2026:\n---\ninputs:\n  value:\n    default: !core!str false\nruns:\n  using: composite\n  steps: []\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write unknown-tag action metadata",
            );
            assert!(
                super::parse_metadata(&root, "action.yml").is_err(),
                "scanner accepted unknown scalar tag: {name}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn runner_yaml_ignores_collection_tags_and_accepts_empty_null() {
        runner_yaml_accepts_collection_tags_and_empty_null();
        runner_yaml_rejects_unknown_tags();
    }

    #[test]
    fn local_uses_detection_preserves_raw_runner_whitespace() {
        let cases = [
            ("exact", "./nested", None),
            (
                "leading-space",
                " ./nested",
                Some("must use a full 40-character SHA pin"),
            ),
            (
                "trailing-space",
                "./nested ",
                Some("has no action metadata or Dockerfile under `nested `"),
            ),
        ];

        for (name, reference, expected_error) in cases {
            let root = fixture(&format!("uses-whitespace-{name}"));
            must(
                fs::create_dir_all(root.join("nested")),
                "create local action target",
            );
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: composite\n  steps:\n    - uses: \"{reference}\"\n"),
                ),
                "write parent action metadata",
            );
            must(
                fs::write(
                    root.join("nested/action.yml"),
                    "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo child\n",
                ),
                "write child action metadata",
            );

            let result = super::super::scan_shape_for_tests(&root, &providers(), "main", &[]);
            match expected_error {
                Some(expected) => assert!(
                    result
                        .err()
                        .unwrap_or_else(|| panic!("whitespace action reference passed: {name}"))
                        .to_string()
                        .contains(expected),
                    "wrong diagnostic for raw uses value {reference:?}"
                ),
                None => {
                    must(result, "scan exact local action marker");
                }
            }
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn runner_yaml_rejects_collection_keys_and_anchors() {
        let collection_keys = [
            ("block-sequence", "? [complex, key]\n: ignored\n"),
            ("block-mapping", "? {complex: key}\n: ignored\n"),
            ("flow-sequence", "{? [complex, key] : ignored}\n"),
            ("flow-mapping", "{? {complex: key} : ignored}\n"),
        ];
        for (name, contents) in collection_keys {
            let error =
                super::validate_runner_yaml_syntax(contents, std::path::Path::new("action.yml"))
                    .err()
                    .unwrap_or_else(|| panic!("Runner accepted {name} YAML mapping key"));
            assert!(
                error
                    .to_string()
                    .contains("does not support collection mapping keys"),
                "unexpected {name} mapping-key error: {error}"
            );
        }

        let error = super::validate_runner_yaml_syntax(
            "base: &base value\ncopy: *base\n",
            std::path::Path::new("action.yml"),
        )
        .err()
        .unwrap_or_else(|| panic!("Runner accepted YAML anchors and aliases"));
        assert!(
            error
                .to_string()
                .contains("does not support YAML anchors or aliases"),
            "unexpected anchor/alias error: {error}"
        );
    }

    #[test]
    fn runner_yaml_rejects_duplicate_keys_before_value_coercion() {
        let cases = [
            ("exact-block", "name: first\nname: second\n"),
            ("ascii-case", "name: first\nNAME: second\n"),
            ("unicode-case", "É: first\né: second\n"),
            ("scalar-equivalent", "1: first\n\"1\": second\n"),
            ("g15-number-equivalent", "1e15: first\n\"1E+15\": second\n"),
            ("null-empty-equivalent", "null: first\n\"\": second\n"),
            ("tagged-scalar-equivalent", "!!str 1: first\n\"1\": second\n"),
            ("flow-map", "{name: first, NAME: second}\n"),
            (
                "nested-any-map",
                "inputs:\n  good:\n    custom: {name: first, NAME: second}\nruns:\n  using: composite\n  steps: []\n",
            ),
        ];
        for (name, contents) in cases {
            let error =
                super::validate_runner_yaml_syntax(contents, std::path::Path::new("action.yml"))
                    .err()
                    .unwrap_or_else(|| {
                        panic!("Runner duplicate mapping keys passed preflight: {name}")
                    });
            let message = error.to_string();
            assert!(
                message
                    .contains("duplicate YAML mapping key after Runner case-insensitive matching")
                    || message.contains("distinct mapping keys collide after string conversion"),
                "unexpected duplicate-key error for {name}: {message}"
            );
        }
    }

    #[test]
    fn runner_scalar_mapping_keys_are_coerced_by_schema_role() {
        let cases = [
            (
                "input-numeric-key",
                "inputs:\n  1:\n    default: numeric input name\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-boolean-key",
                "inputs:\n  true:\n    default: boolean input name\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "docker-env-numeric-key",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    1e15: numeric env key\n",
            ),
            (
                "composite-with-boolean-key",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        true: boolean input key\n",
            ),
            (
                "empty-key-in-any-value",
                "inputs:\n  good:\n    custom:\n      \"\": allowed\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "null-key-in-any-value",
                "inputs:\n  good:\n    custom:\n      null: allowed\nruns:\n  using: composite\n  steps: []\n",
            ),
        ];
        for (name, metadata) in cases {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write Runner scalar-key action metadata",
            );
            must(
                super::parse_metadata(&root, "action.yml"),
                "parse scalar-key action metadata",
            );
            must(
                velnor_runner::action_contract::parse_action_metadata(metadata),
                "parse runtime scalar-key action metadata",
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn implicit_empty_mapping_key_does_not_shift_tagged_scalar_source_indices() {
        let metadata = "inputs:\n  good:\n    custom:\n      : ignored empty key\n      tagged: !!int 0xFFFFFFFF\nruns:\n  using: composite\n  steps: []\n";
        let root = fixture("empty-key-tag-source-index");
        must(
            fs::write(root.join("action.yml"), metadata),
            "write action metadata with implicit empty key",
        );
        let scanner = must(
            super::parse_metadata(&root, "action.yml"),
            "parse implicit empty-key action metadata",
        );
        let inputs = some(scanner.inputs.as_mapping(), "scanner inputs mapping");
        let good = some(inputs.get("good"), "scanner good input");
        let good = some(good.as_mapping(), "scanner good input mapping");
        let custom = some(good.get("custom"), "scanner custom metadata");
        let custom = some(custom.as_mapping(), "scanner custom metadata mapping");
        assert_eq!(
            custom.get("").and_then(serde_yaml::Value::as_str),
            Some("ignored empty key"),
            "normalized custom metadata: {custom:?}"
        );
        assert_eq!(
            custom.get("tagged").and_then(serde_yaml::Value::as_f64),
            Some(-1.0),
            "normalized custom metadata: {custom:?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn action_metadata_admission_enforces_byte_node_and_depth_limits() {
        let root = fixture("metadata-admission-limits");
        let base = "runs:\n  using: composite\n  steps: []\n";
        let mut at_limit = base.to_owned();
        at_limit.push('#');
        at_limit.push_str(&"x".repeat(MAX_ACTION_METADATA_BYTES - at_limit.len()));
        assert_eq!(at_limit.len(), MAX_ACTION_METADATA_BYTES);
        must(
            fs::write(root.join("action.yml"), &at_limit),
            "write 1 MiB action metadata",
        );
        must(
            super::parse_metadata(&root, "action.yml"),
            "parse action metadata at byte limit",
        );

        at_limit.push('x');
        must(
            fs::write(root.join("action.yml"), &at_limit),
            "write over-limit action metadata",
        );
        assert!(
            super::parse_metadata(&root, "action.yml").is_err(),
            "scanner accepted metadata larger than 1 MiB"
        );

        let mut over_node_limit = String::new();
        for index in 0..25_001 {
            let _ = writeln!(over_node_limit, "loose-{index}: value");
        }
        over_node_limit.push_str(base);
        assert!(
            over_node_limit.len() < MAX_ACTION_METADATA_BYTES,
            "node fixture must remain below the byte limit"
        );
        must(
            fs::write(root.join("action.yml"), &over_node_limit),
            "write over-node-limit action metadata",
        );
        assert!(
            super::parse_metadata(&root, "action.yml").is_err(),
            "scanner accepted more than 50,000 YAML nodes"
        );

        let depth_fixture = |nested_sequences: usize| {
            format!(
                "ignored: {}null{}\nruns:\n  using: composite\n  steps: []\n",
                "[".repeat(nested_sequences),
                "]".repeat(nested_sequences)
            )
        };
        must(
            fs::write(
                root.join("action.yml"),
                depth_fixture(MAX_ACTION_METADATA_DEPTH - 1),
            ),
            "write action metadata at depth limit",
        );
        must(
            super::parse_metadata(&root, "action.yml"),
            "parse action metadata at depth limit",
        );
        must(
            fs::write(
                root.join("action.yml"),
                depth_fixture(MAX_ACTION_METADATA_DEPTH),
            ),
            "write action metadata over depth limit",
        );
        assert!(
            super::parse_metadata(&root, "action.yml").is_err(),
            "scanner accepted 65 nested mapping/sequence levels"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runner_well_known_field_names_match_exact_case() {
        for (name, metadata) in [
            ("uppercase-runs", "RUNS:\n  using: composite\n  steps: []\n"),
            (
                "uppercase-using",
                "runs:\n  Using: composite\n  steps: []\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write case-sensitive action metadata",
            );
            assert!(
                super::parse_metadata(&root, "action.yml").is_err(),
                "scanner treated fixed schema field as case-insensitive: {name}"
            );
            assert!(
                velnor_runner::action_contract::parse_action_metadata(metadata).is_err(),
                "runtime treated fixed schema field as case-insensitive: {name}"
            );
            let _ = fs::remove_dir_all(root);
        }

        let metadata =
            "INPUTS:\n  value:\n    default: ignored\nruns:\n  using: composite\n  steps: []\n";
        let root = fixture("uppercase-inputs");
        must(
            fs::write(root.join("action.yml"), metadata),
            "write case-sensitive input field metadata",
        );
        let scanner = must(
            super::parse_metadata(&root, "action.yml"),
            "parse case-sensitive input field metadata",
        );
        let runner = must(
            velnor_runner::action_contract::parse_action_metadata(metadata),
            "parse runtime case-sensitive input field metadata",
        );
        assert!(scanner.inputs.is_null());
        assert!(runner.inputs.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runner_string_mapping_keys_preserve_ordinal_case_rules() {
        for (name, metadata) in [
            (
                "case-insensitive-input-deprecation-message",
                "inputs:\n  good:\n    DePreCaTiOnMeSsAgE: available\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "long-s-remains-ordinal-distinct",
                "s: first\nſ: second\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "dotless-i-remains-ordinal-distinct",
                "I: first\nı: second\nruns:\n  using: composite\n  steps: []\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write Runner string-key action metadata",
            );
            super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
                .unwrap_or_else(|error| panic!("Runner string-key metadata was rejected ({name}): {error}"));
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn runner_scalar_mapping_keys_enforce_schema_and_duplicate_rules() {
        let cases = [
            (
                "root-null-key",
                "null: unknown root metadata\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-null-key",
                "inputs:\n  null:\n    default: nullable input name\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-definition-null-key",
                "inputs:\n  good:\n    null: ignored property\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "output-null-key",
                "outputs:\n  null:\n    description: nullable output name\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "output-definition-null-key",
                "outputs:\n  good:\n    null: ignored property\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "runs-null-key",
                "runs:\n  using: composite\n  null: ignored property\n  steps: []\n",
            ),
            (
                "runs-env-null-key",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    null: value\n",
            ),
            (
                "composite-step-null-key",
                "runs:\n  using: composite\n  steps:\n    - null: unknown property\n      shell: bash\n      run: echo ok\n",
            ),
            (
                "composite-step-with-null-key",
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n      with:\n        null: value\n",
            ),
            (
                "composite-step-env-null-key",
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n      env:\n        null: value\n",
            ),
            (
                "scalar-coercion-duplicate-key",
                "1: first\n\"1\": second\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "unicode-case-insensitive-duplicate-key",
                "É: first\né: second\nruns:\n  using: composite\n  steps: []\n",
            ),
        ];
        for (name, metadata) in cases {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write Runner-incompatible scalar-key action metadata",
            );
            let error = super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| {
                    panic!("Runner-incompatible scalar-key metadata passed: {name}")
                });
            let message = error.to_string();
            assert!(
                message.contains("non-empty strings")
                    || message.contains("duplicate YAML mapping key")
                    || message.contains("distinct mapping keys collide after string conversion"),
                "unexpected scalar-key error for {name}: {message}"
            );
            if name.ends_with("null-key") {
                assert!(
                    message.contains("non-empty strings"),
                    "Runner null key did not become an empty schema key for {name}: {message}"
                );
            }
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn composite_steps_presence_matches_runner_schema() {
        let empty = fixture("composite-empty-steps");
        must(
            fs::write(
                empty.join("action.yml"),
                "runs:\n  using: composite\n  steps: []\n",
            ),
            "write explicit empty composite steps",
        );
        let shape = must(
            super::super::scan_shape_for_tests(&empty, &providers(), "main", &[]),
            "scan explicit empty composite steps",
        );
        assert!(shape
            .units
            .iter()
            .any(|unit| { unit.kind == crate::s2::UnitKind::GithubAction && unit.root == "." }));
        let _ = fs::remove_dir_all(empty);

        let missing = fixture("composite-missing-steps");
        must(
            fs::write(missing.join("action.yml"), "runs:\n  using: composite\n"),
            "write composite metadata without steps",
        );
        let error = super::super::scan_shape_for_tests(&missing, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("composite action without `steps` passed scan"));
        assert!(
            error.to_string().contains("must declare runs.steps"),
            "unexpected missing composite steps error: {error}"
        );
        let _ = fs::remove_dir_all(missing);
    }

    #[test]
    fn composite_continue_on_error_accepts_only_boolean_or_expression() {
        for (name, value, accepted) in [
            ("false", "false", true),
            ("expression", "${{ inputs.enabled }}", true),
            ("quoted-boolean", "\"true\"", false),
            ("number", "1", false),
            ("null", "null", false),
            ("embedded-expression", "before ${{ inputs.enabled }}", false),
        ] {
            let metadata = format!(
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n      continue-on-error: {value}\n"
            );
            let root = fixture(&format!("continue-on-error-{name}"));
            must(
                fs::write(root.join("action.yml"), &metadata),
                "write continue-on-error metadata",
            );
            let scanner_accepts =
                super::super::scan_shape_for_tests(&root, &providers(), "main", &[]).is_ok();
            let runtime_accepts =
                velnor_runner::action_contract::parse_action_metadata(&metadata).is_ok();
            assert_eq!(scanner_accepts, accepted, "scanner result for {name}");
            assert_eq!(runtime_accepts, accepted, "runtime result for {name}");
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn runtime_switch_does_not_trim_using() {
        let root = fixture("untrimmed-runtime");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: ' composite '\n  steps: []\n",
            ),
            "write whitespace-padded runtime metadata",
        );
        let error = super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("whitespace-padded runtime must not match Runner runtime"));
        assert!(
            error
                .to_string()
                .contains("unsupported GitHub Action runtime"),
            "unexpected runtime error: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_action_reference_requires_owner_and_repository() {
        let root = fixture("external-reference-shape");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: foo@0123456789abcdef0123456789abcdef01234567\n",
            ),
            "write malformed repository action reference",
        );
        let error = super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("repository action without owner passed scan"));
        assert!(
            error.to_string().contains("full 40-character SHA pin"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_action_reference_rejects_unsafe_paths_and_accepts_valid_subpath() {
        let invalid = [
            (
                "traversal",
                "foo/bar/../../evil@0123456789abcdef0123456789abcdef01234567",
            ),
            (
                "owner-traversal",
                "foo/../evil@0123456789abcdef0123456789abcdef01234567",
            ),
            (
                "empty-segment",
                "foo/bar//evil@0123456789abcdef0123456789abcdef01234567",
            ),
            (
                "trailing-empty-segment",
                "foo/bar/evil/@0123456789abcdef0123456789abcdef01234567",
            ),
        ];
        for (name, uses) in invalid {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: composite\n  steps:\n    - uses: {uses}\n"),
                ),
                "write unsafe repository action reference",
            );
            let error = super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("unsafe repository action path passed scan: {name}"));
            assert!(
                error.to_string().contains("full 40-character SHA pin"),
                "unexpected unsafe path error for {name}: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }

        let valid = fixture("valid-subpath");
        must(
            fs::write(
                valid.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: foo/bar/sub/action@0123456789abcdef0123456789abcdef01234567\n",
            ),
            "write valid repository action subpath",
        );
        let shape = must(
            super::super::scan_shape_for_tests(&valid, &providers(), "main", &[]),
            "scan valid repository action subpath",
        );
        assert!(shape
            .units
            .iter()
            .any(|unit| { unit.kind == crate::s2::UnitKind::GithubAction && unit.root == "." }));
        let _ = fs::remove_dir_all(valid);
    }

    #[test]
    fn plugin_action_metadata_is_explicitly_unsupported() {
        let root = fixture("plugin-runtime");
        must(
            fs::write(root.join("action.yml"), "runs:\n  plugin: Example.Plugin\n"),
            "write plugin action metadata",
        );
        let error = super::super::scan_shape_for_tests(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("plugin action unexpectedly passed scan"));
        assert!(
            error
                .to_string()
                .contains("unsupported GitHub Action runtime `plugin`"),
            "plugin boundary was not explicit: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "runner precedence fixture keeps the competing action roots in one audit"
    )]
    fn action_entrypoint_precedence_and_bare_dockerfile_match_runner() {
        let root = fixture("entrypoints");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo selected\n    - uses: ./actions/bare\n",
            ),
            "write preferred action metadata",
        );
        must(
            fs::write(
                root.join("action.yaml"),
                "runs:\n  using: unsupported\n  main: missing\n",
            ),
            "write shadowed action metadata",
        );
        must(
            fs::create_dir_all(root.join("actions/bare")),
            "create bare Docker action",
        );
        must(
            fs::write(root.join("actions/bare/dockerfile"), "FROM scratch\n"),
            "write lowercase Dockerfile fallback",
        );
        must(
            fs::write(
                root.join("actions/bare/Cargo.toml"),
                "[package]\nname = \"bare-action\"\nversion = \"0.0.0\"\n",
            ),
            "write project manifest beside referenced Dockerfile action",
        );
        must(
            fs::write(root.join("actions/bare/package.json"), "{}\n"),
            "write second project manifest beside referenced Dockerfile action",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"stable\"\n",
            ),
            "pin fixture Rust toolchain",
        );
        must(
            fs::create_dir_all(root.join("actions/ordinary")),
            "create ordinary Docker project",
        );
        must(
            fs::write(root.join("actions/ordinary/Dockerfile"), "FROM scratch\n"),
            "write unreferenced Dockerfile",
        );
        must(
            fs::write(
                root.join("actions/ordinary/Cargo.toml"),
                "[package]\nname = \"ordinary\"\nversion = \"0.0.0\"\n",
            ),
            "write ordinary project manifest",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/explicit")),
            "create explicit GitHub action namespace",
        );
        must(
            fs::write(
                root.join(".github/actions/explicit/dockerfile"),
                "FROM scratch\n",
            ),
            "write referenced Dockerfile action",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/unreferenced")),
            "create unreferenced Dockerfile action",
        );
        must(
            fs::write(
                root.join(".github/actions/unreferenced/Dockerfile"),
                "FROM scratch\n",
            ),
            "write unreferenced Dockerfile action",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/decoy")),
            "create action path nested under with",
        );
        must(
            fs::write(
                root.join(".github/actions/decoy/Dockerfile"),
                "FROM scratch\n",
            ),
            "write with.uses decoy Dockerfile",
        );
        must(
            fs::create_dir_all(root.join(".github/workflows")),
            "create generated workflow namespace",
        );
        must(
            fs::write(
                root.join(".github/workflows/ignored-action.yml"),
                "name: local action references\njobs:\n  consume:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: ./.github/actions/explicit\n      - uses: actions/checkout@692973e3d937129bcbf40652eb9f2f61becf3332\n        with:\n          uses: ./.github/actions/decoy\n  reusable:\n    uses: ./.github/workflows/reusable.yml\n",
            ),
            "write workflow action references",
        );
        must(
            fs::create_dir_all(root.join("actions/shadowed")),
            "create Docker metadata action",
        );
        must(
            fs::write(
                root.join("actions/shadowed/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo metadata\n",
            ),
            "write metadata action",
        );
        must(
            fs::write(root.join("actions/shadowed/Dockerfile"), "FROM scratch\n"),
            "write shadowed Dockerfile",
        );
        let providers = providers();
        let files = must(
            super::super::file_walk::repository_files(&root, &[]),
            "walk action entrypoints",
        );
        assert_eq!(
            super::workflow_action_roots(&root, &files).unwrap_or_else(|error| panic!("{error}")),
            std::collections::BTreeSet::from([".github/actions/explicit".to_owned()])
        );
        assert!(files
            .iter()
            .any(|file| file == ".github/actions/explicit/dockerfile"));
        assert!(files
            .iter()
            .any(|file| file == ".github/workflows/ignored-action.yml"));
        let shape = must(
            super::super::scan_shape_for_tests(&root, &providers, "main", &[]),
            "scan action entrypoints",
        );
        let root_action = shape
            .units
            .iter()
            .find(|unit| unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("preferred metadata action missing"));
        assert!(root_action
            .pr_commands
            .iter()
            .any(|command| command == "velnor-workflow verify-action --path 'action.yml'"));
        assert!(!shape.units.iter().any(|unit| {
            unit.kind == crate::s2::UnitKind::GithubAction
                && unit
                    .pr_commands
                    .iter()
                    .any(|command| command.contains("action.yaml"))
        }));
        let bare = shape
            .units
            .iter()
            .find(|unit| unit.root == "actions/bare")
            .unwrap_or_else(|| panic!("bare Dockerfile action missing"));
        assert_eq!(bare.kind, crate::s2::UnitKind::GithubAction);
        assert!(bare.capabilities.docker);
        assert!(bare
            .pr_commands
            .iter()
            .any(|command| command
                == "velnor-workflow verify-action --path 'actions/bare/dockerfile'"));
        assert!(!shape.units.iter().any(|unit| {
            unit.kind == crate::s2::UnitKind::GithubAction && unit.root == "actions/ordinary"
        }));
        let explicit = shape
            .units
            .iter()
            .find(|unit| unit.root == ".github/actions/explicit")
            .unwrap_or_else(|| panic!("workflow-referenced .github/actions action missing"));
        assert_eq!(explicit.kind, crate::s2::UnitKind::GithubAction);
        assert!(!shape.units.iter().any(|unit| {
            unit.kind == crate::s2::UnitKind::GithubAction
                && matches!(
                    unit.root.as_str(),
                    ".github/actions/unreferenced" | ".github/actions/decoy"
                )
        }));
        let error = super::verify_action(&root, "action.yaml")
            .err()
            .unwrap_or_else(|| panic!("shadowed action.yaml must not be canonical"));
        assert!(
            error.to_string().contains("canonical action entrypoint"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "scanner classification regression cases stay together"
    )]
    #[test]
    fn docker_images_and_action_path_are_classified_like_runner() {
        let root = fixture("docker-and-action-path");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: ${{ github.action_path }}/scripts/check.sh && ${{ github.action_path }}/scripts/next.sh\n    - uses: actions/example@0123456789abcdef0123456789abcdef01234567\n",
            ),
            "write action_path metadata",
        );
        must(
            fs::write(root.join("scripts/check.sh"), "exit 0\n"),
            "write canonical action_path script",
        );
        must(
            fs::write(root.join("scripts/next.sh"), "exit 0\n"),
            "write second action_path script",
        );
        must(
            fs::create_dir_all(root.join("actions/images")),
            "create image action directory",
        );
        must(
            fs::write(
                root.join("actions/images/action.yml"),
                "runs:\n  using: docker\n  image: docker://ubuntu\n  entrypoint: /container-entrypoint.sh\n  pre-entrypoint: /container-pre.sh\n  post-entrypoint: /container-post.sh\n",
            ),
            "write ordinary image metadata",
        );
        let providers = providers();
        let shape = must(
            super::super::scan_shape_for_tests(&root, &providers, "main", &[]),
            "scan Docker image action",
        );
        let root_action = shape
            .units
            .iter()
            .find(|unit| unit.root == ".")
            .unwrap_or_else(|| panic!("action_path action missing"));
        assert!(root_action
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'scripts/check.sh'"));
        assert!(root_action
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'scripts/next.sh'"));
        let image_action = shape
            .units
            .iter()
            .find(|unit| unit.root == "actions/images")
            .unwrap_or_else(|| panic!("ordinary image action missing"));
        assert!(image_action.capabilities.docker);
        assert_eq!(image_action.pr_commands.len(), 1);
        let _ = fs::remove_dir_all(root);

        let plain_image = fixture("plain-docker-image");
        must(
            fs::write(
                plain_image.join("action.yml"),
                "runs:\n  using: docker\n  image: ubuntu\n",
            ),
            "write unsupported plain Docker image metadata",
        );
        let error = super::super::scan_shape_for_tests(&plain_image, &providers, "main", &[])
            .err()
            .unwrap_or_else(|| panic!("plain Docker image must fail runner-equivalent scan"));
        assert!(
            error
                .to_string()
                .contains("Dockerfile path or docker:// image reference"),
            "unexpected plain Docker image error: {error}"
        );
        let _ = fs::remove_dir_all(plain_image);

        let mutable = fixture("mutable-uses");
        must(
            fs::write(
                mutable.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@main\n",
            ),
            "write mutable action metadata",
        );
        let error = super::super::scan_shape_for_tests(&mutable, &providers, "main", &[])
            .err()
            .unwrap_or_else(|| panic!("mutable external uses must fail"));
        assert!(
            error.to_string().contains("full 40-character SHA pin"),
            "{error}"
        );
        let _ = fs::remove_dir_all(mutable);

        let whitespace = fixture("whitespace-uses");
        must(
            fs::write(
                whitespace.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ' actions/example@0123456789abcdef0123456789abcdef01234567 '\n",
            ),
            "write whitespace action metadata",
        );
        let error = super::super::scan_shape_for_tests(&whitespace, &providers, "main", &[])
            .err()
            .unwrap_or_else(|| panic!("surrounding whitespace must fail strict external uses"));
        assert!(
            error.to_string().contains("full 40-character SHA pin"),
            "{error}"
        );
        let _ = fs::remove_dir_all(whitespace);
    }

    #[test]
    fn local_action_paths_use_runner_markers_and_workspace_root() {
        let root = fixture("local-action-paths");
        must(
            fs::create_dir_all(root.join("actions/parent")),
            "create nested parent action",
        );
        must(
            fs::create_dir_all(root.join("actions/child")),
            "create workspace child action",
        );
        must(
            fs::create_dir_all(root.join("actions/grandchild")),
            "create transitive grandchild action",
        );
        must(
            fs::write(
                root.join("actions/parent/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ./actions/child\n",
            ),
            "write nested parent action metadata",
        );
        must(
            fs::write(
                root.join("actions/child/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ./actions/grandchild\n",
            ),
            "write workspace child action metadata",
        );
        must(
            fs::write(
                root.join("actions/grandchild/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo grandchild\n",
            ),
            "write grandchild action metadata",
        );
        let files = must(
            super::super::file_walk::repository_files(&root, &[]),
            "walk nested local actions",
        );
        let references = must(
            super::local_references(
                &root,
                &must(
                    super::parse_metadata(&root, "actions/parent/action.yml"),
                    "parse nested parent action",
                )
                .runs,
                "actions/parent",
                &files,
                &BTreeSet::new(),
            ),
            "resolve nested action from workspace root",
        );
        assert_eq!(references, ["actions/child/action.yml"]);
        let shape = must(
            super::super::scan_shape_for_tests(&root, &providers(), "main", &[]),
            "scan nested local actions",
        );
        let parent = shape
            .units
            .iter()
            .find(|unit| unit.root == "actions/parent")
            .unwrap_or_else(|| panic!("parent action unit missing"));
        assert!(parent.watch.iter().any(|path| path == "actions/child/**"));
        assert!(parent
            .watch
            .iter()
            .any(|path| path == "actions/grandchild/**"));
        for changed in [
            "actions/child/action.yml",
            "actions/grandchild/action.yml",
            "actions/grandchild/helper.sh",
        ] {
            assert!(
                parent.watch.iter().any(|pattern| {
                    globset::Glob::new(pattern)
                        .is_ok_and(|glob| glob.compile_matcher().is_match(changed))
                }),
                "editing nested helper path {changed} selects its parent consumer"
            );
        }
        let child = shape
            .units
            .iter()
            .find(|unit| unit.root == "actions/child")
            .unwrap_or_else(|| panic!("child action unit missing"));
        assert!(child
            .watch
            .iter()
            .any(|path| path == "actions/grandchild/**"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn composite_scripts_resolve_from_working_directory_and_action_path() {
        let root = fixture("composite-working-directory");
        must(
            fs::create_dir_all(root.join("actions/child/scripts")),
            "create nested action script directory",
        );
        must(
            fs::write(
                root.join("actions/child/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      working-directory: scripts\n      run: node ./workspace.js && node ${{ github.action_path }}/scripts/action.js\n    - shell: bash\n      run: node ./root.js\n",
            ),
            "write composite script metadata",
        );
        must(
            fs::write(root.join("scripts/workspace.js"), "process.exit(0)\n"),
            "write working-directory script",
        );
        must(
            fs::write(
                root.join("actions/child/scripts/action.js"),
                "process.exit(0)\n",
            ),
            "write action_path script",
        );
        must(
            fs::write(root.join("root.js"), "process.exit(0)\n"),
            "write workspace-root script",
        );
        let files = must(
            super::super::file_walk::repository_files(&root, &[]),
            "walk composite working-directory fixture",
        );
        let metadata = must(
            super::parse_metadata(&root, "actions/child/action.yml"),
            "parse composite working-directory metadata",
        );
        let references = must(
            super::local_references(
                &root,
                &metadata.runs,
                "actions/child",
                &files,
                &BTreeSet::new(),
            ),
            "resolve composite script paths",
        );
        assert_eq!(
            references,
            [
                "actions/child/scripts/action.js",
                "root.js",
                "scripts/workspace.js"
            ]
        );
        let shape = must(
            super::super::scan_shape_for_tests(&root, &providers(), "main", &[]),
            "scan composite working-directory fixture",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == "actions/child")
            .unwrap_or_else(|| panic!("composite action unit missing"));
        assert!(action.watch.iter().any(|path| path == "actions/child/**"));
        for path in [
            "actions/child/scripts/action.js",
            "root.js",
            "scripts/workspace.js",
        ] {
            assert!(
                action.watch.iter().any(|watched| watched == path),
                "action watch must include exact local reference {path}: {:?}",
                action.watch
            );
            assert!(
                action
                    .pr_commands
                    .iter()
                    .any(|command| command.contains(path)),
                "action validation must check local reference {path}: {:?}",
                action.pr_commands
            );
        }
        assert_change_selects_action_unit(action, "root.js");
        assert_change_selects_action_unit(action, "scripts/workspace.js");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_composite_working_directory_references_select_consumers() {
        let root = fixture("nested-composite-working-directory");
        must(
            fs::create_dir_all(root.join("actions/parent")),
            "create parent composite directory",
        );
        must(
            fs::create_dir_all(root.join("actions/child")),
            "create child composite directory",
        );
        must(
            fs::create_dir_all(root.join("shared")),
            "create nested composite working directory",
        );
        must(
            fs::write(
                root.join("actions/parent/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ./actions/child\n",
            ),
            "write parent composite metadata",
        );
        must(
            fs::write(
                root.join("actions/child/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      working-directory: shared\n      run: node ./child.js\n",
            ),
            "write child composite metadata",
        );
        must(
            fs::write(root.join("shared/child.js"), "process.exit(0)\n"),
            "write child working-directory script",
        );

        let shape = must(
            super::super::scan_shape_for_tests(&root, &providers(), "main", &[]),
            "scan nested composite fixture",
        );
        let parent = shape
            .units
            .iter()
            .find(|unit| unit.root == "actions/parent")
            .unwrap_or_else(|| panic!("parent action unit missing"));
        assert!(parent.watch.iter().any(|path| path == "actions/child/**"));
        assert!(
            parent.watch.iter().any(|path| path == "shared/child.js"),
            "parent action watch must include nested exact reference: {:?}",
            parent.watch
        );
        assert!(
            parent
                .pr_commands
                .iter()
                .any(|command| command.contains("shared/child.js")),
            "parent action validation must check nested exact reference: {:?}",
            parent.pr_commands
        );
        assert_change_selects_action_unit(parent, "shared/child.js");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_action_paths_accept_only_runner_markers_and_reject_escape_or_symlinks() {
        let root = fixture("local-action-markers");
        must(
            fs::create_dir_all(root.join("actions/child")),
            "create local action target",
        );
        for reference in ["./actions/child", ".\\actions\\child"] {
            assert_eq!(
                super::discovered_local_action_root(&root, reference)
                    .unwrap_or_else(|error| panic!("{error}")),
                Some("actions/child".to_owned()),
                "Runner local marker did not resolve: {reference}"
            );
        }
        for reference in ["actions/child", ".actions/child", "..actions/child"] {
            assert_eq!(
                super::discovered_local_action_root(&root, reference)
                    .unwrap_or_else(|error| panic!("{error}")),
                None,
                "non-Runner marker was accepted: {reference}"
            );
        }
        for reference in [
            "../outside",
            "./../outside",
            "/absolute/path",
            "C:\\outside",
            "./C:/outside",
        ] {
            assert!(
                super::discovered_local_action_root(&root, reference).is_err(),
                "unsafe local path was accepted: {reference}"
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            must(
                symlink(root.join("actions/child"), root.join("actions/link")),
                "create symlinked action component",
            );
            let error = super::discovered_local_action_root(&root, "./actions/link")
                .err()
                .unwrap_or_else(|| panic!("symlinked action component was accepted"));
            assert!(error.to_string().contains("traverses symlink"), "{error}");

            must(
                fs::create_dir_all(root.join("real-root/child")),
                "create symlinked action ancestor target",
            );
            must(
                symlink(root.join("real-root"), root.join("linked-root")),
                "create symlinked action ancestor",
            );
            let error = super::discovered_local_action_root(&root, "./linked-root/child")
                .err()
                .unwrap_or_else(|| panic!("symlinked action root ancestor was accepted"));
            assert!(error.to_string().contains("traverses symlink"), "{error}");

            must(
                fs::create_dir_all(root.join("real-root/action")),
                "create symlinked action entrypoint target",
            );
            must(
                fs::write(root.join("real-root/action/main.js"), "process.exit(0)\n"),
                "write symlinked action entrypoint target",
            );
            let mut references = std::collections::BTreeSet::new();
            let error = super::add_action_path_reference(
                &mut references,
                "main.js",
                "linked-root/action",
                &root,
                &["linked-root/action/main.js".to_owned()],
                &BTreeSet::new(),
            )
            .err()
            .unwrap_or_else(|| panic!("symlinked action root ancestor entrypoint was accepted"));
            assert!(error.to_string().contains("symlink"), "{error}");
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workflow_local_uses_proves_bare_dockerfile_action_without_manifest_guessing() {
        let root = fixture("workflow-bare-docker");
        must(
            fs::write(root.join("Dockerfile"), "FROM scratch\n"),
            "write workflow-referenced Dockerfile",
        );
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"workflow-bare\"\nversion = \"0.0.0\"\n",
            ),
            "write Rust manifest beside action Dockerfile",
        );
        must(
            fs::write(root.join("package.json"), "{}\n"),
            "write package manifest beside action Dockerfile",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"stable\"\n",
            ),
            "pin workflow fixture Rust toolchain",
        );
        must(
            fs::create_dir_all(root.join(".github/workflows")),
            "create workflow source directory",
        );
        must(
            fs::write(
                root.join(".github/workflows/consumer.yml"),
                "name: consumer\njobs:\n  consume:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: ./\n",
            ),
            "write local action consumer workflow",
        );
        let shape = must(
            super::super::scan_shape_for_tests(&root, &providers(), "main", &[]),
            "scan workflow-referenced Docker action",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("workflow-referenced bare Dockerfile action missing"));
        assert_eq!(action.root, ".");
        assert!(action
            .pr_commands
            .iter()
            .any(|command| command == "velnor-workflow verify-action --path 'Dockerfile'"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "runner compatibility matrix keeps all action fixtures in one auditable test"
    )]
    fn runner_matrix_covers_node_hooks_container_images_and_dynamic_paths() {
        let node = fixture("node-hooks");
        must(
            fs::create_dir_all(node.join("dist")),
            "create Node action dist",
        );
        must(
            fs::write(
                node.join("action.yaml"),
                "runs:\n  using: node20\n  main: dist/main.js\n  pre: dist/pre.js\n  post: dist/post.js\n",
            ),
            "write Node hook metadata",
        );
        for file in ["main.js", "pre.js", "post.js"] {
            must(
                fs::write(node.join("dist").join(file), "process.exit(0)\n"),
                "write Node hook entrypoint",
            );
        }
        let shape = must(
            super::super::scan_shape_for_tests(&node, &providers(), "main", &[]),
            "scan Node hook action",
        );
        let node_unit = shape
            .units
            .iter()
            .find(|unit| unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("Node action unit missing"));
        for file in ["dist/main.js", "dist/pre.js", "dist/post.js"] {
            assert!(
                node_unit
                    .pr_commands
                    .iter()
                    .any(|command| command == &format!("test -f '{file}'")),
                "Node hook was not watched: {file}"
            );
        }
        let _ = fs::remove_dir_all(node);

        let images = fixture("image-matrix");
        must(
            fs::create_dir_all(images.join("local")),
            "create local Docker action",
        );
        must(
            fs::create_dir_all(images.join("remote")),
            "create remote Docker action",
        );
        must(
            fs::create_dir_all(images.join("uppercase")),
            "create uppercase Docker scheme action",
        );
        must(
            fs::write(
                images.join("local/action.yml"),
                "runs:\n  using: docker\n  image: ./Dockerfile\n",
            ),
            "write local Docker metadata",
        );
        must(
            fs::write(images.join("local/Dockerfile"), "FROM scratch\n"),
            "write local Dockerfile",
        );
        must(
            fs::write(
                images.join("remote/action.yml"),
                "runs:\n  using: docker\n  image: docker://ubuntu:24.04\n  entrypoint: /inside-image.sh\n",
            ),
            "write remote Docker metadata",
        );
        must(
            fs::write(
                images.join("uppercase/action.yml"),
                "runs:\n  using: docker\n  image: DOCKER://ubuntu:24.04\n  entrypoint: /inside-image.sh\n",
            ),
            "write uppercase Docker scheme metadata",
        );
        let shape = must(
            super::super::scan_shape_for_tests(&images, &providers(), "main", &[]),
            "scan Docker image matrix",
        );
        let local = shape
            .units
            .iter()
            .find(|unit| unit.root == "local" && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("local Docker action missing"));
        assert!(
            local
                .pr_commands
                .iter()
                .any(|command| command == "test -f 'local/Dockerfile'"),
            "local Docker commands: {:?}",
            local.pr_commands
        );
        let remote = shape
            .units
            .iter()
            .find(|unit| unit.root == "remote" && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("remote Docker action missing"));
        assert_eq!(remote.pr_commands.len(), 1);
        let uppercase = shape
            .units
            .iter()
            .find(|unit| unit.root == "uppercase")
            .unwrap_or_else(|| panic!("uppercase Docker scheme action missing"));
        assert_eq!(uppercase.pr_commands.len(), 1);
        let invalid = images.join("invalid");
        must(
            fs::create_dir_all(&invalid),
            "create malformed Docker action",
        );
        must(
            fs::write(
                invalid.join("action.yml"),
                "runs:\n  using: docker\n  image: docker://--privileged\n",
            ),
            "write malformed Docker image metadata",
        );
        let error = super::super::scan_shape_for_tests(&images, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("flag-shaped Docker image must fail scan"));
        assert!(
            error.to_string().contains("must be a Dockerfile path"),
            "{error}"
        );
        let _ = fs::remove_dir_all(images);

        let missing_shell = fixture("missing-shell");
        must(
            fs::write(
                missing_shell.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - run: echo missing-shell\n",
            ),
            "write missing-shell metadata",
        );
        let error = super::super::scan_shape_for_tests(&missing_shell, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("composite run without shell must fail"));
        assert!(
            error.to_string().contains("must declare `shell`"),
            "{error}"
        );
        let _ = fs::remove_dir_all(missing_shell);

        let dynamic = fixture("dynamic-action-path");
        must(
            fs::write(
                dynamic.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: ${{ inputs.script }}/entrypoint.sh\n",
            ),
            "write dynamic action path metadata",
        );
        let error = super::super::scan_shape_for_tests(&dynamic, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("dynamic action path must fail closed"));
        assert!(
            error.to_string().contains("static relative path"),
            "{error}"
        );
        let _ = fs::remove_dir_all(dynamic);
    }

    #[test]
    fn composite_semantics_are_parsed_without_rewriting_uses_conditions_or_io() {
        let root = fixture("composite-semantics");
        must(
            fs::create_dir_all(root.join("nested")),
            "create nested action",
        );
        must(
            fs::write(
                root.join("action.yml"),
            "name: consumer\ninputs:\n  enabled:\n    default: 'true'\noutputs:\n  result:\n    value: ${{ steps.local.outputs.result }}\nruns:\n  using: composite\n  steps:\n    - id: local\n      if: ${{ inputs.enabled }}\n      uses: ./nested\n      with:\n        value: ${{ inputs.enabled }}\n      env:\n        ACTION_MODE: checked\n    - name: external\n      if: always()\n      uses: actions/checkout@692973e3d937129bcbf40652eb9f2f61becf3332\n",
            ),
            "write semantic action metadata",
        );
        must(
            fs::write(
                root.join("nested/action.yml"),
                "name: nested\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo nested\n",
            ),
            "write nested action metadata",
        );
        let shape = must(
            super::super::scan_shape_for_tests(&root, &providers(), "main", &[]),
            "scan semantic action fixture",
        );
        assert_eq!(shape.units.len(), 2);
        let root_action = shape
            .units
            .iter()
            .find(|unit| unit.root == ".")
            .unwrap_or_else(|| panic!("root semantic action unit missing"));
        assert!(root_action
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'nested/action.yml'"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_verifier_rechecks_metadata_and_local_files() {
        let root = fixture("runtime-verifier");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: bash scripts/check.sh\n",
            ),
            "write verifier metadata",
        );
        must(
            fs::write(root.join("scripts/check.sh"), "exit 0\n"),
            "write verifier script",
        );
        must(
            super::verify_action(&root, "action.yml"),
            "verify valid action metadata",
        );
        must(
            fs::remove_file(root.join("scripts/check.sh")),
            "remove verifier script",
        );
        let error = super::verify_action(&root, "action.yml")
            .err()
            .unwrap_or_else(|| panic!("runtime verifier must catch missing script"));
        assert!(error.to_string().contains("missing file"), "{error}");
        let _ = fs::remove_dir_all(root);
    }
}

//! Source-bound GitHub Actions workflow expectation derivation.
//!
//! This parser consumes immutable workflow/action bytes captured by the G0
//! collector. It never derives expected work from run/job result rows. A
//! missing referenced source, dynamic runner target, empty job map, or
//! malformed YAML is an error; callers must fail closed.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    rc::Rc,
};

use crate::g0_contract::{G0WorkflowDependency, G0WorkflowSource};

const MAX_RECURSION: usize = 32;
const MAX_MATRIX_ASSIGNMENTS: usize = 256;
const MAX_MATRIX_ASSIGNMENT_CELLS: usize = 32_768;
const MAX_MATRIX_ASSIGNMENT_BYTES: usize = 8 * 1024 * 1024;
const RUNNER_MAX_WORKFLOW_BYTES: usize = 1024 * 1024;
const RUNNER_MAX_WORKFLOW_DEPTH: usize = 50;
const RUNNER_MAX_WORKFLOW_NODES: usize = 50_000;
const RUNNER_MAX_WORKFLOW_EVENTS: usize = 1_000_000;
const MAX_REQUEST_WORKFLOW_SOURCES: usize = 2_048;
const MAX_REQUEST_WORKFLOW_SOURCE_REFERENCES: usize = 16_384;
const MAX_REQUEST_WORKFLOW_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_REQUEST_WORKFLOW_SOURCE_NODES: usize = 1_000_000;
const MAX_REQUEST_WORKFLOW_DERIVATION_NODES: usize = 4_000_000;
const RUNNER_MAX_EXPRESSION_LENGTH: usize = 21_000;
const RUNNER_MAX_EXPRESSION_DEPTH: usize = 50;
const MAX_EXPRESSION_PARSER_NESTING: usize = 128;
// This is the G0 event subset, not the complete (permissive) legacy Runner
// `on-mapping` schema.
const SUPPORTED_EVENTS: &[&str] = &[
    "merge_group",
    "pull_request",
    "pull_request_target",
    "push",
    "workflow_call",
    "workflow_dispatch",
    "workflow_run",
];

// Checker-supported mappings derived from actions/runner v2.337.0's
// `src/Sdk/WorkflowParser/workflow-v1.0.json`. These are deliberately narrower
// than Runner's full schema: fields outside this projection fail closed rather
// than being accepted without validation. The derived plan records events, job
// IDs, runner targets, matrix assignments, and immutable `uses` edges; it does
// not certify workflow permissions or command behavior.
const WORKFLOW_ROOT_FIELDS: &[&str] = &["on", "name", "description", "run-name", "env", "jobs"];
const REGULAR_JOB_FIELDS: &[&str] = &[
    "needs",
    "if",
    "strategy",
    "name",
    "runs-on",
    "timeout-minutes",
    "cancel-timeout-minutes",
    "continue-on-error",
    "container",
    "services",
    "env",
    "steps",
];
const REUSABLE_JOB_FIELDS: &[&str] =
    &["name", "uses", "with", "secrets", "needs", "if", "strategy"];
const STRATEGY_FIELDS: &[&str] = &["matrix"];
const RUN_STEP_FIELDS: &[&str] = &[
    "name",
    "id",
    "if",
    "timeout-minutes",
    "run",
    "continue-on-error",
    "env",
    "working-directory",
    "shell",
];
const ACTION_STEP_FIELDS: &[&str] = &[
    "name",
    "id",
    "if",
    "continue-on-error",
    "timeout-minutes",
    "uses",
    "with",
    "env",
];
const WORKFLOW_ENV_CONTEXTS: &[&str] = &["github", "inputs", "vars", "secrets"];
const WORKFLOW_CALL_INPUT_DEFAULT_CONTEXTS: &[&str] = &["github", "inputs", "vars"];
const WORKFLOW_CALL_OUTPUT_CONTEXTS: &[&str] = &["github", "inputs", "vars", "jobs"];
const WORKFLOW_CALL_SECRET_CONTEXTS: &[&str] = &[
    "github", "inputs", "vars", "needs", "secrets", "strategy", "matrix",
];
const JOB_ENV_CONTEXTS: &[&str] = &[
    "github", "inputs", "vars", "needs", "strategy", "matrix", "secrets",
];
const STEP_ENV_CONTEXTS: &[&str] = &[
    "github",
    "inputs",
    "vars",
    "needs",
    "strategy",
    "matrix",
    "secrets",
    "steps",
    "job",
    "runner",
    "env",
    "hashFiles(1,255)",
];
const GLOBAL_EXPRESSION_FUNCTIONS: &[(&str, usize, usize)] = &[
    ("case", 3, 255),
    ("contains", 2, 2),
    ("endsWith", 2, 2),
    ("format", 1, 255),
    ("join", 1, 2),
    ("startsWith", 2, 2),
    ("toJson", 1, 1),
    ("fromJson", 1, 1),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedWorkflowPlan {
    pub jobs: Vec<DerivedWorkflowJob>,
    pub child_edges: Vec<DerivedChildEdge>,
    pub events: BTreeSet<String>,
    pub has_action_steps: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedWorkflowJob {
    pub job_id: String,
    pub provider: String,
    pub platform: String,
    pub architecture: String,
    pub uses_reusable_workflow: bool,
    /// One concrete finite matrix assignment.  The logical `job_id` remains
    /// the source job key; the assignment prevents two matrix instances from
    /// being silently collapsed while validating their concrete target.
    pub matrix: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedChildEdge {
    pub workload_id: String,
    pub root_workload_id: String,
    pub repository: String,
    pub workflow_path: String,
    pub event: String,
    pub relation: String,
    pub source_sha: String,
    pub parent_repository: String,
    pub parent_workflow_path: String,
    pub parent_source_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    repository: String,
    path: String,
    root_action: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct WorkflowSourceIdentity {
    repository: String,
    path: String,
    revision: String,
    source_sha: String,
    sha256: String,
}

#[derive(Clone, Copy)]
struct WorkflowDerivationLimits {
    unique_sources: usize,
    source_references: usize,
    unique_source_bytes: usize,
    unique_source_nodes: usize,
    derivation_nodes: usize,
}

impl Default for WorkflowDerivationLimits {
    fn default() -> Self {
        Self {
            unique_sources: MAX_REQUEST_WORKFLOW_SOURCES,
            source_references: MAX_REQUEST_WORKFLOW_SOURCE_REFERENCES,
            unique_source_bytes: MAX_REQUEST_WORKFLOW_SOURCE_BYTES,
            unique_source_nodes: MAX_REQUEST_WORKFLOW_SOURCE_NODES,
            derivation_nodes: MAX_REQUEST_WORKFLOW_DERIVATION_NODES,
        }
    }
}

struct CachedWorkflowSourceBytes {
    bytes: Vec<u8>,
    encoded_fingerprint: [u8; 32],
}

struct ParsedWorkflowSource {
    document: Value,
    events: BTreeSet<String>,
    workflow_call: Option<WorkflowCallContract>,
    node_count: usize,
}

/// Shared across one checker request so repeated references reuse parsed YAML
/// and all distinct sources and derivation work share aggregate limits.
pub(crate) struct WorkflowDerivationContext {
    limits: WorkflowDerivationLimits,
    seen_sources: BTreeSet<WorkflowSourceIdentity>,
    source_bytes: BTreeMap<WorkflowSourceIdentity, CachedWorkflowSourceBytes>,
    parsed_sources: BTreeMap<WorkflowSourceIdentity, Rc<ParsedWorkflowSource>>,
    source_errors: BTreeMap<WorkflowSourceIdentity, String>,
    source_references: usize,
    unique_source_bytes: usize,
    unique_source_nodes: usize,
    derivation_nodes: usize,
    exhausted: Option<String>,
    #[cfg(test)]
    parse_count: usize,
    #[cfg(test)]
    derivation_count: usize,
}

impl Default for WorkflowDerivationContext {
    fn default() -> Self {
        Self::with_limits(WorkflowDerivationLimits::default())
    }
}

impl WorkflowDerivationContext {
    fn with_limits(limits: WorkflowDerivationLimits) -> Self {
        Self {
            limits,
            seen_sources: BTreeSet::new(),
            source_bytes: BTreeMap::new(),
            parsed_sources: BTreeMap::new(),
            source_errors: BTreeMap::new(),
            source_references: 0,
            unique_source_bytes: 0,
            unique_source_nodes: 0,
            derivation_nodes: 0,
            exhausted: None,
            #[cfg(test)]
            parse_count: 0,
            #[cfg(test)]
            derivation_count: 0,
        }
    }

    fn register_source(
        &mut self,
        source: &G0WorkflowSource,
        normalized_path: &str,
    ) -> Result<WorkflowSourceIdentity> {
        if let Some(reason) = &self.exhausted {
            bail!("request-wide workflow derivation budget exhausted: {reason}");
        }
        let next_references = self
            .source_references
            .checked_add(1)
            .ok_or_else(|| anyhow!("request-wide workflow source reference count overflow"))?;
        if next_references > self.limits.source_references {
            let reason = format!("source references exceed {}", self.limits.source_references);
            self.exhausted = Some(reason.clone());
            bail!("request-wide workflow derivation budget exhausted: {reason}");
        }
        self.source_references = next_references;
        let identity = WorkflowSourceIdentity {
            repository: source.repository.clone(),
            path: normalized_path.to_owned(),
            revision: source.revision.clone(),
            source_sha: source.source_sha.clone(),
            sha256: source.sha256.clone(),
        };
        if let Some(reason) = self.source_errors.get(&identity) {
            bail!("captured workflow source previously failed validation: {reason}");
        }
        if !workflow_source_encoded_length_matches(source) {
            let reason = format!(
                "captured workflow source {}/{} exceeds the per-source byte limit or has inconsistent base64 length",
                source.repository, normalized_path
            );
            self.source_errors.insert(identity.clone(), reason.clone());
            bail!("{reason}");
        }
        let encoded_fingerprint: [u8; 32] = Sha256::digest(source.bytes_base64.as_bytes()).into();
        if let Some(cached) = self.source_bytes.get(&identity) {
            if cached.encoded_fingerprint != encoded_fingerprint {
                bail!(
                    "captured workflow source identity {}/{} has conflicting encoded bytes",
                    source.repository,
                    normalized_path
                );
            }
            return Ok(identity);
        }
        if let Some(reason) = &self.exhausted {
            bail!("request-wide workflow derivation budget exhausted: {reason}");
        }
        if !self.seen_sources.contains(&identity) {
            if self.seen_sources.len() >= self.limits.unique_sources {
                let reason = format!("unique source count exceeds {}", self.limits.unique_sources);
                self.exhausted = Some(reason.clone());
                bail!("request-wide workflow derivation budget exhausted: {reason}");
            }
            self.seen_sources.insert(identity.clone());
        }
        let bytes = match BASE64.decode(&source.bytes_base64) {
            Ok(bytes) if bytes.len() as u64 == source.byte_length => bytes,
            Ok(_) => {
                let reason = format!(
                    "captured workflow source {}/{} decodes to an unexpected byte length",
                    source.repository, normalized_path
                );
                self.source_errors.insert(identity.clone(), reason.clone());
                bail!("{reason}");
            }
            Err(error) => {
                let reason = format!(
                    "decode immutable workflow source {}/{}: {error}",
                    source.repository, normalized_path
                );
                self.source_errors.insert(identity.clone(), reason.clone());
                bail!("{reason}");
            }
        };
        let next_total = self
            .unique_source_bytes
            .checked_add(bytes.len())
            .ok_or_else(|| anyhow!("request-wide workflow source byte count overflow"))?;
        if next_total > self.limits.unique_source_bytes {
            let reason = format!(
                "unique source bytes exceed {}",
                self.limits.unique_source_bytes
            );
            self.exhausted = Some(reason.clone());
            bail!("request-wide workflow derivation budget exhausted: {reason}");
        }
        self.unique_source_bytes = next_total;
        self.source_bytes.insert(
            identity.clone(),
            CachedWorkflowSourceBytes {
                bytes,
                encoded_fingerprint,
            },
        );
        Ok(identity)
    }

    fn parse_source(
        &mut self,
        source: &G0WorkflowSource,
        normalized_path: &str,
    ) -> Result<Rc<ParsedWorkflowSource>> {
        let identity = self.register_source(source, normalized_path)?;
        if let Some(parsed) = self.parsed_sources.get(&identity) {
            return Ok(Rc::clone(parsed));
        }
        let bytes = self
            .source_bytes
            .get(&identity)
            .ok_or_else(|| anyhow!("registered workflow source bytes are missing"))?
            .bytes
            .clone();
        let parsed = match parse_workflow_source(source, &bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.source_errors.insert(identity, format!("{error:#}"));
                return Err(error);
            }
        };
        let next_total = self
            .unique_source_nodes
            .checked_add(parsed.node_count)
            .ok_or_else(|| anyhow!("request-wide workflow node count overflow"))?;
        if next_total > self.limits.unique_source_nodes {
            let reason = format!(
                "unique parsed source nodes exceed {}",
                self.limits.unique_source_nodes
            );
            self.exhausted = Some(reason.clone());
            bail!("request-wide workflow derivation budget exhausted: {reason}");
        }
        self.unique_source_nodes = next_total;
        let parsed = Rc::new(parsed);
        self.parsed_sources.insert(identity, Rc::clone(&parsed));
        #[cfg(test)]
        {
            self.parse_count += 1;
        }
        Ok(parsed)
    }

    fn charge_derivation(&mut self, node_count: usize) -> Result<()> {
        if let Some(reason) = &self.exhausted {
            bail!("request-wide workflow derivation budget exhausted: {reason}");
        }
        let next_total = self
            .derivation_nodes
            .checked_add(node_count)
            .ok_or_else(|| anyhow!("request-wide workflow derivation count overflow"))?;
        if next_total > self.limits.derivation_nodes {
            let reason = format!(
                "derived source nodes exceed {}",
                self.limits.derivation_nodes
            );
            self.exhausted = Some(reason.clone());
            bail!("request-wide workflow derivation budget exhausted: {reason}");
        }
        self.derivation_nodes = next_total;
        #[cfg(test)]
        {
            self.derivation_count += 1;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct CapturedWorkflowSource<'a> {
    source: &'a G0WorkflowSource,
    kind: &'a str,
}

#[derive(Clone, Default)]
struct WorkflowCallContract {
    inputs: BTreeMap<String, WorkflowCallInput>,
    secrets: BTreeMap<String, WorkflowCallSecret>,
}

#[derive(Clone)]
struct WorkflowCallInput {
    name: String,
    type_name: String,
    required: bool,
}

#[derive(Clone)]
struct WorkflowCallSecret {
    name: String,
    required: bool,
}

struct DerivedWorkflowSource {
    plan: DerivedWorkflowPlan,
    workflow_call: Option<WorkflowCallContract>,
}

/// Retain scalar mapping-key types until keys can be stringified with Runner's
/// `TemplateToken.ToString` semantics. Noyalib's `Value::Mapping` has already
/// converted every key to text, and its float formatting differs from Runner's
/// `G15` conversion for values such as `1.0`.
enum RawWorkflowYamlValue {
    Null,
    Bool(bool),
    Number(serde_yaml::Number),
    String(String),
    Sequence(Vec<Self>),
    Mapping(Vec<(Self, Self)>),
}

impl RawWorkflowYamlValue {
    fn into_value(self) -> Result<Value> {
        match self {
            Self::Null => Ok(Value::Null),
            Self::Bool(value) => Ok(Value::Bool(value)),
            Self::Number(value) => Ok(Value::Number(value)),
            Self::String(value) => Ok(Value::String(value)),
            Self::Sequence(values) => Ok(Value::Sequence(
                values
                    .into_iter()
                    .map(Self::into_value)
                    .collect::<Result<Vec<_>>>()?,
            )),
            Self::Mapping(entries) => {
                let mut mapping = Mapping::new();
                for (key, value) in entries {
                    let key = key.into_mapping_key()?;
                    if mapping.contains_key(&key) {
                        bail!(
                            "YAML mapping keys collide after Runner scalar-to-string conversion: {key:?}"
                        );
                    }
                    mapping.insert(key, value.into_value()?);
                }
                Ok(Value::Mapping(mapping))
            }
        }
    }

    fn into_mapping_key(self) -> Result<String> {
        match self {
            Self::Null => Ok(String::new()),
            Self::Bool(value) => Ok(if value { "true" } else { "false" }.to_owned()),
            Self::Number(value) => Ok(runner_number_to_string(value.as_f64())),
            Self::String(value) => Ok(value),
            Self::Sequence(_) | Self::Mapping(_) => {
                bail!("workflow YAML mapping keys must be scalar")
            }
        }
    }
}

impl<'de> Deserialize<'de> for RawWorkflowYamlValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(RawWorkflowYamlValueVisitor)
    }
}

struct RawWorkflowYamlValueVisitor;

impl<'de> Visitor<'de> for RawWorkflowYamlValueVisitor {
    type Value = RawWorkflowYamlValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a workflow YAML scalar, sequence, or mapping")
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(RawWorkflowYamlValue::Null)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(RawWorkflowYamlValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(RawWorkflowYamlValue::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(RawWorkflowYamlValue::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(RawWorkflowYamlValue::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E> {
        Ok(RawWorkflowYamlValue::Number(value.into()))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(RawWorkflowYamlValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(RawWorkflowYamlValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or_default());
        while let Some(value) = sequence.next_element()? {
            values.push(value);
        }
        Ok(RawWorkflowYamlValue::Sequence(values))
    }

    fn visit_map<A>(self, mut entries: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Vec::with_capacity(entries.size_hint().unwrap_or_default());
        while let Some(entry) = entries.next_entry()? {
            values.push(entry);
        }
        Ok(RawWorkflowYamlValue::Mapping(values))
    }
}

fn workflow_parser_config() -> serde_yaml::ParserConfig {
    serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error)
        .max_documents(1)
        .max_document_length(RUNNER_MAX_WORKFLOW_BYTES)
        .max_depth(RUNNER_MAX_WORKFLOW_DEPTH)
        .max_nodes(RUNNER_MAX_WORKFLOW_NODES)
        .max_events(RUNNER_MAX_WORKFLOW_EVENTS)
        .max_mapping_keys(RUNNER_MAX_WORKFLOW_NODES)
        .max_sequence_length(RUNNER_MAX_WORKFLOW_NODES)
}

fn workflow_source_encoded_length_matches(source: &G0WorkflowSource) -> bool {
    let encoded = source.bytes_base64.as_bytes();
    if source.byte_length > RUNNER_MAX_WORKFLOW_BYTES as u64
        || encoded.len() > RUNNER_MAX_WORKFLOW_BYTES.div_ceil(3) * 4
        || !encoded.len().is_multiple_of(4)
    {
        return false;
    }
    let padding = if encoded.ends_with(b"==") {
        2
    } else if encoded.ends_with(b"=") {
        1
    } else {
        0
    };
    let Some(content_length) = encoded.len().checked_sub(padding) else {
        return false;
    };
    if encoded[..content_length].contains(&b'=') {
        return false;
    }
    encoded
        .len()
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|length| length.checked_sub(padding))
        .is_some_and(|length| {
            length <= RUNNER_MAX_WORKFLOW_BYTES && length as u64 == source.byte_length
        })
}

fn count_workflow_value_nodes(value: &Value) -> usize {
    let mut pending = vec![value];
    let mut count = 0usize;
    while let Some(value) = pending.pop() {
        count = count.saturating_add(1);
        match value {
            Value::Sequence(values) => pending.extend(values),
            Value::Mapping(values) => {
                for (_, value) in values {
                    count = count.saturating_add(1);
                    pending.push(value);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            _ => {}
        }
    }
    count
}

fn parse_workflow_source(source: &G0WorkflowSource, bytes: &[u8]) -> Result<ParsedWorkflowSource> {
    let text = std::str::from_utf8(bytes).context("workflow source is not UTF-8")?;
    let parser_config = workflow_parser_config()
        .merge_key_policy(serde_yaml::MergeKeyPolicy::Error)
        .with_policy(serde_yaml::policy::DenyAnchors);
    let parsed_document: Value = serde_yaml::from_str_with_config(text, &parser_config)
        .with_context(|| {
            format!(
                "parse workflow source {}/{}",
                source.repository, source.path
            )
        })?;
    reject_case_insensitive_mapping_duplicates(&parsed_document)?;
    let raw_document: RawWorkflowYamlValue =
        serde_yaml::from_str_with_config(text, &workflow_parser_config()).with_context(|| {
            format!(
                "read workflow scalar keys {}/{} with Runner coercion",
                source.repository, source.path
            )
        })?;
    let document = raw_document.into_value()?;
    reject_case_insensitive_mapping_duplicates(&document)?;
    let root_mapping = document
        .as_mapping()
        .ok_or_else(|| anyhow!("workflow source document must be a YAML mapping"))?;
    reject_unknown_fields(root_mapping, WORKFLOW_ROOT_FIELDS, "workflow")?;
    validate_workflow_metadata(root_mapping)?;
    let events = workflow_events(root_mapping)?;
    let workflow_call = workflow_call_contract(root_mapping, &events)?;
    let jobs = mapping_value(root_mapping, "jobs")
        .and_then(Value::as_mapping)
        .ok_or_else(|| anyhow!("workflow source has no jobs mapping"))?;
    if jobs.is_empty() {
        bail!("workflow source has an empty jobs mapping");
    }
    let node_count = count_workflow_value_nodes(&parsed_document)
        .checked_add(count_workflow_value_nodes(&document))
        .ok_or_else(|| anyhow!("workflow node count overflow"))?;
    Ok(ParsedWorkflowSource {
        document,
        events,
        workflow_call,
        node_count,
    })
}

/// Match actions/runner v2.337.0's `NumberToken.ToString` format (`G15` with
/// invariant culture) when YAML scalars become mapping keys or strings.
fn runner_number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".to_owned()
        } else {
            "Infinity".to_owned()
        };
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let magnitude = value.abs();
    if magnitude == 0.0 {
        return format!("{sign}0");
    }

    // `G15` keeps 15 significant digits. Rust's scientific formatter rounds
    // to that precision and gives a normalized exponent for the fixed/scientific
    // notation decision below.
    let rounded = format!("{magnitude:.14e}");
    let Some((mantissa, exponent)) = rounded.split_once('e') else {
        return format!("{sign}{rounded}");
    };
    let exponent = exponent.parse::<i32>().unwrap_or_default();
    let mut digits = mantissa
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }

    if (-4..15).contains(&exponent) {
        let decimal_position = exponent + 1;
        if decimal_position <= 0 {
            let leading_zeros = usize::try_from(-decimal_position).unwrap_or_default();
            return format!("{sign}0.{}{digits}", "0".repeat(leading_zeros));
        }
        let decimal_position = usize::try_from(decimal_position).unwrap_or_default();
        if decimal_position >= digits.len() {
            return format!(
                "{sign}{digits}{}",
                "0".repeat(decimal_position - digits.len())
            );
        }
        return format!(
            "{sign}{}.{}",
            &digits[..decimal_position],
            &digits[decimal_position..]
        );
    }

    let significand = if digits.len() == 1 {
        digits
    } else {
        format!("{}.{}", &digits[..1], &digits[1..])
    };
    let exponent_sign = if exponent >= 0 { "+" } else { "-" };
    format!("{sign}{significand}E{exponent_sign}{:02}", exponent.abs())
}

/// Derive root workflow jobs and recursive child obligations from immutable
/// source blobs. Dependencies must contain every `uses:` source encountered.
pub(crate) fn derive_workflow_plan(
    root: &G0WorkflowSource,
    dependencies: &[G0WorkflowDependency],
) -> Result<DerivedWorkflowPlan> {
    let mut context = WorkflowDerivationContext::default();
    derive_workflow_plan_with_context(root, dependencies, &mut context)
}

pub(crate) fn derive_workflow_plan_with_context(
    root: &G0WorkflowSource,
    dependencies: &[G0WorkflowDependency],
    context: &mut WorkflowDerivationContext,
) -> Result<DerivedWorkflowPlan> {
    let mut sources = BTreeMap::<SourceKey, CapturedWorkflowSource<'_>>::new();
    for dependency in dependencies {
        let key = SourceKey {
            repository: dependency.source.repository.clone(),
            path: normalize_path(&dependency.source.path)?,
            root_action: false,
        };
        context.register_source(&dependency.source, &key.path)?;
        if sources
            .insert(
                key.clone(),
                CapturedWorkflowSource {
                    source: &dependency.source,
                    kind: &dependency.kind,
                },
            )
            .is_some()
        {
            bail!(
                "duplicate captured workflow dependency source {}/{}",
                key.repository,
                key.path
            );
        }
    }
    let root_key = SourceKey {
        repository: root.repository.clone(),
        path: normalize_path(&root.path)?,
        root_action: false,
    };
    context.register_source(root, &root_key.path)?;
    let mut stack = BTreeSet::new();
    let derived = derive_source(root, &sources, &root_key, &mut stack, 0, None, context)?;
    Ok(derived.plan)
}

/// Conservatively identify action-step metadata in a captured workflow source.
/// Malformed source is treated as potentially containing actions so callers
/// keep the corresponding authority blocker active.
fn derive_source(
    source: &G0WorkflowSource,
    sources: &BTreeMap<SourceKey, CapturedWorkflowSource<'_>>,
    source_key: &SourceKey,
    stack: &mut BTreeSet<SourceKey>,
    depth: usize,
    root_workload_id: Option<String>,
    context: &mut WorkflowDerivationContext,
) -> Result<DerivedWorkflowSource> {
    if depth > MAX_RECURSION {
        bail!(
            "workflow source recursion exceeded {} levels at {}/{}",
            MAX_RECURSION,
            source_key.repository,
            source_key.path
        );
    }
    if !stack.insert(source_key.clone()) {
        bail!(
            "workflow source dependency cycle at {}/{}",
            source_key.repository,
            source_key.path
        );
    }
    let parsed = context.parse_source(source, &source_key.path)?;
    context.charge_derivation(parsed.node_count)?;
    let root_mapping = parsed
        .document
        .as_mapping()
        .ok_or_else(|| anyhow!("cached workflow source document must be a YAML mapping"))?;
    let events = parsed.events.clone();
    let jobs = mapping_value(root_mapping, "jobs")
        .and_then(Value::as_mapping)
        .ok_or_else(|| anyhow!("cached workflow source has no jobs mapping"))?;

    let mut plan = DerivedWorkflowPlan {
        jobs: Vec::new(),
        child_edges: Vec::new(),
        events,
        has_action_steps: false,
    };
    for (job_key, job_value) in jobs {
        let job_id = job_key.as_str();
        if !valid_job_id(job_id) {
            bail!("workflow job ID {job_id:?} is outside the supported Runner job-id syntax");
        }
        let job_id = job_id.to_owned();
        let root_for_job = root_workload_id.clone().unwrap_or_else(|| job_id.clone());
        let job = job_value
            .as_mapping()
            .ok_or_else(|| anyhow!("workflow job {job_id} must be a mapping"))?;
        validate_job_shape(job, &format!("workflow job {job_id}"))?;
        let reusable = job_uses(job, &format!("workflow job {job_id}"))?;
        let matrix = job_matrix(job)
            .with_context(|| format!("derive finite matrix for workflow job {job_id}"))?;
        let reusable_target = if let Some(uses) = reusable {
            let (target, pinned_ref) = resolve_reusable_target(uses, source)?;
            let dependency = sources.get(&target).ok_or_else(|| {
                anyhow!(
                    "workflow job {job_id} references uncaptured immutable source {}/{}",
                    target.repository,
                    target.path
                )
            })?;
            if dependency.kind != "reusable_workflow" {
                bail!(
                    "workflow job {job_id} uses {}/{} as a reusable workflow, but its captured dependency kind is {}",
                    target.repository,
                    target.path,
                    dependency.kind
                );
            }
            if dependency.source.source_sha != pinned_ref {
                bail!(
                    "workflow job {job_id} pins {pinned_ref}, but captured source {}/{} is bound to commit {} (revision {})",
                    target.repository,
                    target.path,
                    dependency.source.source_sha,
                    dependency.source.revision
                );
            }
            let child_plan = derive_source(
                dependency.source,
                sources,
                &target,
                stack,
                depth + 1,
                Some(root_for_job.clone()),
                context,
            )?;
            if !child_plan.plan.events.contains("workflow_call") {
                bail!(
                    "reusable workflow {}/{} must declare workflow_call",
                    target.repository,
                    target.path
                );
            }
            let contract = child_plan
                .workflow_call
                .as_ref()
                .ok_or_else(|| anyhow!("reusable workflow call contract is missing"))?;
            validate_reusable_workflow_arguments(job, contract, &format!("workflow job {job_id}"))?;
            Some((target, child_plan, dependency.source.source_sha.clone()))
        } else {
            None
        };
        if reusable.is_none() {
            validate_action_sources(job, sources)?;
        }
        reject_continue_on_error(job, &format!("workflow job {job_id}"))?;
        let job_enabled = mapping_value(job, "if")
            .map(constant_condition)
            .transpose()?
            .unwrap_or(true);
        if !job_enabled {
            // Source references and reusable-workflow interfaces are still
            // validated above. Only runtime obligations disappear on `if: false`.
            continue;
        }
        if reusable.is_some() && matrix.len() != 1 {
            bail!(
                "reusable workflow caller {job_id} expands to multiple matrix assignments, but child edges do not retain caller assignment identity"
            );
        }
        let reusable_target = if let Some((target, child_plan, child_source_sha)) = reusable_target
        {
            let mut child_job_ids = BTreeSet::new();
            if child_plan
                .plan
                .jobs
                .iter()
                .any(|child| !child_job_ids.insert(child.job_id.clone()))
            {
                bail!(
                    "reusable workflow {}/{} expands a logical child job into multiple concrete matrix instances; the child graph has no concrete matrix identity",
                    target.repository,
                    target.path
                );
            }
            if child_plan.plan.jobs.len() != 1 {
                bail!(
                    "reusable workflow {}/{} expands to {} distinct child jobs; the current child graph cannot preserve their individual workload identities",
                    target.repository,
                    target.path,
                    child_plan.plan.jobs.len()
                );
            }
            let child_job = child_plan.plan.jobs.first().cloned().ok_or_else(|| {
                anyhow!(
                    "reusable workflow {}/{} has no derived jobs",
                    target.repository,
                    target.path
                )
            })?;
            if child_plan.plan.jobs.iter().any(|job| {
                job.provider != child_job.provider
                    || job.platform != child_job.platform
                    || job.architecture != child_job.architecture
            }) {
                bail!(
                    "reusable workflow {}/{} has matrix instances with different targets",
                    target.repository,
                    target.path
                );
            }
            plan.has_action_steps |= child_plan.plan.has_action_steps;
            plan.child_edges.extend(child_plan.plan.child_edges);
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job_id.clone(),
                root_workload_id: root_for_job.clone(),
                repository: target.repository,
                workflow_path: target.path,
                event: "workflow_call".to_owned(),
                relation: "reusable_workflow".to_owned(),
                source_sha: child_source_sha,
                parent_repository: source.repository.clone(),
                parent_workflow_path: source.path.clone(),
                parent_source_sha: source.source_sha.clone(),
            });
            Some((
                child_job.provider,
                child_job.platform,
                child_job.architecture,
            ))
        } else {
            None
        };
        let action_step_obligation = if reusable.is_none() {
            action_sources(job)?
        } else {
            false
        };
        for assignment in matrix {
            let (provider, platform, architecture) = if let Some(target) = reusable_target.clone() {
                target
            } else {
                let runs_on = mapping_value(job, "runs-on")
                    .ok_or_else(|| anyhow!("workflow job {job_id} lacks source runs-on target"))?;
                let runs_on = resolve_matrix_value(runs_on, &assignment)?;
                runner_target(&runs_on)
                    .with_context(|| format!("derive runner target for workflow job {job_id}"))?
            };
            plan.has_action_steps |= action_step_obligation;
            plan.jobs.push(DerivedWorkflowJob {
                job_id: job_id.clone(),
                provider,
                platform,
                architecture,
                uses_reusable_workflow: reusable.is_some(),
                matrix: assignment,
            });
        }
    }

    // These triggers are source-derived obligations. They are retained as
    // explicit child relations even when GitHub's run API does not expose a
    // parent link; the run collector must later prove that association.
    if plan.events.contains("workflow_run") {
        for job in &plan.jobs {
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job.job_id.clone(),
                root_workload_id: root_workload_id
                    .clone()
                    .unwrap_or_else(|| job.job_id.clone()),
                repository: source.repository.clone(),
                workflow_path: source.path.clone(),
                event: "workflow_run".to_owned(),
                relation: "workflow_run".to_owned(),
                source_sha: source.source_sha.clone(),
                parent_repository: source.repository.clone(),
                parent_workflow_path: source.path.clone(),
                parent_source_sha: source.source_sha.clone(),
            });
        }
    }
    if plan.events.contains("workflow_dispatch") {
        for job in &plan.jobs {
            plan.child_edges.push(DerivedChildEdge {
                workload_id: job.job_id.clone(),
                root_workload_id: root_workload_id
                    .clone()
                    .unwrap_or_else(|| job.job_id.clone()),
                repository: source.repository.clone(),
                workflow_path: source.path.clone(),
                event: "workflow_dispatch".to_owned(),
                relation: "dispatch".to_owned(),
                source_sha: source.source_sha.clone(),
                parent_repository: source.repository.clone(),
                parent_workflow_path: source.path.clone(),
                parent_source_sha: source.source_sha.clone(),
            });
        }
    }
    stack.remove(source_key);
    Ok(DerivedWorkflowSource {
        plan,
        workflow_call: parsed.workflow_call.clone(),
    })
}

fn validate_action_sources(
    job: &Mapping,
    sources: &BTreeMap<SourceKey, CapturedWorkflowSource<'_>>,
) -> Result<()> {
    let Some(steps_value) = mapping_value(job, "steps") else {
        return Ok(());
    };
    let steps = steps_value
        .as_sequence()
        .ok_or_else(|| anyhow!("workflow job steps must be a sequence"))?;
    for (index, step) in steps.iter().enumerate() {
        let Some(step) = step.as_mapping() else {
            bail!("workflow step {index} must be a mapping");
        };
        let uses = step_uses(step, &format!("workflow step {index}"))?;
        reject_continue_on_error(step, &format!("workflow job step {index}"))?;
        let Some(uses) = uses else {
            continue;
        };
        let (target, pinned_ref) = resolve_action_target(uses)
            .with_context(|| format!("resolve immutable action source in workflow step {index}"))?;
        let dependency = action_dependency(sources, &target).with_context(|| {
            format!(
                "resolve captured action metadata for {}/{}",
                target.repository,
                if target.root_action {
                    "(root action)"
                } else {
                    target.path.as_str()
                }
            )
        })?;
        if dependency.kind != "action" {
            bail!(
                "workflow step {index} uses {}/{} as an action, but its captured dependency kind is {}",
                dependency.source.repository,
                dependency.source.path,
                dependency.kind
            );
        }
        if dependency.source.source_sha != pinned_ref {
            bail!(
                "workflow step {index} pins {pinned_ref}, but captured action source {}/{} is bound to commit {} (revision {})",
                dependency.source.repository,
                dependency.source.path,
                dependency.source.source_sha,
                dependency.source.revision
            );
        }
    }
    Ok(())
}

fn action_sources(job: &Mapping) -> Result<bool> {
    let Some(steps_value) = mapping_value(job, "steps") else {
        return Ok(false);
    };
    let steps = steps_value
        .as_sequence()
        .ok_or_else(|| anyhow!("workflow job steps must be a sequence"))?;
    let mut has_action_steps = false;
    for (index, step) in steps.iter().enumerate() {
        let Some(step) = step.as_mapping() else {
            bail!("workflow step {index} must be a mapping");
        };
        let uses = step_uses(step, &format!("workflow step {index}"))?;
        if let Some(condition) = mapping_value(step, "if")
            && !constant_condition(condition)?
        {
            continue;
        }
        has_action_steps |= uses.is_some();
    }
    Ok(has_action_steps)
}

fn validate_job_shape(job: &Mapping, subject: &str) -> Result<()> {
    let reusable = job_uses(job, subject)?.is_some();
    reject_unknown_fields(
        job,
        if reusable {
            REUSABLE_JOB_FIELDS
        } else {
            REGULAR_JOB_FIELDS
        },
        subject,
    )?;
    validate_optional_string(job, "name", subject, false)?;
    validate_optional_positive_number(job, "timeout-minutes", subject)?;
    validate_optional_positive_number(job, "cancel-timeout-minutes", subject)?;
    if let Some(environment) = mapping_value(job, "env") {
        validate_dynamic_string_mapping(environment, &format!("{subject} env"), JOB_ENV_CONTEXTS)?;
    }
    if let Some(needs) = mapping_value(job, "needs")
        && !matches!(needs, Value::Sequence(dependencies) if dependencies.is_empty())
    {
        bail!("{subject} needs graph is not retained by the current workflow plan");
    }
    if let Some(strategy) = mapping_value(job, "strategy") {
        let strategy = strategy
            .as_mapping()
            .ok_or_else(|| anyhow!("{subject} strategy must be a mapping"))?;
        reject_unknown_fields(strategy, STRATEGY_FIELDS, &format!("{subject} strategy"))?;
    }
    if mapping_value(job, "container").is_some() {
        bail!("{subject} container execution is unsupported until image platform and digest are bound");
    }
    if mapping_value(job, "services").is_some() {
        bail!("{subject} service container execution is unsupported until service image platform and digest are bound");
    }
    if reusable {
        if let Some(with) = mapping_value(job, "with") {
            validate_scalar_mapping(with, &format!("{subject} with"), false)?;
        }
        if let Some(secrets) = mapping_value(job, "secrets") {
            validate_workflow_job_secrets(secrets, subject)?;
        }
        if mapping_value(job, "runs-on").is_some() || mapping_value(job, "steps").is_some() {
            bail!("{subject} cannot combine reusable-workflow uses with runs-on or steps");
        }
        return Ok(());
    }
    let runs_on =
        mapping_value(job, "runs-on").ok_or_else(|| anyhow!("{subject} lacks runs-on"))?;
    validate_runs_on_shape(runs_on, subject)?;
    if let Some(steps_value) = mapping_value(job, "steps") {
        let steps = steps_value
            .as_sequence()
            .ok_or_else(|| anyhow!("{subject} steps must be a sequence"))?;
        let mut known_step_ids = BTreeSet::new();
        for (index, step) in steps.iter().enumerate() {
            let Some(step) = step.as_mapping() else {
                bail!("{subject} step {index} must be a mapping");
            };
            step_uses(step, &format!("{subject} step {index}"))?;
            if let Some(id) = mapping_value(step, "id").and_then(Value::as_str) {
                if !known_step_ids.insert(id.to_ascii_lowercase()) {
                    bail!("{subject} has duplicate step ID {id:?} under Runner case-insensitive comparison");
                }
            }
        }
    }
    Ok(())
}

fn validate_runs_on_shape(value: &Value, subject: &str) -> Result<()> {
    match value {
        Value::String(label) if !label.trim().is_empty() => Ok(()),
        Value::Sequence(labels)
            if !labels.is_empty()
                && labels
                    .iter()
                    .all(|label| label.as_str().is_some_and(|label| !label.trim().is_empty())) =>
        {
            Ok(())
        }
        _ => bail!("{subject} runs-on must be a non-empty string or string sequence"),
    }
}

fn job_uses<'a>(job: &'a Mapping, subject: &str) -> Result<Option<&'a str>> {
    mapping_value(job, "uses")
        .map(|value| {
            value
                .as_str()
                .filter(|uses| !uses.trim().is_empty())
                .ok_or_else(|| anyhow!("{subject} uses must be a non-empty string"))
        })
        .transpose()
}

fn step_uses<'a>(step: &'a Mapping, subject: &str) -> Result<Option<&'a str>> {
    let uses = mapping_value(step, "uses");
    let run = mapping_value(step, "run");
    let supported = match (uses, run) {
        (Some(_), None) => ACTION_STEP_FIELDS,
        (None, Some(_)) => RUN_STEP_FIELDS,
        (Some(_), Some(_)) => bail!("{subject} cannot contain both uses and run"),
        (None, None) => bail!("{subject} must contain exactly one of uses or run"),
    };
    reject_unknown_fields(step, supported, subject)?;
    validate_optional_string(step, "name", subject, false)?;
    validate_optional_string(step, "id", subject, true)?;
    if let Some(id) = mapping_value(step, "id").and_then(Value::as_str)
        && !valid_runner_id(id)
    {
        bail!("{subject} ID {id:?} is outside the supported Runner identifier syntax");
    }
    validate_optional_positive_number(step, "timeout-minutes", subject)?;
    if let Some(env) = mapping_value(step, "env") {
        validate_dynamic_string_mapping(env, &format!("{subject} env"), STEP_ENV_CONTEXTS)?;
    }
    match (uses, run) {
        (Some(_), Some(_)) => bail!("{subject} cannot contain both uses and run"),
        (None, None) => bail!("{subject} must contain exactly one of uses or run"),
        (Some(value), None) => {
            if let Some(with) = mapping_value(step, "with") {
                validate_scalar_mapping(with, &format!("{subject} with"), true)?;
            }
            value
                .as_str()
                .filter(|uses| !uses.trim().is_empty())
                .map(Some)
                .ok_or_else(|| anyhow!("{subject} uses must be a non-empty string"))
        }
        (None, Some(value)) => {
            if value.as_str().is_none_or(|run| run.trim().is_empty()) {
                bail!("{subject} run must be a string");
            }
            validate_optional_string(step, "shell", subject, true)?;
            validate_optional_string(step, "working-directory", subject, true)?;
            Ok(None)
        }
    }
}

/// Return the finite literal matrix assignments for a job.  GitHub's
/// `include`, `exclude`, expressions, and object-valued matrix entries require
/// expression-context evaluation and are intentionally rejected here rather
/// than approximated from a result ledger.
fn job_matrix(job: &Mapping) -> Result<Vec<BTreeMap<String, String>>> {
    let Some(strategy) = mapping_value(job, "strategy") else {
        return Ok(vec![BTreeMap::new()]);
    };
    let strategy = strategy
        .as_mapping()
        .ok_or_else(|| anyhow!("workflow strategy must be a mapping"))?;
    reject_unknown_fields(strategy, STRATEGY_FIELDS, "workflow strategy")?;
    let Some(matrix) = mapping_value(strategy, "matrix") else {
        return Ok(vec![BTreeMap::new()]);
    };
    let matrix = matrix
        .as_mapping()
        .ok_or_else(|| anyhow!("workflow matrix must be a mapping"))?;
    if matrix.contains_key("include") || matrix.contains_key("exclude") {
        bail!("workflow matrix include/exclude requires expression-aware derivation");
    }
    if matrix.is_empty() {
        bail!("workflow matrix cannot be empty");
    }
    let mut assignment_count = 1usize;
    for (key, values) in matrix {
        let key = key.as_str();
        if !valid_runner_keyword(key) {
            bail!("workflow matrix key {key:?} is not a Runner expression property name");
        }
        let values = values
            .as_sequence()
            .ok_or_else(|| anyhow!("workflow matrix key {key} must contain a literal sequence"))?;
        if values.is_empty() {
            bail!("workflow matrix key {key} has no values");
        }
        assignment_count = assignment_count
            .checked_mul(values.len())
            .ok_or_else(|| anyhow!("workflow matrix cardinality overflows usize"))?;
        if assignment_count > MAX_MATRIX_ASSIGNMENTS {
            bail!(
                "workflow matrix expands to {assignment_count} assignments, above the checker limit of {MAX_MATRIX_ASSIGNMENTS}"
            );
        }
    }

    // Validate and own each dimension only after the cross-product cardinality
    // is known to be bounded. This avoids growing an intermediate product for
    // a matrix whose final size cannot be represented by the checker plan.
    let mut vectors = Vec::with_capacity(matrix.len());
    for (key, values) in matrix {
        let key = key.as_str();
        let values = values
            .as_sequence()
            .ok_or_else(|| anyhow!("workflow matrix key {key} must contain a literal sequence"))?;
        let mut scalar_values = BTreeSet::new();
        let mut scalar_values_in_order = Vec::with_capacity(values.len());
        for value in values {
            let value = matrix_scalar(value)
                .with_context(|| format!("workflow matrix key {key} has a non-literal value"))?;
            if !scalar_values.insert(value.clone()) {
                bail!("workflow matrix key {key} repeats value {value}");
            }
            scalar_values_in_order.push(value);
        }
        vectors.push((key.to_owned(), scalar_values_in_order));
    }

    let assignment_cells = assignment_count
        .checked_mul(vectors.len())
        .ok_or_else(|| anyhow!("workflow matrix assignment cell count overflows usize"))?;
    if assignment_cells > MAX_MATRIX_ASSIGNMENT_CELLS {
        bail!(
            "workflow matrix stores {assignment_cells} assignment cells, above the checker limit of {MAX_MATRIX_ASSIGNMENT_CELLS}"
        );
    }
    let mut assignment_bytes = 0usize;
    for (key, values) in &vectors {
        let repeated_keys = key
            .len()
            .checked_mul(assignment_count)
            .ok_or_else(|| anyhow!("workflow matrix key size overflows usize"))?;
        let value_bytes = values
            .iter()
            .try_fold(0usize, |total, value| total.checked_add(value.len()))
            .ok_or_else(|| anyhow!("workflow matrix value size overflows usize"))?;
        let repeated_values = value_bytes
            .checked_mul(assignment_count / values.len())
            .ok_or_else(|| anyhow!("workflow matrix expanded value size overflows usize"))?;
        assignment_bytes = assignment_bytes
            .checked_add(repeated_keys)
            .and_then(|total| total.checked_add(repeated_values))
            .ok_or_else(|| anyhow!("workflow matrix assignment size overflows usize"))?;
    }
    if assignment_bytes > MAX_MATRIX_ASSIGNMENT_BYTES {
        bail!(
            "workflow matrix stores {assignment_bytes} expanded bytes, above the checker limit of {MAX_MATRIX_ASSIGNMENT_BYTES}"
        );
    }

    let mut assignments = vec![BTreeMap::new()];
    for (key, values) in vectors {
        let expanded_count = assignments
            .len()
            .checked_mul(values.len())
            .ok_or_else(|| anyhow!("workflow matrix cardinality overflows usize"))?;
        let base_assignments = assignments;
        let mut expanded = Vec::with_capacity(expanded_count);
        for value in values {
            for assignment in &base_assignments {
                let mut assignment = assignment.clone();
                assignment.insert(key.clone(), value.clone());
                expanded.push(assignment);
            }
        }
        assignments = expanded;
    }
    Ok(assignments)
}

fn validate_workflow_metadata(workflow: &Mapping) -> Result<()> {
    for field in ["name", "description", "run-name"] {
        validate_optional_string(workflow, field, "workflow", false)?;
    }
    if let Some(environment) = mapping_value(workflow, "env") {
        validate_dynamic_string_mapping(environment, "workflow env", WORKFLOW_ENV_CONTEXTS)?;
    }
    Ok(())
}

fn valid_job_id(job_id: &str) -> bool {
    valid_runner_id(job_id)
}

fn valid_runner_id(id: &str) -> bool {
    if id.len() >= 100 || id.starts_with("__") {
        return false;
    }
    let mut bytes = id.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_runner_keyword(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_optional_string(
    mapping: &Mapping,
    field: &str,
    subject: &str,
    non_empty: bool,
) -> Result<()> {
    let Some(value) = mapping_value(mapping, field) else {
        return Ok(());
    };
    match value {
        Value::String(value) if !non_empty || !value.trim().is_empty() => Ok(()),
        _ if non_empty => bail!("{subject} {field} must be a non-empty string"),
        _ => bail!("{subject} {field} must be a string"),
    }
}

fn validate_optional_positive_number(mapping: &Mapping, field: &str, subject: &str) -> Result<()> {
    let Some(value) = mapping_value(mapping, field) else {
        return Ok(());
    };
    if !matches!(value, Value::Number(number) if number.as_f64().is_finite() && number.as_f64() > 0.0)
    {
        bail!("{subject} {field} must be a positive number in the supported source subset");
    }
    Ok(())
}

fn validate_dynamic_string_mapping(
    value: &Value,
    subject: &str,
    allowed_context: &[&str],
) -> Result<()> {
    let mapping = value
        .as_mapping()
        .ok_or_else(|| anyhow!("{subject} must be a mapping"))?;
    for (key, value) in mapping {
        let key = key.as_str();
        if key.trim().is_empty() {
            bail!("{subject} keys must be non-empty strings");
        }
        let value = runner_scalar_to_string(value)
            .ok_or_else(|| anyhow!("{subject} value for {key:?} must be a Runner scalar"))?;
        validate_expression_contexts(
            &value,
            allowed_context,
            &format!("{subject} value for {key:?}"),
        )?;
    }
    Ok(())
}

fn runner_scalar_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(runner_number_to_string(value.as_f64())),
        Value::Null => Some(String::new()),
        Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => None,
    }
}

/// Check root names and function availability against the field's Runner
/// schema context. Property names following `.` are not context names.
fn validate_expression_contexts(
    value: &str,
    allowed_context: &[&str],
    subject: &str,
) -> Result<()> {
    let mut cursor = 0;
    while let Some(relative_start) = value[cursor..].find("${{") {
        let start = cursor + relative_start;
        let expression_start = start + 3;
        let close_start = runner_expression_close(value, expression_start)
            .ok_or_else(|| anyhow!("{subject} has an unclosed Runner expression"))?;
        let close_end = close_start + 2;
        let expression = &value[expression_start..close_start];
        validate_expression_names(expression, allowed_context, subject)?;
        cursor = close_end;
    }
    Ok(())
}

fn runner_expression_close(value: &str, start: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut index = start;
    let mut in_string = false;
    while index < bytes.len() {
        if bytes[index] == b'\'' {
            if in_string && bytes.get(index + 1) == Some(&b'\'') {
                index += 2;
                continue;
            }
            in_string = !in_string;
            index += 1;
            continue;
        }
        if !in_string && bytes[index] == b'}' && bytes.get(index + 1) == Some(&b'}') {
            return Some(index);
        }
        let character = value.get(index..)?.chars().next()?;
        index += character.len_utf8();
    }
    None
}

fn validate_expression_names(
    expression: &str,
    allowed_context: &[&str],
    subject: &str,
) -> Result<()> {
    let tokens = tokenize_workflow_expression(expression, subject)?;
    RunnerExpressionParser {
        tokens,
        cursor: 0,
        allowed_context: allowed_context
            .iter()
            .map(|context| context.to_ascii_lowercase())
            .collect(),
        subject: subject.to_owned(),
        nested_depth: 0,
    }
    .parse()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkflowExpressionTokenKind {
    Identifier(String),
    String,
    Number,
    LeftParenthesis,
    RightParenthesis,
    LeftBracket,
    RightBracket,
    Comma,
    Dot,
    Wildcard,
    Operator(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowExpressionToken {
    kind: WorkflowExpressionTokenKind,
    position: usize,
}

fn tokenize_workflow_expression(
    expression: &str,
    subject: &str,
) -> Result<Vec<WorkflowExpressionToken>> {
    let expression_length = expression.encode_utf16().count();
    if expression_length > RUNNER_MAX_EXPRESSION_LENGTH {
        bail!("{subject} Runner expression exceeds the {RUNNER_MAX_EXPRESSION_LENGTH} character limit");
    }

    let bytes = expression.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(character) = expression
            .get(cursor..)
            .and_then(|tail| tail.chars().next())
        else {
            bail!("{subject} has an invalid UTF-8 boundary in its Runner expression");
        };
        if character.is_whitespace() {
            cursor += character.len_utf8();
            continue;
        }

        let position = cursor;
        let kind = match character {
            '(' => {
                cursor += 1;
                WorkflowExpressionTokenKind::LeftParenthesis
            }
            ')' => {
                cursor += 1;
                WorkflowExpressionTokenKind::RightParenthesis
            }
            '[' => {
                cursor += 1;
                WorkflowExpressionTokenKind::LeftBracket
            }
            ']' => {
                cursor += 1;
                WorkflowExpressionTokenKind::RightBracket
            }
            ',' => {
                cursor += 1;
                WorkflowExpressionTokenKind::Comma
            }
            '*' => {
                cursor += 1;
                WorkflowExpressionTokenKind::Wildcard
            }
            '.' if dot_starts_runner_number(&tokens) => {
                cursor = runner_number_end(bytes, cursor, expression, subject)?;
                let number = &expression[position..cursor];
                if !valid_runner_number(number) {
                    bail!("{subject} has an invalid Runner number at byte {position}");
                }
                WorkflowExpressionTokenKind::Number
            }
            '.' => {
                cursor += 1;
                WorkflowExpressionTokenKind::Dot
            }
            '\'' => {
                cursor = runner_string_end(bytes, cursor).ok_or_else(|| {
                    anyhow!("{subject} has an unclosed Runner string at byte {position}")
                })?;
                WorkflowExpressionTokenKind::String
            }
            '!' | '>' | '<' | '=' | '&' | '|' => {
                let next = bytes.get(cursor + 1).copied();
                let operator = match (character, next) {
                    ('!', Some(b'=')) => "!=",
                    ('>', Some(b'=')) => ">=",
                    ('<', Some(b'=')) => "<=",
                    ('=', Some(b'=')) => "==",
                    ('&', Some(b'&')) => "&&",
                    ('|', Some(b'|')) => "||",
                    ('!', _) => "!",
                    ('>', _) => ">",
                    ('<', _) => "<",
                    _ => bail!("{subject} has an invalid Runner operator at byte {position}"),
                };
                cursor += operator.len();
                WorkflowExpressionTokenKind::Operator(operator.to_owned())
            }
            '+' | '-' | '0'..='9' => {
                cursor = runner_number_end(bytes, cursor, expression, subject)?;
                let number = &expression[position..cursor];
                if !valid_runner_number(number) {
                    bail!("{subject} has an invalid Runner number at byte {position}");
                }
                WorkflowExpressionTokenKind::Number
            }
            value if value.is_ascii_alphabetic() || value == '_' => {
                cursor += value.len_utf8();
                while cursor < bytes.len() {
                    let Some(character) = expression
                        .get(cursor..)
                        .and_then(|tail| tail.chars().next())
                    else {
                        bail!("{subject} has an invalid UTF-8 boundary in its Runner expression");
                    };
                    if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                        cursor += character.len_utf8();
                    } else {
                        break;
                    }
                }
                WorkflowExpressionTokenKind::Identifier(expression[position..cursor].to_owned())
            }
            _ => bail!("{subject} has an unexpected Runner expression symbol at byte {position}"),
        };
        tokens.push(WorkflowExpressionToken { kind, position });
    }
    Ok(tokens)
}

fn dot_starts_runner_number(tokens: &[WorkflowExpressionToken]) -> bool {
    let Some(token) = tokens.last() else {
        return true;
    };
    matches!(
        &token.kind,
        WorkflowExpressionTokenKind::LeftParenthesis
            | WorkflowExpressionTokenKind::LeftBracket
            | WorkflowExpressionTokenKind::Comma
            | WorkflowExpressionTokenKind::Operator(_)
    )
}

fn runner_number_end(
    bytes: &[u8],
    mut cursor: usize,
    expression: &str,
    subject: &str,
) -> Result<usize> {
    while cursor < bytes.len() {
        let Some(character) = expression
            .get(cursor..)
            .and_then(|tail| tail.chars().next())
        else {
            bail!("{subject} has an invalid UTF-8 boundary in its Runner expression");
        };
        if character.is_whitespace()
            || matches!(
                character,
                '(' | ')' | '[' | ']' | ',' | '!' | '>' | '<' | '=' | '&' | '|'
            )
        {
            break;
        }
        cursor += character.len_utf8();
    }
    Ok(cursor)
}

fn valid_runner_number(number: &str) -> bool {
    if matches!(number, "Infinity" | "-Infinity") {
        return true;
    }
    if let Some(hexadecimal) = number.strip_prefix("0x") {
        return !hexadecimal.is_empty()
            && hexadecimal.len() <= 8
            && hexadecimal
                .chars()
                .all(|character| character.is_ascii_hexdigit());
    }
    if let Some(octal) = number.strip_prefix("0o") {
        return !octal.is_empty()
            && octal
                .chars()
                .all(|character| matches!(character, '0'..='7'))
            && u32::from_str_radix(octal, 8).is_ok_and(|value| value <= i32::MAX as u32);
    }

    let number = number
        .strip_prefix('-')
        .or_else(|| number.strip_prefix('+'))
        .unwrap_or(number);
    let (mantissa, exponent) = number
        .split_once(['e', 'E'])
        .map_or((number, None), |(mantissa, exponent)| {
            (mantissa, Some(exponent))
        });
    let mut decimal_points = 0;
    let mut mantissa_digits = 0;
    for character in mantissa.chars() {
        match character {
            '0'..='9' => mantissa_digits += 1,
            '.' => decimal_points += 1,
            _ => return false,
        }
    }
    if decimal_points > 1 || mantissa_digits == 0 {
        return false;
    }
    let Some(exponent) = exponent else {
        return true;
    };
    let exponent = exponent
        .strip_prefix('-')
        .or_else(|| exponent.strip_prefix('+'))
        .unwrap_or(exponent);
    !exponent.is_empty() && exponent.chars().all(|character| character.is_ascii_digit())
}

fn runner_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\'' {
            if bytes.get(cursor + 1) == Some(&b'\'') {
                cursor += 2;
            } else {
                return Some(cursor + 1);
            }
        } else {
            cursor += 1;
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunnerLogicalOperator {
    And,
    Or,
}

#[derive(Clone, Copy, Debug)]
struct RunnerExpressionShape {
    depth: usize,
    logical_operator: Option<RunnerLogicalOperator>,
    maximum_direct_child_depth: usize,
}

impl RunnerExpressionShape {
    fn value() -> Self {
        Self {
            depth: 1,
            logical_operator: None,
            maximum_direct_child_depth: 0,
        }
    }

    fn node(self, other: Option<Self>, subject: &str) -> Result<Self> {
        let depth = other.map_or(self.depth, |other| self.depth.max(other.depth)) + 1;
        Self::with_depth(depth, None, 0, subject)
    }

    fn logical(self, other: Self, operator: RunnerLogicalOperator, subject: &str) -> Result<Self> {
        let left_depth = if self.logical_operator == Some(operator) {
            self.maximum_direct_child_depth
        } else {
            self.depth
        };
        let right_depth = if other.logical_operator == Some(operator) {
            other.maximum_direct_child_depth
        } else {
            other.depth
        };
        let maximum_direct_child_depth = left_depth.max(right_depth);
        Self::with_depth(
            maximum_direct_child_depth + 1,
            Some(operator),
            maximum_direct_child_depth,
            subject,
        )
    }

    fn with_depth(
        depth: usize,
        logical_operator: Option<RunnerLogicalOperator>,
        maximum_direct_child_depth: usize,
        subject: &str,
    ) -> Result<Self> {
        if depth > RUNNER_MAX_EXPRESSION_DEPTH {
            bail!("{subject} Runner expression exceeds the {RUNNER_MAX_EXPRESSION_DEPTH} level depth limit");
        }
        Ok(Self {
            depth,
            logical_operator,
            maximum_direct_child_depth,
        })
    }
}

struct RunnerExpressionParser {
    tokens: Vec<WorkflowExpressionToken>,
    cursor: usize,
    allowed_context: Vec<String>,
    subject: String,
    nested_depth: usize,
}

impl RunnerExpressionParser {
    fn parse(mut self) -> Result<()> {
        if self.tokens.is_empty() {
            bail!("{} contains an empty Runner expression", self.subject);
        }
        self.parse_or()?;
        if self.cursor != self.tokens.len() {
            self.fail("unexpected token after Runner expression")?;
        }
        Ok(())
    }

    fn parse_or(&mut self) -> Result<RunnerExpressionShape> {
        let mut left = self.parse_and()?;
        while self.consume_operator("||") {
            let right = self.parse_and()?;
            left = left.logical(right, RunnerLogicalOperator::Or, &self.subject)?;
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<RunnerExpressionShape> {
        let mut left = self.parse_equality()?;
        while self.consume_operator("&&") {
            let right = self.parse_equality()?;
            left = left.logical(right, RunnerLogicalOperator::And, &self.subject)?;
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<RunnerExpressionShape> {
        let mut left = self.parse_comparison()?;
        while self.consume_operator("==") || self.consume_operator("!=") {
            let right = self.parse_comparison()?;
            left = left.node(Some(right), &self.subject)?;
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<RunnerExpressionShape> {
        let mut left = self.parse_unary()?;
        while self.consume_operator(">")
            || self.consume_operator(">=")
            || self.consume_operator("<")
            || self.consume_operator("<=")
        {
            let right = self.parse_unary()?;
            left = left.node(Some(right), &self.subject)?;
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<RunnerExpressionShape> {
        if self.consume_operator("!") {
            self.enter_nested_expression()?;
            let operand = self.parse_unary();
            self.nested_depth -= 1;
            return operand?.node(None, &self.subject);
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<RunnerExpressionShape> {
        let mut expression = self.parse_primary()?;
        loop {
            if self.consume_kind(|kind| matches!(kind, WorkflowExpressionTokenKind::Dot)) {
                let property = match self.current().map(|token| &token.kind) {
                    Some(
                        WorkflowExpressionTokenKind::Identifier(_)
                        | WorkflowExpressionTokenKind::Number
                        | WorkflowExpressionTokenKind::Wildcard,
                    ) => {
                        self.cursor += 1;
                        RunnerExpressionShape::value()
                    }
                    _ => self.fail("expected Runner property name after '.'")?,
                };
                expression = expression.node(Some(property), &self.subject)?;
                continue;
            }
            if self.consume_kind(|kind| matches!(kind, WorkflowExpressionTokenKind::LeftBracket)) {
                self.enter_nested_expression()?;
                let index = self.parse_or();
                let closing = self
                    .consume_kind(|kind| matches!(kind, WorkflowExpressionTokenKind::RightBracket));
                self.nested_depth -= 1;
                let index = index?;
                if !closing {
                    self.fail("unclosed Runner index expression")?;
                }
                expression = expression.node(Some(index), &self.subject)?;
                continue;
            }
            break;
        }
        Ok(expression)
    }

    fn parse_primary(&mut self) -> Result<RunnerExpressionShape> {
        let Some(token) = self.current().cloned() else {
            return self.fail("expected Runner expression value");
        };
        match token.kind {
            WorkflowExpressionTokenKind::String | WorkflowExpressionTokenKind::Number => {
                self.cursor += 1;
                Ok(RunnerExpressionShape::value())
            }
            WorkflowExpressionTokenKind::Identifier(name) => {
                self.cursor += 1;
                if self.is_next_kind(|kind| {
                    matches!(kind, WorkflowExpressionTokenKind::LeftParenthesis)
                }) {
                    if matches!(
                        name.as_str(),
                        "null" | "true" | "false" | "NaN" | "Infinity"
                    ) {
                        return self.fail("Runner literal cannot be called as a function");
                    }
                    return self.parse_function(&name);
                }
                if matches!(
                    name.as_str(),
                    "null" | "true" | "false" | "NaN" | "Infinity"
                ) {
                    return Ok(RunnerExpressionShape::value());
                }
                if self
                    .allowed_context
                    .iter()
                    .any(|context| context == &name.to_ascii_lowercase())
                {
                    return Ok(RunnerExpressionShape::value());
                }
                Err(anyhow!(
                    "{} uses disallowed Runner context {name}",
                    self.subject
                ))
            }
            WorkflowExpressionTokenKind::LeftParenthesis => {
                self.cursor += 1;
                self.enter_nested_expression()?;
                let expression = self.parse_or();
                let closing = self.consume_kind(|kind| {
                    matches!(kind, WorkflowExpressionTokenKind::RightParenthesis)
                });
                self.nested_depth -= 1;
                let expression = expression?;
                if !closing {
                    self.fail("unclosed Runner group")?;
                }
                Ok(expression)
            }
            _ => self.fail("expected Runner expression value"),
        }
    }

    fn parse_function(&mut self, name: &str) -> Result<RunnerExpressionShape> {
        let (minimum, maximum) = runner_function_arity(name, &self.allowed_context)
            .ok_or_else(|| anyhow!("{} uses disallowed Runner function {name}", self.subject))?;
        self.consume_kind(|kind| matches!(kind, WorkflowExpressionTokenKind::LeftParenthesis));
        self.enter_nested_expression()?;

        let mut parameter_count = 0;
        let mut maximum_parameter_depth = 0;
        if !self.consume_kind(|kind| matches!(kind, WorkflowExpressionTokenKind::RightParenthesis))
        {
            loop {
                let parameter = self.parse_or()?;
                parameter_count += 1;
                maximum_parameter_depth = maximum_parameter_depth.max(parameter.depth);
                if self.consume_kind(|kind| matches!(kind, WorkflowExpressionTokenKind::Comma)) {
                    continue;
                }
                if !self.consume_kind(|kind| {
                    matches!(kind, WorkflowExpressionTokenKind::RightParenthesis)
                }) {
                    self.fail("expected ',' or ')' in Runner function call")?;
                }
                break;
            }
        }
        self.nested_depth -= 1;

        if parameter_count < minimum || parameter_count > maximum {
            bail!(
                "{} Runner function {name} requires {minimum}..={maximum} arguments, got {parameter_count}",
                self.subject
            );
        }
        if name.eq_ignore_ascii_case("case") && parameter_count % 2 == 0 {
            bail!(
                "{} Runner function case requires an odd number of arguments",
                self.subject
            );
        }
        RunnerExpressionShape::with_depth(
            if parameter_count == 0 {
                1
            } else {
                maximum_parameter_depth + 1
            },
            None,
            0,
            &self.subject,
        )
    }

    fn current(&self) -> Option<&WorkflowExpressionToken> {
        self.tokens.get(self.cursor)
    }

    fn is_next_kind(&self, predicate: impl FnOnce(&WorkflowExpressionTokenKind) -> bool) -> bool {
        self.current().is_some_and(|token| predicate(&token.kind))
    }

    fn consume_kind(
        &mut self,
        predicate: impl FnOnce(&WorkflowExpressionTokenKind) -> bool,
    ) -> bool {
        if self.is_next_kind(predicate) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn consume_operator(&mut self, expected: &str) -> bool {
        if self.current().is_some_and(|token| {
            matches!(&token.kind, WorkflowExpressionTokenKind::Operator(operator) if operator == expected)
        }) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn enter_nested_expression(&mut self) -> Result<()> {
        if self.nested_depth >= MAX_EXPRESSION_PARSER_NESTING {
            bail!("{} Runner expression nesting is too deep", self.subject);
        }
        self.nested_depth += 1;
        Ok(())
    }

    fn fail<T>(&self, message: &str) -> Result<T> {
        let position = self
            .current()
            .map(|token| token.position)
            .or_else(|| self.tokens.last().map(|token| token.position + 1))
            .unwrap_or_default();
        Err(anyhow!("{} {message} at byte {position}", self.subject))
    }
}

fn runner_function_arity(name: &str, allowed_context: &[String]) -> Option<(usize, usize)> {
    if let Some((_, minimum, maximum)) = GLOBAL_EXPRESSION_FUNCTIONS
        .iter()
        .find(|(function, _, _)| (*function).eq_ignore_ascii_case(name))
    {
        return Some((*minimum, *maximum));
    }

    allowed_context.iter().find_map(|context| {
        let (function, parameters) = context.split_once('(')?;
        if !function.eq_ignore_ascii_case(name) {
            return None;
        }
        let parameters = parameters.strip_suffix(')')?;
        let (minimum, maximum) = parameters.split_once(',')?;
        Some((
            parse_runner_function_parameter(minimum)?,
            parse_runner_function_parameter(maximum)?,
        ))
    })
}

fn parse_runner_function_parameter(value: &str) -> Option<usize> {
    if value == "MAX" {
        return Some(u8::MAX as usize);
    }
    value.parse().ok()
}

fn validate_scalar_mapping(value: &Value, subject: &str, string_values_only: bool) -> Result<()> {
    let mapping = value
        .as_mapping()
        .ok_or_else(|| anyhow!("{subject} must be a mapping"))?;
    for (key, value) in mapping {
        let key = key.as_str();
        if key.trim().is_empty() {
            bail!("{subject} keys must be non-empty strings");
        }
        let valid_value = if string_values_only {
            matches!(
                value,
                Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null
            )
        } else {
            matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
        };
        if !valid_value {
            bail!("{subject} value for {key:?} has an unsupported type");
        }
    }
    Ok(())
}

fn validate_workflow_job_secrets(value: &Value, subject: &str) -> Result<()> {
    if value.as_str() == Some("inherit") {
        return Ok(());
    }
    let secret_subject = format!("{subject} secrets");
    validate_scalar_mapping(value, &secret_subject, false)?;
    let mapping = value
        .as_mapping()
        .ok_or_else(|| anyhow!("{secret_subject} must be a mapping"))?;
    for (key, value) in mapping {
        let key = key.as_str();
        let value = runner_scalar_to_string(value)
            .ok_or_else(|| anyhow!("{secret_subject} value for {key:?} has an unsupported type"))?;
        validate_expression_contexts(
            &value,
            WORKFLOW_CALL_SECRET_CONTEXTS,
            &format!("{secret_subject} value for {key:?}"),
        )?;
    }
    Ok(())
}

fn matrix_scalar(value: &Value) -> Result<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() && !value.contains("${{") => {
            Ok(value.clone())
        }
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(runner_number_to_string(value.as_f64())),
        _ => bail!("matrix values must be non-empty literal strings, booleans, or numbers"),
    }
}

fn resolve_matrix_value(value: &Value, assignment: &BTreeMap<String, String>) -> Result<Value> {
    match value {
        Value::String(value) if value.contains("${{") => {
            let expression = value
                .trim()
                .strip_prefix("${{")
                .and_then(|value| value.strip_suffix("}}").map(str::trim))
                .ok_or_else(|| anyhow!("matrix expression is not a complete expression"))?;
            let key = expression
                .strip_prefix("matrix.")
                .filter(|key| !key.trim().is_empty())
                .ok_or_else(|| anyhow!("matrix expression is not a direct matrix lookup"))?;
            let resolved = assignment
                .get(key)
                .ok_or_else(|| anyhow!("matrix expression references unknown key {key}"))?;
            Ok(Value::String(resolved.clone()))
        }
        Value::String(_) => Ok(value.clone()),
        Value::Sequence(values) => Ok(Value::Sequence(
            values
                .iter()
                .map(|value| resolve_matrix_value(value, assignment))
                .collect::<Result<Vec<_>>>()?,
        )),
        _ => Ok(value.clone()),
    }
}

fn constant_condition(value: &Value) -> Result<bool> {
    match value {
        Value::Bool(value) => Ok(*value),
        Value::String(value) => match value.trim() {
            "true" | "${{ true }}" => Ok(true),
            "false" | "${{ false }}" => Ok(false),
            _ => bail!("workflow condition requires expression-aware derivation"),
        },
        _ => bail!("workflow condition must be a literal boolean"),
    }
}

fn reject_continue_on_error(mapping: &Mapping, subject: &str) -> Result<()> {
    let Some(value) = mapping_value(mapping, "continue-on-error") else {
        return Ok(());
    };
    match constant_condition(value) {
        Ok(false) => Ok(()),
        Ok(true) => bail!("{subject} enables continue-on-error"),
        Err(_) => bail!("{subject} has a dynamic continue-on-error expression"),
    }
}

fn resolve_reusable_target(
    reference: &str,
    current: &G0WorkflowSource,
) -> Result<(SourceKey, String)> {
    if let Some(path) = reference.strip_prefix("./") {
        if path.contains('@') {
            bail!("same-repository reusable workflow path must not include an @ref");
        }
        return Ok((
            SourceKey {
                repository: current.repository.clone(),
                path: reusable_workflow_path(path)?,
                root_action: false,
            },
            current.source_sha.clone(),
        ));
    }
    let (mut target, pinned_ref) = resolve_external_source_target(reference, false)?;
    target.path = reusable_workflow_path(&target.path)?;
    Ok((target, pinned_ref))
}

fn reusable_workflow_path(path: &str) -> Result<String> {
    let path = normalize_path(path)?;
    let Some(filename) = path.strip_prefix(".github/workflows/") else {
        bail!("reusable workflow path must be under .github/workflows");
    };
    if filename.is_empty()
        || filename.contains('/')
        || !(filename.ends_with(".yml") || filename.ends_with(".yaml"))
    {
        bail!("reusable workflow path must name a .yml or .yaml file directly under .github/workflows");
    }
    Ok(path)
}

fn resolve_action_target(reference: &str) -> Result<(SourceKey, String)> {
    if reference.starts_with("$/") {
        bail!("self-repository $/ action uses syntax is not valid in WorkflowTemplateConverter");
    }
    if reference.starts_with("./") {
        bail!("workspace-relative local action source requires checkout-content binding, which is unsupported");
    }
    resolve_external_source_target(reference, true)
}

fn resolve_external_source_target(
    reference: &str,
    allow_root_action: bool,
) -> Result<(SourceKey, String)> {
    let (target, pinned_ref) = reference
        .split_once('@')
        .ok_or_else(|| anyhow!("workflow source reference {reference} lacks immutable @ref"))?;
    if target.trim().is_empty()
        || pinned_ref.trim().is_empty()
        || pinned_ref.len() != 40
        || !pinned_ref
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("workflow source reference {reference} has empty target/ref");
    }
    if target.starts_with("docker://") || target.starts_with("http://") {
        bail!("workflow source reference {reference} is not an immutable repository source");
    }
    if target.ends_with('/') {
        bail!("workflow source reference {reference} has an empty trailing path segment");
    }
    let mut parts = target.splitn(3, '/');
    let owner = parts.next().unwrap_or_default();
    let repository = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if owner.is_empty() || repository.is_empty() || (path.is_empty() && !allow_root_action) {
        bail!("workflow source reference {reference} lacks owner/repository/path");
    }
    Ok((
        SourceKey {
            repository: format!("{owner}/{repository}"),
            path: if path.is_empty() {
                String::new()
            } else {
                normalize_path(path)?
            },
            root_action: path.is_empty(),
        },
        pinned_ref.to_owned(),
    ))
}

fn action_dependency<'map, 'source>(
    sources: &'map BTreeMap<SourceKey, CapturedWorkflowSource<'source>>,
    target: &SourceKey,
) -> Result<&'map CapturedWorkflowSource<'source>> {
    let expected_paths = if target.root_action {
        ["action.yml".to_owned(), "action.yaml".to_owned()]
    } else {
        [
            format!("{}/action.yml", target.path),
            format!("{}/action.yaml", target.path),
        ]
    };
    let mut candidates = expected_paths.into_iter().filter_map(|path| {
        sources.get(&SourceKey {
            repository: target.repository.clone(),
            path,
            root_action: false,
        })
    });
    let Some(dependency) = candidates.next() else {
        if target.root_action {
            bail!(
                "workflow references uncaptured root action metadata for repository {} (expected action.yml or action.yaml)",
                target.repository
            );
        }
        bail!(
            "workflow references uncaptured immutable action metadata for {}/{} (expected {}/action.yml or {}/action.yaml)",
            target.repository,
            target.path,
            target.path,
            target.path
        );
    };
    if candidates.next().is_some() {
        if target.root_action {
            bail!(
                "workflow root action repository {} has ambiguous action.yml/action.yaml captures",
                target.repository
            );
        }
        bail!(
            "workflow action directory {}/{} has ambiguous action.yml/action.yaml captures",
            target.repository,
            target.path
        );
    }
    Ok(dependency)
}

fn normalize_path(path: &str) -> Result<String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        bail!("workflow source path is not a safe relative path: {path}");
    }
    Ok(path.to_owned())
}

fn workflow_events(mapping: &Mapping) -> Result<BTreeSet<String>> {
    let trigger =
        mapping_value(mapping, "on").ok_or_else(|| anyhow!("workflow source lacks on"))?;
    let mut events = BTreeSet::new();
    match trigger {
        Value::String(event) => {
            insert_supported_event(&mut events, event)?;
        }
        Value::Sequence(values) => {
            for value in values {
                let event = value
                    .as_str()
                    .filter(|event| !event.trim().is_empty())
                    .ok_or_else(|| anyhow!("workflow trigger sequence contains non-string"))?;
                insert_supported_event(&mut events, event)?;
            }
        }
        Value::Mapping(values) => {
            for (key, configuration) in values {
                let event = key.as_str();
                if event.trim().is_empty() {
                    bail!("workflow trigger key is not a non-empty string");
                }
                validate_trigger_configuration(event, configuration)?;
                insert_supported_event(&mut events, event)?;
            }
        }
        _ => bail!("workflow trigger must be a string, sequence, or mapping"),
    }
    if events.is_empty() {
        bail!("workflow source has no trigger events");
    }
    Ok(events)
}

fn insert_supported_event(events: &mut BTreeSet<String>, event: &str) -> Result<()> {
    if !SUPPORTED_EVENTS.contains(&event) {
        bail!("workflow event {event:?} is outside the checker-supported event subset");
    }
    events.insert(event.to_owned());
    Ok(())
}

fn validate_trigger_configuration(event: &str, configuration: &Value) -> Result<()> {
    match configuration {
        Value::Null => Ok(()),
        Value::Mapping(values) if event == "workflow_call" => {
            validate_workflow_call_configuration(values)
        }
        Value::Mapping(values) if event == "workflow_dispatch" && values.is_empty() => Ok(()),
        Value::Mapping(_) if event == "workflow_dispatch" => {
            bail!("workflow_dispatch configuration requires input-schema validation")
        }
        Value::Mapping(values) if values.is_empty() => Ok(()),
        Value::Mapping(_) => bail!(
            "workflow trigger {event} has branch/path/type conditions that require event-aware derivation"
        ),
        _ => bail!("workflow trigger {event} has an unsupported configuration"),
    }
}

fn workflow_call_contract(
    workflow: &Mapping,
    events: &BTreeSet<String>,
) -> Result<Option<WorkflowCallContract>> {
    if !events.contains("workflow_call") {
        return Ok(None);
    }

    let mut contract = WorkflowCallContract::default();
    let Some(Value::Mapping(triggers)) = mapping_value(workflow, "on") else {
        return Ok(Some(contract));
    };
    let Some(configuration) = triggers.get("workflow_call").and_then(Value::as_mapping) else {
        return Ok(Some(contract));
    };

    if let Some(inputs) = mapping_value(configuration, "inputs") {
        let definitions = inputs
            .as_mapping()
            .ok_or_else(|| anyhow!("workflow_call input definitions must be a mapping"))?;
        for (name, specification) in definitions {
            let fields = specification
                .as_mapping()
                .ok_or_else(|| anyhow!("workflow_call input {name} must be a mapping"))?;
            let type_name = mapping_value(fields, "type")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("workflow_call input {name} lacks a valid type"))?;
            let required = mapping_value(fields, "required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let folded_name = supported_ordinal_ignore_case_key(name)?;
            if contract
                .inputs
                .insert(
                    folded_name,
                    WorkflowCallInput {
                        name: name.clone(),
                        type_name: type_name.to_owned(),
                        required,
                    },
                )
                .is_some()
            {
                bail!("workflow_call input names collide case-insensitively");
            }
        }
    }

    if let Some(secrets) = mapping_value(configuration, "secrets") {
        let definitions = secrets
            .as_mapping()
            .ok_or_else(|| anyhow!("workflow_call secret definitions must be a mapping"))?;
        for (name, specification) in definitions {
            let required = if matches!(specification, Value::Null) {
                false
            } else {
                let fields = specification.as_mapping().ok_or_else(|| {
                    anyhow!("workflow_call secret {name} must be a mapping or null")
                })?;
                mapping_value(fields, "required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            let folded_name = supported_ordinal_ignore_case_key(name)?;
            if contract
                .secrets
                .insert(
                    folded_name,
                    WorkflowCallSecret {
                        name: name.clone(),
                        required,
                    },
                )
                .is_some()
            {
                bail!("workflow_call secret names collide case-insensitively");
            }
        }
    }

    Ok(Some(contract))
}

fn validate_reusable_workflow_arguments(
    job: &Mapping,
    contract: &WorkflowCallContract,
    subject: &str,
) -> Result<()> {
    let mut supplied_inputs = BTreeSet::new();
    if let Some(with) = mapping_value(job, "with") {
        let inputs = with
            .as_mapping()
            .ok_or_else(|| anyhow!("{subject} with must be a mapping"))?;
        for (name, value) in inputs {
            let folded_name = supported_ordinal_ignore_case_key(name)?;
            let input = contract.inputs.get(&folded_name).ok_or_else(|| {
                anyhow!("{subject} input {name} is not declared by the called workflow")
            })?;
            let value_type = reusable_input_value_type(value, name, subject)?;
            if value_type != input.type_name {
                bail!(
                    "{subject} input {} has type {value_type}, but the called workflow declares type {}",
                    input.name,
                    input.type_name
                );
            }
            supplied_inputs.insert(folded_name);
        }
    }
    for (folded_name, input) in &contract.inputs {
        if input.required && !supplied_inputs.contains(folded_name) {
            bail!(
                "{subject} omits required called-workflow input {}",
                input.name
            );
        }
    }

    match mapping_value(job, "secrets") {
        None => {}
        Some(Value::String(value)) if value == "inherit" => {
            if let Some(secret) = contract.secrets.values().find(|secret| secret.required) {
                bail!(
                    "{subject} cannot prove required called-workflow secret {} is available through secrets: inherit",
                    secret.name
                );
            }
        }
        Some(value) => {
            let secrets = value
                .as_mapping()
                .ok_or_else(|| anyhow!("{subject} secrets must be a mapping or inherit"))?;
            let mut supplied_secrets = BTreeSet::new();
            for name in secrets.keys() {
                let folded_name = supported_ordinal_ignore_case_key(name)?;
                if !contract.secrets.contains_key(&folded_name) {
                    bail!("{subject} secret {name} is not declared by the called workflow");
                }
                supplied_secrets.insert(folded_name);
            }
            for (folded_name, secret) in &contract.secrets {
                if secret.required && !supplied_secrets.contains(folded_name) {
                    bail!(
                        "{subject} omits required called-workflow secret {}",
                        secret.name
                    );
                }
            }
        }
    }
    if mapping_value(job, "secrets").is_none() {
        if let Some(secret) = contract.secrets.values().find(|secret| secret.required) {
            bail!(
                "{subject} omits required called-workflow secret {}",
                secret.name
            );
        }
    }
    Ok(())
}

fn reusable_input_value_type(value: &Value, name: &str, subject: &str) -> Result<String> {
    match value {
        Value::String(value) if value.contains("${{") => {
            bail!("{subject} input {name} type cannot be verified from an expression")
        }
        Value::String(_) => Ok("string".to_owned()),
        Value::Number(_) => Ok("number".to_owned()),
        Value::Bool(_) => Ok("boolean".to_owned()),
        _ => bail!("{subject} input {name} must have a scalar value"),
    }
}

/// Match WorkflowTemplateConverter's declared-type validation for static
/// workflow_call defaults. String inputs coerce scalar literals to strings;
/// boolean and number inputs require those exact literal token types. Runner
/// defers values containing expression tokens until runtime.
fn validate_workflow_call_input_default_type(
    value: &Value,
    type_name: &str,
    subject: &str,
) -> Result<()> {
    if matches!(value, Value::String(value) if value.contains("${{")) {
        return Ok(());
    }

    let matches_type = match type_name {
        "string" => matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_)),
        "boolean" => matches!(value, Value::Bool(_)),
        "number" => matches!(value, Value::Number(_)),
        _ => false,
    };
    if !matches_type {
        bail!("{subject} does not match declared workflow_call input type {type_name}");
    }
    Ok(())
}

fn validate_workflow_call_configuration(configuration: &Mapping) -> Result<()> {
    for (key, value) in configuration {
        let key = key.as_str();
        if key.trim().is_empty() {
            bail!("workflow_call configuration keys must be non-empty strings");
        }
        match key {
            "inputs" => validate_workflow_call_named_specs(
                value,
                "workflow_call input",
                &["description", "required", "type", "default"],
                &["type"],
                false,
            )?,
            "outputs" => validate_workflow_call_named_specs(
                value,
                "workflow_call output",
                &["description", "value"],
                &["value"],
                false,
            )?,
            "secrets" => validate_workflow_call_named_specs(
                value,
                "workflow_call secret",
                &["description", "required"],
                &[],
                true,
            )?,
            _ => bail!("workflow_call configuration has unsupported key {key}"),
        }
    }
    Ok(())
}

fn validate_workflow_call_named_specs(
    value: &Value,
    subject: &str,
    allowed_fields: &[&str],
    required_fields: &[&str],
    allow_null_specification: bool,
) -> Result<()> {
    let specifications = value
        .as_mapping()
        .ok_or_else(|| anyhow!("{subject} definitions must be a mapping"))?;
    for (name, specification) in specifications {
        let name = name.as_str();
        if name.trim().is_empty() {
            bail!("{subject} names must be non-empty strings");
        }
        if allow_null_specification && matches!(specification, Value::Null) {
            continue;
        }
        let fields = specification
            .as_mapping()
            .ok_or_else(|| anyhow!("{subject} {name} must be a mapping"))?;
        let mut present = BTreeSet::new();
        for (field, field_value) in fields {
            let field = field.as_str();
            if !allowed_fields.contains(&field) {
                bail!("{subject} {name} has unsupported field {field}");
            }
            present.insert(field);
            match field {
                "description" if field_value.as_str().is_none() => {
                    bail!("{subject} {name} description must be a string")
                }
                "value" => {
                    let value = field_value
                        .as_str()
                        .ok_or_else(|| anyhow!("{subject} {name} value must be a string"))?;
                    validate_expression_contexts(
                        value,
                        WORKFLOW_CALL_OUTPUT_CONTEXTS,
                        &format!("{subject} {name} value"),
                    )?;
                }
                "required" if field_value.as_bool().is_none() => {
                    bail!("{subject} {name} required must be a boolean")
                }
                "type"
                    if !field_value
                        .as_str()
                        .is_some_and(|kind| matches!(kind, "string" | "number" | "boolean")) =>
                {
                    bail!("{subject} {name} type must be string, number, or boolean")
                }
                "default" => {
                    if !matches!(
                        field_value,
                        Value::String(_) | Value::Number(_) | Value::Bool(_)
                    ) {
                        bail!("{subject} {name} default must be a scalar");
                    }
                    let value = runner_scalar_to_string(field_value)
                        .ok_or_else(|| anyhow!("{subject} {name} default must be a scalar"))?;
                    validate_expression_contexts(
                        &value,
                        WORKFLOW_CALL_INPUT_DEFAULT_CONTEXTS,
                        &format!("{subject} {name} default"),
                    )?;
                    let type_name = mapping_value(fields, "type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("{subject} {name} lacks a valid type"))?;
                    validate_workflow_call_input_default_type(
                        field_value,
                        type_name,
                        &format!("{subject} {name} default"),
                    )?;
                }
                _ => {}
            }
        }
        for required in required_fields {
            if !present.contains(required) {
                bail!("{subject} {name} lacks required field {required}");
            }
        }
    }
    Ok(())
}

fn runner_target(value: &Value) -> Result<(String, String, String)> {
    let mut labels = Vec::new();
    match value {
        Value::String(label) => labels.push(label.clone()),
        Value::Sequence(values) => {
            for value in values {
                labels.push(
                    value
                        .as_str()
                        .ok_or_else(|| anyhow!("runs-on label is not a string"))?
                        .to_owned(),
                );
            }
        }
        _ => bail!("runs-on must be a string or string sequence"),
    }
    if labels.is_empty() {
        bail!("runs-on must resolve to concrete source labels");
    }
    let labels = labels
        .into_iter()
        .map(|label| label.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    if labels
        .iter()
        .any(|label| label.is_empty() || label.contains("${{"))
    {
        bail!("runs-on must resolve to concrete non-empty source labels");
    }

    if labels.iter().any(|label| label == "self-hosted") {
        let mut platforms = BTreeSet::new();
        let mut architectures = BTreeSet::new();
        for label in &labels {
            match label.as_str() {
                "linux" | "windows" | "macos" => {
                    platforms.insert(label.as_str());
                }
                "arm" => {
                    bail!("32-bit ARM self-hosted runners are unsupported");
                }
                "arm64" => {
                    architectures.insert("arm64");
                }
                "x64" => {
                    architectures.insert("amd64");
                }
                _ => {}
            }
        }
        if platforms.len() != 1 || architectures.len() != 1 {
            bail!("self-hosted runs-on labels do not resolve one platform and architecture");
        }
        let platform = platforms
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("self-hosted platform label is missing"))?;
        let architecture = architectures
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("self-hosted architecture label is missing"))?;
        return Ok((
            "self-hosted".to_owned(),
            platform.to_owned(),
            architecture.to_owned(),
        ));
    }

    if labels.len() != 1 {
        bail!("GitHub-hosted runs-on must use one documented runner label");
    }
    let label = labels
        .first()
        .ok_or_else(|| anyhow!("GitHub-hosted runs-on label is missing"))?;
    let (platform, architecture) = documented_hosted_target(label)
        .ok_or_else(|| anyhow!("unsupported GitHub-hosted runner label {label}"))?;
    Ok((
        "github".to_owned(),
        platform.to_owned(),
        architecture.to_owned(),
    ))
}

fn documented_hosted_target(label: &str) -> Option<(&'static str, &'static str)> {
    match label {
        "ubuntu-slim" | "ubuntu-latest" | "ubuntu-22.04" | "ubuntu-24.04" | "ubuntu-26.04" => {
            Some(("linux", "amd64"))
        }
        "ubuntu-22.04-arm" | "ubuntu-24.04-arm" | "ubuntu-26.04-arm" => Some(("linux", "arm64")),
        "windows-latest" | "windows-2022" | "windows-2025" | "windows-2025-vs2026" => {
            Some(("windows", "amd64"))
        }
        "windows-11-arm" | "windows-11-vs2026-arm" => Some(("windows", "arm64")),
        "macos-15-intel" | "macos-26-intel" | "macos-14-large" | "macos-15-large"
        | "macos-26-large" | "macos-latest-large" => Some(("macos", "amd64")),
        "macos-latest"
        | "macos-14"
        | "macos-15"
        | "macos-26"
        | "macos-14-xlarge"
        | "macos-15-xlarge"
        | "macos-26-xlarge"
        | "macos-latest-xlarge"
        | "xcode-27"
        | "xcode-27-xlarge" => Some(("macos", "arm64")),
        _ => None,
    }
}

fn mapping_value<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(key)
}

fn reject_unknown_fields(mapping: &Mapping, supported: &[&str], subject: &str) -> Result<()> {
    for key in mapping.keys() {
        let key = key.as_str();
        if !supported.contains(&key) {
            bail!("{subject} has unsupported field {key}");
        }
    }
    Ok(())
}

/// Reject case-fold collisions before looking up any workflow field. Runner
/// uses `StringComparer.OrdinalIgnoreCase`; this checker implements ASCII and
/// one-to-one Latin-1 case pairs only. Other cased non-ASCII keys fail closed
/// because Rust's Unicode lowercase tables are not a proven implementation of
/// every .NET runner's ordinal casing tables.
fn reject_case_insensitive_mapping_duplicates(document: &Value) -> Result<()> {
    let mut pending = vec![document];
    while let Some(value) = pending.pop() {
        match value {
            Value::Mapping(mapping) => {
                let mut folded_keys = BTreeMap::<String, String>::new();
                for (key, child) in mapping {
                    let key = key.as_str();
                    if key.contains("${{") {
                        bail!("expression-valued YAML mapping keys are unsupported");
                    }
                    let folded = supported_ordinal_ignore_case_key(key)?;
                    if let Some(previous) = folded_keys.insert(folded, key.to_owned()) {
                        bail!(
                            "YAML mapping keys {previous:?} and {key:?} collide under case-insensitive comparison"
                        );
                    }
                    pending.push(child);
                }
            }
            Value::Sequence(values) => pending.extend(values),
            Value::Tagged(_) => bail!("tagged YAML values are unsupported in workflow sources"),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

fn supported_ordinal_ignore_case_key(key: &str) -> Result<String> {
    let mut folded = String::with_capacity(key.len());
    for character in key.chars() {
        let folded_character = match character {
            'A'..='Z' => character.to_ascii_lowercase(),
            '\u{00C1}' | '\u{00E1}' => '\u{00E1}',
            _ if character.is_ascii() => character,
            _ => bail!(
                "non-ASCII YAML mapping key {key:?} is outside the checker-supported U+00C1/U+00E1 casing subset"
            ),
        };
        folded.push(folded_character);
    }
    Ok(folded)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "parser fixtures intentionally use direct assertions"
)]
mod tests {
    use super::*;

    fn source(repository: &str, path: &str, yaml: &str) -> G0WorkflowSource {
        G0WorkflowSource {
            repository: repository.to_owned(),
            path: path.to_owned(),
            revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            source_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            source_url: format!(
                "https://github.com/{repository}/blob/{}/{}",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", path
            ),
            media_type: "text/yaml".to_owned(),
            canonicalization: "raw-utf8".to_owned(),
            sha256: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_owned(),
            storage_ref:
                "sha256://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            byte_length: yaml.len() as u64,
            bytes_base64: BASE64.encode(yaml.as_bytes()),
            raw_object_refs: vec!["raw-1".to_owned()],
        }
    }

    #[test]
    fn shared_reusable_dependency_is_parsed_once_per_request() {
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/root.yml",
            "on: [push]\njobs:\n  first:\n    uses: ./.github/workflows/reusable.yml\n  second:\n    uses: ./.github/workflows/reusable.yml\n",
        );
        let reusable = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            "on:\n  workflow_call: {}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: reusable.clone(),
        }];
        let mut context = WorkflowDerivationContext::default();

        let plan = derive_workflow_plan_with_context(&root, &dependencies, &mut context)
            .expect("derive callers that share one captured dependency");

        assert_eq!(plan.jobs.len(), 2);
        assert_eq!(plan.child_edges.len(), 2);
        assert_eq!(
            context.parse_count, 2,
            "root and dependency parsed once each"
        );
        assert_eq!(
            context.derivation_count, 3,
            "the shared child is still derived for both callers"
        );
        assert_eq!(
            context.unique_source_bytes,
            root.byte_length as usize + reusable.byte_length as usize
        );
    }

    #[test]
    fn aggregate_distinct_source_byte_budget_fails_closed() {
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/root.yml",
            "on: [push]\njobs:\n  call:\n    uses: ./.github/workflows/reusable.yml\n",
        );
        let reusable = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            "on:\n  workflow_call: {}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: reusable.clone(),
        }];
        let mut limits = WorkflowDerivationLimits::default();
        limits.unique_source_bytes = root.byte_length as usize + reusable.byte_length as usize - 1;
        let mut context = WorkflowDerivationContext::with_limits(limits);

        let error = derive_workflow_plan_with_context(&root, &dependencies, &mut context)
            .expect_err("two distinct sources exceed the request-wide byte cap");

        assert!(format!("{error:#}").contains("unique source bytes exceed"));
        assert_eq!(context.parse_count, 0, "fail during source registration");
    }

    #[test]
    fn aggregate_distinct_source_node_budget_fails_closed() {
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/root.yml",
            "on: [push]\njobs:\n  call:\n    uses: ./.github/workflows/reusable.yml\n",
        );
        let reusable = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            "on:\n  workflow_call: {}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: reusable.clone(),
        }];
        let root_nodes = parse_workflow_source(&root, &BASE64.decode(&root.bytes_base64).unwrap())
            .unwrap()
            .node_count;
        let reusable_nodes =
            parse_workflow_source(&reusable, &BASE64.decode(&reusable.bytes_base64).unwrap())
                .unwrap()
                .node_count;
        let mut limits = WorkflowDerivationLimits::default();
        limits.unique_source_nodes = root_nodes.max(reusable_nodes);
        let mut context = WorkflowDerivationContext::with_limits(limits);

        let error = derive_workflow_plan_with_context(&root, &dependencies, &mut context)
            .expect_err("two distinct parsed sources exceed the request-wide node cap");

        assert!(format!("{error:#}").contains("unique parsed source nodes exceed"));
        assert_eq!(
            context.parse_count, 1,
            "second source rejected at aggregate cap"
        );
    }

    #[test]
    fn repeated_dependency_derivation_obeys_request_wide_work_budget() {
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/root.yml",
            "on: [push]\njobs:\n  first:\n    uses: ./.github/workflows/reusable.yml\n  second:\n    uses: ./.github/workflows/reusable.yml\n",
        );
        let reusable = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            "on:\n  workflow_call: {}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: reusable.clone(),
        }];
        let root_nodes = parse_workflow_source(&root, &BASE64.decode(&root.bytes_base64).unwrap())
            .unwrap()
            .node_count;
        let reusable_nodes =
            parse_workflow_source(&reusable, &BASE64.decode(&reusable.bytes_base64).unwrap())
                .unwrap()
                .node_count;
        let mut limits = WorkflowDerivationLimits::default();
        limits.derivation_nodes = root_nodes + reusable_nodes;
        let mut context = WorkflowDerivationContext::with_limits(limits);

        let error = derive_workflow_plan_with_context(&root, &dependencies, &mut context)
            .expect_err("repeated caller work exceeds the request-wide derivation cap");

        assert!(format!("{error:#}").contains("derived source nodes exceed"));
        assert_eq!(context.parse_count, 2, "shared sources remain parse-cached");
        assert_eq!(
            context.derivation_count, 2,
            "third traversal exceeds the cap"
        );
    }

    #[test]
    fn derives_jobs_recursive_reusable_workflow_and_triggers() {
        let root_yaml = r#"
on:
  workflow_run: {}
  workflow_dispatch: {}
jobs:
  scan:
    uses: ./.github/workflows/reusable.yml
  direct:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
"#;
        let reusable_yaml = r#"
on:
  workflow_call: {}
jobs:
  nested:
    uses: ./.github/workflows/deep.yml
"#;
        let deep_yaml = r#"
on:
  workflow_call: {}
jobs:
  deep:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let reusable = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            reusable_yaml,
        );
        let mut deep = source("tailrocks/velnor", ".github/workflows/deep.yml", deep_yaml);
        deep.revision = "cccccccccccccccccccccccccccccccccccccccc".to_owned();
        let action = source("actions/checkout", "action.yml", "name: checkout\n");
        let dependencies = vec![
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: reusable,
            },
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: deep,
            },
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: action,
            },
        ];
        let plan = derive_workflow_plan(&root, &dependencies).unwrap();
        assert_eq!(
            plan.jobs
                .iter()
                .map(|job| job.job_id.as_str())
                .collect::<Vec<_>>(),
            vec!["scan", "direct"]
        );
        assert!(plan.events.contains("workflow_run"));
        assert!(plan.events.contains("workflow_dispatch"));
        assert!(plan.child_edges.iter().any(|edge| {
            edge.relation == "reusable_workflow" && edge.workflow_path.ends_with("reusable.yml")
        }));
        assert!(plan.child_edges.iter().any(|edge| {
            edge.relation == "reusable_workflow"
                && edge.workflow_path.ends_with("deep.yml")
                && edge.source_sha == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                && edge.parent_workflow_path.ends_with("reusable.yml")
        }));
        assert!(plan
            .child_edges
            .iter()
            .any(|edge| edge.relation == "workflow_run"));
        assert!(plan
            .child_edges
            .iter()
            .any(|edge| edge.relation == "dispatch"));

        let mut stale_capture = dependencies;
        stale_capture[0].source.source_sha = "cccccccccccccccccccccccccccccccccccccccc".to_owned();
        assert!(derive_workflow_plan(&root, &stale_capture).is_err());
    }

    #[test]
    fn omitted_child_source_and_dynamic_job_are_rejected() {
        let yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/missing.yml
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        assert!(derive_workflow_plan(&root, &[]).is_err());

        let dynamic = r#"
on: [push]
jobs:
  scan:
    runs-on: ${{ matrix.os }}
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", dynamic);
        assert!(derive_workflow_plan(&root, &[]).is_err());
    }

    #[test]
    fn local_reusable_workflow_ref_suffix_is_rejected() {
        let root_yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/reusable.yml@main
"#;
        let child_yaml = r#"
on:
  workflow_call: {}
jobs:
  nested:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            child_yaml,
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        assert!(derive_workflow_plan(&root, &dependencies).is_err());
    }

    #[test]
    fn self_repository_action_ref_cannot_satisfy_reusable_child_edge() {
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/root.yml",
            "on: [push]\njobs:\n  child:\n    uses: $/.github/workflows/reusable.yml\n",
        );
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            "on: {workflow_call: {}}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let dependencies = [G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];

        let error = derive_workflow_plan(&root, &dependencies)
            .expect_err("$/ is a step-action reference, not a reusable-workflow child edge");
        let error = format!("{error:#}");
        assert!(error.contains("lacks immutable @ref"), "{error}");

        let action_error = resolve_action_target("$/actions/example")
            .expect_err("WorkflowTemplateConverter does not accept $/ action uses syntax");
        assert!(action_error.to_string().contains("$/ action uses syntax"));
    }

    #[test]
    fn workflow_action_uses_rejects_runner_invalid_self_repository_shorthand() {
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: $/actions/example\n",
        );
        let dependency = [G0WorkflowDependency {
            kind: "action".to_owned(),
            source: source(
                "tailrocks/velnor",
                "actions/example/action.yml",
                "name: example\n",
            ),
        }];
        let error = derive_workflow_plan(&root, &dependency)
            .expect_err("a captured target cannot make $/ valid in a workflow action uses");
        assert!(format!("{error:#}").contains("$/ action uses syntax"));
    }

    #[test]
    fn reusable_source_without_workflow_call_is_rejected() {
        let root_yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/reusable.yml
"#;
        let child_yaml = r#"
on: [push]
jobs:
  nested:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            child_yaml,
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        let error = derive_workflow_plan(&root, &dependencies)
            .expect_err("a job-level uses source must be reusable");
        assert!(error.to_string().contains("must declare workflow_call"));
    }

    #[test]
    fn reusable_child_matrix_instances_are_rejected_before_representative_selection() {
        let root_yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/reusable.yml
"#;
        let child_yaml = r#"
on:
  workflow_call: {}
jobs:
  nested:
    strategy:
      matrix:
        os: [ubuntu-24.04, ubuntu-22.04]
    runs-on: ${{ matrix.os }}
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            child_yaml,
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        let error = derive_workflow_plan(&root, &dependencies)
            .expect_err("child matrix instances cannot be collapsed to the first job");
        assert!(error.to_string().contains("concrete matrix identity"));
    }

    #[test]
    fn reusable_caller_matrix_instances_are_rejected_without_edge_identity() {
        let root_yaml = "on: [push]\njobs:\n  child:\n    strategy:\n      matrix:\n        target: [linux, macos]\n    uses: ./.github/workflows/reusable.yml\n";
        let child_yaml =
            "on: {workflow_call: {}}\njobs:\n  nested:\n    runs-on: ubuntu-24.04\n    steps: []\n";
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            child_yaml,
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        let error = derive_workflow_plan(&root, &dependencies)
            .expect_err("one child edge cannot represent multiple caller assignments");
        assert!(format!("{error:#}").contains("caller assignment identity"));
    }

    #[test]
    fn reusable_child_distinct_jobs_are_not_collapsed_to_first_target() {
        let root_yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/reusable.yml
"#;
        let child_yaml = r#"
on:
  workflow_call: {}
jobs:
  linux:
    runs-on: ubuntu-24.04
    steps: []
  windows:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", root_yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            child_yaml,
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        let error = derive_workflow_plan(&root, &dependencies)
            .expect_err("distinct child jobs cannot collapse to one representative target");
        assert!(format!("{error:#}").contains("distinct child jobs"));
    }

    #[test]
    fn workspace_relative_action_steps_require_checkout_source_binding() {
        for action_path in [
            "./source/.github/actions/setup-velnor-workflow",
            "./policy-setup-action/.github/actions/setup-velnor-workflow",
            "./.github/actions/setup-velnor-workflow",
        ] {
            let yaml = format!(
                "on: [push]\njobs:\n  package:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: {action_path}\n"
            );
            let root = source("tailrocks/velnor", ".github/workflows/release.yml", &yaml);
            let error = derive_workflow_plan(&root, &[])
                .expect_err("workspace-relative action source is not content-bound");
            assert!(format!("{error:#}").contains("workspace-relative local action source"));
            let yaml = format!(
                "on: [push]\njobs:\n  package:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: {action_path}@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
            );
            let root = source("tailrocks/velnor", ".github/workflows/release.yml", &yaml);
            let error = derive_workflow_plan(&root, &[])
                .expect_err("local action paths do not accept @ref pinning");
            assert!(format!("{error:#}").contains("workspace-relative local action source"));
        }
    }

    #[test]
    fn same_repository_reusable_workflow_is_bound_to_callers_commit() {
        let yaml = r#"
on: [push]
jobs:
  child:
    uses: ./.github/workflows/reusable.yml
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            "on: {workflow_call: {}}\njobs:\n  nested:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let dependencies = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: child,
        }];
        let plan = derive_workflow_plan(&root, &dependencies)
            .expect("same-repository reusable workflow is at the caller commit");
        assert_eq!(plan.child_edges.len(), 1);
        assert_eq!(plan.child_edges[0].source_sha, root.source_sha);
    }

    #[test]
    fn reusable_caller_arguments_match_child_contract_and_edge_bindings() {
        let child_ref = "tailrocks/dependency/.github/workflows/reusable.yml@cccccccccccccccccccccccccccccccccccccccc";
        let child_sha = "cccccccccccccccccccccccccccccccccccccccc";
        let child_yaml = r#"
on:
  workflow_call:
    inputs:
      label:
        type: string
        required: true
      attempts:
        type: number
      enabled:
        type: boolean
    secrets:
      token:
        required: true
      optional:
        required: false
jobs:
  build:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let derive = |yaml: &str| {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            let mut child = source(
                "tailrocks/dependency",
                ".github/workflows/reusable.yml",
                child_yaml,
            );
            child.source_sha = child_sha.to_owned();
            child.source_url = format!(
                "https://github.com/tailrocks/dependency/blob/{child_sha}/.github/workflows/reusable.yml"
            );
            derive_workflow_plan(
                &root,
                &[G0WorkflowDependency {
                    kind: "reusable_workflow".to_owned(),
                    source: child,
                }],
            )
        };
        let caller_yaml = |arguments: &str| {
            format!("on: [push]\njobs:\n  call:\n    uses: {child_ref}\n{arguments}")
        };

        let valid_yaml = caller_yaml(
            "    with:\n      label: release\n      attempts: 3\n      enabled: false\n    secrets:\n      token: ${{ secrets.CI_TOKEN }}\n",
        );
        let plan = derive(&valid_yaml).expect("caller arguments match the child interface");
        assert_eq!(plan.jobs.len(), 1);
        assert_eq!(plan.jobs[0].job_id, "call");
        assert_eq!(plan.jobs[0].provider, "github");
        assert_eq!(plan.jobs[0].platform, "linux");
        assert_eq!(plan.jobs[0].architecture, "amd64");
        let edge = plan
            .child_edges
            .iter()
            .find(|edge| edge.relation == "reusable_workflow")
            .expect("source-derived reusable-workflow edge");
        assert_eq!(edge.workload_id, "call");
        assert_eq!(edge.root_workload_id, "call");
        assert_eq!(edge.repository, "tailrocks/dependency");
        assert_eq!(edge.workflow_path, ".github/workflows/reusable.yml");
        assert_eq!(edge.event, "workflow_call");
        assert_eq!(edge.source_sha, child_sha);
        assert_eq!(edge.parent_repository, "tailrocks/velnor");
        assert_eq!(edge.parent_workflow_path, ".github/workflows/ci.yml");
        assert_eq!(
            edge.parent_source_sha,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );

        let cases = [
            (
                "missing required input",
                caller_yaml(
                    "    with:\n      attempts: 3\n      enabled: false\n    secrets:\n      token: ${{ secrets.CI_TOKEN }}\n",
                ),
                "omits required called-workflow input label",
            ),
            (
                "unknown input",
                caller_yaml(
                    "    with:\n      label: release\n      extra: value\n    secrets:\n      token: ${{ secrets.CI_TOKEN }}\n",
                ),
                "input extra is not declared",
            ),
            (
                "input type mismatch",
                caller_yaml(
                    "    with:\n      label: true\n    secrets:\n      token: ${{ secrets.CI_TOKEN }}\n",
                ),
                "input label has type boolean",
            ),
            (
                "unknown secret",
                caller_yaml(
                    "    with:\n      label: release\n    secrets:\n      token: ${{ secrets.CI_TOKEN }}\n      extra: ${{ secrets.EXTRA }}\n",
                ),
                "secret extra is not declared",
            ),
            (
                "missing required secret",
                caller_yaml("    with:\n      label: release\n"),
                "omits required called-workflow secret token",
            ),
            (
                "unverified inherited required secret",
                caller_yaml("    with:\n      label: release\n    secrets: inherit\n"),
                "cannot prove required called-workflow secret token is available",
            ),
            (
                "expression input type",
                caller_yaml(
                    "    with:\n      label: ${{ github.ref }}\n    secrets:\n      token: ${{ secrets.CI_TOKEN }}\n",
                ),
                "input label type cannot be verified from an expression",
            ),
        ];
        for (name, yaml, expected_error) in cases {
            let error = derive(&yaml).expect_err(name);
            assert!(
                format!("{error:#}").contains(expected_error),
                "{name}: {error:#}"
            );
        }
    }

    #[test]
    fn skipped_reusable_caller_still_validates_child_graph_and_contract() {
        let child_ref = "tailrocks/dependency/.github/workflows/reusable.yml@cccccccccccccccccccccccccccccccccccccccc";
        let root_yaml =
            format!("on: [push]\njobs:\n  call:\n    if: false\n    uses: {child_ref}\n");
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &root_yaml);

        let mut required_input = source(
            "tailrocks/dependency",
            ".github/workflows/reusable.yml",
            "on:\n  workflow_call:\n    inputs:\n      target:\n        type: string\n        required: true\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        required_input.source_sha = "cccccccccccccccccccccccccccccccccccccccc".to_owned();
        let error = derive_workflow_plan(
            &root,
            &[G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: required_input,
            }],
        )
        .expect_err("a skipped caller still validates the child interface");
        assert!(format!("{error:#}").contains("omits required called-workflow input target"));

        let mut nested_missing = source(
            "tailrocks/dependency",
            ".github/workflows/reusable.yml",
            "on: {workflow_call: {}}\njobs:\n  nested:\n    uses: tailrocks/dependency/.github/workflows/deep.yml@dddddddddddddddddddddddddddddddddddddddd\n",
        );
        nested_missing.source_sha = "cccccccccccccccccccccccccccccccccccccccc".to_owned();
        let error = derive_workflow_plan(
            &root,
            &[G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: nested_missing,
            }],
        )
        .expect_err("a skipped caller still recursively validates referenced workflows");
        assert!(format!("{error:#}").contains("uncaptured immutable source"));

        let mut valid_child = source(
            "tailrocks/dependency",
            ".github/workflows/reusable.yml",
            "on: {workflow_call: {}}\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        valid_child.source_sha = "cccccccccccccccccccccccccccccccccccccccc".to_owned();
        let plan = derive_workflow_plan(
            &root,
            &[G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: valid_child,
            }],
        )
        .expect("valid skipped caller and child source");
        assert!(plan.jobs.is_empty());
        assert!(plan.child_edges.is_empty());
    }

    #[test]
    fn false_conditions_do_not_hide_invalid_static_uses() {
        let cases = [
            (
                "reusable-workflow uses",
                "on: [push]\njobs:\n  call:\n    if: false\n    uses: ${{ inputs.workflow }}\n",
            ),
            (
                "action-step uses",
                "on: [push]\njobs:\n  scan:\n    if: false\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: ${{ inputs.action }}\n",
            ),
            (
                "missing reusable source",
                "on: [push]\njobs:\n  call:\n    if: false\n    uses: ./.github/workflows/missing.yml\n",
            ),
        ];
        for (name, yaml) in cases {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_err(),
                "{name} must be validated before an `if: false` skip"
            );
        }
    }

    #[test]
    fn malformed_job_and_step_shapes_fail_before_condition_skips() {
        let cases = [
            (
                "job uses type",
                "on: [push]\njobs:\n  scan:\n    if: false\n    uses: {}\n    runs-on: ubuntu-24.04\n",
            ),
            (
                "mixed reusable job",
                "on: [push]\njobs:\n  scan:\n    uses: tailrocks/repo/.github/workflows/child.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    runs-on: ubuntu-24.04\n",
            ),
            (
                "steps sequence type",
                "on: [push]\njobs:\n  scan:\n    if: false\n    runs-on: ubuntu-24.04\n    steps: {}\n",
            ),
            (
                "step uses type",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: {}\n",
            ),
            (
                "step run type",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: 17\n",
            ),
            (
                "both step forms",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n        run: echo invalid\n",
            ),
            (
                "missing step form",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - name: invalid\n",
            ),
        ];

        for (name, yaml) in cases {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_err(),
                "malformed {name} must fail closed"
            );
        }
    }

    #[test]
    fn unknown_workflow_job_strategy_and_step_fields_fail_closed() {
        let cases = [
            (
                "workflow",
                "on: [push]\nunmodeled-root: true\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "workflow job scan",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    unmodeled-job: true\n    steps: []\n",
            ),
            (
                "workflow job scan strategy",
                "on: [push]\njobs:\n  scan:\n    strategy:\n      unmodeled-strategy: true\n      matrix:\n        os: [ubuntu-24.04]\n    runs-on: ${{ matrix.os }}\n    steps: []\n",
            ),
            (
                "workflow job scan step 0",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n        unmodeled-step: true\n",
            ),
            (
                "workflow job reusable",
                "on: [push]\njobs:\n  reusable:\n    uses: tailrocks/child/.github/workflows/ci.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n    unmodeled-job: true\n",
            ),
        ];

        for (subject, yaml) in cases {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            let error = derive_workflow_plan(&root, &[])
                .expect_err("unmodeled workflow fields must fail closed");
            let error = format!("{error:#}");
            assert!(
                error.contains(subject) && error.contains("unsupported field"),
                "unexpected error for {subject}: {error}"
            );
        }
    }

    #[test]
    fn supported_metadata_values_are_type_checked_without_changing_the_plan() {
        let minimal = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n",
        );
        let with_runner_metadata = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            r#"
name: CI
description: Build and test
run-name: CI run
env:
  PLAN_LABEL: test
on: [push]
jobs:
  scan:
    name: Scan
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    cancel-timeout-minutes: 1
    env:
      PLAN_LABEL: test
    steps:
      - id: test
        name: Test
        timeout-minutes: 5
        shell: bash
        working-directory: .
        env:
          PLAN_LABEL: test
        run: echo ok
"#,
        );

        assert_eq!(
            derive_workflow_plan(&minimal, &[]).unwrap(),
            derive_workflow_plan(&with_runner_metadata, &[]).unwrap(),
            "supported metadata is type-checked but does not add plan authority"
        );
    }

    #[test]
    fn environment_values_use_runner_scalar_coercion() {
        let yaml = r#"
on: [push]
env:
  BOOLEAN: true
  NUMBER: 1.0
  NULL_VALUE: null
jobs:
  scan:
    env:
      JOB_BOOLEAN: false
      JOB_NUMBER: 1.25
    runs-on: ubuntu-24.04
    steps:
      - env:
          STEP_BOOLEAN: true
          STEP_NUMBER: 2.5
          STEP_NULL: null
        run: echo ok
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        derive_workflow_plan(&root, &[])
            .expect("Runner converts scalar env values to string tokens");
    }

    #[test]
    fn environment_expression_contexts_follow_runner_scope() {
        assert!(validate_expression_contexts(
            "${{ github.workflow }}",
            WORKFLOW_ENV_CONTEXTS,
            "workflow env"
        )
        .is_ok());
        assert!(validate_expression_contexts(
            "${{ format('{0}', inputs.release) }}",
            WORKFLOW_ENV_CONTEXTS,
            "workflow env"
        )
        .is_ok());
        assert!(
            validate_expression_contexts("${{ matrix.os }}", JOB_ENV_CONTEXTS, "job env").is_ok()
        );
        assert!(validate_expression_contexts(
            "${{ hashFiles('**/Cargo.toml') }}",
            STEP_ENV_CONTEXTS,
            "step env"
        )
        .is_ok());
        for expression in [
            "${{ matrix.os }}",
            "${{ runner.os }}",
            "${{ hashFiles('**/Cargo.toml') }}",
        ] {
            assert!(
                validate_expression_contexts(expression, WORKFLOW_ENV_CONTEXTS, "workflow env")
                    .is_err(),
                "workflow env cannot use this context: {expression}"
            );
        }

        let yaml = r#"
on: [push]
env:
  WORKFLOW: "${{ github.workflow }}"
jobs:
  scan:
    env:
      MATRIX: "${{ matrix.os }}"
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        assert!(derive_workflow_plan(&root, &[]).is_ok());
    }

    #[test]
    fn runner_expression_grammar_and_function_arities_are_checked() {
        for expression in [
            "${{ contains('candidate', inputs.ref) }}",
            "${{ !inputs.skip && startsWith(github.ref, 'refs/') }}",
            "${{ format('{0}', vars.VERSION) == inputs.version }}",
            "${{ format('{0}', 'it''s') }}",
            "${{ case('release', 'release', 'stable') }}",
            "${{ hashFiles('**/Cargo.toml') }}",
        ] {
            validate_expression_contexts(expression, STEP_ENV_CONTEXTS, "step env").unwrap_or_else(
                |error| panic!("Runner accepts this expression: {expression}: {error:#}"),
            );
        }

        for expression in [
            "${{ contains() }}",
            "${{ contains('one') }}",
            "${{ contains('one', 'two', 'three') }}",
            "${{ format() }}",
            "${{ inputs.version && }}",
            "${{ inputs.version ?? 'default' }}",
            "${{ format('{0}', ) }}",
            "${{ (inputs.version }}",
            "${{ hashFiles() }}",
            "${{ inputs.version extra }}",
        ] {
            assert!(
                validate_expression_contexts(expression, STEP_ENV_CONTEXTS, "step env").is_err(),
                "malformed Runner expression must fail closed: {expression}"
            );
        }
    }

    #[test]
    fn runner_job_steps_are_optional_but_step_ids_match_id_builder() {
        let no_steps = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n",
        );
        let plan = derive_workflow_plan(&no_steps, &[]).expect("Runner job with omitted steps");
        assert_eq!(plan.jobs.len(), 1);
        assert!(!plan.has_action_steps);

        for id in ["_step", "step-1", "Step_2"] {
            let yaml = format!(
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: {id}\n        run: echo ok\n"
            );
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_ok(),
                "valid step ID: {id}"
            );
        }

        let invalid_ids = [
            "1step".to_owned(),
            "step.name".to_owned(),
            "__reserved".to_owned(),
            "a".repeat(100),
        ];
        for id in invalid_ids {
            let yaml = format!(
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: {id}\n        run: echo ok\n"
            );
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_err(),
                "invalid step ID: {id}"
            );
        }

        let duplicate = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: Check\n        run: echo first\n      - id: check\n        run: echo second\n",
        );
        let error = derive_workflow_plan(&duplicate, &[])
            .expect_err("Runner IdBuilder forbids duplicate step IDs ignoring case");
        assert!(format!("{error:#}").contains("duplicate step ID"));
    }

    #[test]
    fn unmodeled_fields_and_malformed_supported_values_fail_closed() {
        let cases = [
            (
                "root permissions",
                "on: [push]\npermissions: []\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "root defaults",
                "on: [push]\ndefaults: {unexpected: true}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "root concurrency",
                "on: [push]\nconcurrency: []\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "workflow env value",
                "on: [push]\nenv: {MODE: [fast]}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "job permissions",
                "on: [push]\njobs:\n  scan:\n    permissions: []\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "job outputs",
                "on: [push]\njobs:\n  scan:\n    outputs: []\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "strategy fail-fast",
                "on: [push]\njobs:\n  scan:\n    strategy:\n      fail-fast: []\n      matrix: {target: [linux]}\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "strategy max-parallel",
                "on: [push]\njobs:\n  scan:\n    strategy:\n      max-parallel: false\n      matrix: {target: [linux]}\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "job env value",
                "on: [push]\njobs:\n  scan:\n    env: {MODE: [fast]}\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "step name type",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - name: []\n        run: echo ok\n",
            ),
            (
                "step id type",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: 7\n        run: echo ok\n",
            ),
            (
                "step env type",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - env: []\n        run: echo ok\n",
            ),
            (
                "action with type",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n        with: []\n",
            ),
        ];

        for (name, yaml) in cases {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_err(),
                "{name} must fail closed"
            );
        }
    }

    #[test]
    fn exact_duplicate_yaml_mapping_keys_fail_closed_at_all_levels() {
        let fixtures = [
            (
                "workflow root",
                "on: [push]\non: [pull_request]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "jobs map",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n  scan:\n    runs-on: macos-26\n    steps: []\n",
            ),
            (
                "job map",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    runs-on: macos-26\n    steps: []\n",
            ),
            (
                "step map",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo first\n        run: echo second\n",
            ),
        ];
        for (subject, yaml) in fixtures {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            let error = derive_workflow_plan(&root, &[])
                .expect_err("exact duplicate YAML keys must not choose a last value");
            assert!(
                format!("{error:#}").contains("duplicate"),
                "{subject}: {error:#}"
            );
        }
    }

    #[test]
    fn workflow_merge_keys_are_rejected_before_allowlist_projection() {
        let yaml = "on: [push]\njobs:\n  scan:\n    <<: {runs-on: ubuntu-24.04, steps: []}\n";
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        let error = derive_workflow_plan(&root, &[])
            .expect_err("workflow merge keys must not expand fields into the allowlisted map");
        assert!(format!("{error:#}").contains("merge key"));
    }

    #[test]
    fn workflow_source_must_be_one_unanchored_yaml_document() {
        let multiple_documents = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n---\non: [pull_request]\njobs:\n  other:\n    runs-on: macos-26\n    steps: []\n",
        );
        let error = derive_workflow_plan(&multiple_documents, &[])
            .expect_err("Runner ValidateEnd rejects the second document");
        assert!(format!("{error:#}").contains("document"));

        for yaml in [
            "on: [push]\nname: &workflow_name CI\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            "on: [push]\nenv:\n  FIRST: &value value\n  SECOND: *value\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        ] {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_err(),
                "Runner's default parser rejects YAML anchors and aliases"
            );
        }
    }

    #[test]
    fn runner_mapping_keys_use_scalar_to_string_coercion() {
        let parsed: RawWorkflowYamlValue =
            serde_yaml::from_str_with_config("1.0: float\n", &workflow_parser_config())
                .expect("number mapping key");
        let value = parsed.into_value().expect("Runner key conversion");
        let mapping = value.as_mapping().expect("root mapping");
        assert!(mapping.contains_key("1"), "Runner G15 renders 1.0 as 1");

        let parsed: RawWorkflowYamlValue =
            serde_yaml::from_str_with_config("1: integer\n1.0: float\n", &workflow_parser_config())
                .expect("typed-distinct mapping keys");
        let error = parsed
            .into_value()
            .expect_err("keys that Runner stringifies identically must collide");
        assert!(format!("{error:#}").contains("collide after Runner scalar-to-string"));
    }

    #[test]
    fn source_paths_preserve_identity_and_reject_only_unsafe_segments() {
        assert_eq!(normalize_path(" spaced/path ").unwrap(), " spaced/path ");
        assert_eq!(normalize_path("file..yml").unwrap(), "file..yml");
        assert_ne!(
            normalize_path("action.yml").unwrap(),
            normalize_path(" action.yml ").unwrap()
        );
        for path in [
            "",
            "../action.yml",
            "nested/../action.yml",
            "./action.yml",
            "nested//action.yml",
            "/action.yml",
            "nested\\action.yml",
        ] {
            assert!(
                normalize_path(path).is_err(),
                "unsafe source path: {path:?}"
            );
        }

        let root = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let dependencies = [
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: source("tailrocks/dependency", "action.yml", "name: exact\n"),
            },
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: source("tailrocks/dependency", " action.yml ", "name: spaced\n"),
            },
        ];
        derive_workflow_plan(&root, &dependencies)
            .expect("distinct exact source paths do not collide after trimming");
    }

    #[test]
    fn case_insensitive_yaml_mapping_collisions_fail_closed_at_all_levels() {
        let fixtures = [
            (
                "workflow root",
                "on: [push]\nON: [pull_request]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            ),
            (
                "jobs map",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n  SCAN:\n    runs-on: macos-26\n    steps: []\n",
            ),
            (
                "job map",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    RUNS-ON: macos-26\n    steps: []\n",
            ),
            (
                "step map",
                "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo first\n        RUN: echo second\n",
            ),
        ];
        for (subject, yaml) in fixtures {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            let error = derive_workflow_plan(&root, &[])
                .expect_err("case-insensitive YAML key collisions must fail closed");
            assert!(
                format!("{error:#}").contains("case-insensitive comparison"),
                "{subject}: {error:#}"
            );
        }
    }

    #[test]
    fn documented_unicode_environment_case_pair_is_supported_and_collisions_fail_closed() {
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\nenv:\n  CAFÁ: value\njobs:\n  scan:\n    env:\n      IDÁ: value\n    runs-on: ubuntu-24.04\n    steps:\n      - env:\n          STEPÁ: value\n        run: echo ok\n",
        );
        derive_workflow_plan(&root, &[])
            .expect("a documented one-to-one Unicode case pair is supported for env keys");

        let collisions = [
            "on: [push]\nenv:\n  CAFÁ: one\n  cafá: two\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            "on: [push]\njobs:\n  scan:\n    env:\n      IDÁ: one\n      idá: two\n    runs-on: ubuntu-24.04\n    steps: []\n",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - env:\n          STEPÁ: one\n          stepá: two\n        run: echo ok\n",
        ];
        for yaml in collisions {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            let error = derive_workflow_plan(&root, &[])
                .expect_err("Runner OrdinalIgnoreCase collisions must not collapse silently");
            assert!(format!("{error:#}").contains("case-insensitive comparison"));
        }

        let unsupported = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\nenv:\n  CAFÉ: value\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let error = derive_workflow_plan(&unsupported, &[])
            .expect_err("the checker must not generalize outside its documented Unicode subset");
        assert!(format!("{error:#}").contains("outside the checker-supported"));
    }

    #[test]
    fn job_ids_match_the_pinned_runner_identifier_constraints() {
        for id in ["_scan", "scan-job", "scan_1"] {
            let yaml =
                format!("on: [push]\njobs:\n  {id}:\n    runs-on: ubuntu-24.04\n    steps: []\n");
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
            assert!(derive_workflow_plan(&root, &[]).is_ok(), "valid ID: {id}");
        }

        let longest = format!(
            "on: [push]\njobs:\n  {}:\n    runs-on: ubuntu-24.04\n    steps: []\n",
            "a".repeat(99)
        );
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &longest);
        assert!(derive_workflow_plan(&root, &[]).is_ok());

        let invalid_ids = [
            "1scan".to_owned(),
            "scan.name".to_owned(),
            "__reserved".to_owned(),
            "a".repeat(100),
        ];
        for id in invalid_ids {
            let yaml =
                format!("on: [push]\njobs:\n  {id}:\n    runs-on: ubuntu-24.04\n    steps: []\n");
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_err(),
                "invalid Runner job ID must fail closed: {id}"
            );
        }
    }

    #[test]
    fn unsupported_and_empty_trigger_names_fail_closed() {
        for trigger in [
            "''",
            "unknown_event",
            "[push, unknown_event]",
            "{unknown_event: {}}",
        ] {
            let yaml = format!(
                "on: {trigger}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n"
            );
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_err(),
                "unsupported trigger must fail closed: {trigger}"
            );
        }
    }

    #[test]
    fn skipped_jobs_still_require_valid_runner_steps_and_matrix_shape() {
        let invalid_matrix = "on: [push]\njobs:\n  scan:\n    if: false\n    runs-on: ubuntu-24.04\n    steps: []\n    strategy:\n      matrix:\n        os: ${{ fromJSON('[]') }}\n";
        for yaml in [invalid_matrix] {
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
            assert!(
                derive_workflow_plan(&root, &[]).is_err(),
                "a statically skipped job must still be structurally valid: {yaml}"
            );
        }

        for (count, should_pass) in [(256usize, true), (257, false)] {
            let values = (0..count)
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let yaml = format!(
                "on: [push]\njobs:\n  scan:\n    if: false\n    runs-on: ubuntu-24.04\n    steps: []\n    strategy:\n      matrix:\n        index: [{values}]\n"
            );
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
            let result = derive_workflow_plan(&root, &[]);
            assert_eq!(
                result.is_ok(),
                should_pass,
                "matrix cardinality {count} must be checked even when the job is disabled"
            );
            if !should_pass {
                assert!(format!("{:#}", result.unwrap_err()).contains("checker limit"));
            }
        }

        let long_key = "k".repeat(MAX_MATRIX_ASSIGNMENT_BYTES / 256 + 1);
        let values = (0..256)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let yaml = format!(
            "on: [push]\njobs:\n  scan:\n    if: false\n    runs-on: ubuntu-24.04\n    steps: []\n    strategy:\n      matrix:\n        {long_key}: [{values}]\n"
        );
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
        let error = derive_workflow_plan(&root, &[])
            .expect_err("expanded matrix bytes are bounded even for a skipped job");
        assert!(format!("{error:#}").contains("expanded bytes"));
    }

    #[test]
    fn dependent_job_graph_is_explicitly_blocked() {
        for needs in ["prep", "[prep]", "17"] {
            let yaml = format!(
                "on: [push]\njobs:\n  scan:\n    needs: {needs}\n    runs-on: ubuntu-24.04\n    steps: []\n"
            );
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
            let error = derive_workflow_plan(&root, &[])
                .expect_err("a job dependency graph must not be silently dropped");
            assert!(format!("{error:#}").contains("needs graph is not retained"));
        }
    }

    #[test]
    fn workflow_call_configuration_rejects_unknown_or_malformed_fields() {
        let valid: Value = serde_yaml::from_str(
            "inputs:\n  target:\n    type: string\n    required: false\n    default: linux\noutputs:\n  result:\n    value: ${{ jobs.scan.outputs.result }}\nsecrets:\n  token:\n    required: true\n  optional: null\n",
        )
        .unwrap();
        validate_workflow_call_configuration(valid.as_mapping().unwrap()).unwrap();

        for yaml in [
            "workflow_run: {}\n",
            "inputs:\n  target:\n    type: object\n",
            "inputs:\n  target:\n    type: string\n    default: null\n",
            "inputs:\n  target:\n    type: string\n    required: yes\n",
            "outputs:\n  result:\n    description: result only\n",
        ] {
            let value: Value = serde_yaml::from_str(yaml).unwrap();
            assert!(
                validate_workflow_call_configuration(value.as_mapping().unwrap()).is_err(),
                "malformed workflow_call configuration must fail closed: {yaml}"
            );
        }
    }

    #[test]
    fn workflow_call_defaults_and_outputs_use_runner_schema_contexts() {
        let valid: Value = serde_yaml::from_str(
            r#"
inputs:
  from-github:
    type: string
    default: "${{ github.ref }}"
  from-inputs:
    type: string
    default: "${{ inputs.target }}"
  from-vars:
    type: string
    default: "${{ vars.DEFAULT_TARGET }}"
outputs:
  from-github:
    value: "${{ github.workflow }}"
  from-inputs:
    value: "${{ inputs.target }}"
  from-vars:
    value: "${{ vars.RELEASE }}"
  from-jobs:
    value: "${{ jobs.build.outputs.release }}"
"#,
        )
        .unwrap();
        validate_workflow_call_configuration(valid.as_mapping().unwrap())
            .expect("Runner schema contexts for workflow_call defaults and outputs are accepted");

        let invalid_contexts = [
            (
                "input default secrets",
                "inputs:\n  target:\n    type: string\n    default: '${{ secrets.FLAG }}'\n",
            ),
            (
                "input default needs",
                "inputs:\n  target:\n    type: string\n    default: '${{ needs.build.outputs.target }}'\n",
            ),
            (
                "workflow output secrets",
                "outputs:\n  result:\n    value: '${{ secrets.FLAG }}'\n",
            ),
            (
                "workflow output needs",
                "outputs:\n  result:\n    value: '${{ needs.build.outputs.result }}'\n",
            ),
            (
                "workflow output steps",
                "outputs:\n  result:\n    value: '${{ steps.build.outputs.result }}'\n",
            ),
        ];
        for (name, yaml) in invalid_contexts {
            let value: Value = serde_yaml::from_str(yaml).unwrap();
            let error =
                validate_workflow_call_configuration(value.as_mapping().unwrap()).expect_err(name);
            assert!(
                format!("{error:#}").contains("disallowed Runner context"),
                "{name}: {error:#}"
            );
        }
    }

    #[test]
    fn reusable_workflow_secrets_use_runner_schema_contexts() {
        for expression in [
            "${{ github.ref }}",
            "${{ inputs.token }}",
            "${{ vars.RELEASE_TOKEN }}",
            "${{ needs.prepare.outputs.token }}",
            "${{ secrets.RELEASE_TOKEN }}",
            "${{ strategy.job-index }}",
            "${{ matrix.os }}",
        ] {
            let value: Value = serde_yaml::from_str(&format!("token: \"{expression}\"\n")).unwrap();
            validate_workflow_job_secrets(&value, "workflow job").unwrap_or_else(|error| {
                panic!("Runner accepts this secret context: {expression}: {error:#}")
            });
        }

        for expression in [
            "${{ runner.os }}",
            "${{ steps.build.outputs.token }}",
            "${{ job.status }}",
            "${{ env.RELEASE_TOKEN }}",
            "${{ hashFiles('**/Cargo.toml') }}",
        ] {
            let value: Value = serde_yaml::from_str(&format!("token: \"{expression}\"\n")).unwrap();
            assert!(
                validate_workflow_job_secrets(&value, "workflow job").is_err(),
                "Runner disallows this workflow_call secret context: {expression}"
            );
        }
    }

    #[test]
    fn workflow_call_defaults_match_their_declared_runner_type() {
        let valid: Value = serde_yaml::from_str(
            r#"
inputs:
  string:
    type: string
    default: text
  string-from-boolean:
    type: string
    default: true
  string-from-number:
    type: string
    default: 7
  boolean:
    type: boolean
    default: true
  number:
    type: number
    default: 3.5
  boolean-expression:
    type: boolean
    default: "${{ inputs.enabled }}"
  number-expression:
    type: number
    default: "${{ inputs.release }}"
"#,
        )
        .unwrap();
        validate_workflow_call_configuration(valid.as_mapping().unwrap())
            .expect("Runner accepts matching scalar defaults and defers expression defaults");

        for yaml in [
            "inputs:\n  enabled:\n    type: boolean\n    default: 'true'\n",
            "inputs:\n  enabled:\n    type: boolean\n    default: 1\n",
            "inputs:\n  release:\n    type: number\n    default: '3'\n",
            "inputs:\n  release:\n    type: number\n    default: false\n",
        ] {
            let value: Value = serde_yaml::from_str(yaml).unwrap();
            let error = validate_workflow_call_configuration(value.as_mapping().unwrap())
                .expect_err("typed workflow_call default must match its input type");
            assert!(
                format!("{error:#}").contains("does not match declared workflow_call input type"),
                "wrong default type must identify the declared type mismatch: {error:#}"
            );
        }
    }

    #[test]
    fn action_and_reusable_uses_require_matching_dependency_kind() {
        let action_yaml = r#"
on: [push]
jobs:
  scan:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
        let action = source("actions/checkout", "action.yml", "name: checkout\n");
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", action_yaml);
        let wrong_action_kind = vec![G0WorkflowDependency {
            kind: "scanner".to_owned(),
            source: action,
        }];
        let error = derive_workflow_plan(&root, &wrong_action_kind)
            .expect_err("scanner source must not satisfy an action uses reference");
        assert!(format!("{error:#}").contains("captured dependency kind is scanner"));

        let reusable_yaml = r#"
on: [push]
jobs:
  child:
    uses: tailrocks/velnor/.github/workflows/reusable.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
        let child = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            "on: {workflow_call: {}}\njobs:\n  nested:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            reusable_yaml,
        );
        let wrong_reusable_kind = vec![G0WorkflowDependency {
            kind: "action".to_owned(),
            source: child,
        }];
        let error = derive_workflow_plan(&root, &wrong_reusable_kind)
            .expect_err("action source must not satisfy a reusable-workflow uses reference");
        assert!(format!("{error:#}").contains("captured dependency kind is action"));
    }

    #[test]
    fn action_and_reusable_pins_bind_captured_commit_bytes() {
        let reusable_yaml = r#"
on: [push]
jobs:
  child:
    uses: tailrocks/dependency/.github/workflows/reusable.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            reusable_yaml,
        );
        let reusable = source(
            "tailrocks/dependency",
            ".github/workflows/reusable.yml",
            "on: {workflow_call: {}}\njobs:\n  nested:\n    runs-on: ubuntu-24.04\n    steps: []\n",
        );
        let reusable_dependency = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: reusable,
        }];
        let error = derive_workflow_plan(&root, &reusable_dependency)
            .expect_err("revision must not stand in for the captured commit SHA");
        assert!(format!("{error:#}").contains("pins"));

        let action_yaml = r#"
on: [push]
jobs:
  scan:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", action_yaml);
        let action = source("actions/checkout", "action.yml", "name: checkout\n");
        let action_dependency = vec![G0WorkflowDependency {
            kind: "action".to_owned(),
            source: action,
        }];
        let error = derive_workflow_plan(&root, &action_dependency)
            .expect_err("revision must not stand in for the captured commit SHA");
        assert!(format!("{error:#}").contains("pins"));
    }

    #[test]
    fn root_action_references_bind_root_metadata_file() {
        let yaml = "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        for path in ["action.yml", "action.yaml"] {
            let dependency = vec![G0WorkflowDependency {
                kind: "action".to_owned(),
                source: source("actions/checkout", path, "name: checkout\n"),
            }];
            assert!(derive_workflow_plan(&root, &dependency).is_ok());
        }

        let ambiguous = vec![
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: source("actions/checkout", "action.yml", "name: checkout\n"),
            },
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: source("actions/checkout", "action.yaml", "name: checkout\n"),
            },
        ];
        let error = derive_workflow_plan(&root, &ambiguous)
            .expect_err("root action metadata capture must be unambiguous");
        assert!(format!("{error:#}").contains("ambiguous action.yml/action.yaml"));
    }

    #[test]
    fn root_action_without_capture_fails_closed() {
        let yaml = "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        let error =
            derive_workflow_plan(&root, &[]).expect_err("root action needs captured root metadata");
        assert!(format!("{error:#}").contains("uncaptured root action metadata"));
    }

    #[test]
    fn action_subpaths_bind_metadata_under_the_selected_directory() {
        let yaml = "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/example/nested@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        assert!(
            resolve_action_target("actions/example/@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
                .is_err()
        );
        let directory_capture =
            source("actions/example", "nested", "this is not action metadata\n");
        let manifest = source("actions/example", "nested/action.yaml", "name: nested\n");
        let dependencies = [
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: directory_capture,
            },
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: manifest.clone(),
            },
        ];
        let plan = derive_workflow_plan(&root, &dependencies)
            .expect("the action source is the manifest below its selected subpath");
        assert!(plan.has_action_steps);

        let root_target = SourceKey {
            repository: "actions/example".to_owned(),
            path: "nested".to_owned(),
            root_action: false,
        };
        let captures = BTreeMap::from([
            (
                SourceKey {
                    repository: "actions/example".to_owned(),
                    path: "nested".to_owned(),
                    root_action: false,
                },
                CapturedWorkflowSource {
                    source: &dependencies[0].source,
                    kind: &dependencies[0].kind,
                },
            ),
            (
                SourceKey {
                    repository: "actions/example".to_owned(),
                    path: "nested/action.yaml".to_owned(),
                    root_action: false,
                },
                CapturedWorkflowSource {
                    source: &dependencies[1].source,
                    kind: &dependencies[1].kind,
                },
            ),
        ]);
        let selected = action_dependency(&captures, &root_target).unwrap();
        assert_eq!(selected.source.path, "nested/action.yaml");

        let missing_manifest = [G0WorkflowDependency {
            kind: "action".to_owned(),
            source: source("actions/example", "nested", "name: decoy\n"),
        }];
        let error = derive_workflow_plan(&root, &missing_manifest)
            .expect_err("a capture for the directory name cannot substitute for its manifest");
        assert!(format!("{error:#}").contains("uncaptured immutable action metadata"));
    }

    #[test]
    fn action_semantics_flag_propagates_through_reusable_workflows() {
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/root.yml",
            "on: [push]\njobs:\n  child:\n    uses: ./.github/workflows/reusable.yml\n",
        );
        let reusable = source(
            "tailrocks/velnor",
            ".github/workflows/reusable.yml",
            "on: {workflow_call: {}}\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        );
        let dependencies = [
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: reusable,
            },
            G0WorkflowDependency {
                kind: "action".to_owned(),
                source: source("actions/checkout", "action.yml", "name: checkout\n"),
            },
        ];
        let plan = derive_workflow_plan(&root, &dependencies).unwrap();
        assert!(plan.has_action_steps);
    }

    #[test]
    fn reusable_workflow_targets_are_direct_workflow_files() {
        for path in [".github/workflows/ci.yml", ".github/workflows/release.yaml"] {
            assert_eq!(reusable_workflow_path(path).unwrap(), path);
        }
        for path in [
            "ci.yml",
            ".github/workflows/nested/ci.yml",
            ".github/workflows/ci.json",
            ".github/workflows/",
        ] {
            assert!(
                reusable_workflow_path(path).is_err(),
                "invalid reusable-workflow path must fail closed: {path}"
            );
        }

        let root = source(
            "tailrocks/velnor",
            ".github/workflows/root.yml",
            "on: [push]\njobs: {scan: {runs-on: ubuntu-24.04, steps: []}}\n",
        );
        let (target, pinned_ref) = resolve_reusable_target(
            "tailrocks/dependency/.github/workflows/ci.yml@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &root,
        )
        .expect("external reusable source with exact workflow path");
        assert_eq!(target.repository, "tailrocks/dependency");
        assert_eq!(target.path, ".github/workflows/ci.yml");
        assert!(!target.root_action);
        assert_eq!(pinned_ref, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(resolve_reusable_target(
            "tailrocks/dependency/.github/workflows/nested/ci.yml@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            &root,
        )
        .is_err());
    }

    #[test]
    fn workflow_plan_tracks_action_steps_in_source_bytes() {
        let action_workflow = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        );
        let action_dependency = [G0WorkflowDependency {
            kind: "action".to_owned(),
            source: source("actions/checkout", "action.yml", "name: checkout\n"),
        }];
        assert!(
            derive_workflow_plan(&action_workflow, &action_dependency)
                .unwrap()
                .has_action_steps
        );

        let run_only_workflow = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n",
        );
        assert!(
            !derive_workflow_plan(&run_only_workflow, &[])
                .unwrap()
                .has_action_steps
        );

        let malformed = source("tailrocks/velnor", ".github/workflows/ci.yml", "jobs: []\n");
        assert!(derive_workflow_plan(&malformed, &[]).is_err());
    }

    #[test]
    fn runner_target_uses_documented_exact_labels() {
        let labels = [
            ("ubuntu-22.04", "linux", "amd64"),
            ("ubuntu-24.04", "linux", "amd64"),
            ("ubuntu-22.04-arm", "linux", "arm64"),
            ("ubuntu-24.04-arm", "linux", "arm64"),
            ("ubuntu-26.04-arm", "linux", "arm64"),
            ("windows-2025-vs2026", "windows", "amd64"),
            ("windows-11-arm", "windows", "arm64"),
            ("macos-14-large", "macos", "amd64"),
            ("macos-15-large", "macos", "amd64"),
            ("macos-26-large", "macos", "amd64"),
            ("macos-latest-large", "macos", "amd64"),
            ("macos-14-xlarge", "macos", "arm64"),
            ("macos-15-xlarge", "macos", "arm64"),
            ("macos-26-xlarge", "macos", "arm64"),
            ("macos-latest-xlarge", "macos", "arm64"),
            ("xcode-27", "macos", "arm64"),
            ("xcode-27-xlarge", "macos", "arm64"),
        ];
        for (label, expected_platform, expected_architecture) in labels {
            assert_eq!(
                runner_target(&Value::String(label.to_owned())).expect("documented hosted label"),
                (
                    "github".to_owned(),
                    expected_platform.to_owned(),
                    expected_architecture.to_owned(),
                ),
                "unexpected target for runner label {label}"
            );
        }
        for label in ["ubuntu-99.99", "ubuntu-24.04-custom", "ubuntu"] {
            assert!(
                runner_target(&Value::String(label.to_owned())).is_err(),
                "unknown hosted label {label} must fail closed"
            );
        }
        let self_hosted = Value::Sequence(vec![
            Value::String("self-hosted".to_owned()),
            Value::String("velnor".to_owned()),
            Value::String("linux".to_owned()),
            Value::String("x64".to_owned()),
        ]);
        assert_eq!(
            runner_target(&self_hosted).expect("concrete self-hosted target"),
            (
                "self-hosted".to_owned(),
                "linux".to_owned(),
                "amd64".to_owned(),
            )
        );
        for labels in [
            vec!["self-hosted", "linux", "ARM", "x64"],
            vec!["self-hosted", "linux", "ARM", "arm64"],
            vec!["self-hosted", "ubuntu-24.04", "arm64"],
            vec!["self-hosted", "linux", "amd64"],
        ] {
            let value = Value::Sequence(
                labels
                    .into_iter()
                    .map(|label| Value::String(label.to_owned()))
                    .collect(),
            );
            assert!(
                runner_target(&value).is_err(),
                "unsupported or contradictory architecture labels must fail closed"
            );
        }
        let arbitrary_labels_are_not_host_facts = Value::Sequence(
            ["self-hosted", "ubuntu-24.04", "linux", "x64"]
                .into_iter()
                .map(|label| Value::String(label.to_owned()))
                .collect(),
        );
        assert_eq!(
            runner_target(&arbitrary_labels_are_not_host_facts)
                .expect("only the documented built-in labels prove target"),
            (
                "self-hosted".to_owned(),
                "linux".to_owned(),
                "amd64".to_owned(),
            )
        );
    }

    #[test]
    fn container_jobs_fail_closed_until_image_target_is_bound() {
        let container_workflow = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    container: alpine:latest\n    steps:\n      - run: echo ok\n",
        );
        let error = derive_workflow_plan(&container_workflow, &[])
            .expect_err("container image target must not inherit host runner target");
        assert!(format!("{error:#}").contains("container execution is unsupported"));
    }

    #[test]
    fn service_container_jobs_fail_closed_until_image_target_is_bound() {
        let service_workflow = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            "on: [push]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    services:\n      db:\n        image: postgres:latest\n    steps:\n      - run: echo ok\n",
        );
        let error = derive_workflow_plan(&service_workflow, &[])
            .expect_err("service containers need independently bound image targets");
        assert!(format!("{error:#}").contains("service container execution is unsupported"));
    }

    #[test]
    fn conditional_trigger_filters_are_rejected() {
        let yaml = r#"
on:
  push:
    branches: [main]
jobs:
  scan:
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        let error = derive_workflow_plan(&root, &[])
            .expect_err("branch filters need event-aware source derivation");
        assert!(error.to_string().contains("event-aware derivation"));
    }

    #[test]
    fn continue_on_error_is_rejected_for_jobs_and_steps() {
        let job_yaml = r#"
on: [push]
jobs:
  scan:
    continue-on-error: true
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", job_yaml);
        assert!(derive_workflow_plan(&root, &[])
            .expect_err("required jobs cannot hide failures")
            .to_string()
            .contains("continue-on-error"));

        let step_yaml = r#"
on: [push]
jobs:
  scan:
    runs-on: ubuntu-24.04
    steps:
      - continue-on-error: true
        run: ./scan
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", step_yaml);
        assert!(derive_workflow_plan(&root, &[])
            .expect_err("required steps cannot hide failures")
            .to_string()
            .contains("continue-on-error"));
    }

    #[test]
    fn false_job_conditions_do_not_hide_invalid_continue_on_error() {
        let cases = [
            ("literal true", "continue-on-error: true"),
            (
                "dynamic expression",
                "continue-on-error: ${{ inputs.continue_on_error }}",
            ),
        ];
        for (name, continue_on_error) in cases {
            let yaml = format!(
                "on: [push]\njobs:\n  scan:\n    if: false\n    runs-on: ubuntu-24.04\n    steps:\n      - {continue_on_error}\n        run: ./scan\n"
            );
            let root = source("tailrocks/velnor", ".github/workflows/ci.yml", &yaml);
            let error = derive_workflow_plan(&root, &[])
                .expect_err("a skipped job must still validate continue-on-error");
            assert!(
                format!("{error:#}").contains("continue-on-error"),
                "{name} must be rejected before an `if: false` skip"
            );
        }
    }

    #[test]
    fn runtime_dependent_condition_is_rejected_but_finite_matrix_is_derived() {
        let conditional = r#"
on: [push]
jobs:
  scan:
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-24.04
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", conditional);
        assert!(derive_workflow_plan(&root, &[]).is_err());

        let matrix = r#"
on: [push]
jobs:
  scan:
    strategy:
      matrix:
        os: [linux, windows]
        arch: [x64, arm64]
    runs-on: [self-hosted, "${{ matrix.os }}", "${{ matrix.arch }}"]
    steps: []
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", matrix);
        let plan = derive_workflow_plan(&root, &[]).expect("finite matrix");
        assert_eq!(plan.jobs.len(), 4);
        assert!(plan.jobs.iter().all(|job| job.provider == "self-hosted"));
        assert!(plan.jobs.iter().any(|job| {
            job.matrix.get("os") == Some(&"linux".to_owned())
                && job.matrix.get("arch") == Some(&"arm64".to_owned())
        }));

        let expression_matrix = r#"
on: [push]
jobs:
  scan:
    strategy:
      matrix:
        os: [ubuntu-24.04]
    runs-on: ${{ matrix.missing }}
    steps: []
"#;
        let root = source(
            "tailrocks/velnor",
            ".github/workflows/ci.yml",
            expression_matrix,
        );
        assert!(derive_workflow_plan(&root, &[]).is_err());
    }

    #[test]
    fn literal_conditions_control_source_obligations() {
        let yaml = r#"
on: [push]
jobs:
  omitted:
    if: false
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
  kept:
    if: true
    runs-on: ubuntu-24.04
    steps:
      - if: false
        uses: actions/checkout@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
"#;
        let root = source("tailrocks/velnor", ".github/workflows/ci.yml", yaml);
        let action = source("actions/checkout", "action.yml", "name: checkout\n");
        let dependencies = [G0WorkflowDependency {
            kind: "action".to_owned(),
            source: action,
        }];
        let plan = derive_workflow_plan(&root, &dependencies).expect("literal conditions");
        assert_eq!(
            plan.jobs
                .iter()
                .map(|job| job.job_id.as_str())
                .collect::<Vec<_>>(),
            ["kept"]
        );
        assert!(!plan.has_action_steps);
    }
}

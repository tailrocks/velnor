//! Shared parser/model foundation for GitHub Actions `action.yml` metadata.
//!
//! The wire contract follows the upstream runner schema and manifest loader:
//! [`action_yaml.json`](https://github.com/actions/runner/blob/main/src/Runner.Worker/action_yaml.json),
//! [`ActionManifestManager`](https://github.com/actions/runner/blob/main/src/Runner.Worker/ActionManifestManager.cs),
//! [`YamlObjectReader`](https://github.com/actions/runner/blob/main/src/Sdk/WorkflowParser/Conversion/YamlObjectReader.cs),
//! and [`TemplateReader`](https://github.com/actions/runner/blob/main/src/Sdk/DTObjectTemplating/ObjectTemplating/TemplateReader.cs).
//!
//! This crate is intentionally not wired into `velnor-runner` or
//! `velnor-workflow` yet. That migration is a separate change: both callers
//! currently own behavior beyond this contract (path admission, expression
//! evaluation, execution planning, and Velnor capability policy). Keeping the
//! boundary standalone makes those differences reviewable instead of silently
//! changing runtime behavior in the foundation PR.

#![forbid(unsafe_code)]

use noyalib::policy::DenyAnchors;
use noyalib::{
    from_str_with_config, DuplicateKeyPolicy, MergeKeyPolicy, Number, ParserConfig, Value,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
use thiserror::Error;

/// Result returned by [`parse`].
pub type Result<T> = std::result::Result<T, ParseError>;

/// A parsed Actions action metadata document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionMetadata {
    /// Optional human-readable action name.
    pub name: Option<String>,
    /// Optional human-readable action description.
    pub description: Option<String>,
    /// Input definitions keyed by their authored names.
    pub inputs: BTreeMap<String, ActionInput>,
    /// Output definitions keyed by their authored names.
    pub outputs: BTreeMap<String, ActionOutput>,
    /// The action execution contract.
    pub runs: ActionRuns,
}

/// Input metadata interpreted by the upstream runner.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActionInput {
    /// The default value after runner-compatible scalar-to-string coercion.
    pub default: Option<String>,
    /// The optional deprecation message recognized by the runner.
    pub deprecation_message: Option<String>,
}

/// Output metadata from the strict `output-definition` schema.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActionOutput {
    /// Optional output description.
    pub description: Option<String>,
    /// Optional expression-backed output value.
    pub value: Option<String>,
}

/// Runtime-specific `runs` definitions from the runner schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRuns {
    /// A Docker/container action.
    Docker(DockerRuns),
    /// A JavaScript action.
    Node(NodeRuns),
    /// A composite action.
    Composite(CompositeRuns),
    /// A runner plugin action.
    Plugin(PluginRuns),
}

/// Docker action runtime fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerRuns {
    /// The Docker image or Dockerfile reference.
    pub image: String,
    /// Optional container entrypoint.
    pub entrypoint: Option<String>,
    /// Container arguments after scalar coercion.
    pub args: Vec<String>,
    /// Container environment after scalar coercion.
    pub env: BTreeMap<String, String>,
    /// Optional pre-stage entrypoint.
    pub pre_entrypoint: Option<String>,
    /// Optional pre-stage condition expression.
    pub pre_if: Option<String>,
    /// Optional post-stage entrypoint.
    pub post_entrypoint: Option<String>,
    /// Optional post-stage condition expression.
    pub post_if: Option<String>,
}

/// JavaScript action runtime fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRuns {
    /// One of the node runtimes supported by the upstream manifest loader.
    pub using: String,
    /// Main JavaScript file.
    pub main: String,
    /// Optional pre hook.
    pub pre: Option<String>,
    /// Optional pre condition expression.
    pub pre_if: Option<String>,
    /// Optional post hook.
    pub post: Option<String>,
    /// Optional post condition expression.
    pub post_if: Option<String>,
}

/// Composite action runtime fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositeRuns {
    /// Composite steps in authored order.
    pub steps: Vec<CompositeStep>,
}

/// Plugin action runtime fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRuns {
    /// Plugin identifier.
    pub plugin: String,
}

/// A composite run step or action step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositeStep {
    /// A shell command step. The upstream schema requires `shell` here.
    Run(RunStep),
    /// A nested action invocation.
    Uses(UsesStep),
}

/// A composite `run` step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStep {
    /// Optional step name.
    pub name: Option<String>,
    /// Optional non-empty step identifier.
    pub id: Option<String>,
    /// Optional condition expression.
    pub if_condition: Option<String>,
    /// Command text.
    pub run: String,
    /// Environment after scalar coercion.
    pub env: BTreeMap<String, String>,
    /// Optional boolean literal or expression.
    pub continue_on_error: Option<BooleanValue>,
    /// Optional working directory expression.
    pub working_directory: Option<String>,
    /// Required shell expression.
    pub shell: String,
}

/// A composite `uses` step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsesStep {
    /// Optional step name.
    pub name: Option<String>,
    /// Optional non-empty step identifier.
    pub id: Option<String>,
    /// Optional condition expression.
    pub if_condition: Option<String>,
    /// Action reference.
    pub uses: String,
    /// Optional boolean literal or expression.
    pub continue_on_error: Option<BooleanValue>,
    /// Input values after scalar coercion.
    pub with: BTreeMap<String, String>,
    /// Environment after scalar coercion.
    pub env: BTreeMap<String, String>,
}

/// A literal boolean or an expression which the runner evaluates later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BooleanValue {
    /// A YAML boolean literal.
    Literal(bool),
    /// An expression-containing scalar preserved for evaluation by a caller.
    Expression(String),
}

/// Failure while parsing or validating action metadata.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The YAML parser rejected the document.
    #[error("invalid action metadata YAML: {0}")]
    Yaml(String),
    /// A mapping was required at a named location.
    #[error("{context} must be a mapping")]
    ExpectedMapping { context: String },
    /// A sequence was required at a named location.
    #[error("{context} must be a sequence")]
    ExpectedSequence { context: String },
    /// A scalar was required at a named location.
    #[error("{context} must be a scalar")]
    ExpectedScalar { context: String },
    /// A required property was absent.
    #[error("{context} is required")]
    Missing { context: String },
    /// A string was required to contain at least one character.
    #[error("{context} must not be empty")]
    Empty { context: String },
    /// An unknown field appeared in a strict mapping.
    #[error("unknown field {field:?} in {context}")]
    UnknownField { context: String, field: String },
    /// A key was repeated using exact or case-insensitive spelling.
    #[error("duplicate case-insensitive key {key:?} in {context}")]
    DuplicateKey { context: String, key: String },
    /// A field was used in an incompatible runtime shape.
    #[error("{context}: {message}")]
    Invalid { context: String, message: String },
}

/// Parse one Actions `action.yml` document.
///
/// Parsing uses noyalib's strict YAML 1.2 configuration and duplicate-key
/// error mode. The semantic pass additionally rejects case-insensitive key
/// collisions because the upstream runner uses `StringComparer.OrdinalIgnoreCase`
/// while reading manifest mappings.
///
/// # Errors
///
/// Returns an error for invalid YAML, duplicate keys, unsupported runtime
/// shapes, missing required fields, or values outside the runner schema.
pub fn parse(contents: &str) -> Result<ActionMetadata> {
    let config = ParserConfig::strict()
        .duplicate_key_policy(DuplicateKeyPolicy::Error)
        .merge_key_policy(MergeKeyPolicy::Error)
        .with_policy(DenyAnchors)
        .max_documents(1);
    let root: Value = from_str_with_config(contents, &config)
        .map_err(|error| ParseError::Yaml(error.to_string()))?;
    let root_entries = entries(&root, "action metadata")?;

    let name = optional_string(field(&root_entries, "name"), "name")?;
    let description = optional_string(field(&root_entries, "description"), "description")?;
    let inputs = parse_inputs(field(&root_entries, "inputs"))?;
    let outputs = parse_outputs(field(&root_entries, "outputs"))?;
    let runs = field(&root_entries, "runs")
        .ok_or_else(|| ParseError::Missing {
            context: "runs".into(),
        })
        .and_then(parse_runs)?;

    Ok(ActionMetadata {
        name,
        description,
        inputs,
        outputs,
        runs,
    })
}

fn parse_inputs(value: Option<&Value>) -> Result<BTreeMap<String, ActionInput>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let input_entries = entries(value, "inputs")?;
    let mut result = BTreeMap::new();
    for (name, value) in input_entries {
        if name.is_empty() {
            return Err(ParseError::Empty {
                context: "input name".into(),
            });
        }
        let metadata = entries(value, &format!("input {name:?}"))?;
        let default = optional_string(
            field(&metadata, "default"),
            &format!("input {name:?}.default"),
        )?;
        let deprecation_message = optional_literal_string(
            field(&metadata, "deprecationMessage"),
            &format!("input {name:?}.deprecationMessage"),
        )?;
        result.insert(
            name.to_owned(),
            ActionInput {
                default,
                deprecation_message,
            },
        );
    }
    Ok(result)
}

fn parse_outputs(value: Option<&Value>) -> Result<BTreeMap<String, ActionOutput>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let output_entries = entries(value, "outputs")?;
    let mut result = BTreeMap::new();
    for (name, value) in output_entries {
        if name.is_empty() {
            return Err(ParseError::Empty {
                context: "output name".into(),
            });
        }
        let context = format!("output {name:?}");
        let metadata = strict_entries(value, &context, &["description", "value"])?;
        result.insert(
            name.to_owned(),
            ActionOutput {
                description: optional_string(
                    field(&metadata, "description"),
                    &format!("{context}.description"),
                )?,
                value: optional_string(field(&metadata, "value"), &format!("{context}.value"))?,
            },
        );
    }
    Ok(result)
}

fn parse_runs(value: &Value) -> Result<ActionRuns> {
    let all = entries(value, "runs")?;
    if let Some(plugin) = field(&all, "plugin") {
        strict_entries(value, "plugin runs", &["plugin"])?;
        if field(&all, "using").is_some() {
            return Err(ParseError::Invalid {
                context: "runs".into(),
                message: "plugin and using cannot be combined".into(),
            });
        }
        return Ok(ActionRuns::Plugin(PluginRuns {
            plugin: non_empty_string(Some(plugin), "runs.plugin")?,
        }));
    }

    let using = non_empty_string(field(&all, "using"), "runs.using")?;
    match using.to_ascii_lowercase().as_str() {
        "docker" => parse_docker_runs(value),
        "node12" | "node16" | "node20" | "node24" => parse_node_runs(value, using),
        "composite" => parse_composite_runs(value),
        other => Err(ParseError::Invalid {
            context: "runs.using".into(),
            message: format!("unsupported runtime {other:?}"),
        }),
    }
}

fn parse_docker_runs(value: &Value) -> Result<ActionRuns> {
    let entries = strict_entries(
        value,
        "docker runs",
        &[
            "using",
            "image",
            "entrypoint",
            "args",
            "env",
            "pre-entrypoint",
            "pre-if",
            "post-entrypoint",
            "post-if",
        ],
    )?;
    Ok(ActionRuns::Docker(DockerRuns {
        image: non_empty_string(field(&entries, "image"), "runs.image")?,
        entrypoint: optional_non_empty_string(field(&entries, "entrypoint"), "runs.entrypoint")?,
        args: optional_string_sequence(field(&entries, "args"), "runs.args")?,
        env: optional_string_map(field(&entries, "env"), "runs.env")?,
        pre_entrypoint: optional_non_empty_string(
            field(&entries, "pre-entrypoint"),
            "runs.pre-entrypoint",
        )?,
        pre_if: optional_non_empty_string(field(&entries, "pre-if"), "runs.pre-if")?,
        post_entrypoint: optional_non_empty_string(
            field(&entries, "post-entrypoint"),
            "runs.post-entrypoint",
        )?,
        post_if: optional_non_empty_string(field(&entries, "post-if"), "runs.post-if")?,
    }))
}

fn parse_node_runs(value: &Value, using: String) -> Result<ActionRuns> {
    let entries = strict_entries(
        value,
        "node runs",
        &["using", "main", "pre", "pre-if", "post", "post-if"],
    )?;
    Ok(ActionRuns::Node(NodeRuns {
        using,
        main: non_empty_string(field(&entries, "main"), "runs.main")?,
        pre: optional_non_empty_string(field(&entries, "pre"), "runs.pre")?,
        pre_if: optional_non_empty_string(field(&entries, "pre-if"), "runs.pre-if")?,
        post: optional_non_empty_string(field(&entries, "post"), "runs.post")?,
        post_if: optional_non_empty_string(field(&entries, "post-if"), "runs.post-if")?,
    }))
}

fn parse_composite_runs(value: &Value) -> Result<ActionRuns> {
    let entries = strict_entries(value, "composite runs", &["using", "steps"])?;
    let steps = field(&entries, "steps")
        .ok_or_else(|| ParseError::Missing {
            context: "runs.steps".into(),
        })
        .and_then(|value| parse_steps(value, "runs.steps"))?;
    Ok(ActionRuns::Composite(CompositeRuns { steps }))
}

fn parse_steps(value: &Value, context: &str) -> Result<Vec<CompositeStep>> {
    let Value::Sequence(values) = value else {
        return Err(ParseError::ExpectedSequence {
            context: context.into(),
        });
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| parse_step(value, &format!("{context}[{index}]")))
        .collect()
}

fn parse_step(value: &Value, context: &str) -> Result<CompositeStep> {
    let all = entries(value, context)?;
    let run = field(&all, "run");
    let uses = field(&all, "uses");
    match (run, uses) {
        (Some(run), None) => {
            let entries = strict_entries(
                value,
                context,
                &[
                    "name",
                    "id",
                    "if",
                    "run",
                    "env",
                    "continue-on-error",
                    "working-directory",
                    "shell",
                ],
            )?;
            Ok(CompositeStep::Run(RunStep {
                name: optional_string(field(&entries, "name"), &format!("{context}.name"))?,
                id: optional_non_empty_string(field(&entries, "id"), &format!("{context}.id"))?,
                if_condition: optional_string(field(&entries, "if"), &format!("{context}.if"))?,
                run: scalar_to_string(run, &format!("{context}.run"))?,
                env: optional_string_map(field(&entries, "env"), &format!("{context}.env"))?,
                continue_on_error: optional_boolean(
                    field(&entries, "continue-on-error"),
                    &format!("{context}.continue-on-error"),
                )?,
                working_directory: optional_string(
                    field(&entries, "working-directory"),
                    &format!("{context}.working-directory"),
                )?,
                shell: required_string(field(&entries, "shell"), &format!("{context}.shell"))?,
            }))
        }
        (None, Some(uses)) => {
            let entries = strict_entries(
                value,
                context,
                &[
                    "name",
                    "id",
                    "if",
                    "uses",
                    "continue-on-error",
                    "with",
                    "env",
                ],
            )?;
            Ok(CompositeStep::Uses(UsesStep {
                name: optional_string(field(&entries, "name"), &format!("{context}.name"))?,
                id: optional_non_empty_string(field(&entries, "id"), &format!("{context}.id"))?,
                if_condition: optional_string(field(&entries, "if"), &format!("{context}.if"))?,
                uses: non_empty_string(Some(uses), &format!("{context}.uses"))?,
                continue_on_error: optional_boolean(
                    field(&entries, "continue-on-error"),
                    &format!("{context}.continue-on-error"),
                )?,
                with: optional_string_map(field(&entries, "with"), &format!("{context}.with"))?,
                env: optional_string_map(field(&entries, "env"), &format!("{context}.env"))?,
            }))
        }
        (Some(_), Some(_)) | (None, None) => Err(ParseError::Invalid {
            context: context.into(),
            message: "a composite step must contain exactly one of run or uses".into(),
        }),
    }
}

fn entries<'a>(value: &'a Value, context: &str) -> Result<Vec<(&'a str, &'a Value)>> {
    let Value::Mapping(mapping) = value else {
        return Err(ParseError::ExpectedMapping {
            context: context.into(),
        });
    };
    let mut seen = BTreeSet::new();
    let mut result = Vec::with_capacity(mapping.len());
    for (key, value) in mapping {
        let folded = key.to_ascii_lowercase();
        if !seen.insert(folded) {
            return Err(ParseError::DuplicateKey {
                context: context.into(),
                key: key.clone(),
            });
        }
        result.push((key.as_str(), value));
    }
    Ok(result)
}

fn strict_entries<'a>(
    value: &'a Value,
    context: &str,
    allowed: &[&str],
) -> Result<Vec<(&'a str, &'a Value)>> {
    let result = entries(value, context)?;
    for (key, _) in &result {
        if !allowed
            .iter()
            .any(|allowed| key.eq_ignore_ascii_case(allowed))
        {
            return Err(ParseError::UnknownField {
                context: context.into(),
                field: (*key).to_owned(),
            });
        }
    }
    Ok(result)
}

fn field<'a>(entries: &[(&'a str, &'a Value)], name: &str) -> Option<&'a Value> {
    entries
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| *value)
}

fn optional_string(value: Option<&Value>, context: &str) -> Result<Option<String>> {
    value
        .map(|value| scalar_to_string(value, context))
        .transpose()
}

fn optional_literal_string(value: Option<&Value>, context: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::String(value) => Ok(Some(value.clone())),
        Value::Tagged(value) if value.tag().as_str().eq_ignore_ascii_case("!!str") => {
            optional_literal_string(Some(value.value()), context)
        }
        _ => Err(ParseError::Invalid {
            context: context.into(),
            message: "expected a YAML string".into(),
        }),
    }
}

fn non_empty_string(value: Option<&Value>, context: &str) -> Result<String> {
    let value = value.ok_or_else(|| ParseError::Missing {
        context: context.into(),
    })?;
    let value = scalar_to_string(value, context)?;
    if value.is_empty() {
        return Err(ParseError::Empty {
            context: context.into(),
        });
    }
    Ok(value)
}

fn required_string(value: Option<&Value>, context: &str) -> Result<String> {
    let value = value.ok_or_else(|| ParseError::Missing {
        context: context.into(),
    })?;
    scalar_to_string(value, context)
}

fn optional_non_empty_string(value: Option<&Value>, context: &str) -> Result<Option<String>> {
    value
        .map(|value| {
            let value = scalar_to_string(value, context)?;
            if value.is_empty() {
                return Err(ParseError::Empty {
                    context: context.into(),
                });
            }
            Ok(value)
        })
        .transpose()
}

fn optional_string_sequence(value: Option<&Value>, context: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Value::Sequence(values) = value else {
        return Err(ParseError::ExpectedSequence {
            context: context.into(),
        });
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| scalar_to_string(value, &format!("{context}[{index}]")))
        .collect()
}

fn optional_string_map(value: Option<&Value>, context: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let entries = entries(value, context)?;
    let mut result = BTreeMap::new();
    for (key, value) in entries {
        if key.is_empty() {
            return Err(ParseError::Empty {
                context: format!("{context} key"),
            });
        }
        result.insert(
            key.to_owned(),
            scalar_to_string(value, &format!("{context}.{key}"))?,
        );
    }
    Ok(result)
}

fn optional_boolean(value: Option<&Value>, context: &str) -> Result<Option<BooleanValue>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Bool(value) => Ok(Some(BooleanValue::Literal(*value))),
        Value::String(value) if value.contains("${{") => {
            validate_expression_shape(value, context)?;
            Ok(Some(BooleanValue::Expression(value.clone())))
        }
        _ => Err(ParseError::Invalid {
            context: context.into(),
            message: "expected a YAML boolean or an expression".into(),
        }),
    }
}

fn validate_expression_shape(value: &str, context: &str) -> Result<()> {
    let open = value.matches("${{").count();
    let close = value.matches("}}").count();
    if open == 0 || open != close {
        return Err(ParseError::Invalid {
            context: context.into(),
            message: "expression delimiters are unbalanced".into(),
        });
    }
    Ok(())
}

fn scalar_to_string(value: &Value, context: &str) -> Result<String> {
    match value {
        Value::Null => Ok(String::new()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(runner_number_string(value)),
        Value::String(value) => Ok(value.clone()),
        Value::Tagged(value) if value.tag().as_str().eq_ignore_ascii_case("!!str") => {
            scalar_to_string(value.value(), context)
        }
        Value::Tagged(value) => Err(ParseError::Invalid {
            context: context.into(),
            message: format!("unsupported YAML tag {:?}", value.tag().as_str()),
        }),
        Value::Sequence(_) | Value::Mapping(_) => Err(ParseError::ExpectedScalar {
            context: context.into(),
        }),
    }
}

fn runner_number_string(value: &Number) -> String {
    let value = value.as_f64();
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".into()
        } else {
            "Infinity".into()
        };
    }
    if value == 0.0 {
        return "0".into();
    }
    // The runner uses Double.ToString("G15", InvariantCulture). Rust's
    // precision formatter gives the same significant-digit boundary; the
    // exponent normalization below follows .NET general-format thresholds.
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let magnitude = value.abs();
    let scientific = format!("{magnitude:.14e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .map_or((scientific.as_str(), 0), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().unwrap_or(0))
        });
    let digits = mantissa.replace('.', "").trim_end_matches('0').to_owned();
    if (-4..15).contains(&exponent) {
        let decimal_index = exponent + 1;
        if decimal_index <= 0 {
            let zero_count = usize::try_from(decimal_index.unsigned_abs()).unwrap_or_default();
            return format!("{sign}0.{}{}", "0".repeat(zero_count), digits);
        }
        let decimal_index = usize::try_from(decimal_index).unwrap_or_default();
        if decimal_index >= digits.len() {
            return format!(
                "{sign}{}{}",
                digits,
                "0".repeat(decimal_index - digits.len())
            );
        }
        return format!(
            "{sign}{}.{}",
            &digits[..decimal_index],
            &digits[decimal_index..]
        );
    }
    let exponent_sign = if exponent >= 0 { '+' } else { '-' };
    let mantissa = if digits.len() == 1 {
        digits.clone()
    } else {
        format!("{}.{}", &digits[..1], &digits[1..])
    };
    format!(
        "{sign}{mantissa}E{exponent_sign}{:02}",
        exponent.unsigned_abs()
    )
}

impl fmt::Display for BooleanValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(value) => value.fmt(formatter),
            Self::Expression(value) => value.fmt(formatter),
        }
    }
}

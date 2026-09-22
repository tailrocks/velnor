//! Shared GitHub Action manifest contract.
//!
//! `actions/runner` parses one manifest shape for both downloaded execution
//! and repository scanning. Keep the wire fields, scalar coercion, strict
//! runtime fields, and composite shape here so those consumers cannot drift.

use serde::{Deserialize, Deserializer};
use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, Deserialize)]
pub struct ActionMetadata {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub runs: ActionRuns,
    #[serde(default)]
    pub inputs: BTreeMap<String, ActionInput>,
    #[serde(default)]
    pub outputs: BTreeMap<String, ActionOutput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionInput {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(
        default,
        rename = "default",
        deserialize_with = "deserialize_optional_string_scalar"
    )]
    pub default_value: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionOutput {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRuns {
    pub using: String,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub pre: Option<String>,
    #[serde(default, rename = "pre-if")]
    pub pre_if: Option<String>,
    #[serde(default)]
    pub post: Option<String>,
    #[serde(default, rename = "post-if")]
    pub post_if: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default, rename = "pre-entrypoint")]
    pub pre_entrypoint: Option<String>,
    #[serde(default, rename = "post-entrypoint")]
    pub post_entrypoint: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_strict_string_map")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub steps: Option<Vec<CompositeActionStep>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompositeActionStep {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub uses: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_map")]
    pub with: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_string_map")]
    pub env: BTreeMap<String, String>,
    #[serde(default, rename = "if")]
    pub condition: Option<String>,
    #[serde(default, rename = "working-directory")]
    pub working_directory: Option<String>,
    #[serde(
        default,
        rename = "continue-on-error",
        deserialize_with = "deserialize_optional_boolean_scalar"
    )]
    pub continue_on_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionRuntime {
    JavaScript { node: String, main: String },
    Composite,
    Docker { image: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionMetadataError(String);

impl ActionMetadataError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ActionMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ActionMetadataError {}

impl ActionMetadata {
    pub fn runtime(&self) -> Result<ActionRuntime, ActionMetadataError> {
        let using = self.runs.using.to_ascii_lowercase();
        if matches!(using.as_str(), "node12" | "node16" | "node20" | "node24") {
            let main = self.runs.main.clone().ok_or_else(|| {
                ActionMetadataError::new("JavaScript action metadata missing runs.main")
            })?;
            if main.is_empty() {
                return Err(ActionMetadataError::new(
                    "JavaScript action metadata runs.main must not be empty",
                ));
            }
            return Ok(ActionRuntime::JavaScript { node: using, main });
        }
        if using == "composite" {
            if self.runs.steps.is_none() {
                return Err(ActionMetadataError::new(
                    "composite action metadata missing runs.steps",
                ));
            }
            return Ok(ActionRuntime::Composite);
        }
        if using == "docker" {
            let image = self.runs.image.clone().ok_or_else(|| {
                ActionMetadataError::new("Docker action metadata missing runs.image")
            })?;
            if image.is_empty() {
                return Err(ActionMetadataError::new(
                    "Docker action metadata runs.image must not be empty",
                ));
            }
            return Ok(ActionRuntime::Docker { image });
        }
        Err(ActionMetadataError::new(format!(
            "unsupported action runtime '{}'",
            self.runs.using
        )))
    }
}

pub fn parse_action_metadata(contents: &str) -> Result<ActionMetadata, ActionMetadataError> {
    if !metadata_document_within_budget(contents) {
        return Err(ActionMetadataError::new(
            "action metadata nesting exceeds the admission parser budget",
        ));
    }
    let document = serde_yaml::from_str(contents)
        .map_err(|error| ActionMetadataError::new(format!("parse action metadata: {error}")))?;
    parse_action_metadata_value(document)
}

/// Deserialize a preflighted YAML document through the same typed contract as
/// downloaded action metadata. Scanners may perform source-aware YAML checks
/// first, but they must not define a second action manifest model.
pub fn parse_action_metadata_value(
    document: serde_yaml::Value,
) -> Result<ActionMetadata, ActionMetadataError> {
    let metadata = serde_yaml::from_value(&document)
        .map_err(|error| ActionMetadataError::new(format!("parse action metadata: {error}")))?;
    validate_action_metadata(&metadata)?;
    Ok(metadata)
}

fn validate_action_metadata(metadata: &ActionMetadata) -> Result<(), ActionMetadataError> {
    let using = metadata.runs.using.to_ascii_lowercase();
    if using.is_empty() {
        return Err(ActionMetadataError::new(
            "action metadata runs.using must not be empty",
        ));
    }
    for (field, value) in [
        ("runs.main", metadata.runs.main.as_deref()),
        ("runs.pre", metadata.runs.pre.as_deref()),
        ("runs.pre-if", metadata.runs.pre_if.as_deref()),
        ("runs.post", metadata.runs.post.as_deref()),
        ("runs.post-if", metadata.runs.post_if.as_deref()),
        ("runs.image", metadata.runs.image.as_deref()),
        ("runs.entrypoint", metadata.runs.entrypoint.as_deref()),
        (
            "runs.pre-entrypoint",
            metadata.runs.pre_entrypoint.as_deref(),
        ),
        (
            "runs.post-entrypoint",
            metadata.runs.post_entrypoint.as_deref(),
        ),
    ] {
        if value.is_some_and(str::is_empty) {
            return Err(ActionMetadataError::new(format!(
                "action metadata {field} must not be empty"
            )));
        }
    }
    match using.as_str() {
        "composite" => {
            let steps = metadata.runs.steps.as_deref().ok_or_else(|| {
                ActionMetadataError::new("composite action metadata missing runs.steps")
            })?;
            if metadata.runs.args.is_some()
                || !metadata.runs.env.is_empty()
                || metadata.runs.main.is_some()
                || metadata.runs.image.is_some()
                || metadata.runs.entrypoint.is_some()
                || metadata.runs.pre_entrypoint.is_some()
                || metadata.runs.post_entrypoint.is_some()
            {
                return Err(ActionMetadataError::new(
                    "composite action metadata contains fields for another runtime",
                ));
            }
            for step in steps {
                let has_run = step.run.is_some();
                let has_uses = step.uses.is_some();
                if has_run == has_uses {
                    return Err(ActionMetadataError::new(
                        "composite action step must declare exactly one of runs.steps.run or runs.steps.uses",
                    ));
                }
                if step.uses.as_deref().is_some_and(str::is_empty) {
                    return Err(ActionMetadataError::new(
                        "composite action step uses must not be empty",
                    ));
                }
                if has_run && step.shell.is_none() {
                    return Err(ActionMetadataError::new(
                        "composite action run step must declare shell",
                    ));
                }
                if has_run && !step.with.is_empty() {
                    return Err(ActionMetadataError::new(
                        "composite action run step has with fields",
                    ));
                }
                if has_uses
                    && (step.run.is_some()
                        || step.shell.is_some()
                        || step.working_directory.is_some())
                {
                    return Err(ActionMetadataError::new(
                        "composite action uses step has run/shell/working-directory fields",
                    ));
                }
            }
        }
        "docker" => {
            if metadata.runs.steps.is_some()
                || metadata.runs.main.is_some()
                || metadata.runs.pre.is_some()
                || metadata.runs.post.is_some()
            {
                return Err(ActionMetadataError::new(
                    "Docker action metadata contains fields for another runtime",
                ));
            }
            if metadata.runs.image.is_none() {
                return Err(ActionMetadataError::new(
                    "Docker action metadata missing runs.image",
                ));
            }
        }
        "node12" | "node16" | "node20" | "node24" => {
            if metadata.runs.steps.is_some()
                || metadata.runs.image.is_some()
                || metadata.runs.entrypoint.is_some()
                || metadata.runs.pre_entrypoint.is_some()
                || metadata.runs.post_entrypoint.is_some()
                || metadata.runs.args.is_some()
                || !metadata.runs.env.is_empty()
            {
                return Err(ActionMetadataError::new(
                    "JavaScript action metadata contains fields for another runtime",
                ));
            }
        }
        _ => {
            return Err(ActionMetadataError::new(format!(
                "unsupported action runtime '{}'",
                metadata.runs.using
            )))
        }
    }
    Ok(())
}

const MAX_METADATA_PARSE_NESTING: usize = 64;

fn metadata_document_within_budget(contents: &str) -> bool {
    let mut flow_depth = 0usize;
    let mut block_scalar_indent = None;
    for line in contents.lines() {
        let indentation = line
            .bytes()
            .take_while(|byte| *byte == b' ' || *byte == b'\t')
            .count();
        if indentation > MAX_METADATA_PARSE_NESTING * 2 {
            return false;
        }
        if block_scalar_indent.is_some_and(|base| indentation > base) {
            continue;
        }
        block_scalar_indent = None;
        let mut single_quoted = false;
        let mut double_quoted = false;
        let mut escaped = false;
        for byte in line.bytes() {
            if double_quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    double_quoted = false;
                }
                continue;
            }
            if single_quoted {
                if byte == b'\'' {
                    single_quoted = false;
                }
                continue;
            }
            match byte {
                b'#' => break,
                b'\'' => single_quoted = true,
                b'"' => double_quoted = true,
                b'[' | b'{' => {
                    flow_depth = flow_depth.saturating_add(1);
                    if flow_depth > MAX_METADATA_PARSE_NESTING {
                        return false;
                    }
                }
                b']' | b'}' => flow_depth = flow_depth.saturating_sub(1),
                _ => {}
            }
        }
        let trimmed = line.trim_end();
        if trimmed.ends_with('|') || trimmed.ends_with('>') {
            block_scalar_indent = Some(indentation);
        }
    }
    true
}

fn deserialize_optional_string_scalar<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.map(scalar_string))
}

fn deserialize_optional_boolean_scalar<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None => Ok(None),
        Some(serde_json::Value::String(value)) => Ok(Some(value)),
        Some(serde_json::Value::Bool(value)) => Ok(Some(value.to_string())),
        Some(_) => Err(serde::de::Error::custom(
            "action metadata continue-on-error must be a boolean or string",
        )),
    }
}

fn deserialize_string_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(object) = Option::<BTreeMap<String, serde_json::Value>>::deserialize(deserializer)?
    else {
        return Ok(BTreeMap::new());
    };
    Ok(object
        .into_iter()
        .map(|(name, value)| (name, scalar_string(value)))
        .collect())
}

fn deserialize_strict_string_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(object) = Option::<BTreeMap<String, serde_json::Value>>::deserialize(deserializer)?
    else {
        return Ok(BTreeMap::new());
    };
    object
        .into_iter()
        .map(|(name, value)| match value {
            serde_json::Value::String(value) => Ok((name, value)),
            _ => Err(serde::de::Error::custom(
                "action metadata string map values must be strings",
            )),
        })
        .collect()
}

fn scalar_string(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(value) => value,
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Object(object) => object
            .get("value")
            .or_else(|| object.get("Value"))
            .or_else(|| object.get("lit"))
            .or_else(|| object.get("Lit"))
            .cloned()
            .map(scalar_string)
            .unwrap_or_default(),
        serde_json::Value::Array(_) => String::new(),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "contract tests use explicit assertions"
)]
mod tests {
    use super::{parse_action_metadata, ActionRuntime};

    #[test]
    fn shared_contract_coerces_manifest_scalar_strings() {
        let metadata = parse_action_metadata(
            r#"
inputs:
  enabled:
    default: true
runs:
  using: composite
  steps:
    - run: echo "$VALUE"
      shell: bash
      env:
        VALUE: 7
    - uses: actions/example@0123456789abcdef0123456789abcdef01234567
      with:
        enabled: false
        retries: 2
      env:
        LABEL: true
"#,
        )
        .expect("valid composite manifest");

        assert_eq!(
            metadata.inputs["enabled"].default_value.as_deref(),
            Some("true")
        );
        assert_eq!(metadata.runs.steps.as_deref().unwrap()[0].env["VALUE"], "7");
        assert_eq!(
            metadata.runs.steps.as_deref().unwrap()[1].with["enabled"],
            "false"
        );
        assert_eq!(
            metadata.runs.steps.as_deref().unwrap()[1].with["retries"],
            "2"
        );
        assert_eq!(
            metadata.runtime().expect("composite runtime"),
            ActionRuntime::Composite
        );
    }

    #[test]
    fn shared_contract_keeps_strict_runtime_and_output_fields() {
        assert!(parse_action_metadata(
            "runs:\n  using: node20\n  main: index.js\n  preIf: always()\n"
        )
        .is_err());
        assert!(parse_action_metadata(
            "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    FLAG: false\n"
        )
        .is_err());
        assert!(parse_action_metadata(
            "outputs:\n  result:\n    description: result\n    required: true\nruns:\n  using: node20\n  main: index.js\n"
        )
        .is_err());
    }

    #[test]
    fn shared_contract_requires_valid_composite_shape() {
        assert!(parse_action_metadata("runs:\n  using: composite\n").is_err());
        assert!(
            parse_action_metadata("runs:\n  using: composite\n  steps:\n    - shell: bash\n")
                .is_err()
        );
        assert!(parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      uses: actions/example@v1\n      shell: bash\n"
        )
        .is_err());
        assert!(parse_action_metadata(
            "runs:\n  using: composite\n  steps:\n    - uses: actions/example@v1\n      shell: bash\n"
        )
        .is_err());
    }
}

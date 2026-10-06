//! Generic GitHub Action metadata detector.
//!
//! An action is a repository product, not a project-language unit.  The scan
//! therefore proves only the metadata runtime and local files the metadata
//! names; it never executes an action, shell, JavaScript, or Docker entrypoint.
//! The scan proves only metadata and local files; it never executes action code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use velnor_model::action_reference::ActionImageReference;

use super::file_walk::is_test_support_path;
use super::{unit, RepositoryShape, ScanContext};
use crate::{is_full_revision, parent_path, shell_quote, UnitKind};

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

/// Check mapping keys before `serde_yaml::Value` converts them. Runner first
/// converts scalar keys to strings, then applies schema-specific key checks
/// and ordinal-ignore-case duplicate detection. `Value` loses enough source
/// information that those checks must happen during deserialization.
#[derive(Debug)]
struct RunnerYamlValue;

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

#[derive(Clone, Copy)]
enum RunnerYamlMapRole {
    Manifest,
    Inputs,
    InputDefinition,
    Outputs,
    OutputDefinition,
    Runs,
    RunEnvironment,
    CompositeSteps,
    CompositeStep,
    StepWith,
    StepEnvironment,
    Any,
}

impl RunnerYamlMapRole {
    fn requires_non_empty_keys(self) -> bool {
        !matches!(self, Self::Any)
    }

    fn value_role(self, key: &str) -> Self {
        match self {
            Self::Manifest if runner_ordinal_ignore_case_eq(key, "inputs") => Self::Inputs,
            Self::Manifest if runner_ordinal_ignore_case_eq(key, "outputs") => Self::Outputs,
            Self::Manifest if runner_ordinal_ignore_case_eq(key, "runs") => Self::Runs,
            Self::Inputs => Self::InputDefinition,
            Self::Outputs => Self::OutputDefinition,
            Self::Runs if runner_ordinal_ignore_case_eq(key, "env") => Self::RunEnvironment,
            Self::Runs if runner_ordinal_ignore_case_eq(key, "steps") => Self::CompositeSteps,
            Self::CompositeStep if runner_ordinal_ignore_case_eq(key, "with") => Self::StepWith,
            Self::CompositeStep if runner_ordinal_ignore_case_eq(key, "env") => {
                Self::StepEnvironment
            }
            _ => Self::Any,
        }
    }

    fn sequence_element_role(self) -> Self {
        if matches!(self, Self::CompositeSteps) {
            Self::CompositeStep
        } else {
            Self::Any
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

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(RunnerYamlValue)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(RunnerYamlValue)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(RunnerYamlValue)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(RunnerYamlValue)
    }

    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(RunnerYamlValue)
    }

    fn visit_string<E>(self, _: String) -> Result<Self::Value, E> {
        Ok(RunnerYamlValue)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(RunnerYamlValue)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(RunnerYamlValue)
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
        while sequence
            .next_element_seed(RunnerYamlValueSeed { role })?
            .is_some()
        {}
        Ok(RunnerYamlValue)
    }

    fn visit_map<A>(self, mut mapping: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let role = self.role;
        let mut keys = BTreeSet::new();
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
            let _: RunnerYamlValue =
                mapping.next_value_seed(RunnerYamlValueSeed { role: value_role })?;
        }
        Ok(RunnerYamlValue)
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
        Ok(RunnerYamlMapKey(key.to_string()))
    }

    fn visit_u64<E>(self, key: u64) -> Result<Self::Value, E> {
        Ok(RunnerYamlMapKey(key.to_string()))
    }

    fn visit_f64<E>(self, key: f64) -> Result<Self::Value, E> {
        Ok(RunnerYamlMapKey(key.to_string()))
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

#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the source preflight records independent runner syntax violations"
)]
struct RunnerYamlSyntax {
    has_anchor_or_alias: bool,
    has_complex_mapping_key: bool,
    has_non_string_mapping_key: bool,
    has_duplicate_mapping_key: bool,
}

fn inspect_runner_yaml_syntax(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    base: usize,
    syntax: &mut RunnerYamlSyntax,
) {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

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
        match child {
            GreenChild::Node(child_node) => {
                inspect_runner_yaml_syntax(child_node, source, offset, syntax);
            }
            GreenChild::Token { kind, .. } => match kind {
                SyntaxKind::AnchorMark | SyntaxKind::AliasMark => {
                    syntax.has_anchor_or_alias = true;
                }
                _ => {}
            },
        }
        offset += child.text_len();
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
                inspect_runner_mapping_key(&raw_key, &mut keys, syntax);
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
                        inspect_runner_mapping_key(&raw_key, &mut keys, syntax);
                    }
                    in_key = false;
                }
                SyntaxKind::Comma => {
                    if in_key && !raw_key.is_empty() {
                        if key_has_collection {
                            syntax.has_complex_mapping_key = true;
                        } else {
                            inspect_runner_mapping_key(&raw_key, &mut keys, syntax);
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
                            inspect_runner_mapping_key(&raw_key, &mut keys, syntax);
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
    raw_key: &str,
    keys: &mut BTreeSet<String>,
    syntax: &mut RunnerYamlSyntax,
) {
    if !runner_mapping_key_is_string(raw_key) {
        syntax.has_non_string_mapping_key = true;
        return;
    }
    let key = if raw_key.trim().is_empty() {
        String::new()
    } else {
        let parser_config = serde_yaml::ParserConfig::new()
            .merge_key_policy(serde_yaml::MergeKeyPolicy::AsOrdinary);
        let Ok(key) = serde_yaml::from_str_with_config::<RunnerYamlMapKey>(raw_key, &parser_config)
        else {
            return;
        };
        key.0
    };
    if !keys.insert(runner_ordinal_ignore_case_key(&key)) {
        syntax.has_duplicate_mapping_key = true;
    }
}

/// actions/runner's manifest mappings require string keys.  Noyalib's normal
/// mapping deserializer stringifies scalar keys before schema validation, so
/// classify the source scalar while the CST still preserves whether it was a
/// number, boolean, null, or quoted/plain string.
fn runner_mapping_key_is_string(raw_key: &str) -> bool {
    let raw_key = raw_key.trim();
    if raw_key.is_empty() {
        return false;
    }
    let parser_config =
        serde_yaml::ParserConfig::new().merge_key_policy(serde_yaml::MergeKeyPolicy::AsOrdinary);
    serde_yaml::from_str_with_config::<serde_yaml::Value>(raw_key, &parser_config)
        .is_ok_and(|value| matches!(value, serde_yaml::Value::String(value) if !value.is_empty()))
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

fn normalize_runner_tags(value: &mut serde_yaml::Value) -> Result<(), crate::GeneratorError> {
    match value {
        serde_yaml::Value::Tagged(tagged) => {
            let inner = tagged.value().clone();
            if inner.is_mapping() || inner.is_sequence() {
                *value = inner;
                normalize_runner_tags(value)?;
                return Ok(());
            }
            return Err(crate::GeneratorError::usage(format!(
                "GitHub Action metadata scalar tag `{}` is not supported by actions/runner",
                tagged.tag().as_str()
            )));
        }
        serde_yaml::Value::Mapping(mapping) => {
            for value in mapping.values_mut() {
                normalize_runner_tags(value)?;
            }
        }
        serde_yaml::Value::Sequence(sequence) => {
            for value in sequence {
                normalize_runner_tags(value)?;
            }
        }
        serde_yaml::Value::Null
        | serde_yaml::Value::Bool(_)
        | serde_yaml::Value::Number(_)
        | serde_yaml::Value::String(_) => {}
    }
    Ok(())
}

fn validate_runner_yaml_syntax(
    contents: &str,
    metadata_file: &Path,
) -> Result<(), crate::GeneratorError> {
    let document = serde_yaml::cst::parse_document(contents).map_err(|error| {
        crate::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    let mut syntax = RunnerYamlSyntax::default();
    inspect_runner_yaml_syntax(document.syntax(), contents, 0, &mut syntax);
    if syntax.has_anchor_or_alias {
        return Err(crate::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: actions/runner does not support YAML anchors or aliases",
            metadata_file.display()
        )));
    }
    if syntax.has_complex_mapping_key {
        return Err(crate::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: actions/runner does not support collection mapping keys",
            metadata_file.display()
        )));
    }
    if syntax.has_non_string_mapping_key {
        return Err(crate::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: actions/runner requires mapping keys to be non-empty strings",
            metadata_file.display()
        )));
    }
    if syntax.has_duplicate_mapping_key {
        return Err(crate::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: duplicate YAML mapping key after Runner case-insensitive matching",
            metadata_file.display()
        )));
    }
    Ok(())
}

/// Detect every tracked action metadata file outside test support trees.
pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), crate::GeneratorError> {
    for source in discover_action_sources(context.root, context.files)? {
        let (_, references) = inspect_action_source(&source, context.root, context.files)?;
        let mut commands = vec![format!(
            "velnor-workflow verify-action --path {}",
            shell_quote(&source.path)
        )];
        commands.extend(
            references
                .iter()
                .map(|path| format!("test -f {}", shell_quote(path))),
        );
        let action = unit(
            UnitKind::GithubAction,
            &source.root,
            vec![if source.root == "." {
                "**".to_owned()
            } else {
                format!("{}/**", source.root)
            }],
            commands,
            None,
        );
        shape.units.push(action);
    }
    Ok(())
}

/// Re-validate one action metadata file at execution time. Generation proves
/// the checked-in shape; this command makes the generated unit fail if the
/// metadata or any local entrypoint changes between scan and execution.
pub(crate) fn verify_action(
    root: &Path,
    source_path: &str,
) -> Result<(), crate::GeneratorError> {
    let source = Path::new(source_path);
    if source.is_absolute()
        || source
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub Action source path `{source_path}` must be repository-relative"
        )));
    }
    let files = super::file_walk::repository_files(root, &[])?;
    let Some(canonical) = discover_action_sources(root, &files)?
        .into_iter()
        .find(|candidate| candidate.path == source_path)
    else {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub Action source `{source_path}` is not the canonical action entrypoint"
        )));
    };
    inspect_action_source(&canonical, root, &files).map(|_| ())
}

/// Discover the entrypoint that actions/runner would prepare for every action
/// directory.  `action.yml` wins over `action.yaml`; a bare Dockerfile is the
/// fallback only when neither metadata file exists.  Keeping this decision in
/// one function prevents generation and runtime verification from auditing
/// different files.
fn discover_action_sources(
    root: &Path,
    files: &[String],
) -> Result<Vec<ActionSource>, crate::GeneratorError> {
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
) -> Result<Option<String>, crate::GeneratorError> {
    let reference = reference.trim();
    let relative = if let Some(relative) = reference.strip_prefix("./") {
        relative
    } else if let Some(relative) = reference.strip_prefix(".\\") {
        relative
    } else {
        if is_unsafe_local_action_reference(reference) {
            return Err(crate::GeneratorError::usage(format!(
                "GitHub local action reference `{reference}` must stay inside the repository workspace"
            )));
        }
        return Ok(None);
    };
    if reference.contains('$') || reference.contains("{{") || reference.contains("}}") {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub local action reference `{reference}` must be static"
        )));
    }
    let normalized = relative.replace('\\', "/");
    if normalized.starts_with('/') || has_windows_drive_prefix(&normalized) {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub local action reference `{reference}` must stay inside the repository workspace"
        )));
    }
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(crate::GeneratorError::usage(format!(
                    "GitHub local action reference `{reference}` escapes the repository workspace"
                )));
            }
            component if has_windows_drive_prefix(component) => {
                return Err(crate::GeneratorError::usage(format!(
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

fn is_runner_local_action_reference(reference: &str) -> bool {
    let reference = reference.trim();
    reference.starts_with("./") || reference.starts_with(".\\")
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
) -> Result<(), crate::GeneratorError> {
    let mut path = root.to_path_buf();
    for component in components {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(crate::GeneratorError::usage(format!(
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
                return Err(crate::GeneratorError::io(
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
) -> Result<BTreeSet<String>, crate::GeneratorError> {
    let mut roots = BTreeSet::new();
    for path in files.iter().filter(|path| {
        let path = Path::new(path);
        path.parent() == Some(Path::new(".github/workflows"))
            && matches!(
                path.extension().and_then(std::ffi::OsStr::to_str),
                Some("yml" | "yaml")
            )
    }) {
        let workflow_path = root.join(&path);
        let contents = std::fs::read_to_string(&workflow_path).map_err(|error| {
            crate::GeneratorError::io("read workflow action references", &workflow_path, &error)
        })?;
        let document: serde_yaml::Value = serde_yaml::from_str(&contents).map_err(|error| {
            crate::GeneratorError::usage(format!(
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
) -> Result<(), crate::GeneratorError> {
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
) -> Result<(Option<ActionMetadata>, Vec<String>), crate::GeneratorError> {
    match source.kind {
        ActionSourceKind::Dockerfile => Ok((None, Vec::new())),
        ActionSourceKind::Metadata => {
            let metadata = parse_metadata(root, &source.path)?;
            let references = local_references(root, &metadata.runs, &source.root, files)?;
            Ok((Some(metadata), references))
        }
    }
}

fn parse_metadata(
    root: &Path,
    metadata_path: &str,
) -> Result<ActionMetadata, crate::GeneratorError> {
    let metadata_file = root.join(metadata_path);
    let contents = std::fs::read_to_string(&metadata_file).map_err(|error| {
        crate::GeneratorError::io("read GitHub Action metadata", &metadata_file, &error)
    })?;
    validate_runner_yaml_syntax(&contents, &metadata_file)?;
    let parser_config = serde_yaml::ParserConfig::new()
        // The source-aware preflight applies Runner's scalar stringification
        // and ordinal-ignore-case duplicate rules. Noyalib's Value map has
        // different scalar coercions, so it must not reject those pairs first.
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Last)
        // actions/runner treats `<<` as an ordinary key and rejects aliases.
        .merge_key_policy(serde_yaml::MergeKeyPolicy::AsOrdinary);
    // Inspect source key types before Noyalib's string-keyed Value map erases
    // them, and mirror Runner's scalar-to-string conversion. Schema-specific
    // non-empty checks happen after this parse, only on declared mappings.
    let mut key_preflight =
        serde_yaml::StreamingDeserializer::with_config(&contents, &parser_config);
    let _: RunnerYamlValue = RunnerYamlValue::deserialize(&mut key_preflight).map_err(|error| {
        crate::GeneratorError::usage(format!(
            "parse GitHub Action metadata {} with Runner-compatible mapping keys: {error}",
            metadata_file.display()
        ))
    })?;
    let mut document: serde_yaml::Value =
        serde_yaml::from_str_with_config(&contents, &parser_config).map_err(|error| {
            crate::GeneratorError::usage(format!(
                "parse GitHub Action metadata {}: {error}",
                metadata_file.display()
            ))
        })?;
    normalize_runner_tags(&mut document)?;
    validate_metadata_shape(&document)?;
    let metadata: ActionMetadata = serde_yaml::from_value(&document).map_err(|error| {
        crate::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    Ok(metadata)
}

/// Validate the typed fields that actions/runner's action manifest schema
/// asserts before converting the manifest. Unknown keys stay loose like the
/// runner; known keys never silently fall through a `serde_yaml::Value`.
fn validate_metadata_shape(document: &serde_yaml::Value) -> Result<(), crate::GeneratorError> {
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
        crate::GeneratorError::usage(
            "GitHub Action metadata `runs` must be a mapping, but it is missing".to_owned(),
        )
    })?)
}

fn require_mapping<'a>(
    value: &'a serde_yaml::Value,
    field: &str,
) -> Result<&'a serde_yaml::Mapping, crate::GeneratorError> {
    value.as_mapping().ok_or_else(|| {
        crate::GeneratorError::usage(format!(
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

fn mapping_key<'a>(key: &'a str, field: &str) -> Result<&'a str, crate::GeneratorError> {
    if key.is_empty() {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` keys must not be empty"
        )));
    }
    Ok(key)
}

fn require_string(
    value: &serde_yaml::Value,
    field: &str,
    non_empty: bool,
) -> Result<(), crate::GeneratorError> {
    let text = value.as_str().ok_or_else(|| {
        crate::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` must be a string"
        ))
    })?;
    if non_empty && text.is_empty() {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` must not be empty"
        )));
    }
    Ok(())
}

fn validate_inputs(value: &serde_yaml::Value) -> Result<(), crate::GeneratorError> {
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

fn validate_outputs(value: &serde_yaml::Value) -> Result<(), crate::GeneratorError> {
    let outputs = require_mapping(value, "outputs")?;
    for (name, definition) in outputs {
        mapping_key(name, "outputs")?;
        let definition = require_mapping(definition, "output definition")?;
        for (field, value) in definition {
            let field = mapping_key(field, "output definition")?;
            if field == "description" || field == "value" {
                require_string(value, &format!("outputs.{field}"), false)?;
            } else {
                return Err(crate::GeneratorError::usage(format!(
                    "GitHub Action metadata has unexpected output definition key `{field}`"
                )));
            }
        }
    }
    Ok(())
}

fn validate_runs(value: &serde_yaml::Value) -> Result<(), crate::GeneratorError> {
    let runs = require_mapping(value, "runs")?;
    if let Some(plugin) = mapping_value(runs, "plugin") {
        require_string(plugin, "runs.plugin", true)?;
        return Err(crate::GeneratorError::usage(
            "unsupported GitHub Action runtime `plugin`; Velnor does not execute plugin actions"
                .to_owned(),
        ));
    }
    let using = mapping_value(runs, "using").ok_or_else(|| {
        crate::GeneratorError::usage(
            "GitHub Action metadata `runs.using` must be a non-empty string".to_owned(),
        )
    })?;
    require_string(using, "runs.using", true)?;
    let using = using.as_str().unwrap_or_default().to_ascii_lowercase();
    if using == "composite" && mapping_value(runs, "steps").is_none() {
        return Err(crate::GeneratorError::usage(
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
            return Err(crate::GeneratorError::usage(format!(
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
) -> Result<(), crate::GeneratorError> {
    let sequence = value.as_sequence().ok_or_else(|| {
        crate::GeneratorError::usage(format!(
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
) -> Result<(), crate::GeneratorError> {
    let mapping = require_mapping(value, field)?;
    for (name, value) in mapping {
        mapping_key(name, field)?;
        require_string(value, &format!("{field} entry"), false)?;
    }
    Ok(())
}

fn validate_composite_steps(value: &serde_yaml::Value) -> Result<(), crate::GeneratorError> {
    let steps = value.as_sequence().ok_or_else(|| {
        crate::GeneratorError::usage(
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
                    return Err(crate::GeneratorError::usage(format!(
                        "GitHub Action metadata has unexpected composite step key `{field}`"
                    )));
                }
            }
        }
        let has_run = mapping_value(step, "run").is_some();
        let has_uses = mapping_value(step, "uses").is_some();
        if has_run == has_uses {
            return Err(crate::GeneratorError::usage(format!(
                "GitHub composite action step {index} must declare exactly one of `run` or `uses`"
            )));
        }
        if has_run && mapping_value(step, "with").is_some() {
            return Err(crate::GeneratorError::usage(format!(
                "GitHub composite run step {index} must not declare `with`"
            )));
        }
        if has_uses
            && (mapping_value(step, "shell").is_some()
                || mapping_value(step, "working-directory").is_some())
        {
            return Err(crate::GeneratorError::usage(format!(
                "GitHub composite uses step {index} must not declare `shell` or `working-directory`"
            )));
        }
        if has_run && mapping_value(step, "shell").is_none() {
            return Err(crate::GeneratorError::usage(format!(
                "GitHub composite action step {index} must declare `shell`"
            )));
        }
    }
    Ok(())
}

fn validate_mapping(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::GeneratorError> {
    if !value.is_mapping() {
        return Err(crate::GeneratorError::usage(format!(
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
) -> Result<Vec<String>, crate::GeneratorError> {
    let using = runs.using.to_ascii_lowercase();
    let mut references = BTreeSet::new();
    match using.as_str() {
        "composite" => {
            for step in runs.steps.as_deref().unwrap_or_default() {
                validate_composite_step(step)?;
                if step.run.is_none() && step.uses.is_none() {
                    return Err(crate::GeneratorError::usage(
                        "GitHub composite action step must declare run or uses",
                    ));
                }
                if step.run.is_some() && step.uses.is_some() {
                    return Err(crate::GeneratorError::usage(
                        "GitHub composite action step must not declare both run and uses",
                    ));
                }
                if let Some(run) = &step.run {
                    if step.shell.is_none() {
                        return Err(crate::GeneratorError::usage(
                            "GitHub composite action run step must declare shell",
                        ));
                    }
                    let run = resolve_action_path_expression(run, action_root);
                    for reference in shell_references(&run) {
                        if let Some(reference) = strip_action_path_marker(&reference) {
                            add_local_reference(&mut references, reference, ".", root, files)?;
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
                            )?;
                        }
                    }
                }
                if let Some(uses) = &step.uses
                    && uses.trim().is_empty()
                {
                    return Err(crate::GeneratorError::usage(
                        "GitHub composite action uses value must not be empty",
                    ));
                }
                if let Some(uses) = &step.uses {
                    let uses = uses.trim();
                    if is_runner_local_action_reference(uses) {
                        add_local_action_reference(&mut references, uses, root, files)?;
                    } else if is_unsafe_local_action_reference(uses) {
                        return Err(crate::GeneratorError::usage(format!(
                            "GitHub composite local action `{uses}` must stay inside the repository workspace"
                        )));
                    } else if !is_full_sha_action_reference(uses) {
                        return Err(crate::GeneratorError::usage(format!(
                            "GitHub composite external action `{uses}` must use a full 40-character SHA pin"
                        )));
                    }
                }
            }
        }
        "node12" | "node16" | "node20" | "node24" => {
            let main = runs.main.as_deref().ok_or_else(|| {
                crate::GeneratorError::usage(format!(
                    "GitHub JavaScript action ({using}) metadata must declare runs.main"
                ))
            })?;
            add_local_reference(&mut references, main, action_root, root, files)?;
            for reference in [&runs.pre, &runs.post] {
                if let Some(reference) = reference.as_deref() {
                    add_local_reference(&mut references, reference, action_root, root, files)?;
                }
            }
        }
        "docker" => {
            let image = runs.image.as_deref().ok_or_else(|| {
                crate::GeneratorError::usage(
                    "GitHub Docker action metadata must declare runs.image",
                )
            })?;
            if image.trim().is_empty() {
                return Err(crate::GeneratorError::usage(
                    "GitHub Docker action metadata `runs.image` must not be empty",
                ));
            }
            if is_dockerfile_reference(image) {
                add_local_reference(&mut references, image, action_root, root, files)?;
            } else if !is_docker_image_reference(image) {
                return Err(crate::GeneratorError::usage(format!(
                    "GitHub Docker action metadata `runs.image` must be a Dockerfile path or docker:// image reference, got `{image}`"
                )));
            }
            // Docker action entrypoints are resolved inside the image. They
            // are not host files and must not be mistaken for local scripts.
        }
        other => {
            return Err(crate::GeneratorError::usage(format!(
                "unsupported GitHub Action runtime `{other}`; expected composite, node12/node16/node20/node24, or docker"
            )));
        }
    }
    Ok(references.into_iter().collect())
}

fn is_full_sha_action_reference(value: &str) -> bool {
    if value
        .get(0.."docker://".len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("docker://"))
    {
        return false;
    }
    value.rsplit_once('@').is_some_and(|(action, revision)| {
        let parts = action.split('/').collect::<Vec<_>>();
        parts.len() >= 2
            && parts.iter().all(|segment| {
                !segment.is_empty()
                    && !segment.contains("..")
                    && !segment.chars().any(char::is_whitespace)
            })
            && is_full_revision(revision)
    })
}

/// Match actions/runner's Dockerfile test instead of guessing from an image
/// tag.  An ordinary image such as `ubuntu` is resolved by the container
/// runtime; only a basename named `Dockerfile` or beginning `Dockerfile.` is
/// a host-side build source.
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

fn validate_composite_step(step: &ActionStep) -> Result<(), crate::GeneratorError> {
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
        && !continue_on_error.is_string()
    {
        return Err(crate::GeneratorError::usage(
            "GitHub composite action step `continue-on-error` must be a boolean or expression string",
        ));
    }
    if step.id.as_deref().is_some_and(str::is_empty) {
        return Err(crate::GeneratorError::usage(
            "GitHub composite action step `id` must not be empty",
        ));
    }
    Ok(())
}

fn normalize_working_directory(
    root: &Path,
    working_directory: &str,
) -> Result<String, crate::GeneratorError> {
    let normalized = working_directory.replace('\\', "/");
    if normalized.contains("${{")
        || normalized.contains("{{")
        || normalized.contains("}}")
        || normalized.contains('$')
    {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub composite `working-directory` `{working_directory}` must be static for local script lookup"
        )));
    }
    if normalized.starts_with('/') || has_windows_drive_prefix(&normalized) {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub composite `working-directory` `{working_directory}` must stay inside the repository workspace"
        )));
    }
    let mut components = Vec::new();
    for component in normalized.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(crate::GeneratorError::usage(format!(
                    "GitHub composite `working-directory` `{working_directory}` must not traverse parent directories"
                )));
            }
            component if has_windows_drive_prefix(component) => {
                return Err(crate::GeneratorError::usage(format!(
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

fn add_local_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    base_path: &str,
    root: &Path,
    files: &[String],
) -> Result<(), crate::GeneratorError> {
    let reference = reference.trim().trim_matches(['"', '\'']);
    if reference.is_empty()
        || reference.contains("${{")
        || reference.contains("{{")
        || reference.contains("}}")
        || reference.contains('$')
    {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` must be a static relative path"
        )));
    }
    let normalized = reference.replace('\\', "/");
    if normalized.starts_with('/') || has_windows_drive_prefix(&normalized) {
        return Err(crate::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` must stay inside the repository workspace"
        )));
    }
    let mut components = Vec::new();
    for component in base_path.replace('\\', "/").split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(crate::GeneratorError::usage(format!(
                    "GitHub Action local entrypoint base `{base_path}` escapes the repository workspace"
                )));
            }
            component if has_windows_drive_prefix(component) => {
                return Err(crate::GeneratorError::usage(format!(
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
                return Err(crate::GeneratorError::usage(format!(
                    "GitHub Action local entrypoint `{reference}` escapes the repository workspace"
                )));
            }
            component if has_windows_drive_prefix(component) => {
                return Err(crate::GeneratorError::usage(format!(
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
        && !is_excluded_action_file(root, &repository_path)?
    {
        return Err(crate::GeneratorError::usage(format!(
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
/// owned outputs.
fn is_excluded_action_file(
    root: &Path,
    repository_path: &str,
) -> Result<bool, crate::GeneratorError> {
    let first = repository_path.split('/').next();
    if first != Some("dist") {
        return Ok(false);
    }
    let generator_owned = crate::s2::generator_owned_output_paths(root)
        .map_err(|error| crate::GeneratorError::usage(error.to_string()))?;
    if generator_owned.contains(Path::new(repository_path)) {
        return Ok(false);
    }
    let path = root.join(repository_path);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(crate::GeneratorError::io(
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
) -> Result<(), crate::GeneratorError> {
    let reference = reference.trim();
    let Some(directory) = discovered_local_action_root(root, reference)? else {
        return Err(crate::GeneratorError::usage(format!(
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
    Err(crate::GeneratorError::usage(format!(
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

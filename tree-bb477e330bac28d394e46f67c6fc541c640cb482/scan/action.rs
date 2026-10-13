//! Generic GitHub Action metadata detector.
//!
//! An action is a repository product, not a project-language unit.  The scan
//! therefore proves only the metadata runtime and local files the metadata
//! names; it never executes an action, shell, JavaScript, or Docker entrypoint.
//! Repository-owned consumer fixtures are attached later through the generic
//! `github-action-fixtures` unit contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::de::{Error as _, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use super::file_walk::is_test_support_path;
use super::{unit, RepositoryShape, ScanContext};
use crate::s2::{is_full_revision, parent_path, shell_quote, UnitKind};

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
    steps: Vec<ActionStep>,
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

/// Check mapping keys before `serde_yaml::Value` converts them. The runner's
/// metadata parser accepts only non-empty string mapping keys; validating here
/// prevents numeric, null, and empty YAML keys from being silently coerced.
#[derive(Debug)]
struct RunnerYamlValue;

impl<'de> Deserialize<'de> for RunnerYamlValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(RunnerYamlValueVisitor)
    }
}

struct RunnerYamlValueVisitor;

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
        RunnerYamlValue::deserialize(deserializer)
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RunnerYamlValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<RunnerYamlValue>()?.is_some() {}
        Ok(RunnerYamlValue)
    }

    fn visit_map<A>(self, mut mapping: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = mapping.next_key::<RunnerYamlMapKey>()? {
            if !keys.insert(key.0.to_ascii_lowercase()) {
                return Err(A::Error::custom(format!(
                    "duplicate YAML mapping key after Runner case-insensitive matching: `{}`",
                    key.0
                )));
            }
            let _: RunnerYamlValue = mapping.next_value()?;
        }
        Ok(RunnerYamlValue)
    }
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
        if key.is_empty() {
            return Err(E::custom(
                "actions/runner requires mapping keys to be non-empty strings",
            ));
        }
        Ok(RunnerYamlMapKey(key.to_owned()))
    }

    fn visit_string<E>(self, key: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if key.is_empty() {
            return Err(E::custom(
                "actions/runner requires mapping keys to be non-empty strings",
            ));
        }
        Ok(RunnerYamlMapKey(key))
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom(
            "actions/runner requires mapping keys to be non-empty strings",
        ))
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom(
            "actions/runner requires mapping keys to be non-empty strings",
        ))
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom(
            "actions/runner requires mapping keys to be non-empty strings",
        ))
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom(
            "actions/runner requires mapping keys to be non-empty strings",
        ))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom(
            "actions/runner requires mapping keys to be non-empty strings",
        ))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom(
            "actions/runner requires mapping keys to be non-empty strings",
        ))
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
struct RunnerYamlSyntax {
    has_anchor_or_alias: bool,
    has_complex_mapping_key: bool,
}

fn inspect_runner_yaml_syntax(node: &serde_yaml::cst::GreenNode, syntax: &mut RunnerYamlSyntax) {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    match node.kind() {
        SyntaxKind::MappingEntry => {
            inspect_runner_mapping_entry_key(node, syntax);
        }
        SyntaxKind::FlowMapping => {
            let mut in_key = false;
            for child in node.children() {
                match child {
                    GreenChild::Token {
                        kind: SyntaxKind::OpenBrace | SyntaxKind::Comma,
                        ..
                    } => {
                        in_key = true;
                    }
                    GreenChild::Token {
                        kind: SyntaxKind::ColonIndicator,
                        ..
                    } => in_key = false,
                    GreenChild::Token {
                        kind: SyntaxKind::CloseBrace,
                        ..
                    } => break,
                    GreenChild::Node(_) if in_key => {
                        syntax.has_complex_mapping_key = true;
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    for child in node.children() {
        match child {
            GreenChild::Node(child) => {
                inspect_runner_yaml_syntax(child, syntax);
            }
            GreenChild::Token { kind, .. } => match kind {
                SyntaxKind::AnchorMark | SyntaxKind::AliasMark => {
                    syntax.has_anchor_or_alias = true;
                }
                _ => {}
            },
        }
    }
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

fn validate_runner_mapping_collisions(
    value: &serde_yaml::Value,
) -> Result<(), crate::s2::GeneratorError> {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            let mut keys = BTreeSet::new();
            for (key, value) in mapping {
                let canonical = key.to_ascii_lowercase();
                if !keys.insert(canonical) {
                    return Err(crate::s2::GeneratorError::usage(format!(
                        "GitHub Action metadata has duplicate mapping key `{key}` after Runner case-insensitive matching"
                    )));
                }
                validate_runner_mapping_collisions(value)?;
            }
        }
        serde_yaml::Value::Sequence(sequence) => {
            for value in sequence {
                validate_runner_mapping_collisions(value)?;
            }
        }
        serde_yaml::Value::Tagged(tagged) => {
            validate_runner_mapping_collisions(tagged.value())?;
        }
        serde_yaml::Value::Null
        | serde_yaml::Value::Bool(_)
        | serde_yaml::Value::Number(_)
        | serde_yaml::Value::String(_) => {}
    }
    Ok(())
}

fn normalize_runner_tags(value: &mut serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    match value {
        serde_yaml::Value::Tagged(tagged) => {
            let inner = tagged.value().clone();
            if inner.is_mapping() || inner.is_sequence() {
                *value = inner;
                normalize_runner_tags(value)?;
                return Ok(());
            }
            return Err(crate::s2::GeneratorError::usage(format!(
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
) -> Result<(), crate::s2::GeneratorError> {
    let document = serde_yaml::cst::parse_document(contents).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    let mut syntax = RunnerYamlSyntax::default();
    inspect_runner_yaml_syntax(document.syntax(), &mut syntax);
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
    Ok(())
}

/// Detect every tracked action metadata file outside test support trees.
pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), crate::s2::GeneratorError> {
    for source in discover_action_sources(context.root, context.files)? {
        let (metadata, references) = inspect_action_source(&source, context.root, context.files)?;
        let mut commands = vec![format!(
            "velnor-workflow verify-action --path {}",
            shell_quote(&source.path)
        )];
        commands.extend(
            references
                .iter()
                .map(|path| format!("test -f {}", shell_quote(path))),
        );
        let mut action = unit(
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
    let Some(canonical) = discover_action_sources(root, &files)?
        .into_iter()
        .find(|candidate| candidate.path == source_path)
    else {
        return Err(crate::s2::GeneratorError::usage(format!(
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
) -> Result<Vec<ActionSource>, crate::s2::GeneratorError> {
    let candidates = discover_action_source_candidates(files);
    let mut referenced_roots = BTreeSet::new();
    referenced_roots.extend(workflow_action_roots(root)?);
    for source in candidates.values() {
        if source.kind != ActionSourceKind::Metadata {
            continue;
        }
        let metadata = parse_metadata(root, &source.path)?;
        for step in &metadata.runs.steps {
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
    for file in files.iter().filter(|file| !is_test_support_path(file)) {
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

fn discovered_local_action_root(
    root: &Path,
    reference: &str,
) -> Result<Option<String>, crate::s2::GeneratorError> {
    let reference = reference.trim();
    let relative = if let Some(relative) = reference.strip_prefix("./") {
        relative
    } else if let Some(relative) = reference.strip_prefix(".\\") {
        relative
    } else {
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

fn workflow_action_roots(root: &Path) -> Result<BTreeSet<String>, crate::s2::GeneratorError> {
    let mut roots = BTreeSet::new();
    for path in super::file_walk::workflow_source_files(root)? {
        let workflow_path = root.join(&path);
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
) -> Result<(Option<ActionMetadata>, Vec<String>), crate::s2::GeneratorError> {
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
) -> Result<ActionMetadata, crate::s2::GeneratorError> {
    let metadata_file = root.join(metadata_path);
    let contents = std::fs::read_to_string(&metadata_file).map_err(|error| {
        crate::s2::GeneratorError::io("read GitHub Action metadata", &metadata_file, &error)
    })?;
    validate_runner_yaml_syntax(&contents, &metadata_file)?;
    let parser_config = serde_yaml::ParserConfig::new()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error)
        // actions/runner treats `<<` as an ordinary key and rejects aliases.
        .merge_key_policy(serde_yaml::MergeKeyPolicy::AsOrdinary);
    // Inspect source key types before Noyalib's string-keyed Value map erases
    // them, and mirror Runner's scalar-to-string conversion. Schema-specific
    // non-empty checks happen after this parse, only on declared mappings.
    let mut key_preflight =
        serde_yaml::StreamingDeserializer::with_config(&contents, &parser_config);
    let _: RunnerYamlValue = RunnerYamlValue::deserialize(&mut key_preflight).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {} with Runner-compatible mapping keys: {error}",
            metadata_file.display()
        ))
    })?;
    let mut document: serde_yaml::Value =
        serde_yaml::from_str_with_config(&contents, &parser_config).map_err(|error| {
            crate::s2::GeneratorError::usage(format!(
                "parse GitHub Action metadata {}: {error}",
                metadata_file.display()
            ))
        })?;
    normalize_runner_tags(&mut document)?;
    validate_runner_mapping_collisions(&document)?;
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
            if field.eq_ignore_ascii_case("default")
                || field.eq_ignore_ascii_case("deprecationMessage")
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
            if field.eq_ignore_ascii_case("description") || field.eq_ignore_ascii_case("value") {
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

fn local_references(
    root: &Path,
    runs: &ActionRuns,
    action_root: &str,
    files: &[String],
) -> Result<Vec<String>, crate::s2::GeneratorError> {
    let using = runs.using.to_ascii_lowercase();
    let mut references = BTreeSet::new();
    match using.as_str() {
        "composite" => {
            for step in &runs.steps {
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
                    return Err(crate::s2::GeneratorError::usage(
                        "GitHub composite action uses value must not be empty",
                    ));
                }
                if let Some(uses) = &step.uses {
                    let uses = uses.trim();
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
            add_local_reference(&mut references, main, action_root, root, files)?;
            for reference in [&runs.pre, &runs.post] {
                if let Some(reference) = reference.as_deref() {
                    add_local_reference(&mut references, reference, action_root, root, files)?;
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
                add_local_reference(&mut references, image, action_root, root, files)?;
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
    let value = value.trim();
    if value
        .get(.."docker://".len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("docker://"))
    {
        return false;
    }
    let basename = value.rsplit('/').next().unwrap_or(value);
    let basename = basename.to_ascii_lowercase();
    basename == "dockerfile"
        || basename.starts_with("dockerfile.")
        || basename.ends_with("dockerfile")
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
        && !continue_on_error.is_string()
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

fn add_local_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    base_path: &str,
    root: &Path,
    files: &[String],
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
    if !files.iter().any(|file| file == &repository_path) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action entrypoint `{reference}` resolves to missing file `{repository_path}`"
        )));
    }
    references.insert(repository_path);
    Ok(())
}

fn add_local_action_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    root: &Path,
    files: &[String],
) -> Result<(), crate::s2::GeneratorError> {
    let reference = reference.trim();
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

    use std::fs;
    use std::path::PathBuf;

    use super::{shell_references, shell_tokens};
    use crate::s2::provider::ProviderId;

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
            super::super::scan_shape(&root, &providers(), "main", &[]),
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
        let error = super::super::scan_shape(&missing, &providers(), "main", &[])
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
        let error = super::super::scan_shape(&unknown, &providers(), "main", &[])
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
                "inputs-child-null",
                "inputs:\n  bad:\n    default: null\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
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
                "outputs-child-null",
                "outputs:\n  bad:\n    description: null\nruns:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
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
                "runs:\n  using: docker\n  image: ubuntu\n  args: {}\n",
            ),
            (
                "docker-args-null",
                "runs:\n  using: docker\n  image: ubuntu\n  args: null\n",
            ),
            (
                "docker-env-shape",
                "runs:\n  using: docker\n  image: ubuntu\n  env: []\n",
            ),
            (
                "docker-env-value-null",
                "runs:\n  using: docker\n  image: ubuntu\n  env:\n    BAD: null\n",
            ),
            (
                "docker-condition-shape",
                "runs:\n  using: docker\n  image: ubuntu\n  pre-if: []\n",
            ),
            (
                "runs-unknown-key",
                "runs:\n  using: docker\n  image: ubuntu\n  1: unexpected\n",
            ),
            (
                "node-runtime-disallows-container-args",
                "runs:\n  using: node20\n  main: index.js\n  args: []\n",
            ),
            (
                "runs-env-empty-key",
                "runs:\n  using: docker\n  image: ubuntu\n  env:\n    \"\": unexpected\n",
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
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
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
    fn runner_scalar_mapping_keys_fail_closed_before_schema_coercion() {
        let cases = [
            (
                "root-numeric-key",
                "1: unknown root metadata\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "root-float-key",
                "1.5: unknown root metadata\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "root-boolean-key",
                "true: unknown root metadata\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "root-null-key",
                "null: unknown root metadata\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-numeric-key",
                "inputs:\n  1:\n    default: numeric input name\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-boolean-key",
                "inputs:\n  true:\n    default: boolean input name\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "input-null-key",
                "inputs:\n  null:\n    default: nullable input name\nruns:\n  using: composite\n  steps: []\n",
            ),
            (
                "empty-key-in-any-value",
                "inputs:\n  good:\n    custom:\n      \"\": allowed\nruns:\n  using: composite\n  steps: []\n",
            ),
        ];
        for (name, metadata) in cases {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write Runner scalar-key action metadata",
            );
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("Runner accepted malformed scalar-key metadata: {name}"));
            assert!(
                error.to_string().contains("mapping keys"),
                "unexpected scalar-key error for {name}: {error}"
            );
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
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
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
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
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
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
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
            super::super::scan_shape(&valid, &providers(), "main", &[]),
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
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
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
            super::workflow_action_roots(&root).unwrap_or_else(|error| panic!("{error}")),
            std::collections::BTreeSet::from([".github/actions/explicit".to_owned()])
        );
        assert!(files
            .iter()
            .any(|file| file == ".github/actions/explicit/dockerfile"));
        assert!(!files
            .iter()
            .any(|file| file == ".github/workflows/ignored-action.yml"));
        let shape = must(
            super::super::scan_shape(&root, &providers, "main", &[]),
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
                "runs:\n  using: docker\n  image: ubuntu\n  entrypoint: /container-entrypoint.sh\n  pre-entrypoint: /container-pre.sh\n  post-entrypoint: /container-post.sh\n",
            ),
            "write ordinary image metadata",
        );
        let providers = providers();
        let shape = must(
            super::super::scan_shape(&root, &providers, "main", &[]),
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

        let mutable = fixture("mutable-uses");
        must(
            fs::write(
                mutable.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: actions/example@main\n",
            ),
            "write mutable action metadata",
        );
        let error = super::super::scan_shape(&mutable, &providers, "main", &[])
            .err()
            .unwrap_or_else(|| panic!("mutable external uses must fail"));
        assert!(
            error.to_string().contains("full 40-character SHA pin"),
            "{error}"
        );
        let _ = fs::remove_dir_all(mutable);
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
            fs::write(
                root.join("actions/parent/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ./actions/child\n",
            ),
            "write nested parent action metadata",
        );
        must(
            fs::write(
                root.join("actions/child/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo child\n",
            ),
            "write workspace child action metadata",
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
            ),
            "resolve nested action from workspace root",
        );
        assert_eq!(references, ["actions/child/action.yml"]);
        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan nested local actions",
        );
        assert!(shape.units.iter().any(|unit| unit.root == "actions/parent"));
        assert!(shape.units.iter().any(|unit| unit.root == "actions/child"));
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
            super::local_references(&root, &metadata.runs, "actions/child", &files),
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
        for reference in ["actions/child", ".actions/child"] {
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
            super::super::scan_shape(&root, &providers(), "main", &[]),
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
            super::super::scan_shape(&node, &providers(), "main", &[]),
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
                "runs:\n  using: docker\n  image: DOCKER://Dockerfile\n  entrypoint: /inside-image.sh\n",
            ),
            "write uppercase Docker scheme metadata",
        );
        let shape = must(
            super::super::scan_shape(&images, &providers(), "main", &[]),
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
        let _ = fs::remove_dir_all(images);

        let missing_shell = fixture("missing-shell");
        must(
            fs::write(
                missing_shell.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - run: echo missing-shell\n",
            ),
            "write missing-shell metadata",
        );
        let error = super::super::scan_shape(&missing_shell, &providers(), "main", &[])
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
        let error = super::super::scan_shape(&dynamic, &providers(), "main", &[])
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
            super::super::scan_shape(&root, &providers(), "main", &[]),
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

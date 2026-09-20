//! Generic GitHub Action metadata detector.
//!
//! An action is a repository product, not a project-language unit.  The scan
//! therefore proves only the metadata runtime and local files the metadata
//! names; it never executes an action, shell, JavaScript, or Docker entrypoint.
//! Repository-owned consumer fixtures are attached later through the generic
//! `github-action-fixtures` unit contract.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use serde::Deserialize;
use velnor_action_manifest::{
    normalize_runner_yaml_numbers, normalized_runner_number, validate_boolean_expression,
    validate_template_expressions, ActionExpressionContext,
};

use super::file_walk::is_test_support_path;
use super::{unit, RepositoryShape, ScanContext};
use crate::s2::{is_full_revision, parent_path, shell_quote, UnitKind};

const GITHUB_ACTION_PATH_MARKER: &str = "__VELNOR_GITHUB_ACTION_PATH__";
const GITHUB_WORKSPACE_MARKER: &str = "__VELNOR_GITHUB_WORKSPACE__";
const UNSUPPORTED_SHELL_CWD_MARKER: &str = "__VELNOR_UNSUPPORTED_SHELL_CWD__";
const INVALID_SHELL_TOKEN_MARKER: &str = "\0VELNOR_INVALID_SHELL_TOKEN\0";
const SHELL_SUBSTITUTION_START_MARKER: &str = "\u{1f}VELNOR_COMMAND_SUBSTITUTION:";
const SHELL_SUBSTITUTION_END_MARKER: &str = "\u{1e}";
/// `RunnerPluginManager`'s case-insensitive host plugin registry in Runner v2.337.0.
const RUNNER_PLUGIN_CAPABILITIES: &[&str] = &["checkout", "checkoutV1_1", "publish", "download"];

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

#[derive(Default)]
struct ActionReferences {
    files: BTreeSet<String>,
    actions: BTreeSet<String>,
    watch_directories: BTreeSet<String>,
}

struct ActionFileContext<'a> {
    root: &'a Path,
    files: &'a mut BTreeSet<String>,
    supplemental_files: &'a mut BTreeSet<String>,
    exclude: &'a [String],
}

impl ActionFileContext<'_> {
    fn add_reference(
        &mut self,
        references: &mut BTreeSet<String>,
        reference: &str,
        action_root: &str,
    ) -> Result<(), crate::s2::GeneratorError> {
        let path = normalize_repository_relative_path(action_root, reference);
        if let Some(path) = path.as_deref()
            && !self.files.contains(path)
            && super::file_walk::explicitly_referenced_repository_file(
                self.root,
                path,
                self.exclude,
            )?
        {
            self.files.insert(path.to_owned());
            self.supplemental_files.insert(path.to_owned());
        }
        if let Some(path) = path.as_deref()
            && self.files.contains(path)
            && let Some(target) = super::file_walk::canonical_repository_target(self.root, path)?
            && target != path
        {
            if !self.files.contains(&target)
                && super::file_walk::explicitly_referenced_repository_file(
                    self.root,
                    &target,
                    self.exclude,
                )?
            {
                self.files.insert(target.clone());
                self.supplemental_files.insert(target.clone());
            }
            if !self.files.contains(&target) {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub Action symlink entrypoint `{path}` resolves to untracked or excluded file `{target}`"
                )));
            }
            references.insert(target);
        }
        add_local_reference(references, reference, action_root, self.files)
    }
}

struct InspectedAction {
    source: ActionSource,
    metadata: Option<ActionMetadata>,
    references: Vec<String>,
    watch_directories: Vec<String>,
}

struct ActionAnalysis {
    actions: Vec<InspectedAction>,
    supplemental_files: Vec<String>,
}

struct MetadataActionSources {
    preferred_metadata: BTreeMap<String, String>,
    alternate_metadata: BTreeMap<String, String>,
    sources: BTreeMap<String, ActionSource>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ActionMetadata {
    #[serde(default)]
    inputs: serde_yaml::Mapping,
    #[serde(default)]
    outputs: serde_yaml::Mapping,
    runs: ActionRuns,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ActionRuns {
    #[serde(default)]
    using: Option<String>,
    #[serde(default)]
    plugin: Option<String>,
    #[serde(default)]
    main: Option<String>,
    #[serde(default)]
    pre: Option<String>,
    #[serde(default)]
    post: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default, rename = "pre-entrypoint")]
    pre_entrypoint: Option<String>,
    #[serde(default, rename = "post-entrypoint")]
    post_entrypoint: Option<String>,
    #[serde(default)]
    steps: Option<Vec<ActionStep>>,
    #[serde(flatten)]
    unmodeled: BTreeMap<String, serde_yaml::Value>,
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
    #[serde(default, rename = "working-directory")]
    working_directory: Option<String>,
    #[serde(default, rename = "continue-on-error")]
    continue_on_error: Option<serde_yaml::Value>,
    #[serde(default)]
    with: serde_yaml::Value,
    #[serde(default)]
    env: serde_yaml::Value,
}

/// Detect tracked action metadata and local Dockerfile actions selected by a
/// workflow or another local action.
pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
    exclude: &[String],
) -> Result<Vec<String>, crate::s2::GeneratorError> {
    let analysis = analyze_actions(context.root, context.files, exclude)?;
    for action_details in analysis.actions {
        let source = action_details.source;
        let mut commands = vec![format!(
            "velnor-workflow verify-action --path {}",
            shell_quote(&source.path)
        )];
        commands.extend(
            action_details
                .references
                .iter()
                .map(|path| format!("test -f {}", shell_quote(path))),
        );
        let mut watch = vec![if source.root == "." {
            "**".to_owned()
        } else {
            format!("{}/**", escape_action_watch_path(&source.root))
        }];
        watch.extend(action_details.watch_directories.iter().map(|directory| {
            if directory == "." {
                "**".to_owned()
            } else {
                format!("{}/**", escape_action_watch_path(directory))
            }
        }));
        watch.extend(
            action_details
                .references
                .iter()
                .map(|path| escape_action_watch_path(path)),
        );
        watch.sort();
        watch.dedup();
        let mut action = unit(UnitKind::GithubAction, &source.root, watch, commands, None);
        if source.kind == ActionSourceKind::Dockerfile
            || action_details.metadata.is_some_and(|metadata| {
                metadata
                    .runs
                    .using
                    .as_deref()
                    .is_some_and(|using| using.eq_ignore_ascii_case("docker"))
            })
        {
            action.capabilities.docker = true;
        }
        shape.units.push(action);
    }
    Ok(analysis.supplemental_files)
}

fn escape_action_watch_path(path: &str) -> String {
    globset::escape(path).replace('\\', "\\\\")
}

/// Re-validate one action metadata file at execution time. Generation proves
/// the checked-in shape; this command makes the generated unit fail if the
/// metadata or any local entrypoint changes between scan and execution.
pub(crate) fn verify_action(
    root: &Path,
    source_path: &str,
) -> Result<(), crate::s2::GeneratorError> {
    if !is_safe_repository_file_path(source_path) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action source path `{source_path}` must be repository-relative"
        )));
    }
    let files = super::file_walk::repository_files(root, &[])?;
    let analysis = analyze_actions(root, &files, &[])?;
    if !analysis
        .actions
        .iter()
        .any(|candidate| candidate.source.path == source_path)
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action source `{source_path}` is not the canonical action entrypoint"
        )));
    }
    Ok(())
}

/// Analyze actions selected by repository metadata or an actual local `uses`
/// reference. Metadata actions retain repository-wide discovery; Dockerfile
/// fallback is added only for referenced action directories.
#[expect(
    clippy::too_many_lines,
    reason = "this traversal keeps source selection and recursive action discovery atomic"
)]
fn analyze_actions(
    root: &Path,
    files: &[String],
    exclude: &[String],
) -> Result<ActionAnalysis, crate::s2::GeneratorError> {
    let workflows = super::file_walk::workflow_reference_files(root, exclude)?;
    let workflow_roots = workflow_action_roots(root, &workflows)?;
    let mut action_files = files.iter().cloned().collect::<BTreeSet<_>>();
    let mut supplemental_files = BTreeSet::new();
    let MetadataActionSources {
        preferred_metadata,
        alternate_metadata,
        mut sources,
    } = metadata_action_sources(&action_files)?;

    let mut queue = VecDeque::new();
    for action_root in workflow_roots {
        queue.push_back(action_root);
    }
    for action_root in sources.keys() {
        queue.push_back(action_root.clone());
    }

    let mut processed = BTreeSet::new();
    let mut inspected = BTreeMap::<String, InspectedAction>::new();
    while let Some(action_root) = queue.pop_front() {
        let action_root = exact_action_root(&action_files, &action_root).unwrap_or(action_root);
        if !processed.insert(action_root.clone()) {
            continue;
        }
        ensure_selected_action_files(
            root,
            &action_root,
            &mut action_files,
            &mut supplemental_files,
            exclude,
        )?;
        let source = action_source_in_root(
            &action_root,
            &action_files,
            &preferred_metadata,
            &alternate_metadata,
            true,
        )
        .ok_or_else(|| {
            crate::s2::GeneratorError::usage(format!(
                "local GitHub Action `{action_root}` has no action metadata or Dockerfile"
            ))
        })?;
        sources.insert(action_root.clone(), source.clone());

        let (metadata, action_references) = inspect_action_source(
            root,
            &source,
            &mut action_files,
            &mut supplemental_files,
            exclude,
        )?;
        let mut watched = action_references.files;
        let watch_directories = action_references.watch_directories;
        watched.extend(symlink_target_watch_paths(
            root,
            &action_root,
            &action_files,
        )?);
        for referenced_root in action_references.actions {
            let referenced_root =
                exact_action_root(&action_files, &referenced_root).unwrap_or(referenced_root);
            ensure_selected_action_files(
                root,
                &referenced_root,
                &mut action_files,
                &mut supplemental_files,
                exclude,
            )?;
            let referenced_source = action_source_in_root(
                &referenced_root,
                &action_files,
                &preferred_metadata,
                &alternate_metadata,
                true,
            )
            .ok_or_else(|| {
                crate::s2::GeneratorError::usage(format!(
                    "GitHub composite local action has no action metadata or Dockerfile under `{referenced_root}`"
                ))
            })?;
            watched.insert(referenced_source.path.clone());
            if !sources.contains_key(&referenced_root) {
                sources.insert(referenced_root.clone(), referenced_source);
                queue.push_back(referenced_root);
            } else if !processed.contains(&referenced_root) {
                queue.push_back(referenced_root);
            }
        }
        inspected.insert(
            action_root,
            InspectedAction {
                source,
                metadata,
                references: watched.into_iter().collect(),
                watch_directories: watch_directories.into_iter().collect(),
            },
        );
    }

    Ok(ActionAnalysis {
        actions: inspected.into_values().collect(),
        supplemental_files: supplemental_files.into_iter().collect(),
    })
}

fn symlink_target_watch_paths(
    root: &Path,
    action_root: &str,
    files: &BTreeSet<String>,
) -> Result<BTreeSet<String>, crate::s2::GeneratorError> {
    if action_root == "." {
        return Ok(BTreeSet::new());
    }
    let prefix = format!("{action_root}/");
    let mut targets = BTreeSet::new();
    for path in files.iter().filter(|path| path.starts_with(&prefix)) {
        if let Some(target) = super::file_walk::canonical_repository_target(root, path)?
            && target != *path
        {
            targets.insert(target);
        }
    }
    Ok(targets)
}

fn inspect_action_source(
    root: &Path,
    source: &ActionSource,
    action_files: &mut BTreeSet<String>,
    supplemental_files: &mut BTreeSet<String>,
    exclude: &[String],
) -> Result<(Option<ActionMetadata>, ActionReferences), crate::s2::GeneratorError> {
    if source.kind == ActionSourceKind::Dockerfile {
        return Ok((None, ActionReferences::default()));
    }
    let metadata = parse_metadata(root, &source.path)?;
    let references = local_references_for_action(
        root,
        &metadata.runs,
        &source.root,
        action_files,
        supplemental_files,
        exclude,
    )?;
    Ok((Some(metadata), references))
}

fn metadata_action_sources(
    files: &BTreeSet<String>,
) -> Result<MetadataActionSources, crate::s2::GeneratorError> {
    let mut preferred_metadata = BTreeMap::new();
    let mut alternate_metadata = BTreeMap::new();
    for file in files.iter().filter(|file| !is_test_support_path(file)) {
        let root = parent_path(file);
        let basename = file.rsplit('/').next().unwrap_or_default();
        if basename == "action.yml" {
            preferred_metadata
                .entry(root)
                .or_insert_with(|| file.clone());
        } else if basename == "action.yaml" {
            alternate_metadata
                .entry(root)
                .or_insert_with(|| file.clone());
        }
    }
    let mut sources = BTreeMap::<String, ActionSource>::new();
    let metadata_roots = preferred_metadata
        .keys()
        .chain(alternate_metadata.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for action_root in metadata_roots {
        let source = action_source_in_root(
            &action_root,
            files,
            &preferred_metadata,
            &alternate_metadata,
            false,
        )
        .ok_or_else(|| {
            crate::s2::GeneratorError::usage(format!(
                "GitHub Action metadata root `{action_root}` has no canonical action entrypoint"
            ))
        })?;
        sources.insert(action_root, source);
    }
    Ok(MetadataActionSources {
        preferred_metadata,
        alternate_metadata,
        sources,
    })
}

fn action_source_in_root(
    action_root: &str,
    files: &BTreeSet<String>,
    preferred_metadata: &BTreeMap<String, String>,
    alternate_metadata: &BTreeMap<String, String>,
    allow_dockerfile: bool,
) -> Option<ActionSource> {
    if let Some(path) = preferred_metadata
        .get(action_root)
        .or_else(|| alternate_metadata.get(action_root))
    {
        return Some(ActionSource {
            root: action_root.to_owned(),
            path: path.clone(),
            kind: ActionSourceKind::Metadata,
        });
    }
    if let Some(path) = exact_action_file(files, action_root, "action.yml") {
        return Some(ActionSource {
            root: action_root.to_owned(),
            path,
            kind: ActionSourceKind::Metadata,
        });
    }
    if let Some(path) = exact_action_file(files, action_root, "action.yaml") {
        return Some(ActionSource {
            root: action_root.to_owned(),
            path,
            kind: ActionSourceKind::Metadata,
        });
    }
    if !allow_dockerfile {
        return None;
    }
    if let Some(path) = exact_action_file(files, action_root, "Dockerfile")
        .or_else(|| exact_action_file(files, action_root, "dockerfile"))
    {
        return Some(ActionSource {
            root: action_root.to_owned(),
            path,
            kind: ActionSourceKind::Dockerfile,
        });
    }
    None
}

/// Actions/runner probes these exact action filenames on the Linux target.
fn exact_action_file(
    files: &BTreeSet<String>,
    action_root: &str,
    basename: &str,
) -> Option<String> {
    let exact = super::file_walk::join_repo_path(action_root, basename);
    files.contains(&exact).then_some(exact)
}

/// Preserve exact Linux repository spelling when resolving a selected action.
fn exact_action_root(files: &BTreeSet<String>, action_root: &str) -> Option<String> {
    files
        .iter()
        .map(|file| parent_path(file))
        .find(|root| root == action_root)
}

fn ensure_selected_action_files(
    root: &Path,
    action_root: &str,
    files: &mut BTreeSet<String>,
    supplemental_files: &mut BTreeSet<String>,
    exclude: &[String],
) -> Result<(), crate::s2::GeneratorError> {
    let excludes = super::file_walk::exclude_set(exclude)?;
    let mut selected_without_excludes = if action_root == "." {
        super::file_walk::repository_files(root, &[])?
    } else {
        super::file_walk::selected_action_files(root, action_root, &[])?
    }
    .into_iter()
    .collect::<BTreeSet<_>>();
    selected_without_excludes.extend(files.iter().cloned());
    let mut entrypoint = None;
    for name in ["action.yml", "action.yaml", "Dockerfile", "dockerfile"] {
        entrypoint = exact_action_file(&selected_without_excludes, action_root, name)
            .or(exact_symlink_action_file(root, action_root, name, exclude)?);
        if entrypoint.is_some() {
            break;
        }
    }
    if let Some(relative_path) = entrypoint {
        let path = root.join(&relative_path);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = super::file_walk::canonical_repository_target(root, &relative_path)?
                    .ok_or_else(|| {
                    crate::s2::GeneratorError::usage(format!(
                        "local GitHub Action symlink entrypoint `{relative_path}` is missing"
                    ))
                })?;
                if excludes.is_match(&relative_path) || excludes.is_match(&target) {
                    return Err(crate::s2::GeneratorError::usage(format!(
                        "canonical GitHub Action entrypoint `{relative_path}` is excluded from the scan"
                    )));
                }
                if !root.join(&target).is_file() {
                    return Err(crate::s2::GeneratorError::usage(format!(
                        "local GitHub Action symlink entrypoint `{relative_path}` must target a file"
                    )));
                }
            }
            Ok(metadata) if metadata.is_file() => {
                if excludes.is_match(&relative_path) {
                    return Err(crate::s2::GeneratorError::usage(format!(
                        "canonical GitHub Action entrypoint `{relative_path}` is excluded from the scan"
                    )));
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(crate::s2::GeneratorError::io(
                    "inspect GitHub Action entrypoint",
                    &path,
                    &error,
                ));
            }
        }
    }
    let selected = super::file_walk::selected_action_files(root, action_root, exclude)?;
    for file in selected {
        if !files.contains(&file) {
            supplemental_files.insert(file.clone());
            files.insert(file);
        }
    }
    Ok(())
}

fn exact_symlink_action_file(
    root: &Path,
    action_root: &str,
    basename: &str,
    exclude: &[String],
) -> Result<Option<String>, crate::s2::GeneratorError> {
    let directory = if action_root == "." {
        root.to_path_buf()
    } else {
        root.join(action_root)
    };
    let path = directory.join(basename);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let relative_path = super::file_walk::join_repo_path(action_root, basename);
            if super::file_walk::explicitly_referenced_repository_file(
                root,
                &relative_path,
                exclude,
            )? {
                Ok(Some(relative_path))
            } else {
                Ok(None)
            }
        }
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(crate::s2::GeneratorError::io(
            "inspect local GitHub Action entrypoint",
            &path,
            &error,
        )),
    }
}

fn workflow_action_roots(
    root: &Path,
    workflow_files: &[String],
) -> Result<BTreeSet<String>, crate::s2::GeneratorError> {
    let mut roots = BTreeSet::new();
    for workflow_file in workflow_files {
        let path = root.join(workflow_file);
        let contents = std::fs::read_to_string(&path).map_err(|error| {
            crate::s2::GeneratorError::io("read workflow reference context", &path, &error)
        })?;
        let workflow: serde_yaml::Value = serde_yaml::from_str(&contents).map_err(|error| {
            crate::s2::GeneratorError::usage(format!(
                "parse workflow reference context {}: {error}",
                path.display()
            ))
        })?;
        let Some(jobs) = workflow.get("jobs").and_then(serde_yaml::Value::as_mapping) else {
            continue;
        };
        for (_, job) in jobs {
            let Some(steps) = job.get("steps").and_then(serde_yaml::Value::as_sequence) else {
                // `jobs.<id>.uses` names a reusable workflow, not an action.
                continue;
            };
            for step in steps {
                let Some(uses) = step.get("uses").and_then(serde_yaml::Value::as_str) else {
                    continue;
                };
                if looks_like_local_action_reference(uses) {
                    roots.insert(normalize_local_action_directory(uses)?);
                } else if let Some(path) = self_repository_action_path(uses)? {
                    roots.insert(path);
                }
            }
        }
    }
    Ok(roots)
}

fn parse_metadata(
    root: &Path,
    metadata_path: &str,
) -> Result<ActionMetadata, crate::s2::GeneratorError> {
    let metadata_file = root.join(metadata_path);
    let contents = std::fs::read_to_string(&metadata_file).map_err(|error| {
        crate::s2::GeneratorError::io("read GitHub Action metadata", &metadata_file, &error)
    })?;
    let tagged_numbers = normalize_explicit_runner_numeric_tags(&contents).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    let normalized_contents = normalize_runner_yaml_numbers(&tagged_numbers).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    validate_action_manifest_yaml_syntax(&normalized_contents).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(&normalized_contents).map_err(|error| {
            crate::s2::GeneratorError::usage(format!(
                "parse GitHub Action metadata {}: {error}",
                metadata_file.display()
            ))
        })?;
    validate_runner_yaml_tags(&value).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    validate_action_manifest_schema(&value).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    normalize_action_manifest_strings(&mut value);
    let metadata: ActionMetadata = serde_yaml::from_value(&value).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    Ok(metadata)
}

#[derive(Clone, Copy)]
enum RunnerNumericTag {
    Integer,
    Float,
}

fn normalize_explicit_runner_numeric_tags(source: &str) -> Result<String, String> {
    let document = serde_yaml::cst::parse_document(source).map_err(|error| error.to_string())?;
    let mut removals = Vec::new();
    collect_numeric_tag_removals(document.syntax(), source, 0, &mut removals)?;
    if removals.is_empty() {
        return Ok(source.to_owned());
    }
    removals.sort_unstable_by_key(|(start, _, _)| *start);
    let mut normalized = String::with_capacity(source.len());
    let mut cursor = 0;
    for (start, end, _) in removals {
        if start < cursor {
            return Err("overlapping YAML numeric tag tokens".to_owned());
        }
        normalized.push_str(
            source
                .get(cursor..start)
                .ok_or_else(|| "invalid YAML tag token offset".to_owned())?,
        );
        cursor = end;
    }
    normalized.push_str(
        source
            .get(cursor..)
            .ok_or_else(|| "invalid YAML tag token offset".to_owned())?,
    );
    Ok(normalized)
}

fn collect_numeric_tag_removals(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    offset: usize,
    removals: &mut Vec<(usize, usize, RunnerNumericTag)>,
) -> Result<(), String> {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let mut child_offset = offset;
    let mut pending_numeric_tag = None;
    for child in node.children() {
        match child {
            GreenChild::Token {
                kind: SyntaxKind::TagMark,
                len,
            } => {
                let len =
                    usize::try_from(*len).map_err(|_| "YAML tag length does not fit usize")?;
                let end = child_offset
                    .checked_add(len)
                    .ok_or_else(|| "YAML tag offset overflow".to_owned())?;
                let tag = source
                    .get(child_offset..end)
                    .ok_or_else(|| "invalid YAML tag token offset".to_owned())?;
                pending_numeric_tag = runner_numeric_tag(tag).map(|tag| (tag, child_offset, end));
            }
            GreenChild::Token {
                kind: SyntaxKind::PlainScalar,
                len,
            } => {
                if let Some((tag, tag_start, tag_end)) = pending_numeric_tag.take() {
                    let len = usize::try_from(*len)
                        .map_err(|_| "YAML scalar length does not fit usize")?;
                    let end = child_offset
                        .checked_add(len)
                        .ok_or_else(|| "YAML scalar offset overflow".to_owned())?;
                    let scalar = source
                        .get(child_offset..end)
                        .ok_or_else(|| "invalid YAML scalar token offset".to_owned())?;
                    validate_explicit_numeric_tag(tag, scalar)?;
                    removals.push((tag_start, tag_end, tag));
                }
            }
            GreenChild::Token {
                kind:
                    SyntaxKind::SingleQuotedScalar
                    | SyntaxKind::DoubleQuotedScalar
                    | SyntaxKind::LiteralScalar
                    | SyntaxKind::FoldedScalar,
                ..
            } if pending_numeric_tag.is_some() => {
                return Err("numeric YAML tags require a plain scalar".to_owned());
            }
            GreenChild::Node(child_node) => {
                if pending_numeric_tag.is_some() {
                    return Err("numeric YAML tags cannot tag collections".to_owned());
                }
                collect_numeric_tag_removals(child_node, source, child_offset, removals)?;
            }
            GreenChild::Token {
                kind: SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::Comment,
                ..
            } => {}
            GreenChild::Token { .. } => {
                if pending_numeric_tag.is_some() {
                    return Err("numeric YAML tag must precede a scalar value".to_owned());
                }
            }
        }
        child_offset += child.text_len();
    }
    Ok(())
}

fn runner_numeric_tag(tag: &str) -> Option<RunnerNumericTag> {
    match tag {
        "!!int" | "!<tag:yaml.org,2002:int>" => Some(RunnerNumericTag::Integer),
        "!!float" | "!<tag:yaml.org,2002:float>" => Some(RunnerNumericTag::Float),
        _ => None,
    }
}

fn validate_explicit_numeric_tag(tag: RunnerNumericTag, scalar: &str) -> Result<(), String> {
    let scalar = scalar.trim();
    match tag {
        RunnerNumericTag::Integer if !is_runner_integer_lexeme(scalar) => {
            Err(format!("invalid explicitly tagged YAML integer `{scalar}`"))
        }
        RunnerNumericTag::Float if !is_runner_float_lexeme(scalar) => {
            Err(format!("invalid explicitly tagged YAML float `{scalar}`"))
        }
        _ => Ok(()),
    }
}

fn is_runner_integer_lexeme(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    if let Some(hex) = value.strip_prefix("0x") {
        return !hex.is_empty()
            && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            && u32::from_str_radix(hex, 16).is_ok();
    }
    if let Some(octal) = value.strip_prefix("0o") {
        return !octal.is_empty()
            && octal.bytes().all(|byte| (b'0'..=b'7').contains(&byte))
            && u32::from_str_radix(octal, 8).is_ok_and(|value| i32::try_from(value).is_ok());
    }
    false
}

fn is_runner_float_lexeme(value: &str) -> bool {
    if matches!(
        value,
        ".inf"
            | ".Inf"
            | ".INF"
            | "+.inf"
            | "+.Inf"
            | "+.INF"
            | "-.inf"
            | "-.Inf"
            | "-.INF"
            | ".nan"
            | ".NaN"
            | ".NAN"
    ) {
        return true;
    }
    let value = value.strip_prefix(['+', '-']).unwrap_or(value);
    let mantissa = value.split(['e', 'E']).next().unwrap_or_default();
    let exponent = value.split_once(['e', 'E']).map(|(_, exponent)| exponent);
    let mut parts = mantissa.split('.');
    let integer = parts.next().unwrap_or_default();
    let decimal = parts.next();
    if parts.next().is_some() {
        return false;
    }
    let integer_digits = !integer.is_empty() && integer.bytes().all(|byte| byte.is_ascii_digit());
    let decimal_digits = decimal.is_some_and(|decimal| {
        !decimal.is_empty() && decimal.bytes().all(|byte| byte.is_ascii_digit())
    });
    let valid_mantissa = integer_digits || decimal_digits && integer.is_empty();
    if !valid_mantissa {
        return false;
    }
    exponent.is_none_or(|exponent| {
        let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !exponent.is_empty() && exponent.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn validate_runner_yaml_tags(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    fn visit(value: &serde_yaml::Value) -> Result<(), String> {
        match value {
            serde_yaml::Value::Tagged(tagged) => {
                let inner = tagged.value();
                if tagged.tag().as_str() == "!velnor-runner-number" {
                    if inner.as_str().is_none() {
                        return Err("invalid internal numeric scalar tag".to_owned());
                    }
                } else if inner.is_sequence() || inner.is_mapping() {
                    // Runner's YAML reader consumes collection-start events
                    // without inspecting their tag.
                    visit(inner)?;
                } else {
                    match tagged.tag().as_str() {
                        "tag:yaml.org,2002:str" => {}
                        "tag:yaml.org,2002:null" if inner.is_null() => {}
                        "tag:yaml.org,2002:bool" if inner.as_bool().is_some() => {}
                        "tag:yaml.org,2002:int" | "tag:yaml.org,2002:float"
                            if normalized_runner_number(value).is_some() => {}
                        tag => {
                            return Err(format!(
                                "unsupported or malformed YAML scalar tag `{tag}`"
                            ));
                        }
                    }
                    visit(inner)?;
                }
            }
            serde_yaml::Value::Sequence(values) => {
                for value in values {
                    visit(value)?;
                }
            }
            serde_yaml::Value::Mapping(values) => {
                for value in values.values() {
                    visit(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    visit(value).map_err(metadata_schema_error)
}

/// Runner's `YamlObjectReader` rejects anchors, aliases, and collection keys.
/// The Serde value adapter resolves anchors and stringifies collection keys,
/// so inspect the YAML CST before deserialization can erase those distinctions.
fn validate_action_manifest_yaml_syntax(source: &str) -> Result<(), String> {
    let document = serde_yaml::cst::parse_document(source).map_err(|error| error.to_string())?;
    if !document.anchors().is_empty() || !document.aliases().is_empty() {
        return Err("anchors and aliases are not supported by actions/runner".to_owned());
    }
    if contains_collection_mapping_key(document.syntax()) {
        return Err("mapping keys must be YAML scalars".to_owned());
    }
    if contains_invalid_mapping_key(document.syntax(), source, 0) {
        return Err("mapping keys must be supported YAML scalars".to_owned());
    }
    if contains_duplicate_mapping_key(document.syntax(), source, 0) {
        return Err("duplicate mapping keys are not supported by actions/runner".to_owned());
    }
    Ok(())
}

fn contains_collection_mapping_key(node: &serde_yaml::cst::GreenNode) -> bool {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let has_collection_key = match node.kind() {
        SyntaxKind::MappingEntry => {
            let mut in_key = true;
            node.children().any(|child| match child {
                GreenChild::Token {
                    kind: SyntaxKind::ColonIndicator,
                    ..
                } => {
                    in_key = false;
                    false
                }
                GreenChild::Node(child) if in_key => is_yaml_collection(child.kind()),
                _ => false,
            })
        }
        SyntaxKind::FlowMapping => {
            let mut in_key = true;
            node.children().any(|child| match child {
                GreenChild::Token {
                    kind: SyntaxKind::ColonIndicator,
                    ..
                } => {
                    in_key = false;
                    false
                }
                GreenChild::Token {
                    kind: SyntaxKind::Comma,
                    ..
                } => {
                    in_key = true;
                    false
                }
                GreenChild::Node(child) if in_key => is_yaml_collection(child.kind()),
                _ => false,
            })
        }
        _ => false,
    };
    has_collection_key
        || node.children().any(|child| match child {
            GreenChild::Node(child) => contains_collection_mapping_key(child),
            GreenChild::Token { .. } => false,
        })
}

fn mapping_entry_key_source<'a>(
    node: &serde_yaml::cst::GreenNode,
    source: &'a str,
    offset: usize,
) -> Option<&'a str> {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let mut child_offset = offset;
    for child in node.children() {
        if matches!(
            child,
            GreenChild::Token {
                kind: SyntaxKind::ColonIndicator,
                ..
            }
        ) {
            return source.get(offset..child_offset);
        }
        child_offset += child.text_len();
    }
    None
}

fn contains_invalid_mapping_key(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    offset: usize,
) -> bool {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let invalid_here = match node.kind() {
        SyntaxKind::MappingEntry => mapping_entry_key_source(node, source, offset)
            .is_some_and(|key| yaml_mapping_key_string(key).is_none()),
        SyntaxKind::FlowMapping => {
            let mut child_offset = offset;
            let mut key_start = None;
            let mut invalid = false;
            for child in node.children() {
                match child {
                    GreenChild::Token {
                        kind: SyntaxKind::OpenBrace | SyntaxKind::Comma,
                        ..
                    } => key_start = Some(child_offset + child.text_len()),
                    GreenChild::Token {
                        kind: SyntaxKind::ColonIndicator,
                        ..
                    } => {
                        invalid = key_start.take().is_some_and(|start| {
                            source
                                .get(start..child_offset)
                                .is_some_and(|key| yaml_mapping_key_string(key).is_none())
                        });
                        if invalid {
                            break;
                        }
                    }
                    GreenChild::Token {
                        kind: SyntaxKind::CloseBrace,
                        ..
                    } => key_start = None,
                    GreenChild::Token { .. } | GreenChild::Node(_) => {}
                }
                child_offset += child.text_len();
            }
            invalid
        }
        _ => false,
    };
    invalid_here || {
        let mut child_offset = offset;
        node.children().any(|child| {
            let invalid = match child {
                GreenChild::Node(child_node) => {
                    contains_invalid_mapping_key(child_node, source, child_offset)
                }
                GreenChild::Token { .. } => false,
            };
            child_offset += child.text_len();
            invalid
        })
    }
}

fn yaml_mapping_key_string(source: &str) -> Option<String> {
    let key = normalized_yaml_mapping_key_source(source);
    if key.is_empty() {
        return Some(String::new());
    }
    let value = serde_yaml::from_str::<serde_yaml::Value>(key).ok()?;
    if value.is_sequence() || value.is_mapping() {
        return None;
    }
    validate_runner_yaml_tags(&value).ok()?;
    metadata_scalar_string(&value)
}

fn normalized_yaml_mapping_key_source(source: &str) -> &str {
    let key = source.trim();
    if let Some(after_indicator) = key.strip_prefix('?')
        && (after_indicator.is_empty()
            || after_indicator
                .chars()
                .next()
                .is_some_and(char::is_whitespace))
    {
        after_indicator.trim_start()
    } else {
        key
    }
}

fn contains_duplicate_mapping_key(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    offset: usize,
) -> bool {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let has_duplicate_key = match node.kind() {
        SyntaxKind::BlockMapping => block_mapping_has_duplicate_key(node, source, offset),
        SyntaxKind::FlowMapping => flow_mapping_has_duplicate_key(node, source, offset),
        _ => false,
    };
    if has_duplicate_key {
        return true;
    }

    let mut child_offset = offset;
    for child in node.children() {
        if let GreenChild::Node(child_node) = child
            && contains_duplicate_mapping_key(child_node, source, child_offset)
        {
            return true;
        }
        child_offset += child.text_len();
    }
    false
}

fn block_mapping_has_duplicate_key(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    offset: usize,
) -> bool {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let mut keys = BTreeSet::new();
    let mut child_offset = offset;
    for child in node.children() {
        if let GreenChild::Node(entry) = child
            && entry.kind() == SyntaxKind::MappingEntry
            && mapping_entry_key_source(entry, source, child_offset)
                .and_then(yaml_mapping_key_string)
                .is_some_and(|key| !keys.insert(runner_ordinal_ignore_case_key(&key)))
        {
            return true;
        }
        child_offset += child.text_len();
    }
    false
}

fn flow_mapping_has_duplicate_key(
    node: &serde_yaml::cst::GreenNode,
    source: &str,
    offset: usize,
) -> bool {
    use serde_yaml::cst::{GreenChild, SyntaxKind};

    let mut keys = BTreeSet::new();
    let mut child_offset = offset;
    let mut key_start = None;
    for child in node.children() {
        let child_len = child.text_len();
        match child {
            GreenChild::Token {
                kind: SyntaxKind::OpenBrace | SyntaxKind::Comma,
                ..
            } => key_start = Some(child_offset + child_len),
            GreenChild::Token {
                kind: SyntaxKind::ColonIndicator,
                ..
            } => {
                if key_start.take().is_some_and(|start| {
                    source
                        .get(start..child_offset)
                        .and_then(yaml_mapping_key_string)
                        .is_some_and(|key| !keys.insert(runner_ordinal_ignore_case_key(&key)))
                }) {
                    return true;
                }
            }
            GreenChild::Token {
                kind: SyntaxKind::CloseBrace,
                ..
            } => key_start = None,
            GreenChild::Token { .. } | GreenChild::Node(_) => {}
        }
        child_offset += child_len;
    }
    false
}

fn is_yaml_collection(kind: serde_yaml::cst::SyntaxKind) -> bool {
    use serde_yaml::cst::SyntaxKind;

    matches!(
        kind,
        SyntaxKind::BlockMapping
            | SyntaxKind::BlockSequence
            | SyntaxKind::FlowMapping
            | SyntaxKind::FlowSequence
    )
}

fn validate_action_manifest_schema(
    value: &serde_yaml::Value,
) -> Result<(), crate::s2::GeneratorError> {
    let root = metadata_mapping(value, "action manifest")?;
    validate_metadata_keys(root, "action manifest", true)?;
    for (key, value) in root {
        match key.as_str() {
            field @ ("name" | "description") => {
                validate_metadata_contextless_string(value, field, false)?;
            }
            "inputs" | "outputs" | "runs" => {}
            _ => validate_runner_any_value(value, &format!("action manifest.{key}"))?,
        }
    }
    if let Some(inputs) = metadata_field(root, "inputs") {
        validate_action_inputs(inputs)?;
    }
    if let Some(outputs) = metadata_field(root, "outputs") {
        validate_action_outputs(outputs)?;
    }
    if let Some(runs) = metadata_field(root, "runs") {
        validate_action_runs(runs)?;
    }
    Ok(())
}

fn validate_action_inputs(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let inputs = metadata_mapping(value, "inputs")?;
    validate_metadata_keys(inputs, "inputs", true)?;
    for (name, definition) in inputs {
        let definition = metadata_mapping(definition, &format!("inputs.{name}"))?;
        validate_metadata_keys(definition, &format!("inputs.{name}"), true)?;
        for (field, value) in definition {
            if field.eq_ignore_ascii_case("default") {
                validate_metadata_string(value, &format!("inputs.{name}.default"), false)?;
                validate_metadata_templates(
                    value,
                    &format!("inputs.{name}.default"),
                    ActionExpressionContext::InputDefault,
                )?;
            } else if field.eq_ignore_ascii_case("deprecationMessage") {
                // ActionManifestManager.ConvertInputs reads this loose-schema
                // property with AssertString rather than scalar coercion.
                validate_metadata_assert_string(
                    value,
                    &format!("inputs.{name}.deprecationMessage"),
                )?;
                validate_metadata_contextless_string(
                    value,
                    &format!("inputs.{name}.deprecationMessage"),
                    false,
                )?;
            } else {
                validate_runner_any_value(value, &format!("inputs.{name}.{field}"))?;
            }
        }
    }
    Ok(())
}

fn validate_action_outputs(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let outputs = metadata_mapping(value, "outputs")?;
    validate_metadata_keys(outputs, "outputs", true)?;
    for (name, definition) in outputs {
        let definition = metadata_mapping(definition, &format!("outputs.{name}"))?;
        validate_metadata_keys(definition, &format!("outputs.{name}"), true)?;
        for (field, value) in definition {
            if !matches!(field.as_str(), "description" | "value") {
                return Err(metadata_schema_error(format!(
                    "outputs.{name}.{field} is not supported by actions/runner"
                )));
            }
            if field == "description" {
                validate_metadata_contextless_string(
                    value,
                    &format!("outputs.{name}.description"),
                    false,
                )?;
            } else {
                validate_metadata_string(value, &format!("outputs.{name}.value"), false)?;
                validate_metadata_templates(
                    value,
                    &format!("outputs.{name}.value"),
                    ActionExpressionContext::OutputValue,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_action_runs(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let runs = metadata_mapping(value, "runs")?;
    validate_metadata_keys(runs, "runs", true)?;
    if let Some(plugin) = metadata_field(runs, "plugin") {
        if metadata_field(runs, "using").is_some() {
            return Err(metadata_schema_error(
                "runs.plugin cannot be combined with runs.using",
            ));
        }
        validate_runtime_keys(runs, "runs", &["plugin"])?;
        validate_metadata_contextless_string(plugin, "runs.plugin", true)?;
        let plugin = metadata_scalar_string(plugin).unwrap_or_default();
        if !is_registered_runner_plugin(&plugin) {
            return Err(metadata_schema_error(format!(
                "runs.plugin `{plugin}` is not a registered Runner plugin capability"
            )));
        }
        return Ok(());
    }
    let using = metadata_field(runs, "using")
        .ok_or_else(|| metadata_schema_error("runs.using is required"))?;
    validate_metadata_contextless_string(using, "runs.using", true)?;
    let using = using.as_str().unwrap_or_default().to_ascii_lowercase();
    match using.as_str() {
        "composite" => {
            validate_runtime_keys(runs, "runs", &["using", "steps"])?;
            let steps = metadata_field(runs, "steps").ok_or_else(|| {
                metadata_schema_error("runs.steps is required for composite actions")
            })?;
            let steps = steps.as_sequence().ok_or_else(|| {
                metadata_schema_error("runs.steps must be a sequence for composite actions")
            })?;
            for (index, step) in steps.iter().enumerate() {
                validate_composite_step_schema(step, index)?;
            }
        }
        "docker" => {
            validate_runtime_keys(
                runs,
                "runs",
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
            for field in [
                "image",
                "entrypoint",
                "pre-entrypoint",
                "pre-if",
                "post-entrypoint",
                "post-if",
            ] {
                if let Some(value) = metadata_field(runs, field) {
                    validate_metadata_contextless_string(value, &format!("runs.{field}"), true)?;
                }
            }
            validate_runtime_args(runs)?;
            validate_runtime_env(runs)?;
            if metadata_field(runs, "image").is_none() {
                return Err(metadata_schema_error(
                    "runs.image is required for Docker actions",
                ));
            }
            if let Some(image) = metadata_field(runs, "image") {
                validate_docker_image_uri(image)?;
            }
        }
        "node12" | "node16" | "node20" | "node24" => {
            validate_runtime_keys(
                runs,
                "runs",
                &["using", "main", "pre", "pre-if", "post", "post-if"],
            )?;
            for field in ["main", "pre", "pre-if", "post", "post-if"] {
                if let Some(value) = metadata_field(runs, field) {
                    validate_metadata_contextless_string(value, &format!("runs.{field}"), true)?;
                }
            }
            if metadata_field(runs, "main").is_none() {
                return Err(metadata_schema_error(
                    "runs.main is required for Node actions",
                ));
            }
        }
        _ => {
            return Err(metadata_schema_error(format!(
                "unsupported GitHub Action runtime `{using}`"
            )));
        }
    }
    Ok(())
}

fn validate_docker_image_uri(value: &serde_yaml::Value) -> Result<(), crate::s2::GeneratorError> {
    let image = metadata_scalar_string(value)
        .ok_or_else(|| metadata_schema_error("runs.image must be a string"))?;
    if let Some(suffix) = docker_uri_suffix(&image) {
        if suffix.trim().is_empty() {
            return Err(metadata_schema_error(
                "runs.image docker:// reference must name an image",
            ));
        }
    } else if !is_dockerfile_reference(&image) {
        return Err(metadata_schema_error(
            "runs.image must name a Dockerfile or use the docker:// scheme",
        ));
    }
    Ok(())
}

fn docker_uri_suffix(value: &str) -> Option<&str> {
    let prefix_length = "docker://".len();
    value
        .get(..prefix_length)
        .filter(|prefix| prefix.eq_ignore_ascii_case("docker://"))?;
    value.get(prefix_length..)
}

fn validate_runtime_keys(
    mapping: &serde_yaml::Mapping,
    field: &str,
    allowed: &[&str],
) -> Result<(), crate::s2::GeneratorError> {
    for key in mapping.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(metadata_schema_error(format!(
                "{field}.{key} is not valid for this action runtime"
            )));
        }
    }
    Ok(())
}

fn is_registered_runner_plugin(plugin: &str) -> bool {
    RUNNER_PLUGIN_CAPABILITIES
        .iter()
        .any(|registered| registered.eq_ignore_ascii_case(plugin))
}

fn validate_runtime_args(runs: &serde_yaml::Mapping) -> Result<(), crate::s2::GeneratorError> {
    if let Some(args) = metadata_field(runs, "args") {
        let args = args
            .as_sequence()
            .ok_or_else(|| metadata_schema_error("runs.args must be a sequence of strings"))?;
        for (index, argument) in args.iter().enumerate() {
            validate_metadata_string(argument, &format!("runs.args[{index}]"), false)?;
            validate_metadata_templates(
                argument,
                &format!("runs.args[{index}]"),
                ActionExpressionContext::ContainerRun,
            )?;
        }
    }
    Ok(())
}

fn validate_runtime_env(runs: &serde_yaml::Mapping) -> Result<(), crate::s2::GeneratorError> {
    if let Some(environment) = metadata_field(runs, "env") {
        let environment = metadata_mapping(environment, "runs.env")?;
        validate_metadata_expression_keys(
            environment,
            "runs.env",
            ActionExpressionContext::ContainerRun,
        )?;
        for (key, value) in environment {
            validate_metadata_string(value, &format!("runs.env.{key}"), false)?;
            validate_metadata_templates(
                value,
                &format!("runs.env.{key}"),
                ActionExpressionContext::ContainerRun,
            )?;
        }
    }
    Ok(())
}

fn validate_composite_step_schema(
    value: &serde_yaml::Value,
    index: usize,
) -> Result<(), crate::s2::GeneratorError> {
    let field = format!("runs.steps[{index}]");
    let step = metadata_mapping(value, &field)?;
    validate_metadata_keys(step, &field, true)?;
    let run = metadata_field(step, "run");
    let uses = metadata_field(step, "uses");
    if run.is_some() == uses.is_some() {
        return Err(metadata_schema_error(format!(
            "{field} must declare exactly one of `run` or `uses`"
        )));
    }
    let allowed = if run.is_some() {
        &[
            "run",
            "shell",
            "name",
            "id",
            "if",
            "env",
            "continue-on-error",
            "working-directory",
        ][..]
    } else {
        &[
            "uses",
            "name",
            "id",
            "if",
            "continue-on-error",
            "with",
            "env",
        ][..]
    };
    validate_runtime_keys(step, &field, allowed)?;
    if let Some(run) = run {
        validate_metadata_string(run, &format!("{field}.run"), false)?;
        validate_metadata_templates(
            run,
            &format!("{field}.run"),
            ActionExpressionContext::CompositeString,
        )?;
        let shell = metadata_field(step, "shell").ok_or_else(|| {
            metadata_schema_error(format!("{field}.shell is required for run steps"))
        })?;
        validate_metadata_string(shell, &format!("{field}.shell"), false)?;
        validate_metadata_templates(
            shell,
            &format!("{field}.shell"),
            ActionExpressionContext::CompositeString,
        )?;
        if let Some(shell) = metadata_scalar_string(shell) {
            validate_runner_shell_format(&shell, &format!("{field}.shell"))?;
        }
    } else if let Some(uses) = uses {
        validate_metadata_contextless_string(uses, &format!("{field}.uses"), true)?;
    }
    for field_name in ["name", "working-directory"] {
        if let Some(value) = metadata_field(step, field_name) {
            validate_metadata_string(value, &format!("{field}.{field_name}"), false)?;
            validate_metadata_templates(
                value,
                &format!("{field}.{field_name}"),
                ActionExpressionContext::CompositeString,
            )?;
        }
    }
    if let Some(value) = metadata_field(step, "if") {
        validate_composite_if(value, &format!("{field}.if"))?;
    }
    if let Some(id) = metadata_field(step, "id") {
        validate_metadata_contextless_string(id, &format!("{field}.id"), true)?;
    }
    if let Some(continue_on_error) = metadata_field(step, "continue-on-error") {
        validate_metadata_boolean_expression(
            continue_on_error,
            &format!("{field}.continue-on-error"),
        )?;
    }
    for field_name in ["with", "env"] {
        if let Some(value) = metadata_field(step, field_name) {
            let mapping = metadata_mapping(value, &format!("{field}.{field_name}"))?;
            validate_metadata_expression_keys(
                mapping,
                &format!("{field}.{field_name}"),
                ActionExpressionContext::CompositeString,
            )?;
            for (key, value) in mapping {
                validate_metadata_string(value, &format!("{field}.{field_name}.{key}"), false)?;
                validate_metadata_templates(
                    value,
                    &format!("{field}.{field_name}.{key}"),
                    ActionExpressionContext::CompositeString,
                )?;
            }
        }
    }
    Ok(())
}

fn validate_runner_shell_format(shell: &str, field: &str) -> Result<(), crate::s2::GeneratorError> {
    if shell.is_empty() {
        // ScriptHandler defaults an empty action shell to the platform shell.
        return Ok(());
    }
    if shell.contains("${{") {
        // Runner evaluates shell templates before ScriptHandler parses the
        // command format. The scanner cannot determine its static dialect.
        return Ok(());
    }
    let (command, arguments) = shell.split_once(' ').unwrap_or((shell, ""));
    let arguments = arguments.trim_start();
    let is_builtin = ["cmd", "pwsh", "powershell", "bash", "sh", "python"]
        .iter()
        .any(|builtin| builtin.eq_ignore_ascii_case(command));
    if arguments.is_empty() && is_builtin {
        return Ok(());
    }
    if arguments.contains("{0}") && is_runner_format_string(arguments) {
        return Ok(());
    }
    Err(metadata_schema_error(format!(
        "{field} must be a Runner built-in shell or a format string containing '{{0}}'"
    )))
}

/// Runner calls `String.Format(argumentFormat, scriptPath)`, so only argument
/// index zero is available. Validate the format before a workflow reaches the
/// runner, including escaped braces and malformed format items.
fn is_runner_format_string(format: &str) -> bool {
    let mut characters = format.chars().peekable();
    let mut has_script_argument = false;
    while let Some(character) = characters.next() {
        match character {
            '{' if characters.peek() == Some(&'{') => {
                let _ = characters.next();
            }
            '{' => {
                let mut item = String::new();
                let mut closed = false;
                for character in characters.by_ref() {
                    match character {
                        '{' => return false,
                        '}' => {
                            closed = true;
                            break;
                        }
                        _ => item.push(character),
                    }
                }
                if !closed || !runner_format_item_is_script_argument(&item) {
                    return false;
                }
                has_script_argument = true;
            }
            '}' if characters.peek() == Some(&'}') => {
                let _ = characters.next();
            }
            '}' => return false,
            _ => {}
        }
    }
    has_script_argument
}

fn runner_format_item_is_script_argument(item: &str) -> bool {
    let item = item.trim();
    let index_end = item.find([',', ':']).unwrap_or(item.len());
    if item[..index_end].trim() != "0" {
        return false;
    }
    let mut remainder = &item[index_end..];
    if let Some(alignment) = remainder.strip_prefix(',') {
        let alignment_end = alignment.find(':').unwrap_or(alignment.len());
        if alignment[..alignment_end].trim().parse::<i32>().is_err() {
            return false;
        }
        remainder = &alignment[alignment_end..];
    }
    if let Some(format_specifier) = remainder.strip_prefix(':') {
        !format_specifier.contains(['{', '}'])
    } else {
        remainder.is_empty()
    }
}

fn metadata_mapping<'a>(
    value: &'a serde_yaml::Value,
    field: &str,
) -> Result<&'a serde_yaml::Mapping, crate::s2::GeneratorError> {
    value
        .as_mapping()
        .ok_or_else(|| metadata_schema_error(format!("{field} must be a mapping")))
}

fn metadata_field<'a>(
    mapping: &'a serde_yaml::Mapping,
    field: &str,
) -> Option<&'a serde_yaml::Value> {
    mapping.get(field)
}

fn validate_metadata_keys(
    mapping: &serde_yaml::Mapping,
    field: &str,
    nonempty: bool,
) -> Result<(), crate::s2::GeneratorError> {
    validate_metadata_key_shape(mapping, field, nonempty)?;
    if mapping.keys().any(|key| key.contains("${{")) {
        return Err(metadata_schema_error(format!(
            "{field} mapping keys cannot contain template expressions"
        )));
    }
    Ok(())
}

fn validate_metadata_expression_keys(
    mapping: &serde_yaml::Mapping,
    field: &str,
    context: ActionExpressionContext,
) -> Result<(), crate::s2::GeneratorError> {
    validate_metadata_key_shape(mapping, field, true)?;
    for key in mapping.keys() {
        validate_metadata_templates(
            &serde_yaml::Value::String(key.clone()),
            &format!("{field} key {key}"),
            context,
        )?;
    }
    Ok(())
}

fn validate_metadata_key_shape(
    mapping: &serde_yaml::Mapping,
    field: &str,
    nonempty: bool,
) -> Result<(), crate::s2::GeneratorError> {
    let mut keys = BTreeSet::new();
    for key in mapping.keys() {
        let key = key.to_owned();
        if nonempty && key.is_empty() {
            return Err(metadata_schema_error(format!(
                "{field} keys must not be empty"
            )));
        }
        if !keys.insert(runner_ordinal_ignore_case_key(&key)) {
            return Err(metadata_schema_error(format!(
                "{field} keys must be unique ignoring case"
            )));
        }
    }
    Ok(())
}

/// Runner mapping keys use `StringComparer.OrdinalIgnoreCase`. Keep Rust's
/// one-to-one lowercase mapping, with .NET's shared sigma fold made explicit.
fn runner_ordinal_ignore_case_key(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character == '\u{03c2}' {
                return '\u{03c3}';
            }
            let mut lowercase = character.to_lowercase();
            match (lowercase.next(), lowercase.next()) {
                (Some(lowercase), None) => lowercase,
                _ => character,
            }
        })
        .collect()
}

fn validate_metadata_string(
    value: &serde_yaml::Value,
    field: &str,
    nonempty: bool,
) -> Result<(), crate::s2::GeneratorError> {
    let Some(value) = metadata_scalar_string(value) else {
        return Err(metadata_schema_error(format!("{field} must be a string")));
    };
    if nonempty && value.is_empty() {
        return Err(metadata_schema_error(format!("{field} must not be empty")));
    }
    Ok(())
}

fn validate_metadata_contextless_string(
    value: &serde_yaml::Value,
    field: &str,
    nonempty: bool,
) -> Result<(), crate::s2::GeneratorError> {
    validate_metadata_string(value, field, nonempty)?;
    let value = metadata_scalar_string(value).unwrap_or_default();
    if value.contains("${{") {
        return Err(metadata_schema_error(format!(
            "{field} does not allow template expressions"
        )));
    }
    Ok(())
}

fn validate_metadata_assert_string(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let is_string = value.as_str().is_some()
        || value.as_tagged().is_some_and(|tagged| {
            tagged.tag().as_str() == "tag:yaml.org,2002:str"
                && (tagged.value().as_str().is_some() || tagged.value().is_null())
        });
    if !is_string {
        return Err(metadata_schema_error(format!("{field} must be a string")));
    }
    Ok(())
}

fn validate_runner_any_value(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::s2::GeneratorError> {
    fn visit(value: &serde_yaml::Value, field: &str) -> Result<(), String> {
        match value {
            serde_yaml::Value::String(value) if value.contains("${{") => {
                Err(format!("{field} does not allow template expressions"))
            }
            serde_yaml::Value::Tagged(tagged) => visit(tagged.value(), field),
            serde_yaml::Value::Sequence(values) => {
                for (index, value) in values.iter().enumerate() {
                    visit(value, &format!("{field}[{index}]"))?;
                }
                Ok(())
            }
            serde_yaml::Value::Mapping(values) => {
                for (key, value) in values {
                    if key.contains("${{") {
                        return Err(format!(
                            "{field} mapping keys cannot contain template expressions"
                        ));
                    }
                    visit(value, &format!("{field}.{key}"))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    visit(value, field).map_err(metadata_schema_error)
}

fn validate_composite_if(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let Some(condition) = metadata_scalar_string(value) else {
        return Err(metadata_schema_error(format!("{field} must be a string")));
    };
    let condition = condition.trim();
    if condition.is_empty() {
        return Ok(());
    }
    let wrapped;
    let expression = if condition.starts_with("${{") {
        condition
    } else {
        if condition.contains("${{") || condition.contains("}}") {
            return Err(metadata_schema_error(format!(
                "{field} must be one complete Runner condition expression"
            )));
        }
        wrapped = format!("${{{{ {condition} }}}}");
        &wrapped
    };
    validate_boolean_expression(expression, ActionExpressionContext::CompositeIf).map_err(|_| {
        metadata_schema_error(format!(
            "{field} must contain a valid Runner condition expression"
        ))
    })
}

fn validate_metadata_boolean_expression(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::s2::GeneratorError> {
    if value.is_bool() {
        return Ok(());
    }
    let Some(expression) = metadata_scalar_string(value) else {
        return Err(metadata_schema_error(format!(
            "{field} must be a boolean or a valid template expression"
        )));
    };
    validate_boolean_expression(&expression, ActionExpressionContext::CompositeBoolean).map_err(
        |_| {
            metadata_schema_error(format!(
                "{field} must be a boolean or a valid template expression"
            ))
        },
    )
}

fn validate_metadata_templates(
    value: &serde_yaml::Value,
    field: &str,
    context: ActionExpressionContext,
) -> Result<(), crate::s2::GeneratorError> {
    let Some(value) = metadata_scalar_string(value) else {
        return Err(metadata_schema_error(format!(
            "{field} must be a scalar string"
        )));
    };
    validate_template_expressions(&value, context).map_err(|_| {
        metadata_schema_error(format!("{field} must contain valid template expressions"))
    })
}

/// `TemplateReader` converts scalar YAML values to strings when a schema asks
/// for a string. Preserve that before deserializing the subset used by the
/// scanner, which models those fields as Rust strings.
fn metadata_scalar_string(value: &serde_yaml::Value) -> Option<String> {
    if let Some(value) = normalized_runner_number(value) {
        return Some(value.to_owned());
    }
    if let Some(tagged) = value.as_tagged() {
        let inner = tagged.value();
        return match tagged.tag().as_str() {
            "tag:yaml.org,2002:str" => Some(match inner.as_str() {
                Some(value) => value.to_owned(),
                None if inner.is_null() => String::new(),
                None => inner.to_string(),
            }),
            "tag:yaml.org,2002:null" if inner.is_null() => Some(String::new()),
            "tag:yaml.org,2002:bool" => inner.as_bool().map(|value| value.to_string()),
            "tag:yaml.org,2002:int" | "tag:yaml.org,2002:float" => {
                inner.as_f64().map(velnor_expression::value::format_number)
            }
            _ => None,
        };
    }
    if value.is_sequence() || value.is_mapping() || value.is_tagged() {
        return None;
    }
    Some(match value.as_str() {
        Some(value) => value.to_owned(),
        None if value.is_null() => String::new(),
        None if value.as_f64().is_some() => {
            velnor_expression::value::format_number(value.as_f64()?)
        }
        None => value.to_string(),
    })
}

fn normalize_action_manifest_strings(value: &mut serde_yaml::Value) {
    let Some(root) = value.as_mapping_mut() else {
        return;
    };
    let Some(runs) = root
        .get_mut("runs")
        .and_then(serde_yaml::Value::as_mapping_mut)
    else {
        return;
    };
    for field in [
        "using",
        "plugin",
        "main",
        "pre",
        "post",
        "image",
        "entrypoint",
        "pre-entrypoint",
        "post-entrypoint",
    ] {
        normalize_metadata_string_field(runs, field);
    }
    if let Some(steps) = runs
        .get_mut("steps")
        .and_then(serde_yaml::Value::as_sequence_mut)
    {
        for step in steps {
            let Some(step) = step.as_mapping_mut() else {
                continue;
            };
            for field in [
                "id",
                "name",
                "run",
                "uses",
                "shell",
                "if",
                "working-directory",
            ] {
                normalize_metadata_string_field(step, field);
            }
        }
    }
}

fn normalize_metadata_string_field(mapping: &mut serde_yaml::Mapping, field: &str) {
    if let Some(value) = mapping.get_mut(field)
        && let Some(normalized) = metadata_scalar_string(value)
    {
        *value = serde_yaml::Value::String(normalized);
    }
}

fn metadata_schema_error(message: impl Into<String>) -> crate::s2::GeneratorError {
    crate::s2::GeneratorError::usage(format!(
        "invalid GitHub Action metadata: {}",
        message.into()
    ))
}

fn validate_mapping(
    value: &serde_yaml::Value,
    field: &str,
) -> Result<(), crate::s2::GeneratorError> {
    if !value.is_null() && !value.is_mapping() {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata `{field}` must be a mapping"
        )));
    }
    Ok(())
}

fn local_references_for_action(
    root: &Path,
    runs: &ActionRuns,
    action_root: &str,
    files: &mut BTreeSet<String>,
    supplemental_files: &mut BTreeSet<String>,
    exclude: &[String],
) -> Result<ActionReferences, crate::s2::GeneratorError> {
    let mut file_context = ActionFileContext {
        root,
        files,
        supplemental_files,
        exclude,
    };
    local_references(runs, action_root, &mut file_context)
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NodeSourceToken {
    Identifier(String),
    String(String),
    DynamicString,
    Punctuation(char),
    Newline,
}

fn inspect_node_action_files(
    file_context: &mut ActionFileContext<'_>,
    references: &mut ActionReferences,
    action_root: &str,
    entrypoint: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let initial = normalize_repository_relative_path(action_root, entrypoint).ok_or_else(|| {
        crate::s2::GeneratorError::usage(format!(
            "GitHub Node action entrypoint `{entrypoint}` must be a static relative path"
        ))
    })?;
    let initial = super::file_walk::canonical_repository_target(file_context.root, &initial)?
        .unwrap_or(initial);
    let mut pending = VecDeque::from([initial]);
    let mut inspected = BTreeSet::new();
    while let Some(source_path) = pending.pop_front() {
        if !inspected.insert(source_path.clone()) {
            continue;
        }
        if matches!(
            Path::new(&source_path)
                .extension()
                .and_then(|value| value.to_str()),
            Some("json" | "node")
        ) {
            continue;
        }
        let path = file_context.root.join(&source_path);
        let source = std::fs::read_to_string(&path).map_err(|error| {
            crate::s2::GeneratorError::io("read Node action entrypoint", &path, &error)
        })?;
        let dependencies = node_source_dependencies(&source).map_err(|reason| {
            crate::s2::GeneratorError::usage(format!(
                "Node action entrypoint `{source_path}` contains {reason}; the scanner cannot prove its local dependencies"
            ))
        })?;
        for dependency in dependencies {
            if is_node_builtin_module(&dependency) {
                continue;
            }
            if dependency.starts_with("./")
                || dependency.starts_with("../")
                || cfg!(windows)
                    && (dependency.starts_with(".\\") || dependency.starts_with("..\\"))
            {
                let resolved =
                    resolve_node_relative_dependency(file_context.root, &source_path, &dependency)?;
                file_context.add_reference(&mut references.files, &resolved, ".")?;
                let resolved =
                    super::file_walk::canonical_repository_target(file_context.root, &resolved)?
                        .unwrap_or(resolved);
                pending.push_back(resolved);
            } else if dependency.starts_with('/')
                || dependency.starts_with('\\')
                || dependency.contains(['$', '?', '#'])
            {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "Node action entrypoint `{source_path}` imports unsupported absolute or dynamic dependency `{dependency}`"
                )));
            } else {
                include_node_package_dependency(
                    file_context,
                    references,
                    action_root,
                    &source_path,
                    &dependency,
                )?;
            }
        }
    }
    Ok(())
}

fn node_source_dependencies(source: &str) -> Result<Vec<String>, &'static str> {
    let tokens = node_source_tokens(source)?;
    let mut dependencies = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let NodeSourceToken::Identifier(identifier) = &tokens[index] else {
            index += 1;
            continue;
        };
        if index > 0 && tokens[index - 1] == NodeSourceToken::Punctuation('.') {
            index += 1;
            continue;
        }
        match identifier.as_str() {
            "require" => {
                if tokens.get(index + 1) == Some(&NodeSourceToken::Punctuation('(')) {
                    let Some(NodeSourceToken::String(specifier)) = tokens.get(index + 2) else {
                        return Err("a non-literal require call");
                    };
                    if tokens.get(index + 3) != Some(&NodeSourceToken::Punctuation(')')) {
                        return Err("a require call with unsupported arguments");
                    }
                    dependencies.push(specifier.clone());
                } else if tokens.get(index + 1) == Some(&NodeSourceToken::Punctuation('.'))
                    && tokens.get(index + 2)
                        == Some(&NodeSourceToken::Identifier("resolve".to_owned()))
                    && tokens.get(index + 3) == Some(&NodeSourceToken::Punctuation('('))
                {
                    let Some(NodeSourceToken::String(specifier)) = tokens.get(index + 4) else {
                        return Err("a non-literal require.resolve call");
                    };
                    if tokens.get(index + 5) != Some(&NodeSourceToken::Punctuation(')')) {
                        return Err("a require.resolve call with unsupported arguments");
                    }
                    dependencies.push(specifier.clone());
                }
            }
            "import" => {
                if tokens.get(index + 1) == Some(&NodeSourceToken::Punctuation('.')) {
                    // import.meta is not a module load.
                } else if tokens.get(index + 1) == Some(&NodeSourceToken::Punctuation('(')) {
                    let Some(NodeSourceToken::String(specifier)) = tokens.get(index + 2) else {
                        return Err("a non-literal dynamic import");
                    };
                    if !tokens[index + 3..].contains(&NodeSourceToken::Punctuation(')')) {
                        return Err("an unterminated dynamic import");
                    }
                    dependencies.push(specifier.clone());
                } else if let Some(NodeSourceToken::String(specifier)) = tokens.get(index + 1) {
                    dependencies.push(specifier.clone());
                } else if let Some(specifier) = node_import_from_specifier(&tokens, index + 1)? {
                    dependencies.push(specifier);
                }
            }
            "export" => {
                if let Some(specifier) = node_import_from_specifier(&tokens, index + 1)? {
                    dependencies.push(specifier);
                }
            }
            "eval" if tokens.get(index + 1) == Some(&NodeSourceToken::Punctuation('(')) => {
                return Err("an eval call");
            }
            _ => {}
        }
        index += 1;
    }
    dependencies.sort();
    dependencies.dedup();
    Ok(dependencies)
}

fn node_import_from_specifier(
    tokens: &[NodeSourceToken],
    start: usize,
) -> Result<Option<String>, &'static str> {
    let mut index = start;
    let mut depth = 0usize;
    while let Some(token) = tokens.get(index) {
        match token {
            NodeSourceToken::Punctuation(';') | NodeSourceToken::Newline if depth == 0 => {
                return Ok(None);
            }
            NodeSourceToken::Punctuation('(' | '[' | '{') => depth += 1,
            NodeSourceToken::Punctuation(')' | ']' | '}') => depth = depth.saturating_sub(1),
            NodeSourceToken::Identifier(identifier) if identifier == "from" && depth == 0 => {
                return match tokens.get(index + 1) {
                    Some(NodeSourceToken::String(specifier)) => Ok(Some(specifier.clone())),
                    Some(NodeSourceToken::DynamicString) => Err("a non-literal import source"),
                    _ => Err("an unsupported import source"),
                };
            }
            _ => {}
        }
        index += 1;
    }
    Ok(None)
}

#[expect(
    clippy::too_many_lines,
    reason = "the lexer shares position and escape state across JavaScript token classes"
)]
fn node_source_tokens(source: &str) -> Result<Vec<NodeSourceToken>, &'static str> {
    let characters = source.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        let character = characters[index];
        if character == '\n' || character == '\r' {
            if character == '\r' && characters.get(index + 1) == Some(&'\n') {
                index += 1;
            }
            tokens.push(NodeSourceToken::Newline);
            index += 1;
            continue;
        }
        if character.is_whitespace() {
            index += 1;
            continue;
        }
        if character == '/' && characters.get(index + 1) == Some(&'/') {
            index += 2;
            while index < characters.len() && !matches!(characters[index], '\n' | '\r') {
                index += 1;
            }
            continue;
        }
        if character == '/' && characters.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < characters.len()
                && !(characters[index] == '*' && characters[index + 1] == '/')
            {
                index += 1;
            }
            if index + 1 >= characters.len() {
                return Err("an unterminated comment");
            }
            index += 2;
            continue;
        }
        if character == '/' && node_regex_can_start(&tokens) {
            index += 1;
            let mut in_character_class = false;
            let mut escaped = false;
            let mut closed = false;
            while index < characters.len() {
                let current = characters[index];
                if matches!(current, '\n' | '\r') {
                    return Err("an unterminated regular expression");
                }
                if escaped {
                    escaped = false;
                } else if current == '\\' {
                    escaped = true;
                } else if current == '[' {
                    in_character_class = true;
                } else if current == ']' {
                    in_character_class = false;
                } else if current == '/' && !in_character_class {
                    index += 1;
                    while index < characters.len() && characters[index].is_ascii_alphabetic() {
                        index += 1;
                    }
                    closed = true;
                    break;
                }
                index += 1;
            }
            if !closed {
                return Err("an unterminated regular expression");
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            let (value, next) = node_quoted_string(&characters, index, character)?;
            tokens.push(NodeSourceToken::String(value));
            index = next;
            continue;
        }
        if character == '`' {
            let (value, dynamic, next) = node_template_string(&characters, index)?;
            tokens.push(if dynamic {
                NodeSourceToken::DynamicString
            } else {
                NodeSourceToken::String(value)
            });
            index = next;
            continue;
        }
        if character == '_' || character == '$' || character.is_alphabetic() {
            let start = index;
            index += 1;
            while index < characters.len()
                && (characters[index] == '_'
                    || characters[index] == '$'
                    || characters[index].is_alphanumeric())
            {
                index += 1;
            }
            tokens.push(NodeSourceToken::Identifier(
                characters[start..index].iter().collect(),
            ));
            continue;
        }
        tokens.push(NodeSourceToken::Punctuation(character));
        index += 1;
    }
    Ok(tokens)
}

fn node_regex_can_start(tokens: &[NodeSourceToken]) -> bool {
    match tokens.last() {
        None
        | Some(
            NodeSourceToken::Newline
            | NodeSourceToken::Punctuation(
                '(' | '[' | '{' | '=' | ':' | ',' | ';' | '!' | '?' | '&' | '|' | '\n',
            ),
        ) => true,
        Some(NodeSourceToken::Identifier(identifier)) => matches!(
            identifier.as_str(),
            "return"
                | "throw"
                | "case"
                | "delete"
                | "void"
                | "typeof"
                | "instanceof"
                | "in"
                | "of"
                | "yield"
                | "await"
        ),
        _ => false,
    }
}

fn node_quoted_string(
    characters: &[char],
    start: usize,
    quote: char,
) -> Result<(String, usize), &'static str> {
    let mut value = String::new();
    let mut index = start + 1;
    while index < characters.len() {
        let character = characters[index];
        if character == quote {
            return Ok((value, index + 1));
        }
        if matches!(character, '\n' | '\r') {
            return Err("an unterminated string literal");
        }
        if character != '\\' {
            value.push(character);
            index += 1;
            continue;
        }
        index += 1;
        let Some(escaped) = characters.get(index).copied() else {
            return Err("an unterminated string escape");
        };
        match escaped {
            '\n' => index += 1,
            '\r' if characters.get(index + 1) == Some(&'\n') => index += 2,
            '\\' => {
                value.push('\\');
                index += 1;
            }
            '\'' => {
                value.push('\'');
                index += 1;
            }
            '"' => {
                value.push('"');
                index += 1;
            }
            '/' => {
                value.push('/');
                index += 1;
            }
            'n' => {
                value.push('\n');
                index += 1;
            }
            'r' => {
                value.push('\r');
                index += 1;
            }
            't' => {
                value.push('\t');
                index += 1;
            }
            other => {
                value.push(other);
                index += 1;
            }
        }
    }
    Err("an unterminated string literal")
}

fn node_template_string(
    characters: &[char],
    start: usize,
) -> Result<(String, bool, usize), &'static str> {
    let mut value = String::new();
    let mut dynamic = false;
    let mut escaped = false;
    let mut index = start + 1;
    while index < characters.len() {
        let character = characters[index];
        if escaped {
            value.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '`' {
            return Ok((value, dynamic, index + 1));
        } else if character == '$' && characters.get(index + 1) == Some(&'{') {
            dynamic = true;
            value.push(character);
        } else {
            value.push(character);
        }
        index += 1;
    }
    Err("an unterminated template string")
}

fn is_node_builtin_module(specifier: &str) -> bool {
    let specifier = specifier.strip_prefix("node:").unwrap_or(specifier);
    matches!(
        specifier,
        "assert"
            | "assert/strict"
            | "async_hooks"
            | "buffer"
            | "child_process"
            | "cluster"
            | "console"
            | "constants"
            | "crypto"
            | "dgram"
            | "diagnostics_channel"
            | "dns"
            | "dns/promises"
            | "domain"
            | "events"
            | "fs"
            | "fs/promises"
            | "http"
            | "http2"
            | "https"
            | "module"
            | "net"
            | "os"
            | "path"
            | "path/posix"
            | "path/win32"
            | "perf_hooks"
            | "process"
            | "punycode"
            | "querystring"
            | "readline"
            | "readline/promises"
            | "repl"
            | "stream"
            | "stream/promises"
            | "stream/consumers"
            | "stream/web"
            | "string_decoder"
            | "sys"
            | "timers"
            | "timers/promises"
            | "tls"
            | "trace_events"
            | "tty"
            | "url"
            | "util"
            | "util/types"
            | "v8"
            | "vm"
            | "wasi"
            | "worker_threads"
            | "zlib"
    )
}

fn resolve_node_relative_dependency(
    root: &Path,
    source_path: &str,
    specifier: &str,
) -> Result<String, crate::s2::GeneratorError> {
    let specifier = if cfg!(windows) {
        specifier.replace('\\', "/")
    } else {
        specifier.to_owned()
    };
    let base = parent_path(source_path);
    let candidate = normalize_repository_relative_path(&base, &specifier).ok_or_else(|| {
        crate::s2::GeneratorError::usage(format!(
            "Node action entrypoint `{source_path}` dependency `{specifier}` escapes the checkout"
        ))
    })?;
    if let Some(resolved) = resolve_node_file(root, &candidate, 0)? {
        return Ok(resolved);
    }
    Err(crate::s2::GeneratorError::usage(format!(
        "Node action entrypoint `{source_path}` imports missing dependency `{specifier}`"
    )))
}

fn resolve_node_file(
    root: &Path,
    candidate: &str,
    depth: usize,
) -> Result<Option<String>, crate::s2::GeneratorError> {
    if depth > 8 {
        return Ok(None);
    }
    let path = root.join(candidate);
    if path.is_file() {
        return Ok(Some(candidate.to_owned()));
    }
    for extension in [".js", ".json", ".node", ".mjs", ".cjs"] {
        let with_extension = format!("{candidate}{extension}");
        if root.join(&with_extension).is_file() {
            return Ok(Some(with_extension));
        }
    }
    if !path.is_dir() {
        return Ok(None);
    }
    let package_json = format!("{candidate}/package.json");
    if root.join(&package_json).is_file() {
        let package_path = root.join(&package_json);
        let contents = std::fs::read_to_string(&package_path).map_err(|error| {
            crate::s2::GeneratorError::io("read Node package metadata", &package_path, &error)
        })?;
        let package: serde_json::Value = serde_json::from_str(&contents).map_err(|error| {
            crate::s2::GeneratorError::usage(format!(
                "Node package metadata `{package_json}` is invalid JSON: {error}"
            ))
        })?;
        if package.get("exports").is_some() {
            return Err(crate::s2::GeneratorError::usage(format!(
                "Node package `{candidate}` uses conditional exports that the scanner cannot resolve"
            )));
        }
        if let Some(main) = package.get("main").and_then(serde_json::Value::as_str) {
            let main = normalize_repository_relative_path(candidate, main).ok_or_else(|| {
                crate::s2::GeneratorError::usage(format!(
                    "Node package `{candidate}` has unsafe main entrypoint `{main}`"
                ))
            })?;
            if let Some(resolved) = resolve_node_file(root, &main, depth + 1)? {
                return Ok(Some(resolved));
            }
        }
    }
    for index in [
        "index.js",
        "index.json",
        "index.node",
        "index.mjs",
        "index.cjs",
    ] {
        let index_path = format!("{candidate}/{index}");
        if root.join(&index_path).is_file() {
            return Ok(Some(index_path));
        }
    }
    Ok(None)
}

fn include_node_package_dependency(
    file_context: &mut ActionFileContext<'_>,
    references: &mut ActionReferences,
    action_root: &str,
    source_path: &str,
    specifier: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let package_end = if specifier.starts_with('@') {
        specifier.match_indices('/').nth(1).map(|(index, _)| index)
    } else {
        specifier.find('/')
    }
    .unwrap_or(specifier.len());
    let package_name = &specifier[..package_end];
    if package_name.is_empty() || package_name == "." || package_name == ".." {
        return Err(crate::s2::GeneratorError::usage(format!(
            "Node action entrypoint `{source_path}` imports unsupported dependency `{specifier}`"
        )));
    }
    let mut directory = parent_path(source_path);
    loop {
        let package_root =
            super::file_walk::join_repo_path(&directory, &format!("node_modules/{package_name}"));
        if file_context.root.join(&package_root).exists() {
            let package_files = super::file_walk::selected_action_files(
                file_context.root,
                &package_root,
                file_context.exclude,
            )?;
            if action_root != "." && !package_root.starts_with(&format!("{action_root}/")) {
                references.watch_directories.insert(package_root);
            }
            for package_file in package_files {
                file_context.add_reference(&mut references.files, &package_file, ".")?;
            }
            return Ok(());
        }
        if directory == "." {
            break;
        }
        directory = parent_path(&directory);
    }
    // A package absent from the checkout is supplied by the runner image and
    // is not a repository change input.
    Ok(())
}

fn local_references(
    runs: &ActionRuns,
    action_root: &str,
    file_context: &mut ActionFileContext<'_>,
) -> Result<ActionReferences, crate::s2::GeneratorError> {
    let mut references = ActionReferences::default();
    if runs.plugin.is_some() {
        // Runner resolves this name through RunnerPluginManager. It has no
        // repository entrypoint or Docker image to scan.
        return Ok(references);
    }
    let using = runs
        .using
        .as_deref()
        .ok_or_else(|| metadata_schema_error("runs.using is required"))?
        .to_ascii_lowercase();
    match using.as_str() {
        "composite" => {
            return composite_action_references(runs, action_root, file_context);
        }
        "node12" | "node16" | "node20" | "node24" => {
            let main = runs.main.as_deref().ok_or_else(|| {
                crate::s2::GeneratorError::usage(format!(
                    "GitHub JavaScript action ({using}) metadata must declare runs.main"
                ))
            })?;
            file_context.add_reference(&mut references.files, main, action_root)?;
            inspect_node_action_files(file_context, &mut references, action_root, main)?;
            for reference in [&runs.pre, &runs.post] {
                if let Some(reference) = reference.as_deref() {
                    file_context.add_reference(&mut references.files, reference, action_root)?;
                    inspect_node_action_files(
                        file_context,
                        &mut references,
                        action_root,
                        reference,
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
                let dockerfile_path = normalize_repository_relative_path(action_root, image)
                    .ok_or_else(|| {
                        crate::s2::GeneratorError::usage(format!(
                            "GitHub Action Dockerfile `{image}` escapes the repository"
                        ))
                    })?;
                let build_context = parent_path(&dockerfile_path);
                if build_context != action_root
                    && !(action_root == "."
                        || build_context.starts_with(&format!("{action_root}/")))
                {
                    references.watch_directories.insert(build_context);
                }
                file_context.add_reference(&mut references.files, image, action_root)?;
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
    Ok(references)
}

fn composite_action_references(
    runs: &ActionRuns,
    action_root: &str,
    file_context: &mut ActionFileContext<'_>,
) -> Result<ActionReferences, crate::s2::GeneratorError> {
    let mut references = ActionReferences::default();
    let steps = runs.steps.as_deref().ok_or_else(|| {
        crate::s2::GeneratorError::usage("GitHub composite action metadata must declare runs.steps")
    })?;
    for step in steps {
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
            let shell = step.shell.as_deref().ok_or_else(|| {
                crate::s2::GeneratorError::usage(
                    "GitHub composite action run step must declare shell",
                )
            })?;
            let run = resolve_action_path_expression(run);
            let working_directory =
                composite_working_directory(step.working_directory.as_deref(), action_root)?;
            let shell_references = shell_references_in_action(
                &run,
                &working_directory,
                action_root,
                file_context.files,
                shell,
            );
            if !shell_references.is_empty() {
                for reference in shell_references {
                    add_composite_run_reference(
                        &mut references.files,
                        &reference,
                        action_root,
                        &working_directory,
                        file_context,
                    )?;
                }
            }
        }
        if let Some(uses) = &step.uses
            && uses.is_empty()
        {
            return Err(crate::s2::GeneratorError::usage(
                "GitHub composite action uses value must not be empty",
            ));
        }
        if let Some(uses) = &step.uses {
            if looks_like_local_action_reference(uses) {
                references
                    .actions
                    .insert(normalize_local_action_directory(uses)?);
            } else if let Some(path) = self_repository_action_path(uses)? {
                references.actions.insert(path);
            } else if let Some(image) = uses.strip_prefix("docker://") {
                if image.trim().is_empty() {
                    return Err(crate::s2::GeneratorError::usage(
                        "GitHub composite container action `uses` must name an image",
                    ));
                }
            } else if !is_full_sha_action_reference(uses) {
                // Runner accepts tags here; Velnor applies the repository-wide
                // full-SHA action pin policy.
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub composite external action `{uses}` must use {{org}}/{{repo}}[/path]@ref with a full 40-character SHA pin"
                )));
            }
        }
    }
    Ok(references)
}

fn looks_like_local_action_reference(value: &str) -> bool {
    value.starts_with("./") || value.starts_with(".\\")
}

fn self_repository_action_path(value: &str) -> Result<Option<String>, crate::s2::GeneratorError> {
    if !value.starts_with("$/") {
        return Ok(None);
    }
    Err(crate::s2::GeneratorError::usage(format!(
        "GitHub self-repository action `{value}` requires the workflow repository and ref; the scanner has no workflow run context"
    )))
}

fn normalize_local_action_directory(reference: &str) -> Result<String, crate::s2::GeneratorError> {
    let (path, windows_separators) = if let Some(path) = reference.strip_prefix("./") {
        (path, cfg!(windows))
    } else if let Some(path) = reference.strip_prefix(".\\") {
        if cfg!(windows) {
            (path, true)
        } else {
            // On Unix, .NET Path.Combine treats backslashes as filename
            // characters. Preserve the raw reference so the scanner selects
            // the same path Runner would inspect on this host.
            (reference, false)
        }
    } else {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite local action `{reference}` must be a workspace-relative path"
        )));
    };
    let normalized_path = if windows_separators {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let path = normalized_path.as_str();
    if path.is_empty() {
        return Ok(".".to_owned());
    }
    if path.contains('$')
        || reference.contains("{{")
        || reference.contains("}}")
        || path.starts_with('/')
        || reference.chars().any(char::is_control)
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite local action `{reference}` must be a static workspace-relative path"
        )));
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." if components.pop().is_some() => {}
            ".." => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub composite local action `{reference}` escapes the workspace"
                )));
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(components.join("/"))
    }
}

fn is_full_sha_action_reference(value: &str) -> bool {
    is_valid_repository_action_reference(value)
        .is_some_and(|revision| !value.contains(char::is_whitespace) && is_full_revision(revision))
}

fn is_valid_repository_action_reference(value: &str) -> Option<&str> {
    let mut uses_segments = value.split('@');
    let action = uses_segments.next()?;
    let revision = uses_segments.next()?;
    if uses_segments.next().is_some() || revision.is_empty() {
        return None;
    }
    let path_segments = action
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    (path_segments.len() >= 2
        && path_segments
            .iter()
            .all(|segment| !segment.is_empty() && !matches!(*segment, "." | "..")))
    .then_some(revision)
}

/// Match actions/runner's Dockerfile test instead of guessing from an image
/// tag.  An ordinary image such as `ubuntu` is resolved by the container
/// runtime; only a basename named `Dockerfile` or beginning `Dockerfile.` is
/// a host-side build source.
fn is_dockerfile_reference(value: &str) -> bool {
    if value
        .get(.."docker://".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("docker://"))
    {
        return false;
    }
    let basename = if cfg!(windows) {
        value.rsplit(['/', '\\']).next().unwrap_or(value)
    } else {
        value.rsplit('/').next().unwrap_or(value)
    };
    let basename = basename.to_ascii_lowercase();
    basename == "dockerfile"
        || basename.starts_with("dockerfile.")
        || basename.ends_with("dockerfile")
}

/// Preserve Runner's action-path context while making its expression a single
/// shell token. Other expressions stay opaque and fail when used as file paths.
fn resolve_action_path_expression(value: &str) -> String {
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
            resolved.push_str(GITHUB_ACTION_PATH_MARKER);
        } else if expression.eq_ignore_ascii_case("github.workspace") {
            resolved.push_str(GITHUB_WORKSPACE_MARKER);
        } else {
            resolved.push_str(&value[start..end]);
        }
        cursor = end;
    }
    resolved.push_str(&value[cursor..]);
    resolved
}

fn composite_working_directory(
    working_directory: Option<&str>,
    action_root: &str,
) -> Result<String, crate::s2::GeneratorError> {
    let Some(working_directory) = working_directory else {
        return Ok(".".to_owned());
    };
    let working_directory = resolve_action_path_expression(working_directory);
    if let Some(suffix) = working_directory.strip_prefix(GITHUB_ACTION_PATH_MARKER) {
        if working_directory.matches(GITHUB_ACTION_PATH_MARKER).count() != 1
            || working_directory.contains(GITHUB_WORKSPACE_MARKER)
        {
            return Err(crate::s2::GeneratorError::usage(
                "GitHub Action `working-directory` must be a static workspace-relative path",
            ));
        }
        return append_static_directory(action_root, suffix);
    }
    if let Some(suffix) = working_directory.strip_prefix(GITHUB_WORKSPACE_MARKER) {
        if working_directory.matches(GITHUB_WORKSPACE_MARKER).count() != 1
            || working_directory.contains(GITHUB_ACTION_PATH_MARKER)
        {
            return Err(crate::s2::GeneratorError::usage(
                "GitHub Action `working-directory` must be a static workspace-relative path",
            ));
        }
        return append_static_directory(".", suffix);
    }
    if working_directory.contains(GITHUB_ACTION_PATH_MARKER)
        || working_directory.contains(GITHUB_WORKSPACE_MARKER)
    {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub Action `working-directory` must be a static workspace-relative path",
        ));
    }
    let reference = if working_directory.starts_with("./") || working_directory.starts_with(".\\") {
        working_directory
    } else {
        format!("./{working_directory}")
    };
    normalize_local_action_directory(&reference)
}

fn add_composite_run_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    action_root: &str,
    working_directory: &str,
    file_context: &mut ActionFileContext<'_>,
) -> Result<(), crate::s2::GeneratorError> {
    if reference == UNSUPPORTED_SHELL_CWD_MARKER {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub composite action run contains unsupported shell syntax that can hide a repository path or working-directory change",
        ));
    }
    let (base, path) = if let Some(suffix) = reference.strip_prefix(GITHUB_ACTION_PATH_MARKER) {
        if reference.matches(GITHUB_ACTION_PATH_MARKER).count() != 1
            || reference.contains(GITHUB_WORKSPACE_MARKER)
        {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub Action local entrypoint `{reference}` must be a static relative path"
            )));
        }
        (action_root, expression_relative_path(suffix, reference)?)
    } else if let Some(suffix) = reference.strip_prefix(GITHUB_WORKSPACE_MARKER) {
        if reference.matches(GITHUB_WORKSPACE_MARKER).count() != 1
            || reference.contains(GITHUB_ACTION_PATH_MARKER)
        {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub Action local entrypoint `{reference}` must be a static relative path"
            )));
        }
        (".", expression_relative_path(suffix, reference)?)
    } else if reference.contains(GITHUB_ACTION_PATH_MARKER)
        || reference.contains(GITHUB_WORKSPACE_MARKER)
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` must be a static relative path"
        )));
    } else {
        (working_directory, reference.to_owned())
    };
    if path.contains("${{") || path.contains("{{") || path.contains("}}") || path.contains('$') {
        return file_context.add_reference(references, &path, base);
    }
    let Some(repository_path) = normalize_repository_relative_path(base, &path) else {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` escapes its action directory"
        )));
    };
    let has_glob = path
        .chars()
        .any(|character| matches!(character, '*' | '?' | '[' | '{'));
    if !has_glob {
        return file_context.add_reference(references, &path, base);
    }

    let literal_match = file_context.files.contains(&repository_path);
    let path_matcher = globset::GlobBuilder::new(&repository_path)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .ok()
        .map(|glob| glob.compile_matcher());
    let mut matched_paths = BTreeSet::new();
    if literal_match {
        matched_paths.insert(repository_path.clone());
    }
    if let Some(path_matcher) = path_matcher {
        matched_paths.extend(
            file_context
                .files
                .iter()
                .filter(|file| path_matcher.is_match(file))
                .cloned(),
        );
    }
    if matched_paths.is_empty() {
        return file_context.add_reference(references, &path, base);
    }
    for matched in matched_paths {
        file_context.add_reference(references, &matched, ".")?;
    }
    Ok(())
}

fn expression_relative_path(
    suffix: &str,
    reference: &str,
) -> Result<String, crate::s2::GeneratorError> {
    let Some(suffix) = suffix
        .strip_prefix('/')
        .or_else(|| suffix.strip_prefix('\\'))
    else {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` must be a static relative path"
        )));
    };
    Ok(format!("./{suffix}"))
}

fn append_static_directory(base: &str, suffix: &str) -> Result<String, crate::s2::GeneratorError> {
    if suffix.is_empty() {
        return Ok(base.to_owned());
    }
    if !cfg!(windows) && suffix.starts_with('\\') {
        let combined = format!("{base}{suffix}");
        return normalize_repository_relative_path(".", &combined).ok_or_else(|| {
            crate::s2::GeneratorError::usage(
                "GitHub Action `working-directory` must be a static workspace-relative path",
            )
        });
    }
    let Some(suffix) = suffix.strip_prefix('/').or_else(|| {
        if cfg!(windows) {
            suffix.strip_prefix('\\')
        } else {
            None
        }
    }) else {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub Action `working-directory` must be a static workspace-relative path",
        ));
    };
    let suffix = if cfg!(windows) {
        suffix.replace('\\', "/")
    } else {
        suffix.to_owned()
    };
    normalize_local_action_directory(&format!("./{base}/{suffix}"))
}

fn validate_composite_step(step: &ActionStep) -> Result<(), crate::s2::GeneratorError> {
    // Keep GitHub's expression-bearing fields opaque, but preserve their
    // mapping/value shapes instead of silently treating malformed metadata as
    // a valid action. `if`, `id`, `name`, and `working-directory` remain
    // untouched by the detector and therefore retain the action's semantics.
    validate_mapping(&step.with, "step.with")?;
    validate_mapping(&step.env, "step.env")?;
    if let Some(continue_on_error) = &step.continue_on_error {
        validate_metadata_boolean_expression(continue_on_error, "step.continue-on-error")?;
    }
    if step.id.as_deref().is_some_and(str::is_empty) {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub composite action step `id` must not be empty",
        ));
    }
    Ok(())
}

fn add_local_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    action_root: &str,
    files: &BTreeSet<String>,
) -> Result<(), crate::s2::GeneratorError> {
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
    let Some(repository_path) = normalize_repository_relative_path(action_root, reference) else {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` escapes its action directory"
        )));
    };
    if !files.contains(&repository_path) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action entrypoint `{reference}` resolves to missing file `{repository_path}`"
        )));
    }
    references.insert(repository_path);
    Ok(())
}

fn normalize_repository_relative_path(base: &str, reference: &str) -> Option<String> {
    let normalized_reference = if cfg!(windows) {
        reference.replace('\\', "/")
    } else {
        reference.to_owned()
    };
    if normalized_reference.is_empty()
        || normalized_reference.starts_with('/')
        || cfg!(windows) && is_windows_drive_prefix(&normalized_reference)
        || normalized_reference.contains('$')
        || normalized_reference.contains("{{")
        || normalized_reference.contains("}}")
        || normalized_reference.chars().any(char::is_control)
    {
        return None;
    }
    let mut components = Vec::new();
    for component in base.split('/') {
        match component {
            "" | "." => {}
            ".." => return None,
            value => components.push(value),
        }
    }
    for component in normalized_reference.split('/') {
        match component {
            "" | "." => {}
            ".." if components.pop().is_some() => {}
            ".." => return None,
            value => components.push(value),
        }
    }
    Some(if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    })
}

fn is_safe_relative_reference(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.starts_with('/')
        && !reference.chars().any(char::is_control)
        && !reference.contains('$')
        && !reference.contains("{{")
        && !reference.contains("}}")
        && !reference.split('/').any(|component| component == "..")
}

fn is_safe_repository_file_path(path: &str) -> bool {
    is_safe_relative_reference(path) && !path.split('/').any(str::is_empty)
}

fn is_windows_drive_prefix(value: &str) -> bool {
    value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
}

#[derive(Clone, Debug)]
enum ShellNode {
    Simple(Vec<String>),
    Sequence(Box<ShellNode>, Box<ShellNode>),
    AndIf(Box<ShellNode>, Box<ShellNode>),
    OrIf(Box<ShellNode>, Box<ShellNode>),
    Pipeline(Vec<ShellNode>),
    Background(Box<ShellNode>),
    Subshell(Box<ShellNode>),
    BraceGroup(Box<ShellNode>),
    If {
        condition: Box<ShellNode>,
        consequence: Box<ShellNode>,
        alternative: Option<Box<ShellNode>>,
    },
}

#[derive(Default)]
struct ShellOutcome {
    succeeded: BTreeSet<String>,
    failed: BTreeSet<String>,
}

enum ShellCdTarget {
    NotCd,
    Invalid,
    Directory(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellDialect {
    Posix,
    PowerShell,
    Cmd,
    Python,
    Other,
}

/// Classify the command Runner uses for an action run step. v2.337.0 lists
/// `bash`, `sh`, `cmd`, `powershell`, and `pwsh` as built-ins. Its action-step
/// argument table also gives `python` a default format. Custom command format
/// strings are classified by their command token when it names a known shell.
fn runner_shell_dialect(shell: &str) -> ShellDialect {
    if shell.is_empty() {
        // ScriptHandler defaults empty action shells to pwsh on Windows and
        // bash elsewhere in Runner v2.337.0.
        return if cfg!(windows) {
            ShellDialect::PowerShell
        } else {
            ShellDialect::Posix
        };
    }
    let command = shell.split_once(' ').map_or(shell, |(command, _)| command);
    let command = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let command = command.to_ascii_lowercase();
    let command = command.strip_suffix(".exe").unwrap_or(&command);
    match command {
        "bash" | "sh" => ShellDialect::Posix,
        "pwsh" | "powershell" => ShellDialect::PowerShell,
        "cmd" => ShellDialect::Cmd,
        "python" => ShellDialect::Python,
        _ => ShellDialect::Other,
    }
}

struct ShellReferenceContext<'a> {
    working_directory: &'a str,
    action_root: &'a str,
    files: Option<&'a BTreeSet<String>>,
    dialect: ShellDialect,
}

impl ShellReferenceContext<'_> {
    fn directory_exists(&self, working_directory: &str) -> bool {
        let Some(files) = self.files else {
            return true;
        };
        let directory = resolve_shell_directory(self.working_directory, working_directory);
        let (base, suffix) = if let Some(suffix) = directory.strip_prefix(GITHUB_ACTION_PATH_MARKER)
        {
            (self.action_root, suffix.trim_start_matches('/'))
        } else if let Some(suffix) = directory.strip_prefix(GITHUB_WORKSPACE_MARKER) {
            (".", suffix.trim_start_matches('/'))
        } else if directory.starts_with('/')
            || is_windows_drive_prefix(&directory)
            || directory.starts_with('$')
            || directory.starts_with('~')
        {
            // Outside-repository and dynamic directories stay possible. Any
            // later file reference will fail the static-path check below.
            return true;
        } else {
            (".", directory.as_str())
        };
        let path = if suffix.is_empty() {
            base.to_owned()
        } else {
            super::file_walk::join_repo_path(base, suffix)
        };
        if path == "." {
            return true;
        }
        if path.split('/').any(|component| component == "..") {
            return true;
        }
        let prefix = format!("{}/", path.trim_end_matches('/'));
        files.iter().any(|file| file.starts_with(&prefix))
    }
}

struct ShellParser {
    tokens: Vec<String>,
    position: usize,
}

impl ShellParser {
    fn new(tokens: Vec<String>) -> Self {
        Self {
            tokens,
            position: 0,
        }
    }

    fn parse(mut self) -> Result<Option<ShellNode>, ()> {
        if self.tokens.is_empty() {
            return Ok(None);
        }
        if self
            .tokens
            .iter()
            .any(|token| token == INVALID_SHELL_TOKEN_MARKER)
        {
            return Err(());
        }
        let expression = self.parse_list(&[])?;
        if self.position != self.tokens.len() {
            return Err(());
        }
        Ok(Some(expression))
    }

    fn parse_list(&mut self, closing: &[&str]) -> Result<ShellNode, ()> {
        while self.peek() == Some(";") {
            self.position += 1;
        }
        if self.is_closing(closing) {
            return Ok(ShellNode::Simple(Vec::new()));
        }
        let mut list = None;
        let mut command = self.parse_and_or(closing)?;
        loop {
            if self.is_closing(closing) || self.peek().is_none() {
                return Ok(append_shell_list_command(list, command));
            }
            match self.peek() {
                Some(";") => {
                    self.position += 1;
                    list = Some(append_shell_list_command(list, command));
                    while self.peek() == Some(";") {
                        self.position += 1;
                    }
                    if self.is_closing(closing) || self.peek().is_none() {
                        return Ok(list.unwrap_or_else(|| ShellNode::Simple(Vec::new())));
                    }
                    command = self.parse_and_or(closing)?;
                }
                Some("&") => {
                    self.position += 1;
                    list = Some(append_shell_list_command(
                        list,
                        ShellNode::Background(Box::new(command)),
                    ));
                    if self.is_closing(closing) || self.peek().is_none() {
                        return Ok(list.unwrap_or_else(|| ShellNode::Simple(Vec::new())));
                    }
                    command = self.parse_and_or(closing)?;
                }
                _ => return Err(()),
            }
        }
    }

    fn parse_and_or(&mut self, closing: &[&str]) -> Result<ShellNode, ()> {
        let mut expression = self.parse_pipeline(closing)?;
        loop {
            match self.peek() {
                Some("&&") => {
                    self.position += 1;
                    let next = self.parse_pipeline(closing)?;
                    expression = ShellNode::AndIf(Box::new(expression), Box::new(next));
                }
                Some("||") => {
                    self.position += 1;
                    let next = self.parse_pipeline(closing)?;
                    expression = ShellNode::OrIf(Box::new(expression), Box::new(next));
                }
                _ => return Ok(expression),
            }
        }
    }

    fn parse_pipeline(&mut self, closing: &[&str]) -> Result<ShellNode, ()> {
        let mut commands = vec![self.parse_command(closing)?];
        while matches!(self.peek(), Some("|" | "|&")) {
            self.position += 1;
            commands.push(self.parse_command(closing)?);
        }
        if commands.len() == 1 {
            Ok(commands.remove(0))
        } else {
            Ok(ShellNode::Pipeline(commands))
        }
    }

    fn parse_command(&mut self, closing: &[&str]) -> Result<ShellNode, ()> {
        match self.peek() {
            Some("if") => self.parse_if_command(),
            Some("(") => {
                self.position += 1;
                let expression = self.parse_list(&[")"])?;
                self.expect(")")?;
                Ok(ShellNode::Subshell(Box::new(expression)))
            }
            Some("{") => {
                self.position += 1;
                let expression = self.parse_list(&["}"])?;
                self.expect("}")?;
                Ok(ShellNode::BraceGroup(Box::new(expression)))
            }
            _ => {
                let mut command = Vec::new();
                while let Some(token) = self.peek() {
                    if self.is_closing(closing)
                        || matches!(token, ";" | "&&" | "||" | "|" | "|&" | "&" | ")" | "}")
                    {
                        break;
                    }
                    if matches!(token, "(" | "{") {
                        return Err(());
                    }
                    command.push(token.to_owned());
                    self.position += 1;
                }
                if command.is_empty() {
                    return Err(());
                }
                let command_index = shell_command_index(&command);
                if command.get(command_index).is_some_and(|word| {
                    matches!(
                        word.as_str(),
                        "then"
                            | "else"
                            | "elif"
                            | "fi"
                            | "while"
                            | "until"
                            | "for"
                            | "select"
                            | "do"
                            | "done"
                            | "case"
                            | "esac"
                            | "function"
                            | "coproc"
                            | "!"
                    )
                }) {
                    return Err(());
                }
                Ok(ShellNode::Simple(command))
            }
        }
    }

    fn parse_if_command(&mut self) -> Result<ShellNode, ()> {
        self.expect("if")?;
        self.parse_if_clause()
    }

    fn parse_if_clause(&mut self) -> Result<ShellNode, ()> {
        let condition = self.parse_list(&["then"])?;
        self.expect("then")?;
        let consequence = self.parse_list(&["elif", "else", "fi"])?;
        let alternative = match self.peek() {
            Some("elif") => {
                self.position += 1;
                Some(Box::new(self.parse_if_clause()?))
            }
            Some("else") => {
                self.position += 1;
                let alternative = self.parse_list(&["fi"])?;
                self.expect("fi")?;
                Some(Box::new(alternative))
            }
            Some("fi") => {
                self.position += 1;
                None
            }
            _ => return Err(()),
        };
        Ok(ShellNode::If {
            condition: Box::new(condition),
            consequence: Box::new(consequence),
            alternative,
        })
    }

    fn expect(&mut self, expected: &str) -> Result<(), ()> {
        if self.peek() == Some(expected) {
            self.position += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    fn is_closing(&self, closings: &[&str]) -> bool {
        closings.iter().any(|closing| self.peek() == Some(closing))
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.position).map(String::as_str)
    }
}

fn append_shell_list_command(list: Option<ShellNode>, command: ShellNode) -> ShellNode {
    match list {
        Some(list) => ShellNode::Sequence(Box::new(list), Box::new(command)),
        None => command,
    }
}

#[cfg(test)]
fn shell_references(command: &str) -> Vec<String> {
    shell_references_with_context(
        command,
        &ShellReferenceContext {
            working_directory: ".",
            action_root: ".",
            files: None,
            dialect: ShellDialect::Posix,
        },
    )
}

fn shell_references_in_action(
    command: &str,
    working_directory: &str,
    action_root: &str,
    files: &BTreeSet<String>,
    shell: &str,
) -> Vec<String> {
    shell_references_with_context(
        command,
        &ShellReferenceContext {
            working_directory,
            action_root,
            files: Some(files),
            dialect: runner_shell_dialect(shell),
        },
    )
}

fn shell_references_with_context(
    command: &str,
    context: &ShellReferenceContext<'_>,
) -> Vec<String> {
    if matches!(context.dialect, ShellDialect::Python | ShellDialect::Other) {
        // Arbitrary Python and custom-shell programs can compute file paths at
        // runtime. The action scanner supports only the shell dialects it can
        // parse statically and rejects the rest instead of dropping inputs.
        return if command.trim().is_empty() {
            Vec::new()
        } else {
            vec![UNSUPPORTED_SHELL_CWD_MARKER.to_owned()]
        };
    }
    let expression = match parse_shell_expression(command, context.dialect) {
        Ok(Some(expression)) => expression,
        Ok(None) => return Vec::new(),
        Err(()) => return vec![UNSUPPORTED_SHELL_CWD_MARKER.to_owned()],
    };
    let initial = BTreeSet::from([String::new()]);
    let mut references = Vec::new();
    let _ = analyze_shell_node(&expression, &initial, context, &mut references);
    references
}

fn parse_shell_expression(command: &str, dialect: ShellDialect) -> Result<Option<ShellNode>, ()> {
    ShellParser::new(shell_tokens_for_dialect(command, dialect)).parse()
}

fn analyze_shell_node(
    expression: &ShellNode,
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) -> ShellOutcome {
    match expression {
        ShellNode::Simple(command) => {
            analyze_shell_command(command, working_directories, context, references)
        }
        ShellNode::Sequence(left, right) => {
            let left = analyze_shell_node(left, working_directories, context, references);
            let mut next = left.succeeded;
            next.extend(left.failed);
            if next.is_empty() {
                return ShellOutcome::default();
            }
            analyze_shell_node(right, &next, context, references)
        }
        ShellNode::AndIf(left, right) => {
            let left = analyze_shell_node(left, working_directories, context, references);
            let right = analyze_shell_node(right, &left.succeeded, context, references);
            let mut failed = left.failed;
            failed.extend(right.failed);
            ShellOutcome {
                succeeded: right.succeeded,
                failed,
            }
        }
        ShellNode::OrIf(left, right) => {
            let left = analyze_shell_node(left, working_directories, context, references);
            let right = analyze_shell_node(right, &left.failed, context, references);
            let mut succeeded = left.succeeded;
            succeeded.extend(right.succeeded);
            ShellOutcome {
                succeeded,
                failed: right.failed,
            }
        }
        ShellNode::Pipeline(commands) => {
            for command in commands {
                let _ = analyze_shell_node(command, working_directories, context, references);
            }
            ShellOutcome {
                succeeded: working_directories.clone(),
                failed: working_directories.clone(),
            }
        }
        ShellNode::Background(command) | ShellNode::Subshell(command) => {
            let _ = analyze_shell_node(command, working_directories, context, references);
            ShellOutcome {
                succeeded: working_directories.clone(),
                failed: working_directories.clone(),
            }
        }
        ShellNode::BraceGroup(command) => {
            analyze_shell_node(command, working_directories, context, references)
        }
        ShellNode::If {
            condition,
            consequence,
            alternative,
        } => {
            let condition = analyze_shell_node(condition, working_directories, context, references);
            let consequence =
                analyze_shell_node(consequence, &condition.succeeded, context, references);
            if let Some(alternative) = alternative {
                let alternative =
                    analyze_shell_node(alternative, &condition.failed, context, references);
                let mut succeeded = consequence.succeeded;
                succeeded.extend(alternative.succeeded);
                let mut failed = consequence.failed;
                failed.extend(alternative.failed);
                ShellOutcome { succeeded, failed }
            } else {
                let mut succeeded = consequence.succeeded;
                succeeded.extend(condition.failed);
                ShellOutcome {
                    succeeded,
                    failed: consequence.failed,
                }
            }
        }
    }
}

fn analyze_shell_command(
    command: &[String],
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) -> ShellOutcome {
    analyze_shell_substitutions(command, working_directories, context, references);

    // A redirection is shell syntax, not a positional argument to commands
    // such as `cd`, `pushd`, or `eval`. Keep scanning original redirection
    // operands below, but use a filtered view for command semantics.
    let mut ignored_redirection_references = Vec::new();
    let command_without_redirections =
        shell_command_without_redirections(command, &mut ignored_redirection_references);

    let command_index = shell_command_index(&command_without_redirections);
    let command_name = command_without_redirections
        .get(command_index)
        .and_then(|command| command.rsplit('/').next())
        .unwrap_or_default();
    analyze_shell_inline_command(
        &command_without_redirections,
        command_index,
        command_name,
        working_directories,
        context,
        references,
    );

    if command_name == "eval" {
        return analyze_shell_eval(
            &command_without_redirections,
            working_directories,
            context,
            references,
        );
    }

    analyze_shell_command_references(command, working_directories, context, references);

    if let Some(outcome) = analyze_shell_directory_command(
        &command_without_redirections,
        working_directories,
        context,
        references,
    ) {
        return outcome;
    }

    if shell_command_changes_directory(&command_without_redirections, context.dialect) {
        return ShellOutcome {
            succeeded: BTreeSet::from(["$__VELNOR_UNKNOWN_CWD__".to_owned()]),
            failed: working_directories.clone(),
        };
    }

    ShellOutcome {
        succeeded: working_directories.clone(),
        failed: working_directories.clone(),
    }
}

fn analyze_shell_substitutions(
    command: &[String],
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) {
    for token in command {
        for substitution in shell_command_substitutions(token) {
            match parse_shell_expression(substitution, context.dialect) {
                Ok(Some(expression)) => {
                    let _ =
                        analyze_shell_node(&expression, working_directories, context, references);
                }
                Ok(None) => {}
                Err(()) => {
                    push_unique_shell_reference(
                        references,
                        UNSUPPORTED_SHELL_CWD_MARKER.to_owned(),
                    );
                }
            }
        }
    }
}

fn analyze_shell_inline_command(
    command: &[String],
    command_index: usize,
    command_name: &str,
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) {
    let (inline_command, env_chdir) = if command_name == "env" {
        let Some(wrapped_command) = env_wrapped_command(command, command_index) else {
            push_unique_shell_reference(references, UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
            return;
        };
        wrapped_command
    } else {
        (command.to_vec(), None)
    };
    if let Some(inline_source) = shell_inline_source_for_dialect(&inline_command, context.dialect) {
        match parse_shell_expression(&inline_source, context.dialect) {
            Ok(Some(expression)) => {
                let inline_directories = if let Some(chdir) = env_chdir.as_deref() {
                    working_directories
                        .iter()
                        .map(|directory| resolve_shell_directory(directory, chdir))
                        .collect::<BTreeSet<_>>()
                } else {
                    working_directories.clone()
                };
                let _ = analyze_shell_node(&expression, &inline_directories, context, references);
            }
            Ok(None) => {}
            Err(()) => {
                push_unique_shell_reference(references, UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
            }
        }
    }
}

fn analyze_shell_eval(
    command: &[String],
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) -> ShellOutcome {
    let unsupported = match shell_eval_source(command) {
        Some(Ok(source)) => match parse_shell_expression(&source, context.dialect) {
            Ok(Some(expression)) => {
                return analyze_shell_node(&expression, working_directories, context, references);
            }
            Ok(None) => {
                return ShellOutcome {
                    succeeded: working_directories.clone(),
                    failed: working_directories.clone(),
                };
            }
            Err(()) => true,
        },
        Some(Err(())) | None => true,
    };
    if unsupported {
        push_unique_shell_reference(references, UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
    }
    ShellOutcome {
        succeeded: BTreeSet::new(),
        failed: working_directories.clone(),
    }
}

fn analyze_shell_command_references(
    command: &[String],
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) {
    let mut command_references = Vec::new();
    let command = shell_command_without_redirections(command, &mut command_references);
    shell_segment_reference(&command, context.dialect, &mut command_references);
    shell_ordinary_file_operands(
        &command,
        working_directories,
        context,
        &mut command_references,
    );
    for working_directory in working_directories {
        for reference in &command_references {
            let reference = if working_directory.is_empty()
                || reference == UNSUPPORTED_SHELL_CWD_MARKER
                || reference.starts_with(GITHUB_ACTION_PATH_MARKER)
                || reference.starts_with(GITHUB_WORKSPACE_MARKER)
            {
                reference.clone()
            } else {
                resolve_shell_directory(working_directory, reference)
            };
            push_unique_shell_reference(references, reference);
        }
    }
}

fn shell_ordinary_file_operands(
    command: &[String],
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) {
    let command_index = shell_command_index(command);
    let Some(command_name) = command.get(command_index) else {
        return;
    };
    let command_name = command_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command_name)
        .to_ascii_lowercase();
    if command_name == "env" {
        if let Some((wrapped, chdir)) = env_wrapped_command(command, command_index) {
            let mut nested_directories = working_directories.clone();
            if let Some(chdir) = chdir {
                nested_directories = nested_directories
                    .iter()
                    .map(|directory| resolve_shell_directory(directory, &chdir))
                    .collect();
            }
            shell_ordinary_file_operands(&wrapped, &nested_directories, context, references);
        }
        return;
    }
    if (context.dialect == ShellDialect::Cmd && matches!(command_name.as_str(), "call" | "cmd"))
        || matches!(
            command_name.as_str(),
            "echo" | "printf" | "test" | "true" | "false"
        )
    {
        return;
    }
    if !is_file_reader_command(&command_name, context.dialect) {
        return;
    }

    let arguments = &command[command_index + 1..];
    let start = reader_file_operand_start(&command_name, arguments);
    for argument in arguments {
        if let Some(file_operand) = attached_reader_file_operand(&command_name, argument) {
            add_reader_file_operand_reference(
                file_operand,
                working_directories,
                context,
                references,
            );
        }
    }
    for argument in arguments.iter().skip(start) {
        add_reader_file_operand_reference(argument, working_directories, context, references);
    }
}

fn add_reader_file_operand_reference(
    argument: &str,
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) {
    if argument == "--" || argument.starts_with('-') {
        return;
    }
    if is_dynamic_shell_reference(argument) || has_shell_command_substitution(argument) {
        push_unique_shell_reference(references, UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
        return;
    }
    let path_like = argument.contains('/')
        || argument.contains('\\')
        || argument.contains(['*', '?', '[', '{'])
        || argument.starts_with(GITHUB_ACTION_PATH_MARKER)
        || argument.starts_with(GITHUB_WORKSPACE_MARKER)
        || argument.starts_with('~');
    let tracked = working_directories.iter().any(|directory| {
        let base_directory = if directory.is_empty() {
            context.working_directory.to_owned()
        } else {
            resolve_shell_directory(context.working_directory, directory)
        };
        shell_operand_repository_path(argument, &base_directory, context)
            .is_some_and(|path| context.files.is_some_and(|files| files.contains(&path)))
    });
    if path_like || tracked {
        push_unique_shell_reference(references, argument.to_owned());
    }
}

fn attached_reader_file_operand<'a>(command: &str, argument: &'a str) -> Option<&'a str> {
    if !matches!(
        command,
        "grep" | "egrep" | "fgrep" | "rg" | "sed" | "awk" | "jq"
    ) {
        return None;
    }
    argument.strip_prefix("--file=").or_else(|| {
        argument
            .strip_prefix("-f")
            .filter(|operand| !operand.is_empty())
    })
}

fn is_file_reader_command(command: &str, dialect: ShellDialect) -> bool {
    let common = [
        "cat",
        "head",
        "tail",
        "grep",
        "egrep",
        "fgrep",
        "rg",
        "sed",
        "awk",
        "jq",
        "yq",
        "wc",
        "sort",
        "uniq",
        "cut",
        "comm",
        "cmp",
        "diff",
        "sha256sum",
        "sha1sum",
        "md5sum",
        "b2sum",
        "file",
        "stat",
        "xxd",
        "od",
        "hexdump",
        "base64",
        "tac",
        "strings",
        "rev",
    ];
    common.contains(&command)
        || match dialect {
            ShellDialect::PowerShell => [
                "get-content",
                "gc",
                "type",
                "more",
                "select-string",
                "sls",
                "test-path",
                "get-item",
                "gi",
            ]
            .contains(&command),
            ShellDialect::Cmd => {
                ["type", "more", "find", "findstr", "fc", "certutil"].contains(&command)
            }
            ShellDialect::Posix | ShellDialect::Python | ShellDialect::Other => false,
        }
}

fn reader_file_operand_start(command: &str, arguments: &[String]) -> usize {
    let mut index = 0;
    let mut grep_pattern_supplied = false;
    while let Some(argument) = arguments.get(index) {
        let is_grep = matches!(command, "grep" | "egrep" | "fgrep" | "rg");
        if argument == "--" {
            return if is_grep && !grep_pattern_supplied {
                (index + 2).min(arguments.len())
            } else {
                index + 1
            };
        }
        if matches!(argument.as_str(), "-f" | "--file")
            && matches!(
                command,
                "grep" | "egrep" | "fgrep" | "rg" | "sed" | "awk" | "jq"
            )
        {
            // These readers consume the -f file as an input too.
            return index;
        }
        if is_grep
            && (argument.starts_with("-e") && argument.len() > 2
                || argument.starts_with("--regexp="))
        {
            grep_pattern_supplied = true;
            index += 1;
            continue;
        }
        if is_grep && attached_reader_file_operand(command, argument).is_some() {
            grep_pattern_supplied = true;
            index += 1;
            continue;
        }
        if argument.starts_with('-') {
            if matches!(argument.as_str(), "-f" | "--file")
                && matches!(command, "grep" | "egrep" | "fgrep" | "rg")
            {
                // Preserve grep's pattern-file argument as well as later input files.
                return index;
            }
            if matches!(argument.as_str(), "-e" | "--regexp" | "-f" | "--file")
                && arguments.get(index + 1).is_some()
            {
                if matches!(argument.as_str(), "-e" | "--regexp") {
                    grep_pattern_supplied = true;
                }
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        return match command {
            "grep" | "egrep" | "fgrep" | "rg" if !grep_pattern_supplied => index + 1,
            "tr" => (index + 2).min(arguments.len()),
            _ => index,
        };
    }
    arguments.len()
}

fn shell_operand_repository_path(
    operand: &str,
    current_directory: &str,
    context: &ShellReferenceContext<'_>,
) -> Option<String> {
    if let Some(suffix) = operand.strip_prefix(GITHUB_ACTION_PATH_MARKER) {
        return normalize_repository_relative_path(
            context.action_root,
            suffix.trim_start_matches('/'),
        );
    }
    if let Some(suffix) = operand.strip_prefix(GITHUB_WORKSPACE_MARKER) {
        return normalize_repository_relative_path(".", suffix.trim_start_matches('/'));
    }
    let resolved = resolve_shell_directory(current_directory, operand);
    if let Some(suffix) = resolved.strip_prefix(GITHUB_ACTION_PATH_MARKER) {
        return normalize_repository_relative_path(
            context.action_root,
            suffix.trim_start_matches('/'),
        );
    }
    if let Some(suffix) = resolved.strip_prefix(GITHUB_WORKSPACE_MARKER) {
        return normalize_repository_relative_path(".", suffix.trim_start_matches('/'));
    }
    if resolved.starts_with('/') || is_windows_drive_prefix(&resolved) {
        return None;
    }
    normalize_repository_relative_path(".", &resolved)
}

fn shell_command_without_redirections(
    command: &[String],
    references: &mut Vec<String>,
) -> Vec<String> {
    let mut filtered = Vec::new();
    let mut index = 0;
    while index < command.len() {
        let operator = &command[index];
        let Some(redirection) = shell_redirection_kind(operator) else {
            filtered.push(operator.clone());
            index += 1;
            continue;
        };
        let Some(target) = command.get(index + 1) else {
            references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
            break;
        };
        if redirection == ShellRedirectionKind::InputFile {
            if is_dynamic_shell_reference(target) || has_shell_command_substitution(target) {
                references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
            } else if !target.starts_with('/') && !is_windows_drive_prefix(target) {
                references.push(target.clone());
            }
        }
        index += 2;
    }
    filtered
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ShellRedirectionKind {
    InputFile,
    Other,
}

fn shell_redirection_kind(operator: &str) -> Option<ShellRedirectionKind> {
    let operator = operator.trim_start_matches(|character: char| character.is_ascii_digit());
    if operator.is_empty() {
        return None;
    }
    if matches!(operator, "<" | "<>") {
        return Some(ShellRedirectionKind::InputFile);
    }
    matches!(
        operator,
        ">" | ">>" | ">|" | ">&" | "<&" | "&>" | "&>>" | "<<" | "<<-" | "<<<"
    )
    .then_some(ShellRedirectionKind::Other)
}

fn analyze_shell_cd_command(
    command: &[String],
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) -> Option<ShellOutcome> {
    let target = if context.dialect == ShellDialect::Cmd {
        cmd_shell_cd_target(command)
    } else {
        shell_cd_target(command)
    };
    match target {
        ShellCdTarget::NotCd => None,
        ShellCdTarget::Invalid => Some(ShellOutcome {
            succeeded: BTreeSet::new(),
            failed: working_directories.clone(),
        }),
        ShellCdTarget::Directory(target) => Some(analyze_shell_directory_change(
            &target,
            working_directories,
            context,
            references,
        )),
    }
}

fn analyze_shell_directory_command(
    command: &[String],
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) -> Option<ShellOutcome> {
    if context.dialect != ShellDialect::PowerShell {
        return analyze_shell_cd_command(command, working_directories, context, references);
    }

    match powershell_location_target(command) {
        PowerShellLocationTarget::NotLocation => None,
        PowerShellLocationTarget::Directory(target) => Some(analyze_shell_directory_change(
            &target,
            working_directories,
            context,
            references,
        )),
        PowerShellLocationTarget::Unsupported => {
            push_unique_shell_reference(references, UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
            Some(ShellOutcome {
                succeeded: BTreeSet::from(["$__VELNOR_UNKNOWN_CWD__".to_owned()]),
                failed: working_directories.clone(),
            })
        }
    }
}

fn analyze_shell_directory_change(
    target: &str,
    working_directories: &BTreeSet<String>,
    context: &ShellReferenceContext<'_>,
    references: &mut Vec<String>,
) -> ShellOutcome {
    if is_dynamic_shell_reference(target)
        || has_shell_command_substitution(target)
        || target.starts_with(['/', '~'])
        || is_windows_drive_prefix(target)
    {
        push_unique_shell_reference(references, UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
        return ShellOutcome {
            succeeded: BTreeSet::from(["$__VELNOR_UNKNOWN_CWD__".to_owned()]),
            failed: working_directories.clone(),
        };
    }
    let mut succeeded = BTreeSet::new();
    let mut failed = BTreeSet::new();
    for working_directory in working_directories {
        let resolved = resolve_shell_directory(working_directory, target);
        if context.directory_exists(&resolved) {
            succeeded.insert(resolved);
        } else {
            failed.insert(working_directory.clone());
        }
    }
    ShellOutcome { succeeded, failed }
}

enum PowerShellLocationTarget {
    NotLocation,
    Unsupported,
    Directory(String),
}

fn powershell_location_target(command: &[String]) -> PowerShellLocationTarget {
    let command_index = shell_command_index(command);
    let Some(command_name) = command.get(command_index) else {
        return PowerShellLocationTarget::NotLocation;
    };
    let command_name = command_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command_name);
    if ["set-location", "sl", "cd", "chdir"]
        .iter()
        .all(|name| !command_name.eq_ignore_ascii_case(name))
    {
        return PowerShellLocationTarget::NotLocation;
    }

    let mut target = None;
    let mut arguments = command[command_index + 1..].iter();
    while let Some(argument) = arguments.next() {
        if argument.eq_ignore_ascii_case("-Path") || argument.eq_ignore_ascii_case("-LiteralPath") {
            let Some(path) = arguments.next() else {
                return PowerShellLocationTarget::Unsupported;
            };
            if target.replace(path.clone()).is_some() {
                return PowerShellLocationTarget::Unsupported;
            }
        } else if argument.eq_ignore_ascii_case("-PassThru")
            || argument.eq_ignore_ascii_case("-UseTransaction")
        {
            // These options do not change which directory becomes current.
        } else if argument.starts_with('-') {
            // Parameter abbreviations, provider switches, and location stacks
            // need PowerShell's parameter binder and provider state.
            return PowerShellLocationTarget::Unsupported;
        } else if target.replace(argument.clone()).is_some() {
            return PowerShellLocationTarget::Unsupported;
        }
    }

    PowerShellLocationTarget::Directory(target.unwrap_or_else(|| "$HOME".to_owned()))
}

fn shell_command_index(command: &[String]) -> usize {
    let mut index = 0;
    while command
        .get(index)
        .is_some_and(|token| is_environment_assignment(token))
    {
        index += 1;
    }
    while command.get(index).is_some_and(|token| token == "time") {
        index += 1;
        if command.get(index).is_some_and(|token| token == "-p") {
            index += 1;
        }
    }
    if matches!(
        command.get(index).map(String::as_str),
        Some("command" | "builtin")
    ) {
        index += 1;
        if command.get(index).is_some_and(|token| token == "--") {
            index += 1;
        }
    }
    index
}

fn shell_inline_source(command: &[String]) -> Option<&str> {
    let command_index = shell_command_index(command);
    let command_name = command
        .get(command_index)?
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if !matches!(command_name, "bash" | "sh" | "dash" | "zsh") {
        return None;
    }
    let arguments = &command[command_index + 1..];
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.as_str() {
            "-c" | "--command" => return arguments.get(index + 1).map(String::as_str),
            "--" => return None,
            "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file" => index += 2,
            value if value.starts_with("--rcfile=") || value.starts_with("--init-file=") => {
                index += 1;
            }
            value if value.starts_with('-') || value.starts_with('+') => {
                let mut consumes_next = false;
                for option in value[1..].chars() {
                    match option {
                        'c' => return arguments.get(index + 1).map(String::as_str),
                        'o' | 'O' => {
                            consumes_next = true;
                            break;
                        }
                        _ => {}
                    }
                }
                index += if consumes_next { 2 } else { 1 };
            }
            _ => return None,
        }
    }
    None
}

fn shell_inline_source_for_dialect(command: &[String], dialect: ShellDialect) -> Option<String> {
    if dialect != ShellDialect::Cmd {
        return shell_inline_source(command).map(str::to_owned);
    }
    let command_index = shell_command_index(command);
    let command_name = command
        .get(command_index)?
        .rsplit(['/', '\\'])
        .next()?
        .to_ascii_lowercase();
    if command_name != "cmd" {
        return None;
    }
    let arguments = &command[command_index + 1..];
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if ["/c", "/k"]
            .iter()
            .any(|option| argument.eq_ignore_ascii_case(option))
        {
            let body = arguments.get(index + 1..)?;
            if body.is_empty() {
                return None;
            }
            return Some(body.join(" "));
        }
        if ["/d", "/q", "/a", "/u", "/s"].iter().any(|option| {
            argument.eq_ignore_ascii_case(option)
                || argument
                    .get(..option.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(option))
        }) || ["/v:on", "/v:off", "/e:on", "/e:off"]
            .iter()
            .any(|option| argument.eq_ignore_ascii_case(option))
        {
            index += 1;
        } else {
            return None;
        }
    }
    None
}

fn shell_eval_source(command: &[String]) -> Option<Result<String, ()>> {
    let command_index = shell_command_index(command);
    let command_name = command.get(command_index)?.rsplit('/').next()?;
    if command_name != "eval" {
        return None;
    }
    let arguments = &command[command_index + 1..];
    if arguments.iter().any(|argument| {
        is_dynamic_shell_reference(argument) || has_shell_command_substitution(argument)
    }) {
        return Some(Err(()));
    }
    Some(Ok(arguments.join(" ")))
}

fn shell_cd_target(command: &[String]) -> ShellCdTarget {
    let command_index = shell_command_index(command);
    let Some(command_name) = command.get(command_index) else {
        return ShellCdTarget::NotCd;
    };
    if command_name.rsplit('/').next().unwrap_or(command_name) != "cd" {
        return ShellCdTarget::NotCd;
    }
    let mut target = None;
    let mut options = true;
    let mut previous_directory = false;
    for argument in &command[command_index + 1..] {
        if options && argument == "--" {
            options = false;
            continue;
        }
        if options
            && argument.starts_with('-')
            && argument.len() > 1
            && argument[1..]
                .chars()
                .all(|option| matches!(option, 'L' | 'P' | 'e'))
        {
            continue;
        }
        previous_directory = options && argument == "-";
        if target.replace(argument.clone()).is_some() {
            return ShellCdTarget::Invalid;
        }
    }
    let target = target.unwrap_or_else(|| "$HOME".to_owned());
    ShellCdTarget::Directory(if previous_directory {
        "$OLDPWD".to_owned()
    } else if target == "-" {
        "./-".to_owned()
    } else {
        target
    })
}

fn cmd_shell_cd_target(command: &[String]) -> ShellCdTarget {
    let command_index = shell_command_index(command);
    let Some(command_name) = command.get(command_index) else {
        return ShellCdTarget::NotCd;
    };
    let is_pushd = command_name.eq_ignore_ascii_case("pushd");
    let is_cd = ["cd", "chdir"]
        .iter()
        .any(|name| command_name.eq_ignore_ascii_case(name));
    if !is_pushd && !is_cd {
        return ShellCdTarget::NotCd;
    }
    let mut target = None;
    let mut drive_change = false;
    for argument in &command[command_index + 1..] {
        if !is_pushd && argument.eq_ignore_ascii_case("/d") && !drive_change {
            drive_change = true;
        } else if target.replace(argument.clone()).is_some() {
            return ShellCdTarget::Invalid;
        }
    }
    if is_pushd && target.is_none() {
        return ShellCdTarget::Invalid;
    }
    // `cd` with no path prints the current directory and leaves it unchanged.
    ShellCdTarget::Directory(target.unwrap_or_else(|| ".".to_owned()))
}

fn shell_command_changes_directory(command: &[String], dialect: ShellDialect) -> bool {
    let command_index = shell_command_index(command);
    command.get(command_index).is_some_and(|command| {
        let command = command.rsplit(['/', '\\']).next().unwrap_or(command);
        match dialect {
            ShellDialect::PowerShell => ["push-location", "pop-location", "pushd", "popd"]
                .iter()
                .any(|name| command.eq_ignore_ascii_case(name)),
            ShellDialect::Posix | ShellDialect::Python | ShellDialect::Other => {
                matches!(command, "source" | "." | "eval" | "pushd" | "popd")
            }
            ShellDialect::Cmd => ["pushd", "popd"]
                .iter()
                .any(|name| command.eq_ignore_ascii_case(name)),
        }
    })
}

fn push_unique_shell_reference(references: &mut Vec<String>, reference: String) {
    if !references.contains(&reference) {
        references.push(reference);
    }
}

fn resolve_shell_directory(current: &str, target: &str) -> String {
    if target == "-" {
        return "$OLDPWD".to_owned();
    }
    if target.starts_with('$') || target.starts_with('~') {
        return target.to_owned();
    }
    let (context, base) = if let Some(suffix) = current.strip_prefix(GITHUB_ACTION_PATH_MARKER) {
        (GITHUB_ACTION_PATH_MARKER, suffix)
    } else if let Some(suffix) = current.strip_prefix(GITHUB_WORKSPACE_MARKER) {
        (GITHUB_WORKSPACE_MARKER, suffix)
    } else {
        ("", current)
    };
    let absolute = context.is_empty() && base.starts_with('/');
    if target.starts_with('/') || is_windows_drive_prefix(target) {
        return target.to_owned();
    }
    let (context, base, target) =
        if let Some(suffix) = target.strip_prefix(GITHUB_ACTION_PATH_MARKER) {
            (GITHUB_ACTION_PATH_MARKER, "", suffix)
        } else if let Some(suffix) = target.strip_prefix(GITHUB_WORKSPACE_MARKER) {
            (GITHUB_WORKSPACE_MARKER, "", suffix)
        } else {
            (context, base, target)
        };
    let mut components = base
        .trim_start_matches('/')
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." if components.last().is_some_and(|last| last != "..") => {
                components.pop();
            }
            ".." => components.push("..".to_owned()),
            value => components.push(value.to_owned()),
        }
    }
    let suffix = components.join("/");
    if context.is_empty() && absolute {
        format!("/{suffix}")
    } else if context.is_empty() {
        suffix
    } else if suffix.is_empty() {
        context.to_owned()
    } else {
        format!("{context}/{suffix}")
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "interpreter options must be scanned in order to distinguish script, module, and inline-code operands"
)]
fn shell_segment_reference(tokens: &[String], dialect: ShellDialect, references: &mut Vec<String>) {
    let command_index = shell_command_index(tokens);
    let Some(command) = tokens.get(command_index) else {
        return;
    };
    if has_shell_command_substitution(command) || is_dynamic_shell_reference(command) {
        references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
        return;
    }
    let command_name = command
        .rsplit('/')
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    if shell_inline_source_for_dialect(tokens, dialect).is_some() {
        return;
    }
    if dialect == ShellDialect::Posix && command_name == "trap" {
        references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
        return;
    }
    if dialect == ShellDialect::PowerShell
        && matches!(command_name.as_str(), "iex" | "invoke-expression")
    {
        references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
        return;
    }
    if dialect == ShellDialect::Cmd && command_name == "call" {
        let target = shell_command_index(tokens) + 1;
        let Some(target_command) = tokens.get(target) else {
            references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
            return;
        };
        if target_command.starts_with(':')
            || is_dynamic_shell_reference(target_command)
            || has_shell_command_substitution(target_command)
        {
            references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
            return;
        }
        shell_segment_reference(&tokens[target..], dialect, references);
        return;
    }
    if command_name == "exec" {
        match shell_exec_target(tokens, command_index) {
            Ok(Some(index)) => shell_segment_reference(&tokens[index..], dialect, references),
            Ok(None) => {}
            Err(()) => references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned()),
        }
        return;
    }
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
        ".",
    ];
    if command_name == "env" {
        if let Some((command, chdir)) = env_wrapped_command(tokens, command_index) {
            let reference_start = references.len();
            shell_segment_reference(&command, dialect, references);
            if let Some(chdir) = chdir {
                for reference in &mut references[reference_start..] {
                    *reference = resolve_shell_directory(&chdir, reference);
                }
            }
        } else {
            references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
        }
        return;
    }
    if interpreters.contains(&command_name.as_str()) {
        let mut arguments = &tokens[command_index + 1..];
        if matches!(command_name.as_str(), "bun" | "deno")
            && arguments.first().is_some_and(|argument| argument == "run")
        {
            arguments = &arguments[1..];
        }
        let mut index = 0;
        while index < arguments.len() {
            let argument = &arguments[index];
            if is_inline_code_option(&command_name, argument) {
                if !matches!(command_name.as_str(), "bash" | "sh" | "dash" | "zsh") {
                    references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
                }
                return;
            }
            if let Some((loads_file, is_script)) =
                interpreter_option_argument(&command_name, argument)
            {
                if let Some(value) = arguments.get(index + 1)
                    && loads_file
                    && (is_shell_file_reference(value)
                        || is_dynamic_shell_reference(value)
                        || has_shell_command_substitution(value))
                {
                    references.push(value.clone());
                }
                if is_script {
                    return;
                }
                index += if arguments.get(index + 1).is_some() {
                    2
                } else {
                    1
                };
                continue;
            }
            if let Some(value) = attached_interpreter_file_argument(&command_name, argument) {
                if is_shell_file_reference(value)
                    || is_dynamic_shell_reference(value)
                    || has_shell_command_substitution(value)
                {
                    references.push(value.to_owned());
                }
                index += 1;
                continue;
            }
            if argument.starts_with('-') {
                index += 1;
                continue;
            }
            if has_shell_command_substitution(argument) {
                references.push(UNSUPPORTED_SHELL_CWD_MARKER.to_owned());
                return;
            }
            if is_dynamic_shell_reference(argument) {
                references.push(argument.clone());
                return;
            }
            if argument != "-" && !argument.starts_with('/') && !is_windows_drive_prefix(argument) {
                // The first non-option positional argument is the script for
                // Node, Python, Ruby, Perl, and shell interpreters even when
                // it has no slash or conventional file suffix.
                references.push(argument.clone());
            }
            return;
        }
        return;
    }
    if is_shell_file_reference(command) {
        references.push(command.clone());
    }
}

fn shell_exec_target(command: &[String], command_index: usize) -> Result<Option<usize>, ()> {
    let mut index = command_index + 1;
    while let Some(argument) = command.get(index) {
        match argument.as_str() {
            "--" => {
                index += 1;
                break;
            }
            "-c" | "-l" => index += 1,
            "-a" => {
                if command.get(index + 1).is_none() {
                    return Err(());
                }
                index += 2;
            }
            value if value.starts_with("-a") && value.len() > 2 => index += 1,
            value if value.starts_with('-') && value.len() > 1 => {
                if value[1..].chars().all(|option| matches!(option, 'c' | 'l')) {
                    index += 1;
                } else {
                    return Err(());
                }
            }
            _ => break,
        }
    }
    Ok((index < command.len()).then_some(index))
}

fn env_wrapped_command(
    tokens: &[String],
    env_index: usize,
) -> Option<(Vec<String>, Option<String>)> {
    let mut command_index = env_index + 1;
    let mut chdir = None::<String>;
    let mut split_command = None::<Vec<String>>;
    while let Some(argument) = tokens.get(command_index) {
        match argument.as_str() {
            "--" => {
                command_index += 1;
                break;
            }
            "-i" | "--ignore-environment" | "-0" | "--null" => command_index += 1,
            "-C" | "--chdir" => {
                chdir = Some(tokens.get(command_index + 1)?.clone());
                command_index += 2;
            }
            "-S" | "--split-string" => {
                split_command = Some(split_env_command(tokens.get(command_index + 1)?)?);
                command_index += 2;
                break;
            }
            "-u" | "--unset" => {
                tokens.get(command_index + 1)?;
                command_index += 2;
            }
            _ if argument.starts_with("--chdir=") => {
                chdir = Some(argument["--chdir=".len()..].to_owned());
                command_index += 1;
            }
            _ if argument.starts_with("-C") && argument.len() > 2 => {
                chdir = Some(argument[2..].to_owned());
                command_index += 1;
            }
            _ if argument.starts_with("--split-string=") => {
                split_command = Some(split_env_command(&argument["--split-string=".len()..])?);
                command_index += 1;
                break;
            }
            _ if argument.starts_with("-S") && argument.len() > 2 => {
                split_command = Some(split_env_command(&argument[2..])?);
                command_index += 1;
                break;
            }
            _ if argument.starts_with("--unset=") || is_environment_assignment(argument) => {
                command_index += 1;
            }
            _ => break,
        }
    }
    let mut command = split_command.unwrap_or_default();
    command.extend(tokens[command_index..].iter().cloned());
    Some((command, chdir))
}

fn split_env_command(value: &str) -> Option<Vec<String>> {
    if is_dynamic_shell_reference(value) || has_shell_command_substitution(value) {
        return None;
    }
    let tokens = shell_tokens(value);
    if tokens.iter().any(|token| {
        token == INVALID_SHELL_TOKEN_MARKER
            || matches!(token.as_str(), ";" | "&&" | "||" | "|" | "|&" | "&")
            || is_dynamic_shell_reference(token)
            || has_shell_command_substitution(token)
    }) {
        return None;
    }
    Some(tokens)
}

fn attached_interpreter_file_argument<'a>(interpreter: &str, option: &'a str) -> Option<&'a str> {
    if !matches!(interpreter, "node" | "bun" | "deno") {
        return None;
    }
    let (name, value) = option.split_once('=')?;
    matches!(
        name,
        "--require" | "--import" | "--loader" | "--experimental-loader"
    )
    .then_some(value)
}

fn interpreter_option_argument(interpreter: &str, option: &str) -> Option<(bool, bool)> {
    let lowercase_option = option.to_ascii_lowercase();
    match interpreter {
        "bash" | "sh" | "dash" | "zsh" => match option {
            "-o" | "+o" => Some((false, false)),
            "--rcfile" | "--init-file" => Some((true, false)),
            _ => None,
        },
        "node" | "bun" | "deno" => match option {
            "--require" | "-r" | "--import" | "--loader" | "--experimental-loader" => {
                Some((true, false))
            }
            "--title" | "--conditions" | "--inspect-port" => Some((false, false)),
            _ => None,
        },
        "python" | "python3" => match option {
            "-W" | "-X" | "--check-hash-based-pycs" => Some((false, false)),
            _ => None,
        },
        "ruby" | "perl" => match option {
            "-r" | "-m" => Some((true, false)),
            "-I" => Some((false, false)),
            _ => None,
        },
        "pwsh" | "powershell" => match lowercase_option.as_str() {
            "-file" => Some((true, true)),
            "-executionpolicy" | "-inputformat" | "-outputformat" | "-workingdirectory" => {
                Some((false, false))
            }
            _ => None,
        },
        _ => None,
    }
}

fn is_environment_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
        })
}

fn is_inline_code_option(interpreter: &str, option: &str) -> bool {
    match interpreter {
        "bash" | "sh" | "dash" | "zsh" => matches!(option, "-c" | "--command"),
        "node" | "bun" => {
            matches!(option, "-e" | "--eval" | "-p" | "--print")
                || option.starts_with("--eval=")
                || option.starts_with("--print=")
        }
        "deno" => matches!(option, "eval" | "-e" | "--eval"),
        "python" | "python3" => matches!(option, "-c" | "--command" | "-m"),
        "ruby" | "perl" => option == "-e",
        "pwsh" | "powershell" => {
            option.eq_ignore_ascii_case("-command") || option.eq_ignore_ascii_case("-c")
        }
        _ => false,
    }
}

fn is_shell_file_reference(value: &str) -> bool {
    !value.starts_with('/')
        && !is_windows_drive_prefix(value)
        && (value.starts_with("./")
            || value.starts_with("../")
            || value.contains('/') && !value.starts_with('-')
            || has_script_suffix(value))
}

fn is_dynamic_shell_reference(value: &str) -> bool {
    value.contains("${{") || value.contains("{{") || value.contains('$')
}

fn has_script_suffix(value: &str) -> bool {
    [
        ".sh", ".bash", ".js", ".mjs", ".cjs", ".ts", ".py", ".rb", ".pl", ".ps1", ".cmd", ".bat",
    ]
    .iter()
    .any(|suffix| value.ends_with(suffix))
}

fn shell_command_substitutions(token: &str) -> Vec<&str> {
    let mut substitutions = Vec::new();
    let mut rest = token;
    while let Some(start) = rest.find(SHELL_SUBSTITUTION_START_MARKER) {
        let body_start = start + SHELL_SUBSTITUTION_START_MARKER.len();
        let after_start = &rest[body_start..];
        let Some(end) = after_start.find(SHELL_SUBSTITUTION_END_MARKER) else {
            break;
        };
        substitutions.push(&after_start[..end]);
        let after_body = body_start + end + SHELL_SUBSTITUTION_END_MARKER.len();
        rest = &rest[after_body..];
    }
    substitutions
}

fn has_shell_command_substitution(value: &str) -> bool {
    value.contains(SHELL_SUBSTITUTION_START_MARKER)
}

/// Read a balanced `$()` body. The caller has already consumed its opening `(`.
fn shell_command_substitution_body(
    characters: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<String> {
    let mut body = String::new();
    let mut depth = 1usize;
    let mut quote = None;
    let mut escaped = false;
    while let Some(character) = characters.next() {
        if escaped {
            body.push(character);
            escaped = false;
            continue;
        }
        if quote == Some('"') && character == '\\' {
            body.push(character);
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            body.push(character);
            if character == active {
                quote = None;
            }
            continue;
        }
        if character == '\\' {
            body.push(character);
            escaped = true;
            continue;
        }
        if character == '$' && characters.peek() == Some(&'{') {
            let _ = characters.next();
            body.push_str("${");
            if characters.peek() == Some(&'{') {
                let _ = characters.next();
                body.push('{');
                let mut closing_braces = 0usize;
                for expression_character in characters.by_ref() {
                    body.push(expression_character);
                    if expression_character == '}' {
                        closing_braces += 1;
                        if closing_braces == 2 {
                            break;
                        }
                    } else {
                        closing_braces = 0;
                    }
                }
            }
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
            body.push(character);
            continue;
        }
        match character {
            '(' => {
                depth += 1;
                body.push(character);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(body);
                }
                body.push(character);
            }
            _ => body.push(character),
        }
    }
    None
}

fn consume_shell_special_variable(
    characters: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<&'static str> {
    for (variable, marker) in [
        ("GITHUB_ACTION_PATH", GITHUB_ACTION_PATH_MARKER),
        ("GITHUB_WORKSPACE", GITHUB_WORKSPACE_MARKER),
    ] {
        let mut lookahead = characters.clone();
        let braced = lookahead.peek() == Some(&'{');
        if braced {
            let _ = lookahead.next();
        }
        if !variable
            .chars()
            .all(|expected| lookahead.next() == Some(expected))
        {
            continue;
        }
        if braced {
            if lookahead.next() != Some('}') {
                continue;
            }
        } else if lookahead
            .peek()
            .is_some_and(|character| character.is_ascii_alphanumeric() || *character == '_')
        {
            continue;
        }
        *characters = lookahead;
        return Some(marker);
    }
    None
}

fn shell_tokens_for_dialect(command: &str, dialect: ShellDialect) -> Vec<String> {
    match dialect {
        ShellDialect::PowerShell => powershell_shell_tokens(command),
        ShellDialect::Cmd => cmd_shell_tokens(command),
        ShellDialect::Posix | ShellDialect::Python | ShellDialect::Other => shell_tokens(command),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "PowerShell has distinct backtick, quote, variable, and operator rules"
)]
fn powershell_shell_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if quote == Some('\'') {
            if character == '\'' {
                if characters.peek() == Some(&'\'') {
                    let _ = characters.next();
                    token.push('\'');
                } else {
                    quote = None;
                }
            } else {
                token.push(character);
            }
            continue;
        }
        if character == '`' {
            match characters.next() {
                Some('\n') => continue,
                Some('\r') if characters.peek() == Some(&'\n') => {
                    let _ = characters.next();
                    continue;
                }
                Some(escaped) => token.push(escaped),
                None => return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()],
            }
            continue;
        }
        if quote == Some('"') {
            match character {
                '"' => quote = None,
                '$' if characters.peek() == Some(&'(') => {
                    let _ = characters.next();
                    let Some(body) = powershell_command_substitution_body(&mut characters) else {
                        return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
                    };
                    token.push_str(SHELL_SUBSTITUTION_START_MARKER);
                    token.push_str(&body);
                    token.push_str(SHELL_SUBSTITUTION_END_MARKER);
                }
                '$' if let Some(marker) =
                    consume_powershell_environment_variable(&mut characters) =>
                {
                    token.push_str(marker);
                }
                _ => token.push(character),
            }
            continue;
        }
        if character == '"' {
            quote = Some('"');
            continue;
        }
        if character == '\'' {
            quote = Some('\'');
            continue;
        }
        if character == '$' {
            if characters.peek() == Some(&'(') {
                let _ = characters.next();
                let Some(body) = powershell_command_substitution_body(&mut characters) else {
                    return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
                };
                token.push_str(SHELL_SUBSTITUTION_START_MARKER);
                token.push_str(&body);
                token.push_str(SHELL_SUBSTITUTION_END_MARKER);
            } else if let Some(marker) = consume_powershell_environment_variable(&mut characters) {
                token.push_str(marker);
            } else {
                token.push('$');
            }
            continue;
        }
        match character {
            '#' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                while let Some(comment_character) = characters.next() {
                    if comment_character == '\n' || comment_character == '\r' {
                        if comment_character == '\r' && characters.peek() == Some(&'\n') {
                            let _ = characters.next();
                        }
                        tokens.push(";".to_owned());
                        break;
                    }
                }
            }
            value if value.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                if matches!(value, '\n' | '\r') {
                    if value == '\r' && characters.peek() == Some(&'\n') {
                        let _ = characters.next();
                    }
                    tokens.push(";".to_owned());
                }
            }
            ';' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push(";".to_owned());
            }
            '&' => {
                if characters.peek() == Some(&'&') {
                    let _ = characters.next();
                    if !token.is_empty() {
                        tokens.push(std::mem::take(&mut token));
                    }
                    tokens.push("&&".to_owned());
                } else if token.is_empty() {
                    // PowerShell's prefix call operator invokes the next
                    // command; it does not background the command.
                } else {
                    return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
                }
            }
            '|' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                if characters.peek() == Some(&'|') {
                    let _ = characters.next();
                    tokens.push("||".to_owned());
                } else {
                    tokens.push("|".to_owned());
                }
            }
            '(' | ')' | '{' | '}' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push(character.to_string());
            }
            _ => token.push(character),
        }
    }
    if quote.is_some() {
        return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    normalize_windows_shell_tokens(tokens)
}

fn consume_powershell_environment_variable(
    characters: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<&'static str> {
    let mut lookahead = characters.clone();
    for expected in "env:".chars() {
        if !lookahead
            .next()
            .is_some_and(|actual| actual.eq_ignore_ascii_case(&expected))
        {
            return None;
        }
    }
    let mut name = String::new();
    while lookahead
        .peek()
        .is_some_and(|character| character.is_ascii_alphanumeric() || *character == '_')
    {
        name.push(lookahead.next()?);
    }
    if name.is_empty() {
        return None;
    }
    let marker = if name.eq_ignore_ascii_case("GITHUB_ACTION_PATH") {
        GITHUB_ACTION_PATH_MARKER
    } else if name.eq_ignore_ascii_case("GITHUB_WORKSPACE") {
        GITHUB_WORKSPACE_MARKER
    } else {
        return None;
    };
    *characters = lookahead;
    Some(marker)
}

fn powershell_command_substitution_body(
    characters: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Option<String> {
    let mut body = String::new();
    let mut depth = 1usize;
    let mut quote = None;
    while let Some(character) = characters.next() {
        if character == '`' && quote != Some('\'') {
            let escaped = characters.next()?;
            if escaped == '\n' {
                continue;
            }
            if escaped == '\r' && characters.peek() == Some(&'\n') {
                let _ = characters.next();
                continue;
            }
            body.push(escaped);
            continue;
        }
        if quote == Some('\'') {
            body.push(character);
            if character == '\'' {
                if characters.peek() == Some(&'\'') {
                    body.push(characters.next()?);
                } else {
                    quote = None;
                }
            }
            continue;
        }
        if let Some(active) = quote {
            body.push(character);
            if character == active {
                quote = None;
            }
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
            body.push(character);
            continue;
        }
        match character {
            '(' => {
                depth += 1;
                body.push(character);
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(body);
                }
                body.push(character);
            }
            _ => body.push(character),
        }
    }
    None
}

#[expect(
    clippy::too_many_lines,
    reason = "cmd variable expansion, caret escapes, and command operators need dialect-specific state"
)]
fn cmd_shell_tokens(command: &str) -> Vec<String> {
    if cmd_has_delayed_expansion(command) {
        return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
    }
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '^' {
            match characters.next() {
                Some('\n') => continue,
                Some('\r') if characters.peek() == Some(&'\n') => {
                    let _ = characters.next();
                    continue;
                }
                Some(escaped) => token.push(escaped),
                None => return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()],
            }
            continue;
        }
        if character == '"' {
            if quoted && characters.peek() == Some(&'"') {
                let _ = characters.next();
                token.push('"');
            } else {
                quoted = !quoted;
            }
            continue;
        }
        if character == '%' {
            if characters.peek() == Some(&'%') {
                let _ = characters.next();
                token.push('%');
                continue;
            }
            let mut lookahead = characters.clone();
            let mut name = String::new();
            let mut closed = false;
            for next in lookahead.by_ref() {
                if next == '%' {
                    closed = true;
                    break;
                }
                if matches!(next, '\n' | '\r') {
                    break;
                }
                name.push(next);
            }
            if closed {
                let marker = if name.eq_ignore_ascii_case("GITHUB_ACTION_PATH") {
                    GITHUB_ACTION_PATH_MARKER
                } else if name.eq_ignore_ascii_case("GITHUB_WORKSPACE") {
                    GITHUB_WORKSPACE_MARKER
                } else {
                    "$__VELNOR_DYNAMIC_ENV__"
                };
                characters = lookahead;
                token.push_str(marker);
            } else {
                token.push('%');
            }
            continue;
        }
        if quoted {
            token.push(character);
            continue;
        }
        match character {
            value if value.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                if matches!(value, '\n' | '\r') {
                    if value == '\r' && characters.peek() == Some(&'\n') {
                        let _ = characters.next();
                    }
                    tokens.push(";".to_owned());
                }
            }
            '&' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                if characters.peek() == Some(&'>') {
                    let _ = characters.next();
                    if characters.peek() == Some(&'>') {
                        let _ = characters.next();
                        tokens.push("&>>".to_owned());
                    } else {
                        tokens.push("&>".to_owned());
                    }
                } else if characters.peek() == Some(&'&') {
                    let _ = characters.next();
                    tokens.push("&&".to_owned());
                } else {
                    tokens.push(";".to_owned());
                }
            }
            '|' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                if characters.peek() == Some(&'|') {
                    let _ = characters.next();
                    tokens.push("||".to_owned());
                } else {
                    tokens.push("|".to_owned());
                }
            }
            '(' | ')' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push(character.to_string());
            }
            '<' | '>' => {
                let descriptor = if !token.is_empty()
                    && token.chars().all(|character| character.is_ascii_digit())
                {
                    std::mem::take(&mut token)
                } else {
                    if !token.is_empty() {
                        tokens.push(std::mem::take(&mut token));
                    }
                    String::new()
                };
                let operator = shell_redirection_operator(character, &mut characters);
                tokens.push(format!("{descriptor}{operator}"));
            }
            _ => token.push(character),
        }
    }
    if quoted {
        return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    normalize_windows_shell_tokens(tokens)
}

fn cmd_has_delayed_expansion(command: &str) -> bool {
    let characters = command.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '^' {
            index = (index + 2).min(characters.len());
            continue;
        }
        if characters[index] == '!' {
            let closing = (index + 1..characters.len())
                .take_while(|next| !matches!(characters[*next], '\n' | '\r'))
                .find(|next| characters[*next] == '!');
            if closing.is_some_and(|closing| closing > index + 1) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn normalize_windows_shell_tokens(tokens: Vec<String>) -> Vec<String> {
    tokens
        .into_iter()
        .map(|token| token.replace('\\', "/"))
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "the supported shell lexer keeps quote, escape, expansion, and control-operator state in one place"
)]
fn shell_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            if character == '\n' {
                escaped = false;
                continue;
            }
            if character == '\r' && characters.peek() == Some(&'\n') {
                let _ = characters.next();
                escaped = false;
                continue;
            }
            token.push(character);
            escaped = false;
            continue;
        }
        if quote == Some('"') && character == '\\' {
            escaped = true;
            continue;
        }
        if quote == Some('"') && character == '`' {
            return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
        }
        if quote == Some('"') && character == '$' && characters.peek() == Some(&'(') {
            let _ = characters.next();
            let Some(body) = shell_command_substitution_body(&mut characters) else {
                return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
            };
            token.push_str(SHELL_SUBSTITUTION_START_MARKER);
            token.push_str(&body);
            token.push_str(SHELL_SUBSTITUTION_END_MARKER);
            continue;
        }
        if quote == Some('"')
            && character == '$'
            && let Some(marker) = consume_shell_special_variable(&mut characters)
        {
            token.push_str(marker);
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
        if character == '$' && characters.peek() == Some(&'(') {
            let _ = characters.next();
            let Some(body) = shell_command_substitution_body(&mut characters) else {
                return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
            };
            token.push_str(SHELL_SUBSTITUTION_START_MARKER);
            token.push_str(&body);
            token.push_str(SHELL_SUBSTITUTION_END_MARKER);
            continue;
        }
        if character == '$'
            && let Some(marker) = consume_shell_special_variable(&mut characters)
        {
            token.push_str(marker);
            continue;
        }
        if character == '$' && characters.peek() == Some(&'{') {
            let _ = characters.next();
            if characters.peek() == Some(&'{') {
                let _ = characters.next();
                token.push_str("${{");
                let mut closing_braces = 0;
                for expression_character in characters.by_ref() {
                    token.push(expression_character);
                    if expression_character == '}' {
                        closing_braces += 1;
                        if closing_braces == 2 {
                            break;
                        }
                    } else {
                        closing_braces = 0;
                    }
                }
            } else {
                token.push_str("${");
            }
            continue;
        }
        if character == '`' {
            return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '\\' => escaped = true,
            ';' | '\n' | '\r' => {
                if character == '\r' && characters.peek() == Some(&'\n') {
                    let _ = characters.next();
                }
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push(";".to_owned());
            }
            '&' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                if characters.peek() == Some(&'>') {
                    let _ = characters.next();
                    if characters.peek() == Some(&'>') {
                        let _ = characters.next();
                        tokens.push("&>>".to_owned());
                    } else {
                        tokens.push("&>".to_owned());
                    }
                } else if characters.peek() == Some(&'&') {
                    let _ = characters.next();
                    tokens.push("&&".to_owned());
                } else {
                    tokens.push("&".to_owned());
                }
            }
            '|' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                if characters.peek() == Some(&'|') {
                    let _ = characters.next();
                    tokens.push("||".to_owned());
                } else if characters.peek() == Some(&'&') {
                    let _ = characters.next();
                    tokens.push("|&".to_owned());
                } else {
                    tokens.push("|".to_owned());
                }
            }
            '(' | ')' => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
                tokens.push(character.to_string());
            }
            '<' | '>' => {
                let descriptor = if !token.is_empty()
                    && token.chars().all(|character| character.is_ascii_digit())
                {
                    std::mem::take(&mut token)
                } else {
                    if !token.is_empty() {
                        tokens.push(std::mem::take(&mut token));
                    }
                    String::new()
                };
                let operator = shell_redirection_operator(character, &mut characters);
                tokens.push(format!("{descriptor}{operator}"));
            }
            '{' if token.is_empty() => tokens.push("{".to_owned()),
            '}' if token.is_empty() => tokens.push("}".to_owned()),
            '#' if token.is_empty() => {
                while let Some(comment_character) = characters.next() {
                    if comment_character == '\n' || comment_character == '\r' {
                        if comment_character == '\r' && characters.peek() == Some(&'\n') {
                            let _ = characters.next();
                        }
                        tokens.push(";".to_owned());
                        break;
                    }
                }
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
    if escaped || quote.is_some() {
        return vec![INVALID_SHELL_TOKEN_MARKER.to_owned()];
    }
    tokens
}

fn shell_redirection_operator(
    first: char,
    characters: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> String {
    let mut operator = first.to_string();
    match first {
        '<' if characters.peek() == Some(&'<') => {
            let _ = characters.next();
            operator.push('<');
            if characters.peek() == Some(&'<') {
                let _ = characters.next();
                operator.push('<');
            } else if characters.peek() == Some(&'-') {
                let _ = characters.next();
                operator.push('-');
            }
        }
        '<' if matches!(characters.peek(), Some('>' | '&')) => {
            operator.push(characters.next().unwrap_or_default());
        }
        '>' if matches!(characters.peek(), Some('>' | '|' | '&')) => {
            operator.push(characters.next().unwrap_or_default());
        }
        _ => {}
    }
    operator
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "test assertions name missing fixture evidence"
    )]

    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{normalize_local_action_directory, parse_metadata, shell_references, shell_tokens};
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

    fn pin_fixture_rust_toolchain(root: &Path) {
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.98.1\"\n",
            ),
            "pin Rust toolchain for project-manifest fixture",
        );
    }

    fn write_workflow(root: &Path, relative_path: &str, contents: &str) {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            must(fs::create_dir_all(parent), "create workflow directory");
        }
        must(
            fs::write(path, contents),
            "write workflow reference context",
        );
    }

    #[expect(
        clippy::panic,
        reason = "git fixture setup failures must name their root cause"
    )]
    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "velnor-workflow")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "velnor-workflow")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap_or_else(|error| panic!("run git: {error}"));
        assert!(status.success(), "git {args:?} failed");
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
        assert_eq!(
            shell_references("node ./scripts/main.js"),
            ["./scripts/main.js"]
        );
        assert_eq!(shell_references("node index"), ["index"]);
        assert_eq!(shell_references("bash verify"), ["verify"]);
        assert_eq!(
            shell_references("exec ../../outside.sh"),
            ["../../outside.sh"]
        );
        assert_eq!(
            shell_references("exec -a runner node ./scripts/main.js"),
            ["./scripts/main.js"]
        );
        assert_eq!(shell_references("time -p node index"), ["index"]);
        assert_eq!(shell_references(". scripts/setup"), ["scripts/setup"]);
        assert_eq!(
            shell_references("node --require ./scripts/preload.js ./scripts/main.js"),
            ["./scripts/preload.js", "./scripts/main.js"]
        );
        assert_eq!(
            shell_references("node --require=./scripts/preload.js ./scripts/main.js"),
            ["./scripts/preload.js", "./scripts/main.js"]
        );
        assert_eq!(
            shell_references("node --import=./scripts/import.mjs ./scripts/main.js"),
            ["./scripts/import.mjs", "./scripts/main.js"]
        );
        assert_eq!(
            shell_references("node --loader=./scripts/loader.mjs ./scripts/main.js"),
            ["./scripts/loader.mjs", "./scripts/main.js"]
        );
        assert_eq!(
            shell_references("bash -o pipefail scripts/check.sh"),
            ["scripts/check.sh"]
        );
        assert_eq!(
            shell_references("echo prose.js | node ./scripts/main.js"),
            ["./scripts/main.js"]
        );
        assert_eq!(
            shell_references("cd scripts && node ./build.js"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("if true; then node ./build.js; fi"),
            ["./build.js"]
        );
        assert_eq!(
            shell_references("if cd scripts; then node ./build.js; fi"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("if false; then cd scripts; elif true; then node ./entry.js; else node ./fallback.js; fi"),
            ["./entry.js", "./fallback.js"]
        );
        let existing_directory =
            BTreeSet::from(["scripts/build.js".to_owned(), "fallback.js".to_owned()]);
        assert_eq!(
            super::shell_references_in_action(
                "if cd scripts; then node ./build.js; else node ./fallback.js; fi",
                ".",
                ".",
                &existing_directory,
                "bash",
            ),
            ["scripts/build.js"]
        );
        let missing_directory = BTreeSet::from(["fallback.js".to_owned()]);
        assert_eq!(
            super::shell_references_in_action(
                "if cd scripts; then node ./build.js; else node ./fallback.js; fi",
                ".",
                ".",
                &missing_directory,
                "bash",
            ),
            ["./fallback.js"]
        );
        assert_eq!(
            shell_references("cd scripts | cat; node ./main.js"),
            ["./main.js"]
        );
        assert_eq!(
            shell_references("(cd scripts); node ./main.js"),
            ["./main.js"]
        );
        assert_eq!(
            shell_references("cd scripts & node ./main.js"),
            ["./main.js"]
        );
        assert_eq!(
            shell_references("cd scripts; node ./main.js &"),
            ["scripts/main.js"]
        );
        assert_eq!(
            shell_references("cd scripts; node ./background.js & node ./main.js"),
            ["scripts/background.js", "scripts/main.js"]
        );
        assert_eq!(
            shell_references("{ cd scripts; }; node ./main.js"),
            ["scripts/main.js"]
        );
        assert_eq!(
            shell_references("bash -c 'cd scripts && node ./build.js'"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("bash -c 'node \"$GITHUB_ACTION_PATH/main.js\"'"),
            ["__VELNOR_GITHUB_ACTION_PATH__/main.js"]
        );
        assert_eq!(shell_references("eval 'node ./main.js'"), ["./main.js"]);
        assert_eq!(
            shell_references("eval 'cd scripts && node ./main.js'"),
            ["scripts/main.js"]
        );
        assert_eq!(
            shell_references("echo \"$(node ./nested.js)\""),
            ["./nested.js"]
        );
        assert_eq!(
            shell_references("# node ./comment-only.js && cd ignored\nnode ./actual.js"),
            ["./actual.js"]
        );
        assert_eq!(
            shell_references("cd -P scripts && node ./build.js"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("cd -L -e scripts && node ./build.js"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("cd __VELNOR_GITHUB_ACTION_PATH__/scripts && node ./build.js"),
            ["__VELNOR_GITHUB_ACTION_PATH__/scripts/build.js"]
        );
        assert_eq!(
            shell_references("node \"$GITHUB_ACTION_PATH/main.js\""),
            ["__VELNOR_GITHUB_ACTION_PATH__/main.js"]
        );
        assert_eq!(
            shell_references("node \"${GITHUB_ACTION_PATH}/main.js\""),
            ["__VELNOR_GITHUB_ACTION_PATH__/main.js"]
        );
        assert_eq!(
            shell_references("node '$GITHUB_ACTION_PATH/main.js'"),
            ["$GITHUB_ACTION_PATH/main.js"]
        );
        assert_eq!(
            shell_references("node \\$GITHUB_ACTION_PATH/main.js"),
            ["$GITHUB_ACTION_PATH/main.js"]
        );
        assert_eq!(
            shell_references("cd \"${GITHUB_ACTION_PATH}/scripts\" && node ./build.js"),
            ["__VELNOR_GITHUB_ACTION_PATH__/scripts/build.js"]
        );
        assert_eq!(
            shell_references("node ./one.js\nnode ${{ inputs.script }}/two.js"),
            ["./one.js", "${{ inputs.script }}/two.js"]
        );
        assert_eq!(
            shell_references("python -x ${{ inputs.script }}/entrypoint.py"),
            ["${{ inputs.script }}/entrypoint.py"]
        );
        assert_eq!(
            shell_references("python -X ./config/py.ini ${{ inputs.script }}/entrypoint.py"),
            ["${{ inputs.script }}/entrypoint.py"]
        );
        assert_eq!(
            shell_references("env -i node ${{ inputs.script }}/entrypoint.js"),
            ["${{ inputs.script }}/entrypoint.js"]
        );
        assert_eq!(
            shell_references("env -u TOKEN ACTION_MODE=test node ./scripts/main.js"),
            ["./scripts/main.js"]
        );
        assert_eq!(
            shell_references("env -C scripts node ./build.js"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("env -Cscripts node ./build.js"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("env --chdir=scripts node ./build.js"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("env --chdir scripts node ./build.js"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("env -C /tmp node ./main.js"),
            ["/tmp/main.js"]
        );
        assert_eq!(shell_references("env -S 'node ./build.js'"), ["./build.js"]);
        assert_eq!(
            shell_references("env --split-string='node ./build.js'"),
            ["./build.js"]
        );
        assert_eq!(
            shell_references("env -S 'node --eval=console.log(1)'"),
            ["__VELNOR_UNSUPPORTED_SHELL_CWD__"]
        );
        assert_eq!(
            shell_references("cd scripts >/dev/null && node ./build.js"),
            ["scripts/build.js"]
        );
        assert_eq!(
            shell_references("exec bash < ../shared/check.sh"),
            ["../shared/check.sh"]
        );
        assert_eq!(
            shell_references("bash -eo pipefail -c 'node ./build.js'"),
            ["./build.js"]
        );
    }

    #[test]
    fn shell_reader_operands_resolve_tracked_files_from_current_directory() {
        let files = BTreeSet::from([
            "actions/local/action.yml".to_owned(),
            "actions/local/config.json".to_owned(),
            "actions/shared/policy.json".to_owned(),
            "actions/shared/patterns.txt".to_owned(),
        ]);
        for (command, shell) in [
            ("cat ../shared/policy.json", "bash"),
            ("grep TODO ../shared/policy.json", "bash"),
            (
                "grep -f ../shared/patterns.txt ../shared/policy.json",
                "bash",
            ),
            (
                "grep -f../shared/patterns.txt ../shared/policy.json",
                "bash",
            ),
            (
                "grep --file=../shared/patterns.txt ../shared/policy.json",
                "bash",
            ),
            ("Get-Content ../shared/policy.json", "pwsh"),
            ("type ../shared/policy.json", "cmd"),
            ("cat config.json", "bash"),
        ] {
            let expected = match command {
                "cat config.json" => vec!["config.json"],
                "grep -f ../shared/patterns.txt ../shared/policy.json" => {
                    vec!["../shared/patterns.txt", "../shared/policy.json"]
                }
                "grep -f../shared/patterns.txt ../shared/policy.json"
                | "grep --file=../shared/patterns.txt ../shared/policy.json" => {
                    vec!["../shared/patterns.txt", "../shared/policy.json"]
                }
                _ => vec!["../shared/policy.json"],
            };
            assert_eq!(
                super::shell_references_in_action(
                    command,
                    "actions/local",
                    "actions/local",
                    &files,
                    shell,
                ),
                expected,
                "file operands were not found in {command:?}"
            );
        }
        assert!(super::shell_references_in_action(
            "echo ../shared/policy.json",
            "actions/local",
            "actions/local",
            &files,
            "bash",
        )
        .is_empty());
    }

    #[test]
    fn shell_dynamic_file_construction_fails_closed() {
        for (command, shell) in [
            ("trap 'node ../shared/cleanup.js' EXIT", "bash"),
            ("Invoke-Expression 'node ../shared/build.js'", "pwsh"),
            (
                "setlocal EnableDelayedExpansion & type !GITHUB_ACTION_PATH!\\..\\shared\\policy.json",
                "cmd",
            ),
        ] {
            assert_eq!(
                super::shell_references_in_action(
                    command,
                    "actions/local",
                    "actions/local",
                    &BTreeSet::new(),
                    shell,
                ),
                [super::UNSUPPORTED_SHELL_CWD_MARKER],
                "dynamic file construction was not rejected in {command:?}"
            );
        }
    }

    #[test]
    fn composite_dynamic_shell_forms_fail_closed_before_generating_watches() {
        for (name, shell, command) in [
            (
                "posix-trap",
                "bash",
                "trap 'node ../shared/cleanup.js' EXIT",
            ),
            (
                "powershell-invoke-expression",
                "pwsh",
                "Invoke-Expression 'node ../shared/build.js'",
            ),
            (
                "cmd-delayed-expansion",
                "cmd",
                "setlocal EnableDelayedExpansion & type !GITHUB_ACTION_PATH!\\..\\shared\\policy.json",
            ),
        ] {
            let root = fixture(&format!("dynamic-shell-{name}"));
            must(
                fs::write(
                    root.join("action.yml"),
                    format!(
                        "runs:\n  using: composite\n  steps:\n    - shell: {shell}\n      run: |\n        {command}\n"
                    ),
                ),
                "write dynamic shell fixture",
            );
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| {
                    panic!("dynamic shell construct must fail closed: {name}")
                });
            assert!(
                error.to_string().contains("unsupported shell syntax"),
                "{name}: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn cmd_call_and_cmd_c_nested_scripts_are_scanned() {
        let files = BTreeSet::from([
            "actions/shared/build.cmd".to_owned(),
            "actions/shared/build.js".to_owned(),
        ]);
        for command in [
            "call ..\\shared\\build.cmd",
            "cmd /c \"node ..\\shared\\build.js\"",
        ] {
            assert_eq!(
                super::shell_references_in_action(
                    command,
                    "actions/local",
                    "actions/local",
                    &files,
                    "cmd",
                ),
                if command.starts_with("call") {
                    vec!["../shared/build.cmd"]
                } else {
                    vec!["../shared/build.js"]
                },
                "nested CMD script was not scanned: {command:?}"
            );
        }
        assert!(!super::cmd_has_delayed_expansion("type ^!literal^!"));
        assert!(super::cmd_has_delayed_expansion(
            "type !GITHUB_ACTION_PATH!\\file"
        ));
    }

    #[test]
    fn shell_or_branch_uses_the_original_directory_when_cd_fails() {
        let files = std::collections::BTreeSet::from([
            "actions/local/action.yml".to_owned(),
            "actions/local/main.js".to_owned(),
            "actions/local/scripts/build.js".to_owned(),
        ]);
        assert_eq!(
            super::shell_references_in_action(
                "cd missing || node ./main.js",
                "actions/local",
                "actions/local",
                &files,
                "bash",
            ),
            ["./main.js"]
        );
        assert_eq!(
            super::shell_references_in_action(
                "cd scripts && node ./build.js",
                "actions/local",
                "actions/local",
                &files,
                "bash",
            ),
            ["scripts/build.js"]
        );
    }

    #[test]
    fn shell_exec_target_is_scanned_and_workspace_escape_is_rejected() {
        let root = fixture("exec-outside-script");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: exec ../../outside.sh\n",
            ),
            "write exec escape fixture",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("exec script escape must be found and rejected"));
        assert!(
            error.to_string().contains("escapes its action directory"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cmd_cd_drive_switch_resolves_later_paths_from_new_directory() {
        let files = std::collections::BTreeSet::from([
            "actions/local/action.yml".to_owned(),
            "actions/local/build.js".to_owned(),
            "actions/local/scripts/build.js".to_owned(),
        ]);
        assert_eq!(
            super::shell_references_in_action(
                "cd /d .\\scripts && node .\\build.js",
                "actions/local",
                "actions/local",
                &files,
                "cmd",
            ),
            ["scripts/build.js"]
        );
        assert_eq!(
            super::shell_references_in_action(
                "PUSHD scripts & node .\\build.js",
                "actions/local",
                "actions/local",
                &files,
                "cmd",
            ),
            ["scripts/build.js"]
        );
    }

    #[test]
    fn runner_shell_dialect_matches_pinned_builtin_shells() {
        assert_eq!(
            super::runner_shell_dialect(""),
            if cfg!(windows) {
                super::ShellDialect::PowerShell
            } else {
                super::ShellDialect::Posix
            }
        );
        for shell in ["bash", "sh"] {
            assert_eq!(
                super::runner_shell_dialect(shell),
                super::ShellDialect::Posix
            );
        }
        for shell in ["powershell", "pwsh", "PowerShell.exe -NoProfile {0}"] {
            assert_eq!(
                super::runner_shell_dialect(shell),
                super::ShellDialect::PowerShell
            );
        }
        assert_eq!(super::runner_shell_dialect("cmd"), super::ShellDialect::Cmd);
        assert_eq!(
            super::runner_shell_dialect("python"),
            super::ShellDialect::Python
        );
        assert_eq!(
            super::runner_shell_dialect("custom-shell --script {0}"),
            super::ShellDialect::Other
        );
    }

    #[test]
    fn powershell_set_location_resolves_composite_script_from_new_directory() {
        let root = fixture("powershell-set-location-cwd");
        let action_root = root.join(".github/actions/foo");
        must(
            fs::create_dir_all(action_root.join("scripts")),
            "create PowerShell script directory",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: pwsh\n      working-directory: '${{ github.action_path }}'\n      run: Set-Location scripts; node ./build.js\n",
            ),
            "write PowerShell composite action metadata",
        );
        must(
            fs::write(action_root.join("build.js"), "decoy in initial directory\n"),
            "write decoy script in initial directory",
        );
        must(
            fs::write(
                action_root.join("scripts/build.js"),
                "actual script after Set-Location\n",
            ),
            "write script in PowerShell destination directory",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan PowerShell composite action",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == ".github/actions/foo" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("workflow-selected PowerShell action missing"));
        assert!(
            action
                .pr_commands
                .iter()
                .any(|command| command == "test -f '.github/actions/foo/scripts/build.js'"),
            "script after Set-Location was not selected: {:?}",
            action.pr_commands
        );
        assert!(
            !action
                .pr_commands
                .iter()
                .any(|command| command == "test -f '.github/actions/foo/build.js'"),
            "decoy in the initial directory was selected: {:?}",
            action.pr_commands
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn windows_shell_paths_preserve_backslashes_continuations_and_action_env() {
        let root = fixture("windows-shell-action-paths");
        let action_root = root.join(".github/actions/foo");
        must(
            fs::create_dir_all(action_root.join("scripts")),
            "create Windows shell script directory",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
        );
        let metadata = "runs:\n  using: composite\n  steps:\n    - shell: pwsh\n      working-directory: '${{ github.action_path }}'\n      run: Set-Location .\\scripts; node .\\build.js\n    - shell: pwsh\n      working-directory: '${{ github.action_path }}'\n      run: |\n        node `\n          .\\continued.js\n    - shell: pwsh\n      run: node \"$env:GITHUB_ACTION_PATH/main.js\"\n    - shell: cmd\n      run: node \"%GITHUB_ACTION_PATH%\\cmd.js\"\n    - shell: pwsh\n      working-directory: '${{ github.action_path }}__PATH_SEPARATOR__scripts'\n      run: node .\\working.js\n"
            .replace("__PATH_SEPARATOR__", if cfg!(windows) { "\\" } else { "/" });
        must(
            fs::write(action_root.join("action.yml"), metadata),
            "write PowerShell and cmd composite metadata",
        );
        for (relative, contents) in [
            ("build.js", "decoy in initial directory\n"),
            ("scripts/build.js", "after Set-Location\n"),
            ("continued.js", "after PowerShell continuation\n"),
            ("main.js", "PowerShell action path env\n"),
            ("cmd.js", "cmd action path env\n"),
            ("scripts/working.js", "backslash working directory\n"),
        ] {
            must(
                fs::write(action_root.join(relative), contents),
                "write Windows shell script fixture",
            );
        }

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan PowerShell and cmd paths",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == ".github/actions/foo" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("workflow-selected Windows shell action missing"));
        for reference in [
            ".github/actions/foo/scripts/build.js",
            ".github/actions/foo/continued.js",
            ".github/actions/foo/main.js",
            ".github/actions/foo/cmd.js",
            ".github/actions/foo/scripts/working.js",
        ] {
            assert!(
                action
                    .pr_commands
                    .iter()
                    .any(|command| command == &format!("test -f '{reference}'")),
                "missing Windows-shell reference {reference}: {:?}",
                action.pr_commands
            );
        }
        assert!(
            action
                .pr_commands
                .iter()
                .all(|command| { command != "test -f '.github/actions/foo/build.js'" }),
            "the initial-directory decoy was selected: {:?}",
            action.pr_commands
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn shell_references_ignore_script_suffixes_in_prose_and_inline_code() {
        for command in [
            "echo this description ends in helper.sh",
            "printf '%s' 'the filename is action.js'",
            "bash -c 'echo this description ends in helper.sh'",
        ] {
            assert!(
                shell_references(command).is_empty(),
                "treated prose as a file reference in {command:?}"
            );
        }
        assert_eq!(
            shell_references("node --eval 'console.log(\"action.js\")'"),
            ["__VELNOR_UNSUPPORTED_SHELL_CWD__"]
        );
        assert_eq!(
            shell_references("node --eval=console.log(\"action.js\")"),
            ["__VELNOR_UNSUPPORTED_SHELL_CWD__"]
        );
        assert_eq!(
            shell_references("node `printf main.js`"),
            ["__VELNOR_UNSUPPORTED_SHELL_CWD__"]
        );
        assert_eq!(
            shell_references("! node ./main.js"),
            ["__VELNOR_UNSUPPORTED_SHELL_CWD__"]
        );
        assert_eq!(
            shell_references("eval \"$DYNAMIC_COMMAND\""),
            ["__VELNOR_UNSUPPORTED_SHELL_CWD__"]
        );
    }

    #[test]
    fn inline_node_python_and_powershell_code_fails_closed() {
        for (name, run) in [
            ("node-eval", "node -e \"require('./helper.js')\""),
            ("python-command", "python -c \"import helper\""),
            ("python-module", "python -m helper"),
            (
                "powershell-command",
                "pwsh -Command \"Get-Content ./helper.txt\"",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!(
                        "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: >-\n        {run}\n"
                    ),
                ),
                "write inline interpreter fixture",
            );
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("inline dependency code must fail closed: {name}"));
            assert!(
                error.to_string().contains("unsupported shell syntax"),
                "{error}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn input_deprecation_message_key_is_case_insensitive_and_requires_string() {
        let root = fixture("uppercase-input-deprecation-message");
        must(
            fs::write(
                root.join("action.yml"),
                "inputs:\n  token:\n    DEPRECATIONMESSAGE: Deprecated input\nruns:\n  using: node20\n  main: index.js\n",
            ),
            "write uppercase input deprecation message fixture",
        );
        assert!(parse_metadata(&root, "action.yml").is_ok());

        must(
            fs::write(
                root.join("action.yml"),
                "inputs:\n  token:\n    DEPRECATIONMESSAGE: []\nruns:\n  using: node20\n  main: index.js\n",
            ),
            "write malformed uppercase input deprecation message fixture",
        );
        assert!(parse_metadata(&root, "action.yml").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unsupported_custom_shell_fails_closed_after_runner_format_validation() {
        let root = fixture("unsupported-ruby-shell");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: 'ruby {0}'\n      run: \"load './helper.rb'\"\n",
            ),
            "write custom Ruby shell fixture",
        );
        assert!(
            parse_metadata(&root, "action.yml").is_ok(),
            "Runner accepts a custom shell format with {{0}}"
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("unsupported Ruby script syntax must fail closed"));
        assert!(
            error.to_string().contains("unsupported shell syntax"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runner_shell_formats_allow_only_the_single_script_argument() {
        for format in [
            "bash {0} {1}",
            "ruby {0} {1}",
            "bash {1}",
            "bash {0",
            "bash {0,12}",
            "bash {0:format}",
        ] {
            let root = fixture("invalid-shell-format");
            must(
                fs::write(
                    root.join("action.yml"),
                    format!(
                        "runs:\n  using: composite\n  steps:\n    - shell: '{format}'\n      run: echo ok\n"
                    ),
                ),
                "write invalid Runner shell format fixture",
            );
            let error = parse_metadata(&root, "action.yml")
                .err()
                .unwrap_or_else(|| {
                    panic!("Runner-invalid shell format must be rejected: {format}")
                });
            assert!(error.to_string().contains("shell"), "{error}");
            let _ = fs::remove_dir_all(root);
        }

        for format in ["ruby {0}", "bash {{script}} {0}"] {
            let root = fixture("valid-shell-format");
            must(
                fs::write(
                    root.join("action.yml"),
                    format!(
                        "runs:\n  using: composite\n  steps:\n    - shell: '{format}'\n      run: echo ok\n"
                    ),
                ),
                "write valid Runner shell format fixture",
            );
            assert!(parse_metadata(&root, "action.yml").is_ok(), "{format}");
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn empty_action_shell_uses_the_runner_platform_default() {
        let root = fixture("empty-action-shell-default");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: ''\n      run: node ./main.js\n",
            ),
            "write empty-shell composite metadata",
        );
        must(
            fs::write(root.join("main.js"), "process.exit(0)\n"),
            "write empty-shell dependency",
        );
        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan empty-shell action",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("empty-shell action missing"));
        assert!(action
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'main.js'"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dynamic_python_script_after_skip_first_line_option_fails_closed() {
        let root = fixture("dynamic-python-script");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: python -x ${{ inputs.script }}/entrypoint.py\n",
            ),
            "write Python dynamic script fixture",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("dynamic Python script path must fail closed"));
        assert!(
            error.to_string().contains("static relative path"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dynamic_shell_script_after_escaped_newline_fails_closed() {
        let root = fixture("dynamic-shell-script-continuation");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: |\n        node \\\n          ${{ inputs.script }}/entrypoint.js\n",
            ),
            "write continued dynamic script fixture",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("dynamic script after line continuation must fail closed"));
        assert!(
            error.to_string().contains("static relative path"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dynamic_shell_script_after_env_ignore_option_fails_closed() {
        let root = fixture("dynamic-env-shell-script");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: env -i node ${{ inputs.script }}/entrypoint.js\n",
            ),
            "write env-wrapped dynamic script fixture",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("dynamic script after env options must fail closed"));
        assert!(
            error.to_string().contains("static relative path"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dynamic_node_module_option_fails_closed() {
        let root = fixture("dynamic-node-preload-module");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: node --require=${{ inputs.module }} ./main.js\n",
            ),
            "write dynamic Node module fixture",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("dynamic Node module path must fail closed"));
        assert!(
            error.to_string().contains("static relative path"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dynamic_env_chdir_entrypoint_fails_closed() {
        let root = fixture("dynamic-env-chdir-entrypoint");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: env --chdir=${{ inputs.directory }} node ./main.js\n",
            ),
            "write dynamic env chdir fixture",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("dynamic env chdir must fail closed"));
        assert!(
            error.to_string().contains("static relative path"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dynamic_shell_directory_change_fails_closed() {
        let root = fixture("dynamic-shell-directory-change");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: cd '${{ inputs.directory }}' && node ./main.js\n",
            ),
            "write dynamic shell directory fixture",
        );
        must(
            fs::write(root.join("main.js"), "process.exit(0)\n"),
            "write decoy entrypoint in the initial directory",
        );

        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("dynamic shell directory must fail closed"));
        assert!(
            error.to_string().contains("unsupported shell syntax"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_rejects_null_inputs_and_outputs() {
        for field in ["inputs", "outputs"] {
            let root = fixture(field);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("{field}: null\nruns:\n  using: node20\n  main: index.js\n"),
                ),
                "write null metadata fixture",
            );
            let error = parse_metadata(&root, "action.yml")
                .err()
                .unwrap_or_else(|| panic!("null {field} must be rejected"));
            assert!(error.to_string().contains("action.yml"), "{field}: {error}");
            let _ = fs::remove_dir_all(root);
        }

        for (field, definition) in [
            ("inputs", "  token: null\n"),
            ("outputs", "  result: null\n"),
            ("outputs", "  result:\n    description: []\n"),
        ] {
            let root = fixture(&format!("nested-{field}"));
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("{field}:\n{definition}runs:\n  using: node20\n  main: index.js\n"),
                ),
                "write nested null metadata fixture",
            );
            let error = parse_metadata(&root, "action.yml")
                .err()
                .unwrap_or_else(|| panic!("invalid {field} definition must be rejected"));
            assert!(error.to_string().contains(field), "{field}: {error}");
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn metadata_validates_unmodeled_runs_property_shapes() {
        let invalid_properties = [
            ("docker", "image: docker://", "runs.image"),
            ("docker", "image: DOCKER://", "runs.image"),
            ("docker", "args: null", "runs.args"),
            ("docker", "args: {argument: value}", "runs.args"),
            ("docker", "env: null", "runs.env"),
            ("docker", "env:\n  TOKEN: [value]", "runs.env"),
            ("docker", "env:\n  \"\": token", "runs.env"),
            ("docker", "pre-if: []", "runs.pre-if"),
            ("docker", "pre-if: null", "runs.pre-if"),
            ("docker", "post-if: {}", "runs.post-if"),
            ("docker", "pre-if: ''", "runs.pre-if"),
            ("node20", "pre: []", "runs.pre"),
            ("node20", "post: {}", "runs.post"),
            ("node20", "pre-if: []", "runs.pre-if"),
            ("node20", "post-if: null", "runs.post-if"),
            ("node20", "post-if: {}", "runs.post-if"),
            ("node20", "pre-if: ''", "runs.pre-if"),
            ("node20", "args: []", "runs.args"),
        ];
        for (using, property, expected_field) in invalid_properties {
            let root = fixture(expected_field);
            let property = property.replace('\n', "\n  ");
            let required_runtime_property = if using == "docker" {
                "  image: docker://ubuntu\n"
            } else {
                "  main: index.js\n"
            };
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: {using}\n{required_runtime_property}  {property}\n"),
                ),
                "write malformed runs fixture",
            );
            let error = parse_metadata(&root, "action.yml")
                .err()
                .unwrap_or_else(|| panic!("malformed {expected_field} must be rejected"));
            assert!(
                error.to_string().contains(expected_field),
                "{expected_field}: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }

        for (name, image) in [
            ("ordinary-tag-is-not-docker-image", "ubuntu"),
            ("trailing-space-is-not-dockerfile", "\"Dockerfile \""),
        ] {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: docker\n  image: {image}\n"),
                ),
                "write invalid Docker image fixture",
            );
            let error = parse_metadata(&root, "action.yml")
                .err()
                .unwrap_or_else(|| panic!("Runner-invalid Docker image must be rejected: {name}"));
            assert!(error.to_string().contains("runs.image"), "{error}");
            let _ = fs::remove_dir_all(root);
        }

        for image in [
            "Dockerfile",
            "containers/Dockerfile.release",
            "DOCKER://ubuntu",
        ] {
            let root = fixture("valid-docker-image-shape");
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: docker\n  image: '{image}'\n"),
                ),
                "write valid Docker image fixture",
            );
            assert!(parse_metadata(&root, "action.yml").is_ok(), "{image}");
            let _ = fs::remove_dir_all(root);
        }

        let root = fixture("valid-unmodeled-runs-properties");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: docker\n  image: docker://ubuntu\n  args: [123]\n  env:\n    TOKEN: true\n  pre-if: always()\n  post-if: false\n",
            ),
            "write valid runs fixture",
        );
        assert!(parse_metadata(&root, "action.yml").is_ok());
        let _ = fs::remove_dir_all(root);

        for (name, malformed_runs_field) in [
            ("docker-args-not-sequence", "args: { argument: value }"),
            ("docker-args-nested-sequence", "args: [[nested]]"),
            ("docker-env-not-mapping", "env: []"),
            ("docker-env-nested-value", "env:\n    TOKEN: []"),
            ("docker-pre-if-null", "pre-if: null"),
            ("docker-post-if-empty", "post-if: ''"),
        ] {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: docker\n  image: docker://ubuntu\n  {malformed_runs_field}\n"),
                ),
                "write malformed unmodeled runs field",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_err(),
                "{name} must be rejected"
            );
            let _ = fs::remove_dir_all(root);
        }

        let root = fixture("null-docker-image");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: docker\n  image: null\n",
            ),
            "write null Docker image fixture",
        );
        let error = parse_metadata(&root, "action.yml")
            .err()
            .unwrap_or_else(|| panic!("null Docker image must be rejected"));
        assert!(error.to_string().contains("runs.image"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the pinned Runner schema mismatch matrix keeps runtime variants and field rules auditable together"
    )]
    fn metadata_rejects_runner_schema_mismatches() {
        let invalid_metadata = [
            (
                "wrong-runtime-using",
                "runs:\n  using: ' composite '\n  steps: []\n",
            ),
            (
                "unknown-runtime-field",
                "runs:\n  using: node20\n  main: index.js\n  args: []\n",
            ),
            (
                "camel-case-pre-if",
                "runs:\n  using: node20\n  main: index.js\n  preIf: success()\n",
            ),
            (
                "camel-case-post-if",
                "runs:\n  using: node20\n  main: index.js\n  postIf: always()\n",
            ),
            (
                "camel-case-pre-entrypoint",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  preEntrypoint: setup.sh\n",
            ),
            (
                "camel-case-post-entrypoint",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  postEntrypoint: cleanup.sh\n",
            ),
            (
                "camel-case-working-directory",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      workingDirectory: scripts\n",
            ),
            (
                "camel-case-continue-on-error",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continueOnError: true\n",
            ),
            (
                "unknown-composite-step-field",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      extra: value\n",
            ),
            (
                "shell-format-missing-script-placeholder",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: 'bash -e'\n",
            ),
            (
                "unknown-shell-format-missing-script-placeholder",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: ruby\n",
            ),
            (
                "malformed-composite-if-template",
                "runs:\n  using: composite\n  steps:\n    - if: \"${{ inputs.enabled && }}\"\n      run: echo ok\n      shell: bash\n",
            ),
            (
                "malformed-composite-if-plain",
                "runs:\n  using: composite\n  steps:\n    - if: \"success() &&\"\n      run: echo ok\n      shell: bash\n",
            ),
            (
                "malformed-composite-step-with",
                "runs:\n  using: composite\n  steps:\n    - uses: owner/repo@0123456789012345678901234567890123456789\n      with:\n        value: [123]\n",
            ),
            (
                "invalid-continue-on-error-string",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: 'maybe'\n",
            ),
            (
                "quoted-boolean-continue-on-error",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: 'false'\n",
            ),
            (
                "malformed-continue-on-error-expression",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"${{ inputs.ignore_failure + }}\"\n",
            ),
            (
                "unknown-continue-on-error-context",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"${{ secrets.token }}\"\n",
            ),
            (
                "unknown-continue-on-error-function",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"${{ inputs.ignore_failure && mystery() }}\"\n",
            ),
            (
                "uppercase-default-expression-unknown-context",
                "inputs:\n  token:\n    Default: \"${{ secrets.token }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "empty-continue-on-error-expression",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"${{ }}\"\n",
            ),
            (
                "status-function-not-allowed-in-continue-on-error",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"${{ success() }}\"\n",
            ),
            (
                "output-value-unknown-context",
                "outputs:\n  result:\n    value: \"${{ secrets.token }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "output-value-unsupported-function",
                "outputs:\n  result:\n    value: \"${{ hashFiles('**') }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "output-value-malformed-expression",
                "outputs:\n  result:\n    value: \"${{ steps.build.outputs.result + }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
        ];
        for (name, metadata) in invalid_metadata {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write invalid Runner schema fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_err(),
                "{name} must be rejected"
            );
            let _ = fs::remove_dir_all(root);
        }

        let root = fixture("expression-continue-on-error");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      continue-on-error: \"${{ inputs.ignore_failure || hashFiles('**') == '' }}\"\n",
            ),
            "write continue-on-error expression fixture",
        );
        assert!(parse_metadata(&root, "action.yml").is_ok());
        let _ = fs::remove_dir_all(root);

        let root = fixture("uppercase-input-default-string");
        must(
            fs::write(
                root.join("action.yml"),
                "inputs:\n  token:\n    Default: value\nruns:\n  using: node20\n  main: index.js\n",
            ),
            "write uppercase input default fixture",
        );
        assert!(parse_metadata(&root, "action.yml").is_ok());
        let _ = fs::remove_dir_all(root);

        let root = fixture("uppercase-input-default-any-shape");
        must(
            fs::write(
                root.join("action.yml"),
                "inputs:\n  token:\n    Default: []\nruns:\n  using: node20\n  main: index.js\n",
            ),
            "write uppercase input default shape fixture",
        );
        assert!(parse_metadata(&root, "action.yml").is_err());
        let _ = fs::remove_dir_all(root);

        let root = fixture("uppercase-input-default-expression");
        must(
            fs::write(
                root.join("action.yml"),
                "inputs:\n  token:\n    Default: \"${{ runner.os }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            "write uppercase input default expression fixture",
        );
        assert!(
            parse_metadata(&root, "action.yml").is_ok(),
            "case-insensitive default keys use Runner's default expression context"
        );
        let _ = fs::remove_dir_all(root);

        let root = fixture("output-value-valid-context");
        must(
            fs::write(
                root.join("action.yml"),
                "outputs:\n  result:\n    value: \"prefix-${{ steps.build.outputs.result }}-${{ inputs.suffix }}\"\nruns:\n  using: composite\n  steps:\n    - id: build\n      run: echo ok\n      shell: bash\n",
            ),
            "write valid output value expression fixture",
        );
        assert!(parse_metadata(&root, "action.yml").is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_enforces_field_specific_runner_expression_contexts() {
        let invalid_metadata = [
            (
                "input-default-context",
                "inputs:\n  token:\n    default: \"${{ inputs.token }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "container-args-context",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  args: [\"${{ env.PATH }}\"]\n",
            ),
            (
                "container-env-context",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    TOKEN: \"${{ env.PATH }}\"\n",
            ),
            (
                "composite-run-context",
                "runs:\n  using: composite\n  steps:\n    - run: \"${{ secrets.token }}\"\n      shell: bash\n",
            ),
            (
                "composite-shell-context",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: \"${{ secrets.token }}\"\n",
            ),
            (
                "composite-name-context",
                "runs:\n  using: composite\n  steps:\n    - name: \"${{ secrets.token }}\"\n      run: echo ok\n      shell: bash\n",
            ),
            (
                "composite-if-context",
                "runs:\n  using: composite\n  steps:\n    - if: \"${{ secrets.token }}\"\n      run: echo ok\n      shell: bash\n",
            ),
            (
                "composite-working-directory-context",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      working-directory: \"${{ secrets.token }}\"\n",
            ),
            (
                "composite-with-context",
                "runs:\n  using: composite\n  steps:\n    - uses: owner/repo@0123456789012345678901234567890123456789\n      with:\n        value: \"${{ secrets.token }}\"\n",
            ),
            (
                "composite-env-context",
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n      env:\n        TOKEN: \"${{ secrets.token }}\"\n",
            ),
            (
                "expression-input-name-without-context",
                "inputs:\n  \"${{ runner.os }}\":\n    default: value\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "container-env-key-context",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    \"${{ env.PATH }}\": value\n",
            ),
            (
                "composite-with-key-context",
                "runs:\n  using: composite\n  steps:\n    - uses: owner/repo@0123456789012345678901234567890123456789\n      with:\n        \"${{ secrets.token }}\": value\n",
            ),
        ];
        for (name, manifest) in invalid_metadata {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), manifest),
                "write invalid expression-context fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_err(),
                "{name} must reject an expression outside its Runner context"
            );
            let _ = fs::remove_dir_all(root);
        }

        for (name, manifest) in [
            (
                "valid-input-default-context",
                "inputs:\n  token:\n    default: \"${{ runner.os }}-${{ hashFiles('**') }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "valid-container-context",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  args: [\"${{ inputs.tag }}\"]\n  env:\n    TAG: \"${{ inputs.tag }}\"\n",
            ),
            (
                "valid-container-env-key-context",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  env:\n    \"${{ inputs.env_name }}\": value\n",
            ),
            (
                "valid-composite-context",
                "runs:\n  using: composite\n  steps:\n    - run: \"${{ inputs.command }}\"\n      shell: \"${{ runner.os }}\"\n      name: \"${{ inputs.label }}\"\n      working-directory: \"${{ inputs.directory }}\"\n      env:\n        VALUE: \"${{ inputs.value }}\"\n",
            ),
            (
                "valid-composite-with-key-context",
                "runs:\n  using: composite\n  steps:\n    - uses: owner/repo@0123456789012345678901234567890123456789\n      with:\n        \"${{ inputs.input_name }}\": value\n      env:\n        \"${{ inputs.env_name }}\": \"${{ inputs.value }}\"\n",
            ),
            (
                "valid-composite-if-context",
                "runs:\n  using: composite\n  steps:\n    - if: \"${{ success() && hashFiles('**') == '' }}\"\n      run: echo ok\n      shell: bash\n",
            ),
            (
                "valid-output-context",
                "outputs:\n  result:\n    value: \"${{ steps.build.outputs.result }}-${{ env.PATH }}\"\nruns:\n  using: composite\n  steps:\n    - id: build\n      run: echo ok\n      shell: bash\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), manifest),
                "write valid expression-context fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_ok(),
                "{name} must accept expressions in its Runner context"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn metadata_rejects_contextless_expressions_and_matches_runner_tags() {
        for (name, metadata) in [
            (
                "expression-in-action-name",
                "name: \"${{ github.action_path }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "expression-in-action-description",
                "description: \"${{ github.action_path }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "expression-in-output-description",
                "outputs:\n  result:\n    description: \"${{ github.action_path }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "expression-in-docker-image",
                "runs:\n  using: docker\n  image: \"${{ inputs.image }}\"\n",
            ),
            (
                "expression-in-docker-entrypoint",
                "runs:\n  using: docker\n  image: docker://ubuntu\n  entrypoint: \"${{ inputs.entrypoint }}\"\n",
            ),
            (
                "expression-in-loose-input-property",
                "inputs:\n  token:\n    required: \"${{ runner.os }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "expression-in-loose-root-property",
                "custom: \"${{ github.action_path }}\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write contextless action metadata fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_err(),
                "{name} must reject expressions without a Runner context"
            );
            let _ = fs::remove_dir_all(root);
        }

        let root = fixture("runner-explicit-core-tags");
        must(
            fs::write(
                root.join("action.yml"),
                "inputs:\n  integer:\n    default: !!int 7\n  float:\n    default: !!float 1.25\n  string:\n    default: !!str 007\n  boolean:\n    default: !!bool true\nruns:\n  using: node20\n  main: index.js\n",
            ),
            "write explicit core scalar tags fixture",
        );
        let metadata = parse_metadata(&root, "action.yml")
            .expect("Runner accepts supported explicit core scalar tags");
        for (name, expected) in [
            ("integer", "7"),
            ("float", "1.25"),
            ("string", "007"),
            ("boolean", "true"),
        ] {
            let value = metadata
                .inputs
                .get(name)
                .and_then(serde_yaml::Value::as_mapping)
                .and_then(|input| input.get("default"))
                .and_then(super::metadata_scalar_string);
            assert_eq!(value.as_deref(), Some(expected), "{name}");
        }
        let _ = fs::remove_dir_all(root);

        for (name, metadata) in [
            (
                "custom-scalar-tag",
                "inputs:\n  token:\n    required: !custom value\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "quoted-explicit-integer-tag",
                "inputs:\n  token:\n    default: !!int \"7\"\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "invalid-explicit-integer-tag",
                "inputs:\n  token:\n    default: !!int not-a-number\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "invalid-explicit-boolean-tag",
                "custom: !!bool yes\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "invalid-explicit-null-tag",
                "custom: !!null not-null\nruns:\n  using: node20\n  main: index.js\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write invalid explicit scalar tag fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_err(),
                "{name} must follow Runner's YAML scalar-tag handling"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn metadata_uses_runner_g15_for_numeric_string_fields() {
        let root = fixture("runner-g15-numeric-values");
        must(
            fs::write(
                root.join("action.yml"),
                "inputs:\n  precise:\n    default: 1.2345678901234567\n  exponent:\n    default: 1e15\n  tiny:\n    default: 1e-5\n  negative-zero:\n    default: -0\n  hex:\n    default: 0xFFFFFFFF\n  octal:\n    default: 0o10\nruns:\n  using: node20\n  main: index.js\n",
            ),
            "write G15 numeric fixture",
        );
        let metadata = parse_metadata(&root, "action.yml").expect("Runner numbers parse");
        let input = metadata
            .inputs
            .get("precise")
            .and_then(serde_yaml::Value::as_mapping)
            .unwrap_or_else(|| panic!("precise input mapping"));
        let default = input
            .get("default")
            .unwrap_or_else(|| panic!("precise input default"));
        assert_eq!(
            super::metadata_scalar_string(default).as_deref(),
            Some("1.23456789012346")
        );
        let negative_zero = metadata
            .inputs
            .get("negative-zero")
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|input| input.get("default"))
            .and_then(super::metadata_scalar_string);
        assert_eq!(negative_zero.as_deref(), Some("-0"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn registered_runner_plugin_actions_have_no_repository_entrypoints() {
        let root = fixture("runner-plugin-action");
        must(
            fs::write(root.join("action.yml"), "runs:\n  plugin: checkout\n"),
            "write Runner plugin action fixture",
        );
        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan registered Runner plugin action",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("Runner plugin action unit missing"));
        assert!(!action.capabilities.docker);
        assert!(action
            .pr_commands
            .iter()
            .all(|command| !command.starts_with("test -f ")));
        let _ = fs::remove_dir_all(root);

        for (name, metadata) in [
            (
                "unknown-runner-plugin-action",
                "runs:\n  plugin: not-registered\n",
            ),
            (
                "mixed-runner-plugin-action",
                "runs:\n  plugin: checkout\n  using: node20\n  main: index.js\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write invalid Runner plugin action fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_err(),
                "{name} must be rejected"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn metadata_rejects_yaml_anchors_aliases_and_collection_keys() {
        let invalid_metadata = [
            (
                "action-anchor",
                "runs: &runs\n  using: node20\n  main: index.js\n",
            ),
            (
                "action-alias",
                "defaults: &defaults\n  default: shared\ninputs:\n  token: *defaults\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "block-collection-key",
                "? [complex, key]\n: value\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "flow-collection-key",
                "inputs: { [complex, key]: {default: value} }\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "tagged-mapping-key",
                "inputs:\n  !custom token:\n    default: value\nruns:\n  using: node20\n  main: index.js\n",
            ),
        ];
        for (name, metadata) in invalid_metadata {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write invalid YAML syntax fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_err(),
                "{name} must be rejected"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn metadata_matches_runner_scalar_key_coercion() {
        for (name, metadata) in [
            (
                "block-null-input-key",
                "inputs:\n  null:\n    default: value\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "flow-null-input-key",
                "inputs: {null: {default: value}}\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "tilde-null-output-key",
                "outputs:\n  ~:\n    value: value\nruns:\n  using: node20\n  main: index.js\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write null YAML key fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_ok(),
                "Runner stringifies null scalar mapping keys: {name}"
            );
            let _ = fs::remove_dir_all(root);
        }

        for (name, metadata) in [
            (
                "coerced-scalar-input-keys",
                "inputs:\n  123:\n    default: number\n  true:\n    default: boolean\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "coerced-scalar-output-keys",
                "outputs:\n  123:\n    value: number\n  false:\n    value: boolean\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "nested-any-scalar-keys",
                "custom: {null: empty-key, 42: numeric-key, false: boolean-key}\nruns:\n  using: node20\n  main: index.js\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write scalar-coercion metadata fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_ok(),
                "Runner stringifies scalar mapping keys: {name}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn metadata_rejects_runner_mapping_case_mismatches() {
        for (name, metadata) in [
            (
                "case-insensitive-input-duplicate",
                "inputs:\n  token:\n    default: first\n  TOKEN:\n    default: second\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "exact-input-metadata-duplicate",
                "inputs:\n  token:\n    default: first\n    default: second\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "unicode-case-insensitive-input-duplicate",
                "inputs:\n  é:\n    default: first\n  É:\n    default: second\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "sigma-final-sigma-input-duplicate",
                "inputs:\n  Σ:\n    default: first\n  ς:\n    default: second\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "greek-extended-case-input-duplicate",
                "inputs:\n  ᾀ:\n    default: first\n  ᾈ:\n    default: second\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "numeric-deprecation-message",
                "inputs:\n  token:\n    DeprecationMessage: 42\nruns:\n  using: node20\n  main: index.js\n",
            ),
            (
                "uppercase-output-schema-key",
                "outputs:\n  result:\n    Description: result\nruns:\n  using: node20\n  main: index.js\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write case-mismatched metadata fixture",
            );
            assert!(
                parse_metadata(&root, "action.yml").is_err(),
                "{name} must be rejected"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn local_action_reference_preserves_linux_backslashes() {
        assert_eq!(
            must(
                normalize_local_action_directory(".\\actions\\foo"),
                "normalize backslash-prefixed local action reference"
            ),
            if cfg!(windows) {
                "actions/foo"
            } else {
                ".\\actions\\foo"
            }
        );
        assert_eq!(
            must(
                normalize_local_action_directory(".\\"),
                "normalize Windows workspace reference"
            ),
            if cfg!(windows) { "." } else { ".\\" }
        );
        assert!(!super::looks_like_local_action_reference(" ./actions/foo"));
        assert!(!super::looks_like_local_action_reference("$/actions/foo"));
        assert!(normalize_local_action_directory(" ./actions/foo").is_err());
        assert_eq!(
            must(
                normalize_local_action_directory("./actions/../foo"),
                "normalize safe workspace parent reference"
            ),
            "foo"
        );
        assert!(normalize_local_action_directory("./../outside").is_err());
        assert_eq!(
            must(
                normalize_local_action_directory("./C:/action"),
                "normalize Linux-relative drive-looking path"
            ),
            "C:/action"
        );
        assert_eq!(
            super::normalize_repository_relative_path("actions/local", r"scripts\main.js"),
            Some(r"actions/local/scripts\main.js".to_owned())
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn linux_local_action_paths_keep_backslashes_as_filename_characters() {
        let root = fixture("linux-literal-backslash-action-path");
        let action_root = root.join(r".\actions\foo");
        let traversal_looking_root = root.join(r".\..\outside");
        must(
            fs::create_dir_all(&action_root),
            "create action directory with literal backslash",
        );
        must(
            fs::create_dir_all(&traversal_looking_root),
            "create in-repository directory whose name resembles traversal on Windows",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: '.\\actions\\foo'\n      - uses: '.\\..\\outside'\n",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: node20\n  main: scripts\\main.js\n",
            ),
            "write action manifest with literal backslash entrypoint",
        );
        must(
            fs::write(action_root.join(r"scripts\main.js"), "process.exit(0)\n"),
            "write literal backslash entrypoint",
        );
        must(
            fs::write(
                traversal_looking_root.join("action.yml"),
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            "write action under traversal-looking literal path",
        );
        must(
            fs::write(traversal_looking_root.join("index.js"), "process.exit(0)\n"),
            "write in-repository traversal-looking entrypoint",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan Linux backslash paths",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == r".\actions\foo")
            .unwrap_or_else(|| panic!("backslash local action missing"));
        assert!(action
            .pr_commands
            .iter()
            .any(|command| command == r"test -f '.\actions\foo/scripts\main.js'"));
        assert!(shape.units.iter().any(|unit| {
            unit.root == r".\..\outside"
                && unit
                    .pr_commands
                    .iter()
                    .any(|command| command == r"test -f '.\..\outside/index.js'")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn composite_entrypoints_allow_parent_paths_only_inside_repository() {
        let files = BTreeSet::from([
            ".github/actions/shared/tool.js".to_owned(),
            "package.json".to_owned(),
        ]);
        let mut references = BTreeSet::new();
        must(
            super::add_local_reference(
                &mut references,
                "../shared/tool.js",
                ".github/actions/foo",
                &files,
            ),
            "resolve safe action-relative parent path",
        );
        must(
            super::add_local_reference(
                &mut references,
                "../../../package.json",
                ".github/actions/foo",
                &files,
            ),
            "resolve safe repository-root parent path",
        );
        assert_eq!(
            references,
            BTreeSet::from([
                ".github/actions/shared/tool.js".to_owned(),
                "package.json".to_owned(),
            ])
        );
        assert!(super::add_local_reference(
            &mut BTreeSet::new(),
            "../../../../outside.js",
            ".github/actions/foo",
            &files,
        )
        .is_err());
    }

    #[test]
    fn composite_external_uses_matches_runner_reference_shapes() {
        const SHA: &str = "0123456789012345678901234567890123456789";
        for (name, reference) in [
            ("full-sha-owner-repo", format!("owner/repo@{SHA}")),
            (
                "full-sha-owner-repo-path",
                format!("owner/repo/tools/action@{SHA}"),
            ),
            (
                "full-sha-backslash-repository-path",
                format!("owner\\repo\\tools\\action@{SHA}"),
            ),
            (
                "full-sha-repeated-separators",
                format!("///owner//repo///tools//action@{SHA}"),
            ),
            ("full-sha-leading-separator", format!("/owner/repo@{SHA}")),
            (
                "container-registry-reference",
                "docker://alpine:3.8".to_owned(),
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: composite\n  steps:\n    - uses: '{reference}'\n"),
                ),
                "write valid external action reference fixture",
            );
            let shape = must(
                super::super::scan_shape(&root, &providers(), "main", &[]),
                "scan valid external action reference",
            );
            assert!(
                shape.units.iter().any(|unit| {
                    unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction
                }),
                "Runner-valid reference `{reference}` must produce an action unit"
            );
            if reference.starts_with("docker://") {
                let action = shape
                    .units
                    .iter()
                    .find(|unit| unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction)
                    .unwrap_or_else(|| panic!("external-reference wrapper action missing"));
                assert!(
                    action
                        .pr_commands
                        .iter()
                        .all(|command| !command.starts_with("test -f ")),
                    "external references must not resolve repository files: {:?}",
                    action.pr_commands
                );
            }
            let _ = fs::remove_dir_all(root);
        }

        for (name, reference) in [
            ("owner-only-reference", format!("owner@{SHA}")),
            ("missing-ref-reference", "owner/repo@".to_owned()),
            ("multiple-ref-separators", format!("owner/repo@{SHA}@extra")),
            ("empty-container-registry-image", "docker://".to_owned()),
            (
                "mixed-case-container-registry-reference",
                "Docker://alpine:3.8".to_owned(),
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: composite\n  steps:\n    - uses: '{reference}'\n"),
                ),
                "write malformed external action reference fixture",
            );
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("malformed Runner reference `{reference}` must fail"));
            if reference == "docker://" {
                assert!(
                    error.to_string().contains("must name an image"),
                    "{reference}: {error}"
                );
            } else {
                assert!(
                    error.to_string().contains("{org}/{repo}[/path]@ref"),
                    "{reference}: {error}"
                );
            }
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn self_repository_action_paths_fail_closed_without_repository_context() {
        let root = fixture("self-repository-docker-action");
        let docker_action = root.join(".github/actions/docker-only");
        must(
            fs::create_dir_all(&docker_action),
            "create self-repository Docker action",
        );
        must(
            fs::write(docker_action.join("Dockerfile"), "FROM scratch\n"),
            "write self-repository Dockerfile action",
        );
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: '$//.github/actions/docker-only'\n",
            ),
            "write composite self-repository reference",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: '$/.github/actions/docker-only'\n",
        );

        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("self-repository target must require workflow context"));
        assert!(
            error
                .to_string()
                .contains("requires the workflow repository and ref"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);

        for (name, reference) in [
            ("empty-self-reference", "$/"),
            ("escaping-self-reference", "$/../../outside"),
        ] {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: composite\n  steps:\n    - uses: '{reference}'\n"),
                ),
                "write unsafe self-repository action reference",
            );
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("unsafe self-repository path must fail: {reference}"));
            assert!(
                error
                    .to_string()
                    .contains("requires the workflow repository and ref"),
                "{reference}: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }

        for reference in ["$/C:/outside", "$/\\outside"] {
            let error = super::self_repository_action_path(reference)
                .err()
                .unwrap_or_else(|| panic!("self-repository path needs workflow context"));
            assert!(
                error
                    .to_string()
                    .contains("requires the workflow repository and ref"),
                "{reference}: {error}"
            );
        }
    }

    #[test]
    fn workflow_self_repository_reference_fails_closed_without_context() {
        let root = fixture("workflow-self-repository-docker-only");
        let action_root = root.join(".github/actions/docker-only");
        must(
            fs::create_dir_all(&action_root),
            "create self-repository Docker action",
        );
        must(
            fs::write(action_root.join("Dockerfile"), "FROM scratch\n"),
            "write Dockerfile-only action",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: '$/.github/actions/docker-only'\n",
        );

        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("workflow self-repository reference needs context"));
        assert!(
            error
                .to_string()
                .contains("requires the workflow repository and ref"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn composite_run_external_file_is_in_unit_watch() {
        let root = fixture("composite-external-file-watch");
        let action_root = root.join(".github/actions/foo");
        must(
            fs::create_dir_all(&action_root),
            "create selected composite action",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/shared")),
            "create shared script directory",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      working-directory: '${{ github.action_path }}'\n      run: |\n        cat ../shared/policy.json >/dev/null\n        node ../shared/tool.js\n        node \"${{ github.action_path }}/../shared/tool.js\"\n",
            ),
            "write composite external file reference",
        );
        must(
            fs::write(
                root.join(".github/actions/shared/tool.js"),
                "process.exit(0)\n",
            ),
            "write shared script",
        );
        must(
            fs::write(
                root.join(".github/actions/shared/policy.json"),
                "{\"allowed\": true}\n",
            ),
            "write shared file reader input",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan composite external file reference",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == ".github/actions/foo" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("selected composite action unit missing"));
        assert!(
            action
                .watch
                .iter()
                .any(|path| path == ".github/actions/shared/tool.js"),
            "external composite file must be watched: {:?}",
            action.watch
        );
        assert!(
            action
                .pr_commands
                .iter()
                .any(|command| command == "test -f '.github/actions/shared/tool.js'"),
            "external composite file must be checked: {:?}",
            action.pr_commands
        );
        assert!(
            action
                .watch
                .iter()
                .any(|path| path == ".github/actions/shared/policy.json"),
            "ordinary file operands must be watched: {:?}",
            action.watch
        );
        assert!(
            action
                .pr_commands
                .iter()
                .any(|command| { command == "test -f '.github/actions/shared/policy.json'" }),
            "ordinary file operands must be checked: {:?}",
            action.pr_commands
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn node_static_dependencies_outside_action_root_are_watched_recursively() {
        let root = fixture("node-static-dependencies");
        let action_root = root.join(".github/actions/local");
        let shared_root = root.join(".github/actions/shared");
        must(
            fs::create_dir_all(&action_root),
            "create Node action directory",
        );
        must(
            fs::create_dir_all(&shared_root),
            "create shared Node dependency directory",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/local\n",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            "write Node action metadata",
        );
        for (path, source) in [
            (
                action_root.join("index.js"),
                "const shared = require('../shared/lib.js');\nimport { value } from '../shared/export.mjs';\nimport './helper.js';\n",
            ),
            (action_root.join("helper.js"), "module.exports = true;\n"),
            (
                shared_root.join("lib.js"),
                "module.exports = require('./nested.js');\n",
            ),
            (shared_root.join("nested.js"), "module.exports = true;\n"),
            (
                shared_root.join("export.mjs"),
                "export { value } from './leaf.mjs';\n",
            ),
            (shared_root.join("leaf.mjs"), "export const value = true;\n"),
        ] {
            must(fs::write(path, source), "write Node source dependency");
        }

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan Node action dependencies",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == ".github/actions/local"
                    && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("workflow-selected Node action missing"));
        for path in [
            ".github/actions/local/index.js",
            ".github/actions/local/helper.js",
            ".github/actions/shared/lib.js",
            ".github/actions/shared/nested.js",
            ".github/actions/shared/export.mjs",
            ".github/actions/shared/leaf.mjs",
        ] {
            assert!(
                action.watch.iter().any(|watched| watched == path),
                "Node dependency was not watched: {path}; {:?}",
                action.watch
            );
            assert!(
                action
                    .pr_commands
                    .iter()
                    .any(|command| command == &format!("test -f '{path}'")),
                "Node dependency was not checked: {path}; {:?}",
                action.pr_commands
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn node_symlink_entrypoint_resolves_imports_from_canonical_target() {
        let root = fixture("node-symlink-entrypoint");
        let action_root = root.join(".github/actions/local");
        let shared_root = root.join(".github/actions/shared");
        must(
            fs::create_dir_all(&action_root),
            "create symlinked Node action directory",
        );
        must(
            fs::create_dir_all(&shared_root),
            "create canonical Node source directory",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/local\n",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: node20\n  main: linked.js\n",
            ),
            "write symlinked Node action metadata",
        );
        must(
            fs::write(
                shared_root.join("main.js"),
                "module.exports = require('./dependency.js');\n",
            ),
            "write canonical Node entrypoint",
        );
        must(
            fs::write(
                shared_root.join("dependency.js"),
                "module.exports = true;\n",
            ),
            "write canonical Node dependency",
        );
        must(
            std::os::unix::fs::symlink("../shared/main.js", action_root.join("linked.js")),
            "link Node action entrypoint to shared source",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan symlinked Node action",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == ".github/actions/local"
                    && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("workflow-selected symlinked Node action missing"));
        for path in [
            ".github/actions/local/linked.js",
            ".github/actions/shared/main.js",
            ".github/actions/shared/dependency.js",
        ] {
            assert!(
                action.watch.iter().any(|watched| watched == path),
                "symlink-resolved Node input was not watched: {path}; {:?}",
                action.watch
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_dockerfile_parent_build_context_is_watched() {
        let root = fixture("external-docker-build-context");
        let action_root = root.join(".github/actions/local");
        let build_context = root.join(".github/actions/shared");
        must(
            fs::create_dir_all(&action_root),
            "create local Docker action",
        );
        must(
            fs::create_dir_all(&build_context),
            "create external Docker build context",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/local\n",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: docker\n  image: ../shared/Dockerfile\n",
            ),
            "write Docker action metadata",
        );
        must(
            fs::write(build_context.join("Dockerfile"), "FROM scratch\n"),
            "write external Dockerfile",
        );
        must(
            fs::write(build_context.join("copy.txt"), "build context input\n"),
            "write sibling Docker build input",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan Docker action build context",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == ".github/actions/local"
                    && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("workflow-selected Docker action missing"));
        assert!(
            action
                .watch
                .iter()
                .any(|path| path == ".github/actions/shared/**"),
            "external Docker build context was not watched: {:?}",
            action.watch
        );
        assert!(action
            .pr_commands
            .iter()
            .any(|command| command == "test -f '.github/actions/shared/Dockerfile'"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn metadata_entrypoint_backslashes_follow_runner_host_path_rules() {
        let node = fixture("metadata-backslash-node-paths");
        let node_files = if cfg!(windows) {
            ["scripts/main.js", "scripts/pre.js", "scripts/post.js"]
        } else {
            ["scripts\\main.js", "scripts\\pre.js", "scripts\\post.js"]
        };
        if cfg!(windows) {
            must(
                fs::create_dir_all(node.join("scripts")),
                "create Node scripts",
            );
        }
        must(
            fs::write(
                node.join("action.yml"),
                "runs:\n  using: node20\n  main: 'scripts\\main.js'\n  pre: 'scripts\\pre.js'\n  post: 'scripts\\post.js'\n",
            ),
            "write Node metadata with Windows separators",
        );
        for relative in node_files {
            must(
                fs::write(node.join(relative), "process.exit(0);\n"),
                "write Node metadata entrypoint",
            );
        }
        let node_shape = must(
            super::super::scan_shape(&node, &providers(), "main", &[]),
            "scan Node metadata backslash paths",
        );
        let node_unit = node_shape
            .units
            .iter()
            .find(|unit| unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("Node action missing"));
        for relative in node_files {
            assert!(
                node_unit
                    .pr_commands
                    .iter()
                    .any(|command| command == &format!("test -f '{relative}'")),
                "Node backslash entrypoint was not selected: {relative}"
            );
        }
        let _ = fs::remove_dir_all(node);

        let docker = fixture("metadata-backslash-docker-path");
        if cfg!(windows) {
            must(
                fs::create_dir_all(docker.join("sub")),
                "create Docker subdir",
            );
        }
        must(
            fs::write(
                docker.join("action.yml"),
                "runs:\n  using: docker\n  image: 'sub\\Dockerfile'\n",
            ),
            "write Docker metadata with Windows separator",
        );
        let dockerfile = if cfg!(windows) {
            "sub/Dockerfile"
        } else {
            "sub\\Dockerfile"
        };
        must(
            fs::write(docker.join(dockerfile), "FROM scratch\n"),
            "write Dockerfile with host path semantics",
        );
        let docker_shape = must(
            super::super::scan_shape(&docker, &providers(), "main", &[]),
            "scan Docker metadata backslash path",
        );
        let docker_unit = docker_shape
            .units
            .iter()
            .find(|unit| unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("Docker action missing"));
        assert!(docker_unit
            .pr_commands
            .iter()
            .any(|command| command == &format!("test -f '{dockerfile}'")));
        let _ = fs::remove_dir_all(docker);

        let composite = fixture("metadata-backslash-working-directory");
        let action_root = composite.join(".github/actions/foo");
        let working_directory = if cfg!(windows) {
            action_root.join("scripts")
        } else {
            composite.join(".github/actions/foo\\scripts")
        };
        must(
            fs::create_dir_all(&action_root),
            "create composite action with backslash working directory",
        );
        must(
            fs::create_dir_all(&working_directory),
            "create host-specific action working directory",
        );
        write_workflow(
            &composite,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: pwsh\n      working-directory: '${{ github.action_path }}\\scripts'\n      run: node ./working.js\n",
            ),
            "write action working directory with backslash separator",
        );
        must(
            fs::write(working_directory.join("working.js"), "process.exit(0)\n"),
            "write host-specific working-directory dependency",
        );
        let composite_shape = must(
            super::super::scan_shape(&composite, &providers(), "main", &[]),
            "scan action working directory with backslash separator",
        );
        let composite_unit = composite_shape
            .units
            .iter()
            .find(|unit| {
                unit.root == ".github/actions/foo" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("composite action missing"));
        let working_file = if cfg!(windows) {
            ".github/actions/foo/scripts/working.js"
        } else {
            ".github/actions/foo\\scripts/working.js"
        };
        assert!(
            composite_unit
                .pr_commands
                .iter()
                .any(|command| command == &format!("test -f '{working_file}'")),
            "backslash working-directory used the wrong host path: {:?}",
            composite_unit.pr_commands
        );
        let _ = fs::remove_dir_all(composite);
    }

    #[test]
    fn composite_action_accepts_explicit_empty_steps_sequence() {
        let root = fixture("empty-composite-steps");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps: []\n",
            ),
            "write empty composite metadata",
        );
        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan empty composite action",
        );
        assert!(shape
            .units
            .iter()
            .any(|unit| { unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction }));
        let _ = fs::remove_dir_all(root);

        for (name, metadata) in [
            ("missing-composite-steps", "runs:\n  using: composite\n"),
            (
                "null-composite-steps",
                "runs:\n  using: composite\n  steps: null\n",
            ),
        ] {
            let root = fixture(name);
            must(
                fs::write(root.join("action.yml"), metadata),
                "write incomplete composite metadata",
            );
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("{name} must fail scan"));
            assert!(error.to_string().contains("runs.steps"), "{name}: {error}");
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn shell_tokens_keep_quoted_script_paths_together() {
        assert_eq!(
            shell_tokens("node './dist/main.js' --flag"),
            ["node", "./dist/main.js", "--flag"]
        );
        assert_eq!(
            shell_tokens("${{ inputs.script }}/entrypoint.sh"),
            ["${{ inputs.script }}/entrypoint.sh"]
        );
        assert_eq!(
            shell_tokens("node \\\n  ${{ inputs.script }}/entrypoint.js"),
            ["node", "${{ inputs.script }}/entrypoint.js"]
        );
        assert_eq!(
            shell_tokens("node \\\r\n  ${{ inputs.script }}/entrypoint.js"),
            ["node", "${{ inputs.script }}/entrypoint.js"]
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
    fn linux_runner_manifest_probe_requires_exact_case() {
        let root = fixture("linux-case-sensitive-action-manifest");
        let action_root = root.join(".github/actions/foo");
        must(
            fs::create_dir_all(&action_root),
            "create exact action directory",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
        );
        must(
            fs::write(
                action_root.join("Action.YML"),
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            "write case-varied Runner manifest",
        );
        must(
            fs::write(action_root.join("index.js"), "process.exit(0)\n"),
            "write action main script",
        );

        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("Linux runner must ignore case-varied manifest names"));
        assert!(
            error
                .to_string()
                .contains("no action metadata or Dockerfile"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn action_entrypoint_probe_uses_exact_runner_names() {
        let metadata_files = BTreeSet::from([
            "action.yml".to_owned(),
            "action.yaml".to_owned(),
            "Action.YML".to_owned(),
        ]);
        let metadata = super::metadata_action_sources(&metadata_files).unwrap();
        assert_eq!(
            metadata.preferred_metadata.get(".").map(String::as_str),
            Some("action.yml")
        );

        let docker_files = BTreeSet::from(["Dockerfile".to_owned(), "dockerfile".to_owned()]);
        let source = super::action_source_in_root(
            ".",
            &docker_files,
            &BTreeMap::new(),
            &BTreeMap::new(),
            true,
        )
        .unwrap_or_else(|| panic!("canonical Dockerfile probe failed"));
        assert_eq!(source.path, "Dockerfile");
    }

    #[test]
    fn action_watch_globs_escape_literal_repository_paths() {
        let root = fixture("literal-glob-action-path");
        let action_root = root.join("actions/foo[bar]");
        must(
            fs::create_dir_all(&action_root),
            "create action path with glob metacharacters",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: './actions/foo[bar]'\n",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - run: echo ok\n      shell: bash\n",
            ),
            "write action metadata",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan action path with glob characters",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == "actions/foo[bar]")
            .unwrap_or_else(|| panic!("literal action path missing"));
        assert!(
            action.watch.iter().any(|pattern| {
                globset::Glob::new(pattern).is_ok_and(|glob| {
                    glob.compile_matcher()
                        .is_match("actions/foo[bar]/action.yml")
                })
            }),
            "literal action path does not match its watch set: {:?}",
            action.watch
        );
        assert!(
            action.watch.iter().all(|pattern| {
                !globset::Glob::new(pattern)
                    .is_ok_and(|glob| glob.compile_matcher().is_match("actions/fooa/action.yml"))
            }),
            "watch set matched a different path: {:?}",
            action.watch
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn shell_globs_add_each_literal_repository_input() {
        let root = fixture("composite-shell-glob-inputs");
        must(
            fs::create_dir_all(root.join("scripts")),
            "create shell glob input directory",
        );
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: node ./scripts/*.js\n",
            ),
            "write shell glob action",
        );
        for name in ["one.js", "two.js"] {
            must(
                fs::write(root.join("scripts").join(name), "process.exit(0)\n"),
                "write shell glob input",
            );
        }

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan shell glob inputs",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("glob action missing"));
        for name in ["one.js", "two.js"] {
            assert!(action
                .pr_commands
                .iter()
                .any(|command| command == &format!("test -f 'scripts/{name}'")));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn shell_github_action_path_resolves_inside_the_composite_action() {
        let root = fixture("github-action-path-shell-reference");
        must(
            fs::create_dir_all(root.join("actions/local")),
            "create local composite action",
        );
        must(
            fs::write(
                root.join("actions/local/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      working-directory: ${{ github.action_path }}\n      run: node \"$GITHUB_ACTION_PATH/main.js\"\n",
            ),
            "write composite metadata using action path",
        );
        must(
            fs::write(root.join("actions/local/main.js"), "process.exit(0)\n"),
            "write action-path script",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan action-path shell reference",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == "actions/local" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("local composite action missing"));
        assert!(
            action
                .pr_commands
                .iter()
                .any(|command| command == "test -f 'actions/local/main.js'"),
            "action path entrypoint not selected: {:?}",
            action.pr_commands
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn root_action_supplements_tracked_build_and_dependency_entrypoints() {
        let root = fixture("root-action-pruned-entrypoints");
        git(&root, &["init", "-q"]);
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: node build/entrypoint.js\n    - shell: bash\n      run: node node_modules/pkg/entrypoint.js\n",
            ),
            "write root action metadata",
        );
        for (path, contents) in [
            ("build/entrypoint.js", "build entrypoint\n"),
            ("node_modules/pkg/entrypoint.js", "dependency entrypoint\n"),
        ] {
            let path = root.join(path);
            if let Some(parent) = path.parent() {
                must(
                    fs::create_dir_all(parent),
                    "create root action entrypoint path",
                );
            }
            must(fs::write(path, contents), "write root action entrypoint");
        }
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "tracked root action entrypoints"]);

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan root action with pruned entrypoints",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("root action unit missing"));
        for path in [
            "test -f 'build/entrypoint.js'",
            "test -f 'node_modules/pkg/entrypoint.js'",
        ] {
            assert!(
                action.pr_commands.iter().any(|command| command == path),
                "missing selected root action path {path}"
            );
        }
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
    fn action_entrypoint_precedence_and_bare_dockerfile_match_runner() {
        let root = fixture("entrypoints");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo selected\n",
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
            fs::write(root.join("actions/bare/Dockerfile"), "FROM scratch\n"),
            "write canonical Dockerfile",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./actions/bare\n",
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
                && unit.pr_commands.iter().any(|command| {
                    command.contains("action.yaml") || command.contains("Action.YML")
                })
        }));
        let bare = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == "actions/bare" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("bare Dockerfile action missing"));
        assert_eq!(bare.kind, crate::s2::UnitKind::GithubAction);
        assert!(bare.capabilities.docker);
        assert!(bare
            .pr_commands
            .iter()
            .any(|command| command
                == "velnor-workflow verify-action --path 'actions/bare/Dockerfile'"));
        assert!(
            !shape
                .files()
                .iter()
                .any(|file| file == ".github/workflows/ci.yml"),
            "workflow reference context leaked into product inputs"
        );
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
    fn unreferenced_docker_contexts_and_reusable_workflows_are_not_actions() {
        let root = fixture("unreferenced-docker-contexts");
        pin_fixture_rust_toolchain(&root);
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"image_project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write root project manifest",
        );
        must(
            fs::write(root.join("Dockerfile"), "FROM scratch\n"),
            "write root image Dockerfile",
        );
        must(
            fs::create_dir_all(root.join("containers/image")),
            "create ordinary nested Docker context",
        );
        must(
            fs::write(root.join("containers/image/Dockerfile"), "FROM scratch\n"),
            "write nested image Dockerfile",
        );
        must(
            fs::create_dir_all(root.join("generated-action")),
            "create generated action target",
        );
        must(
            fs::write(root.join("generated-action/Dockerfile"), "FROM scratch\n"),
            "write generated action target Dockerfile",
        );
        write_workflow(
            &root,
            ".github/workflows/caller.yml",
            "name: caller\njobs:\n  reusable:\n    uses: ./.github/workflows/reusable.yml\n",
        );
        write_workflow(
            &root,
            ".github/workflows/reusable.yml",
            "name: reusable\non:\n  workflow_call:\n",
        );
        write_workflow(
            &root,
            ".github/workflows/generated.yml",
            "# Generated by velnor-workflow.\njobs:\n  build:\n    steps:\n      - uses: ./generated-action\n",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan unreferenced Docker contexts",
        );
        for action_root in [".", "containers/image", "generated-action"] {
            assert!(
                !shape.units.iter().any(|unit| {
                    unit.root == action_root && unit.kind == crate::s2::UnitKind::GithubAction
                }),
                "unreferenced Docker context became an action: {action_root}"
            );
        }
        for docker_root in [".", "containers/image", "generated-action"] {
            assert!(
                shape.units.iter().any(|unit| {
                    unit.root == docker_root && unit.kind == crate::s2::UnitKind::Docker
                }),
                "Docker build context missing: {docker_root}"
            );
        }
        for workflow in [
            ".github/workflows/caller.yml",
            ".github/workflows/reusable.yml",
            ".github/workflows/generated.yml",
        ] {
            assert!(
                !shape.files().iter().any(|file| file == workflow),
                "workflow reference context leaked into product inputs: {workflow}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workflow_step_uses_selects_manifest_backed_dockerfile_action() {
        let root = fixture("workflow-dockerfile-action");
        pin_fixture_rust_toolchain(&root);
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"image_project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write root project manifest",
        );
        must(
            fs::write(root.join("Dockerfile"), "FROM scratch\n"),
            "write root image Dockerfile",
        );
        must(
            fs::create_dir_all(root.join("actions/docker")),
            "create referenced Dockerfile action",
        );
        must(
            fs::write(
                root.join("actions/docker/Cargo.toml"),
                "[package]\nname = \"docker_action\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write action project manifest",
        );
        must(
            fs::write(root.join("actions/docker/Dockerfile"), "FROM scratch\n"),
            "write action Dockerfile",
        );
        must(
            fs::create_dir_all(root.join("containers/build")),
            "create unrelated Docker context",
        );
        must(
            fs::write(root.join("containers/build/Dockerfile"), "FROM scratch\n"),
            "write unrelated Dockerfile",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: actions/checkout@v4\n      - uses: ./actions/docker\n",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan workflow-selected Dockerfile action",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == "actions/docker" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("workflow-selected Dockerfile action missing"));
        assert!(action.capabilities.docker);
        assert!(action
            .pr_commands
            .iter()
            .any(|command| command
                == "velnor-workflow verify-action --path 'actions/docker/Dockerfile'"));
        assert!(
            !shape
                .units
                .iter()
                .any(|unit| { unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction }),
            "root Docker build context became an action"
        );
        assert!(super::verify_action(&root, "actions/docker/Dockerfile").is_ok());
        assert!(
            !shape
                .files()
                .iter()
                .any(|file| file == ".github/workflows/ci.yml"),
            "workflow file entered product inputs"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_composite_local_action_resolves_from_workspace_root() {
        let root = fixture("workspace-root-resolution");
        pin_fixture_rust_toolchain(&root);
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"image_project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write root project manifest",
        );
        must(
            fs::write(root.join("Dockerfile"), "FROM scratch\n"),
            "write root image Dockerfile",
        );
        must(
            fs::create_dir_all(root.join("actions/parent")),
            "create nested parent action",
        );
        must(
            fs::write(
                root.join("actions/parent/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: './actions/child'\n",
            ),
            "write nested parent metadata",
        );
        must(
            fs::create_dir_all(root.join("actions/child")),
            "create workspace-root child action",
        );
        must(
            fs::write(
                root.join("actions/child/Cargo.toml"),
                "[package]\nname = \"child_action\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write child project manifest",
        );
        must(
            fs::write(root.join("actions/child/Dockerfile"), "FROM scratch\n"),
            "write child Dockerfile",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan workspace-root child action",
        );
        let parent = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == "actions/parent" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("nested parent action missing"));
        assert!(parent
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'actions/child/Dockerfile'"));
        assert!(shape.units.iter().any(|unit| {
            unit.root == "actions/child" && unit.kind == crate::s2::UnitKind::GithubAction
        }));
        assert!(
            !shape
                .units
                .iter()
                .any(|unit| { unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction }),
            "root Docker build context became an action"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workflow_selected_github_action_uses_workspace_relative_children() {
        let root = fixture("github-action-workspace-resolution");
        pin_fixture_rust_toolchain(&root);
        must(
            fs::create_dir_all(root.join(".github/actions/foo")),
            "create workflow-selected GitHub action",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ./bar\n",
            ),
            "write GitHub action metadata",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/foo/dist")),
            "create checked-in action dist directory",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/dist/index.js"),
                "entrypoint\n",
            ),
            "write checked-in action dist file",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/foo/node_modules/package")),
            "create ignored action dependency directory",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/node_modules/package/generated.js"),
                "generated\n",
            ),
            "write ignored action dependency file",
        );
        must(
            fs::create_dir_all(root.join("bar")),
            "create workspace-root child action",
        );
        must(
            fs::write(
                root.join("bar/Cargo.toml"),
                "[package]\nname = \"bar\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write child project manifest",
        );
        must(
            fs::write(root.join("bar/Dockerfile"), "FROM scratch\n"),
            "write child Dockerfile",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan explicitly selected .github action",
        );
        let parent = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == ".github/actions/foo" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("workflow-selected .github action missing"));
        assert!(parent.pr_commands.iter().any(|command| {
            command == "velnor-workflow verify-action --path '.github/actions/foo/action.yml'"
        }));
        assert!(parent
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'bar/Dockerfile'"));
        assert!(shape
            .units
            .iter()
            .any(|unit| { unit.root == "bar" && unit.kind == crate::s2::UnitKind::GithubAction }));
        assert!(shape
            .files()
            .iter()
            .any(|file| { file == ".github/actions/foo/action.yml" }));
        assert!(shape
            .files()
            .iter()
            .any(|file| file == ".github/actions/foo/dist/index.js"));
        assert!(shape
            .files()
            .iter()
            .any(|file| file == ".github/actions/foo/node_modules/package/generated.js"));
        assert!(!shape
            .files()
            .iter()
            .any(|file| file == ".github/workflows/ci.yml"));
        assert!(super::verify_action(&root, ".github/actions/foo/action.yml").is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the Git-backed shape fixture keeps reference discovery and product filtering auditable together"
    )]
    fn tracked_workflow_selects_github_action_tree_end_to_end() {
        let root = fixture("tracked-github-action-selection");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        pin_fixture_rust_toolchain(&root);
        must(
            fs::create_dir_all(root.join(".github/workflows")),
            "create tracked workflow directory",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/foo/dist")),
            "create tracked action directory",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/foo/build/output")),
            "create tracked generated action build directory",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/foo/node_modules/pkg")),
            "create tracked generated action dependency",
        );
        must(
            fs::create_dir_all(root.join(".github/actions/foo/.github")),
            "create nested action GitHub directory",
        );
        must(
            fs::create_dir_all(root.join("actions/child")),
            "create workspace-root Dockerfile action",
        );
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"docker_project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write root Rust manifest",
        );
        must(
            fs::write(root.join("Dockerfile"), "FROM scratch\n"),
            "write ordinary root Docker build context",
        );
        must(
            fs::write(
                root.join(".github/workflows/ci.yml"),
                "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
            ),
            "write tracked action workflow reference",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: node ${{ github.action_path }}/build/output/cache\n    - shell: bash\n      run: node ${{ github.action_path }}/node_modules/pkg/index.js\n    - shell: bash\n      run: node ${{ github.action_path }}/.github/entry.js\n    - uses: ./actions/child\n",
            ),
            "write tracked composite action",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/dist/index.js"),
                "checked-in action distribution\n",
            ),
            "write tracked action distribution",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/build/output/cache"),
                "generated action build output\n",
            ),
            "write tracked action build output",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/node_modules/pkg/index.js"),
                "generated dependency\n",
            ),
            "write tracked generated dependency",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/.github/entry.js"),
                "nested action entrypoint\n",
            ),
            "write nested action GitHub file",
        );
        must(
            fs::write(
                root.join("actions/child/Cargo.toml"),
                "[package]\nname = \"child_action\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write child Rust manifest",
        );
        must(
            fs::write(root.join("actions/child/Dockerfile"), "FROM scratch\n"),
            "write child Dockerfile",
        );
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "tracked local action"]);

        must(
            fs::write(
                root.join(".github/actions/foo/untracked.txt"),
                "untracked action decoy\n",
            ),
            "write untracked selected-action decoy",
        );
        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan Git-backed workflow-selected action",
        );
        let parent = shape
            .units
            .iter()
            .find(|unit| unit.root == ".github/actions/foo")
            .unwrap_or_else(|| panic!("Git-backed .github action was not selected"));
        assert!(parent
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'actions/child/Dockerfile'"));
        for reference in [
            ".github/actions/foo/build/output/cache",
            ".github/actions/foo/node_modules/pkg/index.js",
            ".github/actions/foo/.github/entry.js",
        ] {
            assert!(
                parent
                    .pr_commands
                    .iter()
                    .any(|command| command == &format!("test -f '{reference}'")),
                "missing selected action file check for {reference}"
            );
        }
        assert!(shape.units.iter().any(|unit| {
            unit.root == "actions/child" && unit.kind == crate::s2::UnitKind::GithubAction
        }));
        assert!(!shape
            .units
            .iter()
            .any(|unit| { unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction }));
        for included in [
            ".github/actions/foo/action.yml",
            ".github/actions/foo/dist/index.js",
            ".github/actions/foo/node_modules/pkg/index.js",
            ".github/actions/foo/build/output/cache",
            ".github/actions/foo/.github/entry.js",
        ] {
            assert!(
                shape.files().iter().any(|file| file == included),
                "missing {included}"
            );
        }
        for ignored in [
            ".github/actions/foo/untracked.txt",
            ".github/workflows/ci.yml",
        ] {
            assert!(
                !shape.files().iter().any(|file| file == ignored),
                "included {ignored}"
            );
        }
        assert!(super::verify_action(&root, ".github/actions/foo/action.yml").is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workflow_selecting_repository_root_uses_exact_dockerfile_name() {
        let root = fixture("root-dockerfile-precedence");
        must(
            fs::write(root.join("Dockerfile"), "FROM scratch\n"),
            "write exact Dockerfile",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./\n",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan exact root Dockerfile action",
        );
        assert!(shape.units.iter().any(|unit| {
            unit.root == "."
                && unit.kind == crate::s2::UnitKind::GithubAction
                && unit
                    .pr_commands
                    .iter()
                    .any(|command| command == "velnor-workflow verify-action --path 'Dockerfile'")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_and_missing_local_action_paths_fail_closed() {
        for (name, reference) in [
            ("traversal", "./../outside"),
            ("absolute", "/outside"),
            ("double-slash", ".//outside"),
            ("empty-self-repository", "$/"),
            ("drive-forward", "C:/outside"),
            ("drive-backslash", "C:\\outside"),
            ("unc", "\\\\server\\share"),
        ] {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: composite\n  steps:\n    - uses: '{reference}'\n"),
                ),
                "write unsafe local action metadata",
            );
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("unsafe local action path must fail: {reference}"));
            assert!(
                error.to_string().contains("workspace-relative")
                    || error.to_string().contains("escapes the workspace")
                    || error.to_string().contains("static workspace-relative")
                    || error
                        .to_string()
                        .contains("requires the workflow repository and ref")
                    || error.to_string().contains("{org}/{repo}"),
                "{reference}: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }

        let root = fixture("missing-local-action");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ./missing\n",
            ),
            "write missing local action metadata",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("missing local action must fail scan"));
        assert!(error
            .to_string()
            .contains("no action metadata or Dockerfile"));
        let _ = fs::remove_dir_all(root);

        let workflow = fixture("workflow-drive-path");
        write_workflow(
            &workflow,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: C:/outside\n",
        );
        let shape = must(
            super::super::scan_shape(&workflow, &providers(), "main", &[]),
            "scan external workflow reference",
        );
        assert!(!shape
            .units
            .iter()
            .any(|unit| unit.kind == crate::s2::UnitKind::GithubAction));
        let _ = fs::remove_dir_all(workflow);

        let workflow_missing = fixture("workflow-missing-local-action");
        write_workflow(
            &workflow_missing,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./missing\n",
        );
        let error = super::super::scan_shape(&workflow_missing, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("missing workflow action must fail scan"));
        assert!(error
            .to_string()
            .contains("no action metadata or Dockerfile"));
        let _ = fs::remove_dir_all(workflow_missing);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_local_action_paths_follow_contained_targets_and_reject_escapes() {
        use std::os::unix::fs::symlink;

        let root = fixture("symlinked-local-action");
        for directory in ["actions/real", "actions", ".github/workflows", "shared"] {
            must(
                fs::create_dir_all(root.join(directory)),
                "create symlink action tree",
            );
        }
        must(
            fs::write(
                root.join("actions/real/action.yml"),
                "runs:\n  using: docker\n  image: Dockerfile\n",
            ),
            "write real local action metadata",
        );
        must(
            fs::write(root.join("actions/real/Dockerfile"), "FROM scratch\n"),
            "write real local action Dockerfile",
        );
        must(
            symlink("real", root.join("actions/linked")),
            "create contained local action directory symlink",
        );
        must(
            fs::write(
                root.join(".github/workflows/ci.yml"),
                "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./actions/linked\n",
            ),
            "write workflow selecting symlinked local action",
        );
        must(
            fs::write(root.join("shared/main.js"), "process.exit(0)\n"),
            "write symlink entrypoint target",
        );
        must(
            fs::create_dir_all(root.join("actions/entry")),
            "create action with symlinked entrypoint",
        );
        must(
            fs::write(
                root.join("actions/entry/action.yml"),
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            "write action with symlinked entrypoint metadata",
        );
        must(
            symlink("../../shared/main.js", root.join("actions/entry/index.js")),
            "create contained action entrypoint symlink",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan in-checkout action symlinks",
        );
        let linked = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == "actions/linked" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("symlinked action directory was not discovered"));
        assert!(linked.capabilities.docker);
        assert!(linked
            .watch
            .iter()
            .any(|path| path == "actions/real/Dockerfile"));
        let entry = shape
            .units
            .iter()
            .find(|unit| {
                unit.root == "actions/entry" && unit.kind == crate::s2::UnitKind::GithubAction
            })
            .unwrap_or_else(|| panic!("action with symlinked entrypoint was not discovered"));
        assert!(entry.watch.iter().any(|path| path == "shared/main.js"));
        assert!(entry
            .pr_commands
            .iter()
            .any(|command| command == "test -f 'shared/main.js'"));
        let _ = fs::remove_dir_all(root);

        let root = fixture("symlinked-root-entrypoint");
        must(
            fs::write(
                root.join("action.yaml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo selected\n",
            ),
            "write lowercase action metadata",
        );
        must(
            fs::write(root.join("Dockerfile"), "FROM scratch\n"),
            "write Dockerfile fallback",
        );
        must(
            symlink("action.yaml", root.join("action.yml")),
            "create preferred metadata symlink",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./\n",
        );
        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "read in-checkout action metadata symlink",
        );
        assert!(shape.units.iter().any(|unit| {
            unit.root == "."
                && unit.kind == crate::s2::UnitKind::GithubAction
                && unit
                    .pr_commands
                    .iter()
                    .any(|command| command == "velnor-workflow verify-action --path 'action.yml'")
        }));
        let _ = fs::remove_dir_all(root);

        let root = fixture("escaping-symlink-action-directory");
        let outside = fixture("symlink-action-outside-target");
        must(
            fs::create_dir_all(root.join("actions")),
            "create action directory for escaping symlink",
        );
        must(
            fs::write(outside.join("action.yml"), "runs: {}\n"),
            "write outside symlink target metadata",
        );
        must(
            symlink(&outside, root.join("actions/escape")),
            "create escaping action directory symlink",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "jobs:\n  build:\n    steps:\n      - uses: ./actions/escape\n",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("symlink target outside checkout must fail"));
        assert!(
            error.to_string().contains("escapes the checkout"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);

        let root = fixture("cyclic-action-symlink");
        let action_root = root.join("actions/cycle");
        must(
            fs::create_dir_all(&action_root),
            "create cyclic action directory",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo ok\n",
            ),
            "write cyclic action metadata",
        );
        must(
            symlink(".", action_root.join("loop")),
            "create action symlink cycle",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "jobs:\n  build:\n    steps:\n      - uses: ./actions/cycle\n",
        );
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("symlink cycle must fail closed"));
        assert!(error.to_string().contains("cycle"), "{error}");
        let _ = fs::remove_dir_all(root);

        let root = fixture("unused-dockerfile-symlink");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo selected\n",
            ),
            "write preferred valid action metadata",
        );
        must(
            symlink("missing-target", root.join("Dockerfile")),
            "create unused Dockerfile symlink",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./\n",
        );
        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "select valid metadata without inspecting unused Dockerfile fallback",
        );
        assert!(shape
            .units
            .iter()
            .any(|unit| { unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dockerfile_fallback_matches_runner_for_manifest_backed_local_actions() {
        let root = fixture("manifest-backed-docker-actions");
        pin_fixture_rust_toolchain(&root);
        let manifest_fixtures = [
            (
                "cargo",
                "Cargo.toml",
                "[package]\nname = \"cargo_docker_action\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            (
                "node",
                "package.json",
                "{\"name\":\"node-docker-action\",\"version\":\"1.0.0\",\"scripts\":{}}\n",
            ),
            ("make", "Makefile", "all:\n\t@true\n"),
        ];
        let mut steps = String::new();
        for (name, _, _) in &manifest_fixtures {
            steps.push_str("    - uses: ./actions/");
            steps.push_str(name);
            steps.push('\n');
        }
        must(
            fs::write(
                root.join("action.yml"),
                format!("runs:\n  using: composite\n  steps:\n{steps}"),
            ),
            "write parent action metadata",
        );

        for (name, manifest, contents) in manifest_fixtures {
            let action_root = root.join("actions").join(name);
            must(
                fs::create_dir_all(&action_root),
                "create manifest-backed local action",
            );
            must(
                fs::write(action_root.join(manifest), contents),
                "write local action project manifest",
            );
            must(
                fs::write(action_root.join("Dockerfile"), "FROM scratch\n"),
                "write local action Dockerfile",
            );
        }

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan manifest-backed Dockerfile actions",
        );
        let parent_action = shape
            .units
            .iter()
            .find(|unit| unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("parent composite action missing"));
        for (name, _, _) in manifest_fixtures {
            let dockerfile = format!("actions/{name}/Dockerfile");
            let local_action = shape
                .units
                .iter()
                .find(|unit| {
                    unit.root == format!("actions/{name}")
                        && unit.kind == crate::s2::UnitKind::GithubAction
                })
                .unwrap_or_else(|| panic!("manifest-backed Dockerfile action missing: {name}"));
            assert!(local_action.capabilities.docker);
            assert!(local_action.pr_commands.iter().any(|command| command
                == &format!("velnor-workflow verify-action --path '{dockerfile}'")));
            assert!(parent_action
                .pr_commands
                .iter()
                .any(|command| command == &format!("test -f '{dockerfile}'")));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_invalid_metadata_still_fails_with_dockerfile_and_project_manifest() {
        let root = fixture("nested-invalid-metadata");
        pin_fixture_rust_toolchain(&root);
        let nested = root.join("nested");
        must(fs::create_dir_all(&nested), "create nested action");
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ./nested\n",
            ),
            "write parent action metadata",
        );
        must(
            fs::write(
                nested.join("action.yml"),
                "runs:\n  using: unsupported\n  main: missing\n",
            ),
            "write invalid nested action metadata",
        );
        must(
            fs::write(
                nested.join("Cargo.toml"),
                "[package]\nname = \"nested_action\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            "write nested project manifest",
        );
        must(
            fs::write(nested.join("Dockerfile"), "FROM scratch\n"),
            "write nested Dockerfile",
        );

        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("invalid nested action metadata must fail the scan"));
        assert!(
            error
                .to_string()
                .contains("unsupported GitHub Action runtime `unsupported`"),
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
                "runs:\n  using: docker\n  image: docker://ubuntu\n  entrypoint: /container-entrypoint.sh\n  pre-entrypoint: /container-pre.sh\n  post-entrypoint: /container-post.sh\n",
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
    fn mixed_case_docker_uri_is_not_treated_as_a_local_dockerfile() {
        let root = fixture("mixed-case-docker-uri");
        must(
            fs::create_dir_all(root.join("actions/remote")),
            "create remote Docker action directory",
        );
        must(
            fs::write(
                root.join("actions/remote/action.yml"),
                "runs:\n  using: docker\n  image: DoCkEr://ghcr.io/example/Dockerfile\n",
            ),
            "write mixed-case Docker URI metadata",
        );
        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan mixed-case Docker URI",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == "actions/remote")
            .unwrap_or_else(|| panic!("remote Docker action missing"));
        assert_eq!(action.pr_commands.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_composite_action_tags_follow_velnor_pin_policy() {
        for (name, reference) in [
            ("git-tag", "actions/checkout@v4"),
            ("leading-space-local-path", " ./actions/child"),
        ] {
            let root = fixture(name);
            must(
                fs::write(
                    root.join("action.yml"),
                    format!("runs:\n  using: composite\n  steps:\n    - uses: '{reference}'\n"),
                ),
                "write external action pin fixture",
            );
            let error = super::super::scan_shape(&root, &providers(), "main", &[])
                .err()
                .unwrap_or_else(|| panic!("mutable external action must fail policy: {reference}"));
            assert!(
                error.to_string().contains("full 40-character SHA pin"),
                "{reference}: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn composite_run_paths_use_runner_working_directories() {
        let root = fixture("composite-run-working-directories");
        let action_root = root.join(".github/actions/foo");
        for directory in [
            root.join("scripts"),
            root.join("packages/app/scripts"),
            action_root.join("scripts"),
            action_root.join("subdir"),
        ] {
            must(
                fs::create_dir_all(directory),
                "create composite script directory",
            );
        }
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
        );
        must(
            fs::write(
                action_root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: ./scripts/workspace.sh\n    - shell: bash\n      working-directory: '${{ github.workspace }}'\n      run: ./scripts/workspace-expression.sh\n    - shell: bash\n      working-directory: packages/app\n      run: ./scripts/package.sh\n    - shell: bash\n      run: ${{ github.action_path }}/scripts/action-path.sh\n    - shell: bash\n      working-directory: '${{ github.action_path }}/subdir'\n      run: ./working-directory.sh\n",
            ),
            "write composite run path metadata",
        );
        for file in [
            root.join("scripts/workspace.sh"),
            root.join("scripts/workspace-expression.sh"),
            root.join("packages/app/scripts/package.sh"),
            action_root.join("scripts/action-path.sh"),
            action_root.join("subdir/working-directory.sh"),
        ] {
            must(fs::write(file, "exit 0\n"), "write composite run script");
        }

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan composite run paths",
        );
        let action = shape
            .units
            .iter()
            .find(|unit| unit.root == ".github/actions/foo")
            .unwrap_or_else(|| panic!("workflow-selected composite action missing"));
        for path in [
            "scripts/workspace.sh",
            "scripts/workspace-expression.sh",
            "packages/app/scripts/package.sh",
            ".github/actions/foo/scripts/action-path.sh",
            ".github/actions/foo/subdir/working-directory.sh",
        ] {
            assert!(
                action
                    .pr_commands
                    .iter()
                    .any(|command| command == &format!("test -f '{path}'")),
                "composite script path was resolved against the wrong directory: {path}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn scan_excludes_apply_to_workflow_context_and_selected_action_inputs() {
        let workflow_excluded = fixture("workflow-reference-excluded");
        must(
            fs::create_dir_all(workflow_excluded.join(".github/actions/docker")),
            "create workflow-selected Docker action",
        );
        write_workflow(
            &workflow_excluded,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/docker\n",
        );
        must(
            fs::write(
                workflow_excluded.join(".github/actions/docker/Dockerfile"),
                "FROM scratch\n",
            ),
            "write Dockerfile-only selected action",
        );
        let excluded_workflow = vec![".github/workflows/ci.yml".to_owned()];
        let shape = must(
            super::super::scan_shape(&workflow_excluded, &providers(), "main", &excluded_workflow),
            "scan while excluding workflow reference context",
        );
        assert!(!shape
            .units
            .iter()
            .any(|unit| unit.kind == crate::s2::UnitKind::GithubAction));
        assert!(!shape
            .files()
            .iter()
            .any(|file| file.starts_with(".github/")));
        let _ = fs::remove_dir_all(workflow_excluded);

        let action_excluded = fixture("selected-action-input-excluded");
        must(
            fs::create_dir_all(action_excluded.join(".github/actions/foo")),
            "create selected action directory",
        );
        write_workflow(
            &action_excluded,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
        );
        must(
            fs::write(
                action_excluded.join(".github/actions/foo/action.yml"),
                "runs:\n  using: docker\n  image: docker://ubuntu\n",
            ),
            "write selected action metadata",
        );
        must(
            fs::write(
                action_excluded.join(".github/actions/foo/notes.txt"),
                "excluded supplemental input\n",
            ),
            "write excluded supplemental action input",
        );
        let excluded_note = vec![".github/actions/foo/notes.txt".to_owned()];
        let shape = must(
            super::super::scan_shape(&action_excluded, &providers(), "main", &excluded_note),
            "scan selected action with an excluded supplemental input",
        );
        assert!(shape
            .files()
            .iter()
            .any(|file| file == ".github/actions/foo/action.yml"));
        assert!(!shape
            .files()
            .iter()
            .any(|file| file == ".github/actions/foo/notes.txt"));
        let excluded_metadata = vec![".github/actions/foo/action.yml".to_owned()];
        let error =
            super::super::scan_shape(&action_excluded, &providers(), "main", &excluded_metadata)
                .err()
                .unwrap_or_else(|| {
                    panic!("excluding a selected canonical action entrypoint must fail")
                });
        assert!(
            error.to_string().contains("is excluded from the scan"),
            "{error}"
        );
        let _ = fs::remove_dir_all(action_excluded);
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
        assert!(error.to_string().contains("shell"), "{error}");
        let _ = fs::remove_dir_all(missing_shell);

        let dynamic = fixture("dynamic-action-path");
        must(
            fs::write(
                dynamic.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: |\n        echo setup\n        node ${{ inputs.script }}/entrypoint.sh\n",
            ),
            "write multiline dynamic action path metadata",
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
            "name: consumer\ninputs:\n  enabled:\n    default: true\noutputs:\n  result:\n    value: ${{ steps.local.outputs.result }}\nruns:\n  using: composite\n  steps:\n    - id: local\n      if: ${{ inputs.enabled }}\n      uses: ./nested\n      with:\n        value: ${{ inputs.enabled }}\n      env:\n        ACTION_MODE: checked\n    - name: external\n      if: always()\n      uses: actions/checkout@692973e3d937129bcbf40652eb9f2f61becf3332\n",
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

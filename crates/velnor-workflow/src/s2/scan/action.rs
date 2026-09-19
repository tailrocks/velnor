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

use super::file_walk::is_test_support_path;
use super::{unit, RepositoryShape, ScanContext};
use crate::s2::{is_full_revision, parent_path, shell_quote, UnitKind};

const GITHUB_ACTION_PATH_MARKER: &str = "__VELNOR_GITHUB_ACTION_PATH__";
const GITHUB_WORKSPACE_MARKER: &str = "__VELNOR_GITHUB_WORKSPACE__";

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
}

struct InspectedAction {
    source: ActionSource,
    metadata: Option<ActionMetadata>,
    references: Vec<String>,
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
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default, rename = "pre-entrypoint", alias = "preEntrypoint")]
    pre_entrypoint: Option<String>,
    #[serde(default, rename = "post-entrypoint", alias = "postEntrypoint")]
    post_entrypoint: Option<String>,
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
            || action_details
                .metadata
                .is_some_and(|metadata| metadata.runs.using.eq_ignore_ascii_case("docker"))
        {
            action.capabilities.docker = true;
        }
        shape.units.push(action);
    }
    Ok(analysis.supplemental_files)
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

        let (metadata, action_references) = match source.kind {
            ActionSourceKind::Dockerfile => (None, ActionReferences::default()),
            ActionSourceKind::Metadata => {
                let metadata = parse_metadata(root, &source.path)?;
                let references = local_references(&metadata.runs, &source.root, &action_files)?;
                (Some(metadata), references)
            }
        };
        let mut watched = action_references.files;
        for referenced_root in action_references.actions {
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
            },
        );
    }

    Ok(ActionAnalysis {
        actions: inspected.into_values().collect(),
        supplemental_files: supplemental_files.into_iter().collect(),
    })
}

fn metadata_action_sources(
    files: &BTreeSet<String>,
) -> Result<MetadataActionSources, crate::s2::GeneratorError> {
    let mut preferred_metadata = BTreeMap::new();
    let mut alternate_metadata = BTreeMap::new();
    for file in files.iter().filter(|file| !is_test_support_path(file)) {
        let root = parent_path(file);
        match file.rsplit('/').next() {
            Some("action.yml") => {
                preferred_metadata.insert(root, file.clone());
            }
            Some("action.yaml") => {
                alternate_metadata.insert(root, file.clone());
            }
            _ => {}
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
    if files.contains(&super::file_walk::join_repo_path(action_root, "action.yml")) {
        return Some(ActionSource {
            root: action_root.to_owned(),
            path: super::file_walk::join_repo_path(action_root, "action.yml"),
            kind: ActionSourceKind::Metadata,
        });
    }
    if files.contains(&super::file_walk::join_repo_path(
        action_root,
        "action.yaml",
    )) {
        return Some(ActionSource {
            root: action_root.to_owned(),
            path: super::file_walk::join_repo_path(action_root, "action.yaml"),
            kind: ActionSourceKind::Metadata,
        });
    }
    if !allow_dockerfile {
        return None;
    }
    for name in ["Dockerfile", "dockerfile"] {
        let path = super::file_walk::join_repo_path(action_root, name);
        if files.contains(&path) {
            return Some(ActionSource {
                root: action_root.to_owned(),
                path,
                kind: ActionSourceKind::Dockerfile,
            });
        }
    }
    None
}

fn ensure_selected_action_files(
    root: &Path,
    action_root: &str,
    files: &mut BTreeSet<String>,
    supplemental_files: &mut BTreeSet<String>,
    exclude: &[String],
) -> Result<(), crate::s2::GeneratorError> {
    let excludes = super::file_walk::exclude_set(exclude)?;
    for name in ["action.yml", "action.yaml", "Dockerfile", "dockerfile"] {
        let relative_path = super::file_walk::join_repo_path(action_root, name);
        let path = root.join(&relative_path);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(crate::s2::GeneratorError::usage(format!(
                    "local GitHub Action `{action_root}` has a symlink entrypoint `{name}`"
                )));
            }
            Ok(metadata) if metadata.is_file() => {
                if excludes.is_match(&relative_path) {
                    return Err(crate::s2::GeneratorError::usage(format!(
                        "canonical GitHub Action entrypoint `{relative_path}` is excluded from the scan"
                    )));
                }
                break;
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
    let metadata: ActionMetadata = serde_yaml::from_str(&contents).map_err(|error| {
        crate::s2::GeneratorError::usage(format!(
            "parse GitHub Action metadata {}: {error}",
            metadata_file.display()
        ))
    })?;
    validate_mapping(&metadata.inputs, "inputs")?;
    validate_mapping(&metadata.outputs, "outputs")?;
    Ok(metadata)
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

fn local_references(
    runs: &ActionRuns,
    action_root: &str,
    files: &BTreeSet<String>,
) -> Result<ActionReferences, crate::s2::GeneratorError> {
    let using = runs.using.trim().to_ascii_lowercase();
    let mut references = ActionReferences::default();
    match using.as_str() {
        "composite" => return composite_action_references(runs, action_root, files),
        "node12" | "node16" | "node20" | "node24" => {
            let main = runs.main.as_deref().ok_or_else(|| {
                crate::s2::GeneratorError::usage(format!(
                    "GitHub JavaScript action ({using}) metadata must declare runs.main"
                ))
            })?;
            add_local_reference(&mut references.files, main, action_root, files)?;
            for reference in [&runs.pre, &runs.post] {
                if let Some(reference) = reference.as_deref() {
                    add_local_reference(&mut references.files, reference, action_root, files)?;
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
                add_local_reference(&mut references.files, image, action_root, files)?;
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
    files: &BTreeSet<String>,
) -> Result<ActionReferences, crate::s2::GeneratorError> {
    let mut references = ActionReferences::default();
    if runs.steps.is_empty() {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub composite action metadata must declare at least one runs.steps entry",
        ));
    }
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
            if step.shell.as_deref().is_none_or(str::is_empty) {
                return Err(crate::s2::GeneratorError::usage(
                    "GitHub composite action run step must declare shell",
                ));
            }
            let run = resolve_action_path_expression(run);
            let shell_references = shell_references(&run);
            if !shell_references.is_empty() {
                let working_directory =
                    composite_working_directory(step.working_directory.as_deref(), action_root)?;
                for reference in shell_references {
                    add_composite_run_reference(
                        &mut references.files,
                        &reference,
                        action_root,
                        &working_directory,
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
            if looks_like_local_action_reference(uses) {
                references
                    .actions
                    .insert(normalize_local_action_directory(uses)?);
            } else if !is_full_sha_action_reference(uses) {
                // Runner accepts tags here; Velnor applies the repository-wide
                // full-SHA action pin policy.
                return Err(crate::s2::GeneratorError::usage(format!(
                    "GitHub composite external action `{uses}` must use a full 40-character SHA pin"
                )));
            }
        }
    }
    Ok(references)
}

fn looks_like_local_action_reference(value: &str) -> bool {
    value.starts_with('.')
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.starts_with("$/")
        || value.starts_with('$')
        || value.as_bytes().get(1) == Some(&b':')
            && value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
}

fn normalize_local_action_directory(reference: &str) -> Result<String, crate::s2::GeneratorError> {
    let reference = reference.trim();
    let normalized_reference = reference.to_owned();
    let (path, allows_workspace_root) = if let Some(path) = normalized_reference.strip_prefix("./")
    {
        (path, true)
    } else if let Some(path) = reference.strip_prefix("$/") {
        if path.contains('@') {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub self-repository action `{reference}` must be a nonempty workspace-relative path without an `@ref` suffix"
            )));
        }
        (path, false)
    } else {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite local action `{reference}` must be a workspace-relative path"
        )));
    };
    if path.is_empty() {
        if allows_workspace_root {
            return Ok(".".to_owned());
        }
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite local action `{reference}` must name a workspace-relative path"
        )));
    }
    if path.contains('\\')
        || path.contains('$')
        || normalized_reference.contains("{{")
        || normalized_reference.contains("}}")
        || path.starts_with('/')
        || path.split('/').next().is_some_and(is_windows_drive_prefix)
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
    value.split_once('@').is_some_and(|(action, revision)| {
        !action.is_empty() && !action.contains(char::is_whitespace) && is_full_revision(revision)
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
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("docker://"))
    {
        return false;
    }
    let basename = value.rsplit('/').next().unwrap_or(value);
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
    files: &BTreeSet<String>,
) -> Result<(), crate::s2::GeneratorError> {
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
    add_local_reference(references, &path, base, files)
}

fn expression_relative_path(
    suffix: &str,
    reference: &str,
) -> Result<String, crate::s2::GeneratorError> {
    let Some(suffix) = suffix.strip_prefix('/') else {
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
    let Some(suffix) = suffix.strip_prefix('/') else {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub Action `working-directory` must be a static workspace-relative path",
        ));
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
    if let Some(continue_on_error) = &step.continue_on_error
        && !continue_on_error.is_bool()
        && !continue_on_error.is_string()
    {
        return Err(crate::s2::GeneratorError::usage(
            "GitHub composite action step `continue-on-error` must be a boolean or expression string",
        ));
    }
    for (field, value) in [
        ("id", step.id.as_deref()),
        ("name", step.name.as_deref()),
        ("if", step.condition.as_deref()),
        ("working-directory", step.working_directory.as_deref()),
    ] {
        if value.is_some_and(str::is_empty) {
            return Err(crate::s2::GeneratorError::usage(format!(
                "GitHub composite action step `{field}` must not be empty"
            )));
        }
    }
    Ok(())
}

fn add_local_reference(
    references: &mut BTreeSet<String>,
    reference: &str,
    action_root: &str,
    files: &BTreeSet<String>,
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
    if !is_safe_relative_reference(reference) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` escapes its action directory"
        )));
    }
    let normalized = reference
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>()
        .join("/");
    let repository_path = super::file_walk::join_repo_path(action_root, &normalized);
    if !files.contains(&repository_path) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action entrypoint `{reference}` resolves to missing file `{repository_path}`"
        )));
    }
    references.insert(repository_path);
    Ok(())
}

fn is_safe_relative_reference(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.starts_with('/')
        && !reference.starts_with('\\')
        && !reference.contains('\\')
        && !reference.chars().any(char::is_control)
        && !reference.contains('$')
        && !reference.contains("{{")
        && !reference.contains("}}")
        && !reference.split('/').any(|component| component == "..")
        && !reference
            .split('/')
            .next()
            .is_some_and(is_windows_drive_prefix)
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
        let looks_like_path = token.starts_with("./")
            || token.starts_with("../")
            || token.contains('/') && has_script_suffix(token)
            || has_script_suffix(token)
            || command_position && token.contains('/') && !token.starts_with('-');
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
    use std::path::{Path, PathBuf};
    use std::process::Command;

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
            fs::write(root.join("actions/bare/dockerfile"), "FROM scratch\n"),
            "write lowercase Dockerfile fallback",
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
                "runs:\n  using: composite\n  steps:\n    - uses: ./actions/child\n    - uses: $/actions/child\n",
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
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n      - uses: $/.github/actions/foo\n",
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
        assert!(!shape
            .files()
            .iter()
            .any(|file| file.starts_with(".github/actions/foo/node_modules/")));
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
                "runs:\n  using: composite\n  steps:\n    - uses: ./actions/child\n",
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
        ] {
            assert!(
                shape.files().iter().any(|file| file == included),
                "missing {included}"
            );
        }
        for ignored in [
            ".github/actions/foo/node_modules/pkg/index.js",
            ".github/actions/foo/build/output/cache",
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
    fn workflow_selecting_repository_root_uses_runner_dockerfile_case_precedence() {
        let root = fixture("root-dockerfile-precedence");
        must(
            fs::write(root.join("Dockerfile"), "FROM scratch\n"),
            "write preferred Dockerfile",
        );
        must(
            fs::write(root.join("dockerfile"), "FROM busybox\n"),
            "write lowercase Dockerfile",
        );
        write_workflow(
            &root,
            ".github/workflows/ci.yml",
            "name: ci\njobs:\n  build:\n    steps:\n      - uses: ./\n",
        );

        let shape = must(
            super::super::scan_shape(&root, &providers(), "main", &[]),
            "scan workflow-selected repository root",
        );
        let root_action = shape
            .units
            .iter()
            .find(|unit| unit.root == "." && unit.kind == crate::s2::UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("workflow-selected root action missing"));
        assert!(root_action.capabilities.docker);
        assert!(root_action
            .pr_commands
            .iter()
            .any(|command| command == "velnor-workflow verify-action --path 'Dockerfile'"));
        assert!(!root_action
            .pr_commands
            .iter()
            .any(|command| command.contains("dockerfile'")));
        assert!(super::verify_action(&root, "Dockerfile").is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_and_missing_local_action_paths_fail_closed() {
        for (name, reference) in [
            ("traversal", "./../outside"),
            ("absolute", "/outside"),
            ("double-slash", ".//outside"),
            ("dollar-double-slash", "$//outside"),
            ("empty-self-repository", "$/"),
            ("self-repository-ref", "$/outside@v1"),
            ("windows-relative", ".\\actions\\child"),
            ("windows-traversal", ".\\..\\outside"),
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
                    || error.to_string().contains("static workspace-relative"),
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
        let error = super::super::scan_shape(&workflow, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("absolute workflow action path must fail scan"));
        assert!(error.to_string().contains("workspace-relative"), "{error}");
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
    fn symlinked_local_action_paths_are_rejected() {
        use std::os::unix::fs::symlink;

        let root = fixture("symlinked-local-action");
        must(
            fs::create_dir_all(root.join("actions/real")),
            "create real local action",
        );
        must(
            fs::write(root.join("actions/real/Dockerfile"), "FROM scratch\n"),
            "write real local action Dockerfile",
        );
        must(
            fs::create_dir_all(root.join("actions")),
            "create local actions directory",
        );
        must(
            symlink("real", root.join("actions/linked")),
            "create symlinked local action",
        );
        must(
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - uses: ./actions/linked\n",
            ),
            "write parent local action metadata",
        );

        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("symlinked local action target must fail scan"));
        assert!(error.to_string().contains("symlink"), "{error}");
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
        let error = super::super::scan_shape(&root, &providers(), "main", &[])
            .err()
            .unwrap_or_else(|| panic!("symlinked preferred metadata must fail"));
        assert!(error
            .to_string()
            .contains("symlink entrypoint `action.yml`"));
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
            ("container-tag", "docker://alpine:3.8"),
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
                "runs:\n  using: docker\n  image: ubuntu\n",
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
        assert!(error.to_string().contains("must declare shell"), "{error}");
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

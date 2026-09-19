//! Generic GitHub Action metadata detector.
//!
//! An action is a repository product, not a project-language unit.  The scan
//! therefore proves only the metadata runtime and local files the metadata
//! names; it never executes an action, shell, JavaScript, or Docker entrypoint.
//! Repository-owned consumer fixtures are attached later through the generic
//! `github-action-fixtures` unit contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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

/// Detect every tracked action metadata file outside test support trees.
pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), crate::s2::GeneratorError> {
    for source in discover_action_sources(context.files) {
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
    let Some(canonical) = discover_action_sources(&files)
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
fn discover_action_sources(files: &[String]) -> Vec<ActionSource> {
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
                return Some(ActionSource {
                    root,
                    path: path.clone(),
                    kind: ActionSourceKind::Metadata,
                });
            }
            dockerfiles.get(&root).map(|path| ActionSource {
                root,
                path: path.clone(),
                kind: ActionSourceKind::Dockerfile,
            })
        })
        .collect()
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
            let references = local_references(&metadata.runs, &source.root, files)?;
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
    files: &[String],
) -> Result<Vec<String>, crate::s2::GeneratorError> {
    let using = runs.using.trim().to_ascii_lowercase();
    let mut references = BTreeSet::new();
    match using.as_str() {
        "composite" => {
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
                    for reference in shell_references(&run) {
                        add_local_reference(&mut references, &reference, action_root, files)?;
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
                    if uses.starts_with('.') {
                        add_local_action_reference(&mut references, uses, action_root, files)?;
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
            add_local_reference(&mut references, main, action_root, files)?;
            for reference in [&runs.pre, &runs.post] {
                if let Some(reference) = reference.as_deref() {
                    add_local_reference(&mut references, reference, action_root, files)?;
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
                add_local_reference(&mut references, image, action_root, files)?;
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
    if value.starts_with("docker://") {
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
            resolved.push('.');
        } else {
            resolved.push_str(&value[start..end]);
        }
        cursor = end;
    }
    resolved.push_str(&value[cursor..]);
    resolved
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
    let path = Path::new(reference);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action local entrypoint `{reference}` escapes its action directory"
        )));
    }
    let normalized = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    let repository_path = super::file_walk::join_repo_path(action_root, &normalized);
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
    action_root: &str,
    files: &[String],
) -> Result<(), crate::s2::GeneratorError> {
    let reference = reference.trim();
    if reference.contains('$') || reference.contains("${{") {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite local action `{reference}` must be a static relative path"
        )));
    }
    let path = Path::new(reference.trim_start_matches("./"));
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub composite local action `{reference}` escapes its action directory"
        )));
    }
    let normalized = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    let directory = super::file_walk::join_repo_path(action_root, &normalized);
    if let Some(source) = discover_action_sources(files)
        .into_iter()
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

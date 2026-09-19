//! Generic GitHub Action metadata detector.
//!
//! An action is a repository product, not a project-language unit.  The scan
//! therefore proves only the metadata runtime and local files the metadata
//! names; it never executes an action, shell, JavaScript, or Docker entrypoint.
//! Repository-owned consumer fixtures are attached later through the generic
//! `github-action-fixtures` unit contract.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use super::file_walk::is_test_support_path;
use super::{unit, RepositoryShape, ScanContext};
use crate::s2::{parent_path, shell_quote, UnitKind};

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
    let mut seen_roots = BTreeSet::new();
    for metadata_path in context.files.iter().filter(|file| {
        matches!(file.rsplit('/').next(), Some("action.yml" | "action.yaml"))
            && !is_test_support_path(file)
    }) {
        let action_root = parent_path(metadata_path);
        if !seen_roots.insert(action_root.clone()) {
            return Err(crate::s2::GeneratorError::usage(format!(
                "action directory `{action_root}` contains both action.yml and action.yaml; keep one metadata entrypoint"
            )));
        }
        let metadata = parse_metadata(context.root, metadata_path)?;
        let references = local_references(&metadata.runs, &action_root, context.files)?;
        let mut commands = vec![format!(
            "velnor-workflow verify-action --path {}",
            shell_quote(metadata_path)
        )];
        commands.extend(
            references
                .iter()
                .map(|path| format!("test -f {}", shell_quote(path))),
        );
        let mut action = unit(
            UnitKind::GithubAction,
            &action_root,
            vec![if action_root == "." {
                "**".to_owned()
            } else {
                format!("{action_root}/**")
            }],
            commands,
            None,
        );
        if metadata.runs.using.eq_ignore_ascii_case("docker") {
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
    metadata_path: &str,
) -> Result<(), crate::s2::GeneratorError> {
    let metadata = Path::new(metadata_path);
    if metadata.is_absolute()
        || metadata
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata path `{metadata_path}` must be repository-relative"
        )));
    }
    let files = super::file_walk::repository_files(root, &[])?;
    if !files.iter().any(|file| file == metadata_path) {
        return Err(crate::s2::GeneratorError::usage(format!(
            "GitHub Action metadata `{metadata_path}` is not a tracked repository file"
        )));
    }
    let action_root = parent_path(metadata_path);
    let metadata = parse_metadata(root, metadata_path)?;
    local_references(&metadata.runs, &action_root, &files).map(|_| ())
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
                    for reference in shell_references(run) {
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
                if let Some(uses) = &step.uses
                    && uses.trim_start().starts_with("./")
                {
                    add_local_action_reference(&mut references, uses, action_root, files)?;
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
            if !image.contains("://") && !image.contains(':') {
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
    if reference.is_empty() || reference.contains("${{") || reference.contains('$') {
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
    for metadata in ["action.yml", "action.yaml"] {
        let candidate = super::file_walk::join_repo_path(&directory, metadata);
        if files.iter().any(|file| file == &candidate) {
            references.insert(candidate);
            return Ok(());
        }
    }
    Err(crate::s2::GeneratorError::usage(format!(
        "GitHub composite local action `{reference}` has no action.yml or action.yaml under `{directory}`"
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
    fn composite_semantics_are_parsed_without_rewriting_uses_conditions_or_io() {
        let root = fixture("composite-semantics");
        must(
            fs::create_dir_all(root.join("nested")),
            "create nested action",
        );
        must(
            fs::write(
                root.join("action.yml"),
                "name: consumer\ninputs:\n  enabled:\n    default: true\noutputs:\n  result:\n    value: ${{ steps.local.outputs.result }}\nruns:\n  using: composite\n  steps:\n    - id: local\n      if: ${{ inputs.enabled }}\n      uses: ./nested\n      with:\n        value: ${{ inputs.enabled }}\n      env:\n        ACTION_MODE: checked\n    - name: external\n      if: always()\n      uses: actions/checkout@v4\n",
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

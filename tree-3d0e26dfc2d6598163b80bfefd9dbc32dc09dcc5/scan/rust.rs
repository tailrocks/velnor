//! Rust detector: Cargo manifests, workspace graph, and Rust source facts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use super::file_walk::{
    files_named, has_extension, join_repo_path, path_prefix, resolve_repo_path,
};
use super::{RepositoryShape, ScanContext};
use crate::{
    identifier_suffix, parent_path, shell_change_dir, shell_quote, CacheSpec, GeneratorError, Unit,
    UnitKind,
};

fn cargo_deny_has_license_policy(root: &Path, file_set: &BTreeSet<String>) -> bool {
    ["deny.toml", ".cargo/deny.toml"].into_iter().any(|path| {
        file_set.contains(path)
            && fs::read_to_string(root.join(path))
                .is_ok_and(|contents| contents.lines().any(|line| line.trim() == "[licenses]"))
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CargoDependency {
    pub(crate) name: String,
    pub(crate) package_name: Option<String>,
    pub(crate) path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CargoManifestFacts {
    pub(crate) root: String,
    pub(crate) has_package: bool,
    pub(crate) package_name: Option<String>,
    pub(crate) has_workspace: bool,
    pub(crate) workspace_members: Vec<String>,
    pub(crate) dependencies: Vec<CargoDependency>,
    pub(crate) build_script: Option<String>,
    pub(crate) binary_targets: Vec<String>,
    pub(crate) test_targets: Vec<String>,
    pub(crate) features: Vec<String>,
}

#[derive(Default)]
struct RustAnalysis {
    units: Vec<Unit>,
    detected: Vec<String>,
    limitations: Vec<String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "Rust evidence normalization keeps graph construction together"
)]
fn analyze_rust_manifests(
    root: &Path,
    files: &[String],
    file_set: &BTreeSet<String>,
    manifests: &[String],
) -> Result<RustAnalysis, GeneratorError> {
    let mut facts = Vec::new();
    for manifest in manifests {
        let manifest_root = parent_path(manifest);
        let path = root.join(manifest);
        let contents = fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read Cargo manifest", &path, &error))?;
        let parsed = parse_cargo_manifest(&manifest_root, &contents);
        for (field, value) in parsed
            .package_name
            .iter()
            .map(|value| ("package name", value))
            .chain(
                parsed
                    .build_script
                    .iter()
                    .map(|value| ("build script", value)),
            )
            .chain(
                parsed
                    .binary_targets
                    .iter()
                    .map(|value| ("binary target", value)),
            )
            .chain(
                parsed
                    .test_targets
                    .iter()
                    .map(|value| ("test target", value)),
            )
        {
            if value.chars().any(char::is_control) {
                return Err(GeneratorError::usage(format!(
                    "Cargo {field} contains control characters at {}",
                    path.display()
                )));
            }
        }
        facts.push(parsed);
    }

    let workspace_root = facts
        .iter()
        .find(|manifest| manifest.has_workspace)
        .map(|manifest| manifest.root.clone());
    let has_nextest =
        file_set.contains(".config/nextest.toml") || file_set.contains("nextest.toml");
    let has_cargo_deny = file_set.contains("deny.toml") || file_set.contains(".cargo/deny.toml");
    let has_cargo_audit = file_set.contains("audit.toml") || file_set.contains(".cargo/audit.toml");
    let mut result = RustAnalysis::default();

    if workspace_root.is_some() {
        let member_count = facts.iter().filter(|manifest| manifest.has_package).count();
        result
            .detected
            .push(format!("rust-workspace-members:{member_count}"));
    } else if facts.iter().any(|manifest| manifest.has_package) {
        result.detected.push("rust-package".to_owned());
    }
    if has_nextest {
        result.detected.push("cargo-nextest-policy".to_owned());
    }
    if has_cargo_deny {
        result.detected.push("cargo-deny-policy".to_owned());
    }
    if has_cargo_audit {
        result.detected.push("cargo-audit-policy".to_owned());
    }
    if files
        .iter()
        .any(|file| matches!(file.rsplit('/').next(), Some("mise.toml" | "mise.lock")))
        && facts.iter().any(|manifest| manifest.has_package)
    {
        result.detected.push("mise-rust-toolchain".to_owned());
    }

    let package_facts = facts
        .iter()
        .filter(|manifest| manifest.has_package)
        .collect::<Vec<_>>();
    let mut package_ids = BTreeMap::new();
    let mut roots_by_manifest = BTreeMap::new();
    let mut rust_id_counts = BTreeMap::new();
    for manifest in &package_facts {
        let suffix = manifest
            .package_name
            .as_deref()
            .map(identifier_suffix)
            .filter(|suffix| !suffix.is_empty())
            .unwrap_or_else(|| identifier_suffix(&manifest.root));
        let base_id = format!("rust-{suffix}");
        let count = rust_id_counts.entry(base_id.clone()).or_insert(0_usize);
        *count += 1;
        let id = if *count == 1 {
            base_id
        } else {
            format!("{base_id}-{count}")
        };
        roots_by_manifest.insert(join_repo_path(&manifest.root, "Cargo.toml"), id.clone());
        if let Some(package_name) = &manifest.package_name {
            package_ids.insert(package_name.clone(), id);
        }
    }

    let workspace_manifest = workspace_root
        .as_deref()
        .map(|workspace| join_repo_path(workspace, "Cargo.toml"));
    for manifest in &package_facts {
        let manifest_path = join_repo_path(&manifest.root, "Cargo.toml");
        let prefix = path_prefix(&manifest.root);
        let command_prefix = shell_change_dir(&manifest.root);
        let default_build_script = join_repo_path(&manifest.root, "build.rs");
        let build_script = manifest.build_script.as_deref().or_else(|| {
            file_set
                .contains(&default_build_script)
                .then_some("build.rs")
        });
        let has_test_sources = file_set.iter().any(|file| {
            file.starts_with(&format!("{prefix}tests/"))
                || file.starts_with(&format!("{prefix}benches/"))
        });
        let package_selector = manifest.package_name.as_deref().map_or_else(
            || format!("--manifest-path {}", shell_quote("Cargo.toml")),
            |name| format!("--package {}", shell_quote(name)),
        );
        let cargo_lock_flag = if file_set.contains("Cargo.lock") {
            "--locked"
        } else {
            ""
        };
        let test_command = if has_nextest {
            format!(
                "{command_prefix}cargo nextest run {cargo_lock_flag} --all-features {package_selector}"
            )
        } else {
            format!(
                "{command_prefix}cargo test {cargo_lock_flag} --all-features {package_selector}"
            )
        };
        let commands = vec![
            format!(
                "{command_prefix}cargo fmt --manifest-path {} -- --check",
                shell_quote("Cargo.toml")
            ),
            format!(
                "{command_prefix}cargo clippy {cargo_lock_flag} --no-deps --all-targets --all-features {package_selector} -- -D warnings"
            ),
            test_command,
        ];
        let mut watch = vec![
            manifest_path.clone(),
            "Cargo.lock".to_owned(),
            "rust-toolchain.toml".to_owned(),
            "rust-toolchain".to_owned(),
            "mise.toml".to_owned(),
            "mise.lock".to_owned(),
            ".cargo/**".to_owned(),
            format!("{prefix}**/*.rs"),
            format!("{prefix}src/**"),
            format!("{prefix}tests/**"),
            format!("{prefix}examples/**"),
            format!("{prefix}benches/**"),
        ];
        if let Some(workspace_manifest) = &workspace_manifest
            && workspace_manifest != &manifest_path
        {
            watch.push(workspace_manifest.clone());
        }
        if let Some(build_script) = build_script {
            if let Some(build_path) = resolve_repo_path(&manifest.root, build_script) {
                watch.push(build_path);
            }
            result.detected.push(format!(
                "rust-build-script:{}",
                manifest.package_name.as_deref().unwrap_or(&manifest.root)
            ));
        }
        if !manifest.binary_targets.is_empty() {
            result.detected.push(format!(
                "rust-binaries:{}:{}",
                manifest.package_name.as_deref().unwrap_or(&manifest.root),
                manifest.binary_targets.len()
            ));
        }
        if !manifest.test_targets.is_empty() || has_test_sources {
            result.detected.push(format!(
                "rust-test-targets:{}:{}",
                manifest.package_name.as_deref().unwrap_or(&manifest.root),
                manifest
                    .test_targets
                    .len()
                    .max(usize::from(has_test_sources))
            ));
        }
        if !manifest.features.is_empty() {
            result.detected.push(format!(
                "rust-features:{}:{}",
                manifest.package_name.as_deref().unwrap_or(&manifest.root),
                manifest.features.len()
            ));
        }
        watch.extend(include_str_paths(root, files, file_set, &manifest.root)?);
        watch.sort();
        watch.dedup();
        let cache_key_files = vec![
            ".cargo/**".to_owned(),
            "Cargo.toml".to_owned(),
            manifest_path.clone(),
            "Cargo.lock".to_owned(),
            "rust-toolchain.toml".to_owned(),
            "rust-toolchain".to_owned(),
        ];
        result.units.push(Unit {
            id: roots_by_manifest
                .get(&manifest_path)
                .cloned()
                .unwrap_or_else(|| format!("rust-{}", identifier_suffix(&manifest.root))),
            label: format!(
                "Rust crate ({})",
                manifest.package_name.as_deref().unwrap_or(&manifest.root)
            ),
            kind: UnitKind::Rust,
            root: manifest.root.clone(),
            watch,
            pr_commands: commands.clone(),
            full_commands: commands,
            github_pr_commands: None,
            github_full_commands: None,
            velnor_pr_commands: None,
            velnor_full_commands: None,
            depends_on: Vec::new(),
            cache: Some(CacheSpec {
                key_files: cache_key_files,
                paths: vec!["~/.cargo/registry".to_owned(), "~/.cargo/git".to_owned()],
            }),
            tool_version: None,
        });
    }

    if has_cargo_deny || has_cargo_audit {
        let mut commands = Vec::new();
        if has_cargo_deny {
            let command = if cargo_deny_has_license_policy(root, file_set) {
                "cargo deny check"
            } else {
                result.limitations.push(
                    "Cargo deny license checks are skipped because deny.toml declares no [licenses] policy.".to_owned(),
                );
                "cargo deny check advisories bans sources"
            };
            commands.push(command.to_owned());
        }
        if has_cargo_audit {
            commands.push("cargo audit".to_owned());
        }
        result.units.push(Unit {
            id: "rust-policy".to_owned(),
            label: "Rust dependency policy".to_owned(),
            kind: UnitKind::Rust,
            root: ".".to_owned(),
            watch: vec![
                "Cargo.toml".to_owned(),
                "Cargo.lock".to_owned(),
                "deny.toml".to_owned(),
                ".cargo/deny.toml".to_owned(),
                "audit.toml".to_owned(),
                ".cargo/audit.toml".to_owned(),
            ],
            pr_commands: commands.clone(),
            full_commands: commands,
            github_pr_commands: None,
            github_full_commands: None,
            velnor_pr_commands: None,
            velnor_full_commands: None,
            depends_on: Vec::new(),
            cache: Some(CacheSpec {
                key_files: vec![
                    ".cargo/**".to_owned(),
                    "Cargo.toml".to_owned(),
                    "Cargo.lock".to_owned(),
                ],
                paths: vec!["~/.cargo/registry".to_owned(), "~/.cargo/git".to_owned()],
            }),
            tool_version: None,
        });
    }

    for manifest in &package_facts {
        let owner = roots_by_manifest
            .get(&join_repo_path(&manifest.root, "Cargo.toml"))
            .cloned()
            .unwrap_or_default();
        let Some(unit) = result.units.iter_mut().find(|unit| unit.id == owner) else {
            continue;
        };
        let mut dependencies = BTreeSet::new();
        for dependency in &manifest.dependencies {
            let dependency_root = dependency
                .path
                .as_deref()
                .and_then(|path| resolve_repo_path(&manifest.root, path))
                .map(|path| join_repo_path(&path, "Cargo.toml"));
            let dependency_id = dependency_root
                .and_then(|path| roots_by_manifest.get(&path).cloned())
                .or_else(|| {
                    dependency
                        .package_name
                        .as_ref()
                        .and_then(|name| package_ids.get(name).cloned())
                })
                .or_else(|| package_ids.get(&dependency.name).cloned());
            if let Some(dependency_id) = dependency_id.filter(|id| id != &owner) {
                dependencies.insert(dependency_id);
            }
        }
        unit.depends_on = dependencies.into_iter().collect();
    }

    if facts.iter().any(|manifest| {
        manifest.build_script.is_some()
            || file_set.contains(&join_repo_path(&manifest.root, "build.rs"))
    }) {
        result.limitations.push(
            "Rust build scripts are watched but their system dependencies and generated outputs are not inferred.".to_owned(),
        );
    }
    if files.iter().any(|file| file == "Cargo.lock") {
        result.detected.push("cargo-lockfile".to_owned());
    }
    result.detected.sort();
    result.detected.dedup();
    result.limitations.sort();
    result.limitations.dedup();
    Ok(result)
}

fn include_str_paths(
    root: &Path,
    files: &[String],
    file_set: &BTreeSet<String>,
    package_root: &str,
) -> Result<Vec<String>, GeneratorError> {
    let prefix = path_prefix(package_root);
    let mut targets = BTreeSet::new();
    for source in files.iter().filter(|file| {
        has_extension(file, "rs") && (package_root == "." || file.starts_with(&prefix))
    }) {
        let source_contents = fs::read_to_string(root.join(source))
            .map_err(|error| GeneratorError::io("read Rust source", &root.join(source), &error))?;
        let included_paths = parse_include_str_literals(&source_contents).map_err(|error| {
            GeneratorError::usage(format!("{error} in {}", root.join(source).display()))
        })?;
        for included in included_paths {
            let target = resolve_repo_path(&parent_path(source), &included).ok_or_else(|| {
                GeneratorError::usage(format!(
                    "include_str! escapes the repository from {}: {included}",
                    root.join(source).display()
                ))
            })?;
            if !file_set.contains(&target) && !static_github_input_exists(root, &target)? {
                return Err(GeneratorError::usage(format!(
                    "include_str! target does not exist: {} -> {}",
                    root.join(source).display(),
                    target
                )));
            }
            targets.insert(target);
        }
    }
    Ok(targets.into_iter().collect())
}

pub(crate) fn parse_include_str_literals(source: &str) -> Result<Vec<String>, String> {
    const MACRO: &str = "include_str!";
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut included = Vec::new();
    while cursor < bytes.len() {
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'/') {
            cursor = skip_line_comment(bytes, cursor + 2);
            continue;
        }
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            cursor = skip_block_comment(bytes, cursor + 2);
            continue;
        }
        if let Some(end) = skip_raw_string(bytes, cursor) {
            cursor = end;
            continue;
        }
        if bytes[cursor] == b'"' {
            cursor = skip_quoted_literal(bytes, cursor, b'"')
                .map_err(|error| format!("{error} at byte {cursor}"))?;
            continue;
        }
        if bytes[cursor] == b'\'' && is_char_literal_start(bytes, cursor) {
            cursor = skip_quoted_literal(bytes, cursor, b'\'')
                .map_err(|error| format!("{error} at byte {cursor}"))?;
            continue;
        }

        if bytes[cursor..].starts_with(MACRO.as_bytes())
            && (cursor == 0 || !is_rust_identifier_byte(bytes[cursor - 1]))
        {
            let mut argument = cursor + MACRO.len();
            while bytes.get(argument).is_some_and(u8::is_ascii_whitespace) {
                argument += 1;
            }
            if bytes.get(argument) != Some(&b'(') {
                cursor += MACRO.len();
                continue;
            }
            argument += 1;
            while bytes.get(argument).is_some_and(u8::is_ascii_whitespace) {
                argument += 1;
            }
            if bytes.get(argument) != Some(&b'"') {
                return Err("include_str! must use a plain string literal".to_owned());
            }
            let literal_end = skip_quoted_literal(bytes, argument, b'"')
                .map_err(|error| format!("{error} at byte {argument}"))?;
            let literal = &source[argument..literal_end];
            let value = serde_json::from_str::<String>(literal).map_err(|error| {
                format!("include_str! must use a plain string literal: {error}")
            })?;
            let mut close = literal_end;
            while bytes.get(close).is_some_and(u8::is_ascii_whitespace) {
                close += 1;
            }
            if bytes.get(close) != Some(&b')') {
                return Err("unterminated include_str!".to_owned());
            }
            included.push(value);
            cursor = close + 1;
            continue;
        }
        cursor += 1;
    }
    Ok(included)
}

fn is_rust_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_line_comment(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() && bytes[cursor] != b'\n' {
        cursor += 1;
    }
    cursor
}

fn skip_block_comment(bytes: &[u8], mut cursor: usize) -> usize {
    let mut depth = 1;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'/' && bytes[cursor + 1] == b'*' {
            depth += 1;
            cursor += 2;
        } else if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return cursor;
            }
        } else {
            cursor += 1;
        }
    }
    bytes.len()
}

fn skip_raw_string(bytes: &[u8], cursor: usize) -> Option<usize> {
    let (hash_start, content_start) = match bytes.get(cursor..) {
        Some([b'r', rest @ ..]) => (
            cursor + 1,
            cursor + 1 + rest.iter().take_while(|byte| **byte == b'#').count(),
        ),
        Some([b'b', b'r', rest @ ..]) => (
            cursor + 2,
            cursor + 2 + rest.iter().take_while(|byte| **byte == b'#').count(),
        ),
        _ => return None,
    };
    if bytes.get(content_start) != Some(&b'"') {
        return None;
    }
    let hashes = content_start - hash_start;
    let mut end = content_start + 1;
    while end < bytes.len() {
        if bytes[end] == b'"'
            && bytes
                .get(end + 1..end + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return Some(end + 1 + hashes);
        }
        end += 1;
    }
    Some(bytes.len())
}

fn is_char_literal_start(bytes: &[u8], cursor: usize) -> bool {
    match bytes.get(cursor + 1) {
        Some(b'\\') => true,
        Some(byte) if *byte != b'\'' && *byte != b'\n' => bytes.get(cursor + 2) == Some(&b'\''),
        _ => false,
    }
}

fn skip_quoted_literal(bytes: &[u8], mut cursor: usize, delimiter: u8) -> Result<usize, String> {
    cursor += 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = cursor.saturating_add(2),
            character if character == delimiter => return Ok(cursor + 1),
            b'\n' if delimiter == b'\'' => {
                return Err(format!("unterminated quoted literal at byte {cursor}"));
            }
            _ => cursor += 1,
        }
    }
    Err(format!("unterminated quoted literal at byte {cursor}"))
}

pub(crate) fn static_github_input_exists(
    root: &Path,
    target: &str,
) -> Result<bool, GeneratorError> {
    if !target.starts_with(".github/")
        || target == ".github/UNIFIED-ACTIONS.md"
        || target.starts_with(".github/ci/")
        || target.starts_with(".github/workflows/")
    {
        return Ok(false);
    }
    let path = root.join(target);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(GeneratorError::io(
            "inspect include_str target",
            &path,
            &error,
        )),
    }
}

pub(crate) fn cargo_dependency_name(key: &str) -> &str {
    key.strip_suffix(".workspace").unwrap_or(key)
}

pub(crate) fn parse_cargo_manifest(root: &str, contents: &str) -> CargoManifestFacts {
    let mut facts = CargoManifestFacts {
        root: root.to_owned(),
        has_package: false,
        package_name: None,
        has_workspace: false,
        workspace_members: Vec::new(),
        dependencies: Vec::new(),
        build_script: None,
        binary_targets: Vec::new(),
        test_targets: Vec::new(),
        features: Vec::new(),
    };
    let mut section = String::new();
    let lines = contents.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let line = strip_toml_comment(lines[index]).trim().to_owned();
        index += 1;
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let normalized_section = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim_start_matches('[')
                .trim_end_matches(']');
            normalized_section.clone_into(&mut section);
            if section == "package" {
                facts.has_package = true;
            }
            if section == "workspace" {
                facts.has_workspace = true;
            }
            continue;
        }
        let Some((key, first_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').to_owned();
        let mut value = first_value.trim().to_owned();
        while toml_array_is_open(&value) && index < lines.len() {
            value.push(' ');
            value.push_str(strip_toml_comment(lines[index]).trim());
            index += 1;
        }
        match section.as_str() {
            "package" if key == "name" => facts.package_name = toml_string_value(&value),
            "package" if key == "build" => facts.build_script = toml_string_value(&value),
            "workspace" if key == "members" => {
                facts.workspace_members = toml_array_values(&value);
            }
            "bin" if key == "name" => {
                if let Some(name) = toml_string_value(&value) {
                    facts.binary_targets.push(name);
                }
            }
            "test" if key == "name" => {
                if let Some(name) = toml_string_value(&value) {
                    facts.test_targets.push(name);
                }
            }
            "features" => facts.features.push(key),
            section if is_cargo_dependency_section(section) => {
                facts.dependencies.push(CargoDependency {
                    name: cargo_dependency_name(&key).to_owned(),
                    package_name: toml_inline_string(&value, "package"),
                    path: toml_inline_string(&value, "path"),
                });
            }
            _ => {}
        }
    }
    facts
}

fn is_cargo_dependency_section(section: &str) -> bool {
    matches!(
        section,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    ) || (section != "workspace.dependencies" && section.ends_with(".dependencies"))
}

fn strip_toml_comment(line: &str) -> String {
    let mut output = String::new();
    let mut basic = false;
    let mut literal = false;
    let mut escaped = false;
    for character in line.chars() {
        if basic {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                basic = false;
            }
        } else if literal {
            if character == '\'' {
                literal = false;
            }
        } else if character == '"' {
            basic = true;
        } else if character == '\'' {
            literal = true;
        } else if character == '#' {
            break;
        }
        output.push(character);
    }
    output
}

fn toml_array_is_open(value: &str) -> bool {
    let mut depth = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if active == '"' && character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character == '[' {
            depth += 1;
        } else if character == ']' {
            depth -= 1;
        }
    }
    depth > 0
}

fn toml_string_value(value: &str) -> Option<String> {
    let value = value.trim();
    let quote = value.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut result = String::new();
    let mut escaped = false;
    for character in value.chars().skip(1) {
        if quote == '"' && escaped {
            result.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if quote == '"' && character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(result);
        } else {
            result.push(character);
        }
    }
    None
}

fn toml_array_values(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut quote = None;
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if let Some(active) = quote {
            if active == '"' && escaped {
                current.push(match character {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
                escaped = false;
            } else if active == '"' && character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
                values.push(std::mem::take(&mut current));
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        }
    }
    values
}

fn toml_inline_string(value: &str, wanted_key: &str) -> Option<String> {
    let value = value.trim().strip_prefix('{')?.strip_suffix('}')?;
    value.split(',').find_map(|entry| {
        let (key, value) = entry.split_once('=')?;
        (key.trim().trim_matches('"') == wanted_key).then(|| toml_string_value(value.trim()))?
    })
}

pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), GeneratorError> {
    let cargo_manifests = files_named(context.files, "Cargo.toml");
    if cargo_manifests.is_empty() {
        return Ok(());
    }
    let rust = analyze_rust_manifests(
        context.root,
        context.files,
        context.file_set,
        &cargo_manifests,
    )?;
    shape.units.extend(rust.units);
    shape.detected.extend(rust.detected);
    shape.limitations.extend(rust.limitations);
    Ok(())
}

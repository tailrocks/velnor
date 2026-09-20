//! Skills/plugin repository detector.
//!
//! A skills repository is metadata, documentation, and helper tooling. Its
//! nested examples are not executable project units. This detector validates
//! the repository-owned contract before generic language detectors run, then
//! returns the template paths that generic detectors must not claim.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use super::{unit, RepositoryShape, ScanContext};
use crate::s2::{GeneratorError, UnitKind};

const CATALOG: &str = "catalog.json";
const DOCS_INDEX: &str = "docs/index.json";
const DOCS_README: &str = "docs/README.md";
const PROVIDER_PLUGIN_FILES: [&str; 3] = [
    ".codex-plugin/plugin.json",
    ".kimi-plugin/plugin.json",
    ".claude-plugin/plugin.json",
];
const MARKETPLACE_FILE: &str = ".claude-plugin/marketplace.json";
const VELNOR_MISE_TOML: &str = include_str!("../../../../../mise.toml");
const VELNOR_MISE_LOCK: &str = include_str!("../../../../../mise.lock");

const HELPER_TEMPLATE_GLOB: &str = "*/templates/*";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkillsCheck {
    GeneratedDocs,
    HelperSyntax,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrontmatterMode {
    LiveDefinition,
    EmbeddedTemplate,
}

impl SkillsCheck {
    fn command(self, bun: &str) -> String {
        match self {
            Self::GeneratedDocs => format!(
                "set -o pipefail && test \"$(bun --version)\" = \"{bun}\" && tmp=$(mktemp -d) && trap 'rm -rf \"$tmp\"' EXIT && repo=$(basename \"$PWD\") && mkdir \"$tmp/$repo\" && git archive --format=tar HEAD | tar -x -C \"$tmp/$repo\" && cp -R \"$tmp/$repo/docs\" \"$tmp/docs.expected\" && bun \"$tmp/$repo/scripts/generate-docs.ts\" && diff -ru \"$tmp/docs.expected\" \"$tmp/$repo/docs\""
            ),
            Self::HelperSyntax => format!(
                "set -o pipefail && test \"$(bun --version)\" = \"{bun}\" && tmp=$(mktemp -d) && trap 'rm -rf \"$tmp\"' EXIT && find scripts -type f -name '*.ts' -not -path '{HELPER_TEMPLATE_GLOB}' -print0 | xargs -0 bun build --target=bun --no-bundle --outdir \"$tmp\""
            ),
        }
    }
}

fn verification_commands(bun: &str, generated_docs: bool, helper_syntax: bool) -> Vec<String> {
    let mut commands = Vec::new();
    if generated_docs {
        commands.push(SkillsCheck::GeneratedDocs.command(bun));
    }
    if helper_syntax {
        commands.push(SkillsCheck::HelperSyntax.command(bun));
    }
    commands
}

fn central_bun_version() -> Result<String, GeneratorError> {
    let configured = VELNOR_MISE_TOML
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("\"aqua:oven-sh/bun\" = \"")?
                .strip_suffix('"')
        })
        .ok_or_else(|| {
            GeneratorError::usage(
                "generator mise.toml must pin aqua:oven-sh/bun to an exact version",
            )
        })?;
    let mut in_bun_lock = false;
    let locked = VELNOR_MISE_LOCK
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.starts_with("[[") {
                in_bun_lock = line == "[[tools.\"aqua:oven-sh/bun\"]]";
                return None;
            }
            if in_bun_lock {
                return line
                    .strip_prefix("version = \"")
                    .and_then(|value| value.strip_suffix('"'));
            }
            None
        })
        .ok_or_else(|| {
            GeneratorError::usage(
                "generator mise.lock must contain the pinned aqua:oven-sh/bun version",
            )
        })?;
    if configured != locked {
        return Err(GeneratorError::usage(format!(
            "generator mise Bun version {configured} does not match locked version {locked}"
        )));
    }
    validate_provider_version(configured, "generator Bun toolchain")?;
    Ok(configured.to_owned())
}

fn target_bun_version(
    context: &ScanContext<'_>,
    template_files: &BTreeSet<String>,
) -> Result<String, GeneratorError> {
    let mut versions = BTreeSet::new();
    for file in context
        .files
        .iter()
        .filter(|file| file.ends_with("package.json") && !template_files.contains(*file))
    {
        let path = context.root.join(file);
        let value = parse_json(
            &read_text(&path, "read package manager manifest")?,
            &path,
            file,
        )?;
        let Some(object) = value.as_object() else {
            return Err(GeneratorError::usage(format!(
                "{file} must contain an object to declare packageManager"
            )));
        };
        let Some(package_manager) = object.get("packageManager") else {
            continue;
        };
        let Some(spec) = package_manager.as_str() else {
            return Err(GeneratorError::usage(format!(
                "{file} packageManager must be a string"
            )));
        };
        let Some((manager, version)) = spec.split_once('@') else {
            if spec == "bun" {
                return Err(GeneratorError::usage(format!(
                    "{file} packageManager must pin Bun to an exact version"
                )));
            }
            continue;
        };
        if manager != "bun" {
            continue;
        }
        validate_provider_version(version, &format!("{file} packageManager"))?;
        versions.insert(version.to_owned());
    }
    if versions.len() > 1 {
        return Err(GeneratorError::usage(format!(
            "Skills package manifests declare conflicting Bun versions: {}",
            versions.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    versions
        .into_iter()
        .next()
        .map_or_else(central_bun_version, Ok)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Frontmatter {
    values: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Catalog {
    names: Vec<String>,
    plugin_name: String,
}

/// Detect and validate one canonical skills/plugin repository.
///
/// The empty result means the repository is not a skills repository. A plugin
/// marker without the complete contract is an error: silently falling through
/// to a language detector would turn a malformed plugin into a false ordinary
/// project.
pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<BTreeSet<String>, GeneratorError> {
    if !has_plugin_marker(context) {
        return Ok(BTreeSet::new());
    }
    let catalog = read_catalog(context)?;
    validate_plugin_manifests(context, &catalog.plugin_name)?;
    validate_skills(context, &catalog.names)?;
    let generated_docs = has_generated_docs_surface(context);
    if generated_docs {
        validate_docs(context, &catalog.names)?;
    }
    validate_links(context)?;
    let template_files = template_files(context, &catalog.names);
    validate_templates(context, &template_files)?;
    let bun_version = target_bun_version(context, &template_files)?;
    let helper_syntax = has_helper_sources(context, &template_files);
    if generated_docs && !context.file_set.contains("scripts/generate-docs.ts") {
        return Err(GeneratorError::usage(
            "skills plugin is missing scripts/generate-docs.ts",
        ));
    }
    let mut skill_unit = unit(
        UnitKind::Skills,
        ".",
        vec![
            "README.md".to_owned(),
            CATALOG.to_owned(),
            "plugin.json".to_owned(),
            ".codex-plugin/**".to_owned(),
            ".kimi-plugin/**".to_owned(),
            ".claude-plugin/**".to_owned(),
            "skills/**".to_owned(),
            "docs/index.json".to_owned(),
            "docs/README.md".to_owned(),
            "docs/skills/**".to_owned(),
            "scripts/**".to_owned(),
        ],
        Vec::new(),
        None,
    );
    skill_unit.tool_version = Some(bun_version.clone());
    let commands = verification_commands(&bun_version, generated_docs, helper_syntax);
    skill_unit.pr_commands.clone_from(&commands);
    skill_unit.full_commands.clone_from(&commands);
    shape.units.push(skill_unit);
    shape.detected.push("skills-plugin".to_owned());
    shape.detected.push(format!("skills-bun:{bun_version}"));
    shape
        .detected
        .push(format!("skills-count:{}", catalog.names.len()));
    Ok(template_files)
}

fn has_generated_docs_surface(context: &ScanContext<'_>) -> bool {
    context.file_set.contains(DOCS_INDEX)
        || context.file_set.contains(DOCS_README)
        || context.file_set.contains("scripts/generate-docs.ts")
        || context.files.iter().any(|file| file.starts_with("docs/"))
}

fn has_helper_sources(context: &ScanContext<'_>, template_files: &BTreeSet<String>) -> bool {
    context.files.iter().any(|file| {
        file.starts_with("scripts/")
            && file.ends_with(".ts")
            && file != "scripts/generate-docs.ts"
            && !template_files.contains(file)
    })
}

fn has_plugin_marker(context: &ScanContext<'_>) -> bool {
    PROVIDER_PLUGIN_FILES
        .iter()
        .any(|path| context.file_set.contains(*path))
        || context.file_set.contains(MARKETPLACE_FILE)
        || (context.file_set.contains(CATALOG)
            && context
                .files
                .iter()
                .any(|file| file.starts_with("skills/") && file.ends_with("/SKILL.md")))
}

fn read_catalog(context: &ScanContext<'_>) -> Result<Catalog, GeneratorError> {
    require_file(context, CATALOG)?;
    let path = context.root.join(CATALOG);
    let text = read_text(&path, "read catalog.json")?;
    let value = parse_json(&text, &path, "catalog.json")?;
    let object = value.as_object().ok_or_else(|| {
        GeneratorError::usage("catalog.json must contain an object with a skills array")
    })?;
    let entries = object
        .get("skills")
        .and_then(Value::as_array)
        .ok_or_else(|| GeneratorError::usage("catalog.json must contain a skills array"))?;
    let mut names = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry
            .as_str()
            .ok_or_else(|| GeneratorError::usage("catalog.json skills entries must be strings"))?;
        validate_skill_name(name)?;
        if names.iter().any(|candidate| candidate == name) {
            return Err(GeneratorError::usage(format!(
                "catalog.json contains duplicate skill `{name}`"
            )));
        }
        names.push(name.to_owned());
    }
    if names.is_empty() {
        return Err(GeneratorError::usage("catalog.json skills array is empty"));
    }
    let root_plugin = read_json_object(context, "plugin.json")?;
    let plugin_name = string_field(&root_plugin, "name", "plugin.json")?.to_owned();
    if plugin_name.is_empty()
        || plugin_name.trim() != plugin_name
        || plugin_name.contains('/')
        || plugin_name.contains('\\')
        || plugin_name
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(GeneratorError::usage(
            "plugin.json name must be a nonempty safe component",
        ));
    }
    Ok(Catalog { names, plugin_name })
}

fn validate_skill_name(name: &str) -> Result<(), GeneratorError> {
    if name.is_empty()
        || name.trim().is_empty()
        || name != name.trim()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(GeneratorError::usage(format!(
            "catalog.json contains unsafe skill name `{name}`"
        )));
    }
    Ok(())
}

fn parse_markdown_destination(input: &str) -> Option<(&str, usize)> {
    let bytes = input.as_bytes();
    let mut index = 0;
    skip_markdown_whitespace(bytes, &mut index);
    let target_start;
    let target_end;
    if bytes.get(index) == Some(&b'<') {
        index += 1;
        target_start = index;
        while index < bytes.len() {
            match bytes[index] {
                b'\\' if index + 1 < bytes.len() => index += 2,
                b'>' => break,
                _ => index += 1,
            }
        }
        if bytes.get(index) != Some(&b'>') {
            return None;
        }
        target_end = index;
        index += 1;
    } else {
        target_start = index;
        let mut depth = 0_usize;
        while index < bytes.len() {
            match bytes[index] {
                b'\\' if index + 1 < bytes.len() => index += 2,
                b'(' => {
                    depth += 1;
                    index += 1;
                }
                b')' if depth == 0 => break,
                b')' => {
                    depth -= 1;
                    index += 1;
                }
                byte if byte.is_ascii_whitespace() && depth == 0 => break,
                _ => index += 1,
            }
        }
        target_end = index;
    }
    if target_start == target_end {
        return None;
    }
    skip_markdown_whitespace(bytes, &mut index);
    if bytes.get(index) != Some(&b')') {
        match bytes.get(index) {
            Some(b'"' | b'\'') => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' if index + 1 < bytes.len() => index += 2,
                        byte if byte == quote => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            Some(b'(') => {
                let mut depth = 0_usize;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' if index + 1 < bytes.len() => index += 2,
                        b'(' => {
                            depth += 1;
                            index += 1;
                        }
                        b')' => {
                            depth = depth.checked_sub(1)?;
                            index += 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => index += 1,
                    }
                }
            }
            _ => return None,
        }
        skip_markdown_whitespace(bytes, &mut index);
    }
    (bytes.get(index) == Some(&b')')).then(|| (&input[target_start..target_end], index + 1))
}

fn skip_markdown_whitespace(bytes: &[u8], index: &mut usize) {
    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
        *index += 1;
    }
}

fn normalize_markdown_destination(target: &str) -> Option<String> {
    let path = target.split_once('#').map_or(target, |(path, _)| path);
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    let target = unescape_markdown_target(path)?;
    let target = percent_decode_markdown_uri(&target)?;
    (!target.contains('\\') && !target.chars().any(char::is_control)).then_some(target)
}

fn unescape_markdown_target(target: &str) -> Option<String> {
    let mut result = String::with_capacity(target.len());
    let mut chars = target.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            let escaped = chars.next()?;
            if !escaped.is_ascii_punctuation() {
                return None;
            }
            result.push(escaped);
        } else {
            result.push(character);
        }
    }
    Some(result)
}

fn percent_decode_markdown_uri(target: &str) -> Option<String> {
    let bytes = target.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let first = hex_value(*bytes.get(index + 1)?)?;
        let second = hex_value(*bytes.get(index + 2)?)?;
        let byte = (first << 4) | second;
        if matches!(byte, b'/' | b'\\' | b'.') {
            return None;
        }
        decoded.push(byte);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_external_uri(target: &str) -> bool {
    target.split_once(':').is_some_and(|(scheme, _)| {
        matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto"
        )
    })
}

fn has_uri_scheme(target: &str) -> bool {
    let Some((scheme, _)) = target.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.chars().enumerate().all(|(index, character)| {
            character.is_ascii_alphabetic()
                || (index > 0
                    && (character.is_ascii_digit() || matches!(character, '+' | '-' | '.')))
        })
}

fn validate_plugin_manifests(
    context: &ScanContext<'_>,
    expected_name: &str,
) -> Result<(), GeneratorError> {
    // Provider objects intentionally remain forward-compatible: their
    // provider-specific fields differ, so only shared compatibility fields
    // are validated here while recursive duplicate JSON keys are rejected by
    // the strict parser.
    let providers = PROVIDER_PLUGIN_FILES
        .iter()
        .filter(|path| context.file_set.contains(**path))
        .map(|path| Ok((*path, read_json_object(context, path)?)))
        .collect::<Result<Vec<_>, GeneratorError>>()?;
    if providers.is_empty() && !context.file_set.contains(MARKETPLACE_FILE) {
        return Err(GeneratorError::usage(
            "skills plugin requires at least one provider or marketplace manifest",
        ));
    }
    let mut shared_version = None;
    for (path, object) in &providers {
        let name = string_field(object, "name", path)?;
        if name != expected_name {
            return Err(GeneratorError::usage(format!(
                "{path} name `{name}` does not match plugin.json name `{expected_name}`"
            )));
        }
        let version = string_field(object, "version", path)?;
        validate_provider_version(version, path)?;
        if shared_version.is_none() {
            shared_version = Some(version.to_owned());
        } else if shared_version.as_deref() != Some(version) {
            return Err(GeneratorError::usage(format!(
                "{path} version does not match the other provider manifests"
            )));
        }
        if matches!(
            *path,
            ".codex-plugin/plugin.json" | ".kimi-plugin/plugin.json"
        ) && string_field(object, "skills", path)? != "./skills/"
        {
            return Err(GeneratorError::usage(format!(
                "{path} skills must be `./skills/`"
            )));
        }
    }
    if context.file_set.contains(MARKETPLACE_FILE) {
        let marketplace = read_json_object(context, MARKETPLACE_FILE)?;
        let marketplace_plugins = marketplace
            .get("plugins")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                GeneratorError::usage(
                    ".claude-plugin/marketplace.json must contain a plugins array",
                )
            })?;
        if marketplace_plugins.len() != 1 {
            return Err(GeneratorError::usage(
                ".claude-plugin/marketplace.json must contain exactly one plugin",
            ));
        }
        let marketplace_plugin = marketplace_plugins[0].as_object().ok_or_else(|| {
            GeneratorError::usage(".claude-plugin/marketplace.json plugins entries must be objects")
        })?;
        let marketplace_version = string_field(marketplace_plugin, "version", MARKETPLACE_FILE)?;
        validate_provider_version(marketplace_version, MARKETPLACE_FILE)?;
        if string_field(marketplace_plugin, "name", MARKETPLACE_FILE)? != expected_name
            || string_field(marketplace_plugin, "source", MARKETPLACE_FILE)? != "./"
            || shared_version
                .as_deref()
                .is_some_and(|version| version != marketplace_version)
        {
            return Err(GeneratorError::usage(
                ".claude-plugin/marketplace.json plugin name, source, and version do not match provider manifests",
            ));
        }
    }
    Ok(())
}

fn validate_provider_version(version: &str, path: &str) -> Result<(), GeneratorError> {
    let (without_build, build) = version
        .split_once('+')
        .map_or((version, None), |(version, build)| (version, Some(build)));
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(core, prerelease)| {
            (core, Some(prerelease))
        });
    let parts = core.split('.').collect::<Vec<_>>();
    let valid_core = parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty()
                && (part == &"0" || !part.starts_with('0'))
                && part.bytes().all(|byte| byte.is_ascii_digit())
        });
    let valid_prerelease = prerelease.is_none_or(|value| {
        !value.is_empty()
            && value.split('.').all(|part| {
                !part.is_empty()
                    && (part == "0"
                        || !part.starts_with('0')
                        || !part.bytes().all(|byte| byte.is_ascii_digit()))
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
    });
    let valid_build = build.is_none_or(|value| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
            && value.split('.').all(|part| !part.is_empty())
    });
    if !valid_core || !valid_prerelease || !valid_build {
        return Err(GeneratorError::usage(format!(
            "{path} version `{version}` must be semantic version `MAJOR.MINOR.PATCH` with optional prerelease/build metadata"
        )));
    }
    Ok(())
}

fn validate_skills(context: &ScanContext<'_>, names: &[String]) -> Result<(), GeneratorError> {
    let expected = names
        .iter()
        .map(|name| format!("skills/{name}/SKILL.md"))
        .collect::<BTreeSet<_>>();
    for path in &expected {
        require_file(context, path)?;
        let text = read_text(&context.root.join(path), "read skill definition")?;
        let frontmatter = parse_frontmatter(&text, path, FrontmatterMode::LiveDefinition)?;
        let expected_name = path
            .strip_prefix("skills/")
            .and_then(|value| value.strip_suffix("/SKILL.md"))
            .ok_or_else(|| GeneratorError::usage(format!("invalid skill path `{path}`")))?;
        if frontmatter.values.get("name").map(String::as_str) != Some(expected_name) {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter name does not match catalog entry `{expected_name}`"
            )));
        }
    }
    let extra = context
        .files
        .iter()
        .filter(|file| is_root_skill_definition(file))
        .filter(|file| !expected.contains(*file))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(path) = extra.first() {
        return Err(GeneratorError::usage(format!(
            "skill definition `{path}` is not listed in catalog.json"
        )));
    }
    Ok(())
}

fn validate_docs(context: &ScanContext<'_>, names: &[String]) -> Result<(), GeneratorError> {
    require_file(context, DOCS_INDEX)?;
    require_file(context, DOCS_README)?;
    let path = context.root.join(DOCS_INDEX);
    let text = read_text(&path, "read docs/index.json")?;
    let entries = parse_json(&text, &path, DOCS_INDEX)?
        .as_array()
        .cloned()
        .ok_or_else(|| GeneratorError::usage("docs/index.json must contain an array"))?;
    let entry_names = entries
        .iter()
        .map(|entry| {
            let object = entry
                .as_object()
                .ok_or_else(|| GeneratorError::usage("docs/index.json entries must be objects"))?;
            let name = nonempty_string_field(object, "name", DOCS_INDEX)?.to_owned();
            for field in ["description", "overview", "definition", "source"] {
                nonempty_string_field(object, field, DOCS_INDEX)?;
            }
            let overview = nonempty_string_field(object, "overview", DOCS_INDEX)?;
            let definition = nonempty_string_field(object, "definition", DOCS_INDEX)?;
            require_docs_file(context, overview)?;
            require_docs_file(context, definition)?;
            Ok(name)
        })
        .collect::<Result<Vec<_>, GeneratorError>>()?;
    if entry_names != names {
        return Err(GeneratorError::usage(
            "docs/index.json names must exactly match catalog.json order",
        ));
    }
    let readme = read_text(&context.root.join(DOCS_README), "read docs README")?;
    for name in names {
        let marker = format!("- [{name}](skills/{name}/index.md)");
        if !readme.contains(&marker) {
            return Err(GeneratorError::usage(format!(
                "docs/README.md is missing catalog skill `{name}`"
            )));
        }
    }
    Ok(())
}

fn validate_links(context: &ScanContext<'_>) -> Result<(), GeneratorError> {
    for file in context.files.iter().filter(|file| {
        file.starts_with("skills/")
            && Path::new(file)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("mdx")
                })
    }) {
        let text = read_text(&context.root.join(file), "read Markdown reference")?;
        let mut fenced = None;
        for (line_number, line) in text.lines().enumerate() {
            if let Some((fence, length)) = markdown_fence(line) {
                match fenced {
                    None => fenced = Some((fence, length)),
                    Some((open_fence, open_length))
                        if fence == open_fence && length >= open_length =>
                    {
                        fenced = None;
                    }
                    Some(_) => {}
                }
                continue;
            }
            if fenced.is_some() {
                continue;
            }
            let mut offset = 0;
            while let Some(start) = find_markdown_link_start(line, offset) {
                let destination = &line[start + 2..];
                let Some((raw_target, consumed)) = parse_markdown_destination(destination) else {
                    return Err(GeneratorError::usage(format!(
                        "malformed Markdown link at {file}:{}",
                        line_number + 1
                    )));
                };
                offset = start + 2 + consumed;
                let target = normalize_markdown_destination(raw_target).ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "unsafe Markdown link {raw_target} at {file}:{}",
                        line_number + 1
                    ))
                })?;
                if target.is_empty() || is_external_uri(&target) {
                    continue;
                }
                if is_markdown_placeholder(&target) {
                    continue;
                }
                let resolved = resolve_markdown_path(file, &target).ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "unsafe Markdown link {target} at {file}:{}",
                        line_number + 1
                    ))
                })?;
                if !context.file_set.contains(&resolved)
                    && !context
                        .files
                        .iter()
                        .any(|candidate| candidate.starts_with(&format!("{resolved}/")))
                {
                    return Err(GeneratorError::usage(format!(
                        "missing Markdown link {target} at {file}:{}",
                        line_number + 1
                    )));
                }
            }
        }
    }
    Ok(())
}

fn markdown_fence(line: &str) -> Option<(u8, usize)> {
    let bytes = line.trim_start().as_bytes();
    let fence = *bytes.first()?;
    if fence != b'`' && fence != b'~' {
        return None;
    }
    let length = bytes.iter().take_while(|byte| **byte == fence).count();
    (length >= 3).then_some((fence, length))
}

fn find_markdown_link_start(line: &str, from: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut index = from;
    let mut code_ticks = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
            continue;
        }
        if bytes[index] == b'`' {
            let start = index;
            while bytes.get(index) == Some(&b'`') {
                index += 1;
            }
            let run = index - start;
            if code_ticks == 0 {
                code_ticks = run;
            } else if code_ticks == run {
                code_ticks = 0;
            }
            continue;
        }
        if code_ticks == 0 && bytes[index] == b']' && bytes.get(index + 1) == Some(&b'(') {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn resolve_markdown_path(source: &str, target: &str) -> Option<String> {
    if target.starts_with('/')
        || target.starts_with("//")
        || target.contains('\\')
        || target.chars().any(char::is_control)
        || has_uri_scheme(target)
    {
        return None;
    }
    let mut parts = source
        .rsplit_once('/')
        .map(|(parent, _)| parent.split('/').map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            value => parts.push(value.to_owned()),
        }
    }
    Some(parts.join("/"))
}

fn is_root_skill_definition(file: &str) -> bool {
    let mut segments = file.split('/');
    segments.next() == Some("skills")
        && segments.next().is_some()
        && segments.next() == Some("SKILL.md")
        && segments.next().is_none()
}

fn require_docs_file(context: &ScanContext<'_>, relative: &str) -> Result<(), GeneratorError> {
    let target = normalize_markdown_destination(relative).ok_or_else(|| {
        GeneratorError::usage(format!(
            "{DOCS_INDEX} contains unsafe generated-document path {relative}"
        ))
    })?;
    if target.is_empty() || is_external_uri(&target) || is_markdown_placeholder(&target) {
        return Err(GeneratorError::usage(format!(
            "{DOCS_INDEX} contains non-local generated-document path {relative}"
        )));
    }
    let resolved = resolve_markdown_path(DOCS_INDEX, &target).ok_or_else(|| {
        GeneratorError::usage(format!(
            "{DOCS_INDEX} contains unsafe generated-document path {relative}"
        ))
    })?;
    if !resolved.starts_with("docs/") {
        return Err(GeneratorError::usage(format!(
            "{DOCS_INDEX} path {relative} escapes docs/"
        )));
    }
    require_file(context, &resolved)
}

fn is_markdown_placeholder(target: &str) -> bool {
    target.split('/').any(|component| {
        let inner = component
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'));
        inner.is_some_and(|value| {
            let mut characters = value.chars();
            characters
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic())
                && characters.all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
                })
        })
    })
}

fn template_files(context: &ScanContext<'_>, names: &[String]) -> BTreeSet<String> {
    let mut files = names
        .iter()
        .flat_map(|name| {
            let prefix = format!("skills/{name}/templates/");
            context
                .files
                .iter()
                .filter(move |file| file.starts_with(&prefix))
                .cloned()
        })
        .collect::<BTreeSet<_>>();
    for prefix in helper_template_prefixes(context) {
        files.extend(
            context
                .files
                .iter()
                .filter(|file| file.starts_with(&prefix))
                .cloned(),
        );
    }
    files
}

fn helper_template_prefixes(context: &ScanContext<'_>) -> BTreeSet<String> {
    let mut prefixes = BTreeSet::new();
    let helper_roots = context
        .files
        .iter()
        .filter_map(|file| {
            let relative = file.strip_prefix("scripts/")?;
            let helper = relative.split('/').next()?;
            (!helper.is_empty()).then(|| format!("scripts/{helper}/"))
        })
        .collect::<BTreeSet<_>>();
    for root in helper_roots {
        let prefix = format!("{root}templates/");
        if !context.files.iter().any(|file| file.starts_with(&prefix)) {
            continue;
        }
        let declares_templates = context.files.iter().any(|file| {
            file.starts_with(&root)
                && !file.starts_with(&prefix)
                && is_helper_source(file)
                && read_text(&context.root.join(file), "read helper source")
                    .is_ok_and(|text| helper_source_declares_templates(&text))
        });
        if declares_templates {
            prefixes.insert(prefix);
        }
    }
    prefixes
}

fn is_helper_source(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "ts" | "tsx" | "js" | "jsx" | "sh"))
}

fn helper_source_declares_templates(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let quote = bytes[index];
        if !matches!(quote, b'\'' | b'"' | b'`') {
            index += 1;
            continue;
        }
        index += 1;
        let start = index;
        let mut escaped = false;
        while index < bytes.len() {
            let byte = bytes[index];
            if escaped {
                escaped = false;
                index += 1;
                continue;
            }
            if byte == b'\\' {
                escaped = true;
                index += 1;
                continue;
            }
            if byte == quote {
                let literal = &text[start..index];
                if literal
                    .replace("\\/", "/")
                    .split('/')
                    .any(|component| component == "templates")
                {
                    return true;
                }
                index += 1;
                break;
            }
            index += 1;
        }
    }
    false
}

fn validate_templates(
    context: &ScanContext<'_>,
    template_files: &BTreeSet<String>,
) -> Result<(), GeneratorError> {
    for file in template_files {
        let path = context.root.join(file);
        let text = read_text(&path, "read template")?;
        match Path::new(file)
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("md") if file.ends_with("/SKILL.md") => {
                parse_frontmatter(&text, file, FrontmatterMode::EmbeddedTemplate)?;
            }
            Some("json") => {
                parse_json(&text, &path, "JSON template")?;
            }
            Some("toml") => {
                text.parse::<toml::Table>().map_err(|error| {
                    GeneratorError::usage(format!("invalid TOML template {file}: {error}"))
                })?;
            }
            Some("yaml" | "yml") => {
                serde_yaml::from_str::<serde_yaml::Value>(&text).map_err(|error| {
                    GeneratorError::usage(format!("invalid YAML template {file}: {error}"))
                })?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_frontmatter(
    text: &str,
    path: &str,
    mode: FrontmatterMode,
) -> Result<Frontmatter, GeneratorError> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(GeneratorError::usage(format!(
            "{path} is missing opening frontmatter delimiter"
        )));
    }
    let mut yaml_lines = Vec::new();
    let mut closed = false;
    for raw_line in lines {
        let line = raw_line.trim_end_matches('\r');
        if line == "---" {
            closed = true;
            break;
        }
        yaml_lines.push(line.to_owned());
    }
    if !closed {
        return Err(GeneratorError::usage(format!(
            "{path} is missing closing frontmatter delimiter"
        )));
    }
    let parser = serde_yaml::ParserConfig::strict()
        .max_alias_expansions(0)
        .merge_key_policy(serde_yaml::MergeKeyPolicy::Error);
    let yaml =
        serde_yaml::from_str_with_config::<serde_yaml::Value>(&yaml_lines.join("\n"), &parser)
            .map_err(|error| {
                GeneratorError::usage(format!("{path} has invalid YAML frontmatter: {error}"))
            })?;
    reject_yaml_tags(&yaml, path)?;
    let mapping = yaml.as_mapping().ok_or_else(|| {
        GeneratorError::usage(format!("{path} frontmatter must contain a YAML mapping"))
    })?;
    validate_frontmatter_mapping(mapping, path)?;
    if mode == FrontmatterMode::LiveDefinition {
        let Some(name) = mapping.get("name").and_then(serde_yaml::Value::as_str) else {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter name must be a YAML string"
            )));
        };
        validate_skill_name(name).map_err(|_| {
            GeneratorError::usage(format!(
                "{path} frontmatter name {name} is not a safe live skill name"
            ))
        })?;
    }
    let values = mapping
        .iter()
        .filter_map(|(key, value)| {
            let value = match value {
                serde_yaml::Value::String(value) => value.clone(),
                serde_yaml::Value::Bool(value) => value.to_string(),
                _ => return None,
            };
            Some((key.to_owned(), value))
        })
        .collect();
    Ok(Frontmatter { values })
}

fn reject_yaml_tags(value: &serde_yaml::Value, path: &str) -> Result<(), GeneratorError> {
    match value {
        serde_yaml::Value::Tagged(_) => Err(GeneratorError::usage(format!(
            "{path} frontmatter does not allow YAML tags or aliases"
        ))),
        serde_yaml::Value::Sequence(values) => {
            for value in values {
                reject_yaml_tags(value, path)?;
            }
            Ok(())
        }
        serde_yaml::Value::Mapping(mapping) => {
            for (_, value) in mapping {
                reject_yaml_tags(value, path)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

const FRONTMATTER_KEYS: [&str; 6] = [
    "name",
    "description",
    "argument-hint",
    "license",
    "user-invocable",
    "disable-model-invocation",
];

fn validate_frontmatter_mapping(
    mapping: &serde_yaml::Mapping,
    path: &str,
) -> Result<(), GeneratorError> {
    for (key, value) in mapping {
        let key = key.as_str();
        if !FRONTMATTER_KEYS.contains(&key) {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter contains unsupported key `{key}`"
            )));
        }
        if matches!(key, "name" | "description" | "argument-hint" | "license") && !value.is_string()
        {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter {key} must be a YAML string"
            )));
        }
        if matches!(key, "user-invocable" | "disable-model-invocation") && !value.is_bool() {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter {key} must be a YAML boolean"
            )));
        }
    }
    for key in ["name", "description"] {
        if !mapping.contains_key(key) {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter requires {key}"
            )));
        }
        if mapping
            .get(key)
            .and_then(serde_yaml::Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(GeneratorError::usage(format!(
                "{path} frontmatter {key} must not be empty"
            )));
        }
    }
    Ok(())
}

fn read_json_object(
    context: &ScanContext<'_>,
    relative: &str,
) -> Result<serde_json::Map<String, Value>, GeneratorError> {
    require_file(context, relative)?;
    let path = context.root.join(relative);
    let text = read_text(&path, "read JSON manifest")?;
    let value = parse_json(&text, &path, relative)?;
    value.as_object().cloned().ok_or_else(|| {
        GeneratorError::usage(format!("JSON manifest {relative} must contain an object"))
    })
}

fn parse_json(text: &str, path: &Path, label: &str) -> Result<Value, GeneratorError> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = StrictJsonValue::deserialize(&mut deserializer).map_err(|error| {
        GeneratorError::usage(format!("invalid {label} at {}: {error}", path.display()))
    })?;
    deserializer.end().map_err(|error| {
        GeneratorError::usage(format!("invalid {label} at {}: {error}", path.display()))
    })?;
    Ok(value.0)
}

struct StrictJsonValue(Value);

impl<'de> Deserialize<'de> for StrictJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJsonValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value with unique object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(|number| StrictJsonValue(Value::Number(number)))
            .ok_or_else(|| E::custom("JSON number is not finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictJsonValue>()? {
            values.push(value.0);
        }
        Ok(StrictJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key}"
                )));
            }
            let value = map.next_value::<StrictJsonValue>()?;
            object.insert(key, value.0);
        }
        Ok(StrictJsonValue(Value::Object(object)))
    }
}

fn string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<&'a str, GeneratorError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| GeneratorError::usage(format!("{path} requires string field `{key}`")))
}

fn nonempty_string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<&'a str, GeneratorError> {
    let value = string_field(object, key, path)?;
    if value.trim().is_empty() {
        return Err(GeneratorError::usage(format!(
            "{path} field {key} must not be empty"
        )));
    }
    Ok(value)
}

fn require_file(context: &ScanContext<'_>, relative: &str) -> Result<(), GeneratorError> {
    if context.file_set.contains(relative) {
        Ok(())
    } else {
        Err(GeneratorError::usage(format!(
            "skills plugin is missing tracked file `{relative}`"
        )))
    }
}

fn read_text(path: &Path, operation: &str) -> Result<String, GeneratorError> {
    fs::read_to_string(path).map_err(|error| GeneratorError::io(operation, path, &error))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::manual_let_else,
    clippy::panic,
    clippy::unreachable,
    reason = "negative detector fixtures intentionally use direct failure assertions"
)]
mod tests {
    use super::{
        detect, is_markdown_placeholder, markdown_fence, normalize_markdown_destination,
        parse_frontmatter, parse_markdown_destination, resolve_markdown_path,
        validate_provider_version, FrontmatterMode,
    };
    use crate::s2::provider::{ProviderId, ProviderSet};
    use crate::s2::scan::{RepositoryShape, ScanContext};
    use crate::s2::{GeneratorError, UnitKind};
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    #[expect(clippy::panic, reason = "fixture setup failures must name their cause")]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    struct Fixture {
        root: PathBuf,
        files: Vec<String>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "velnor-skills-detector-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            let files = [
                "catalog.json",
                "plugin.json",
                ".codex-plugin/plugin.json",
                ".kimi-plugin/plugin.json",
                ".claude-plugin/plugin.json",
                ".claude-plugin/marketplace.json",
                "docs/index.json",
                "docs/README.md",
                "docs/skills/example/index.md",
                "docs/skills/example/definition.md",
                "skills/example/SKILL.md",
                "skills/example/references/policy.md",
                "skills/example/templates/package.json",
                "skills/example/templates/Cargo.toml",
                "skills/example/templates/config.yaml",
                "skills/example/templates/skill/SKILL.md",
                "skills/example/helpers/package.json",
                "skills/example/foo bar.md",
                "skills/example/diagram_(v1).md",
                "scripts/generate-docs.ts",
                "scripts/helper.ts",
                "scripts/template-helper/install.ts",
                "scripts/template-helper/package.json",
                "scripts/template-helper/templates/Cargo.toml",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
            let fixture = Self { root, files };
            for file in &fixture.files {
                let content = match file.as_str() {
                    "catalog.json" => r#"{"skills":["example"]}"#,
                    "plugin.json" => r#"{"name":"example-plugin"}"#,
                    ".codex-plugin/plugin.json" | ".kimi-plugin/plugin.json" => {
                        r#"{"name":"example-plugin","version":"1.0.0","skills":"./skills/"}"#
                    }
                    ".claude-plugin/plugin.json" => {
                        r#"{"name":"example-plugin","version":"1.0.0"}"#
                    }
                    ".claude-plugin/marketplace.json" => {
                        r#"{"plugins":[{"name":"example-plugin","source":"./","version":"1.0.0"}]}"#
                    }
                    "docs/index.json" => {
                        r#"[{"name":"example","description":"Example","overview":"skills/example/index.md","definition":"skills/example/definition.md","source":"https://example.invalid/example"}]"#
                    }
                    "docs/README.md" => "- [example](skills/example/index.md)\n",
                    "docs/skills/example/index.md" => "# Example\n",
                    "docs/skills/example/definition.md" => "# Definition\n",
                    "skills/example/SKILL.md" => {
                        r#"---
name: example
description: >-
  Use the example skill.
argument-hint: "<path>"
license: Apache-2.0
user-invocable: true
---
# Example
See [policy](references/policy.md "title"), [templates](templates/), [diagram](diagram_(v1).md), and [space](foo%20bar.md?view=full#section). External [web](https://example.invalid), [mail](mailto:dev@example.invalid), and [anchor](#section) are non-local. Inline code `[fake](missing.md)` is not a link.
"#
                    }
                    "skills/example/references/policy.md" => "# Policy\n",
                    "skills/example/templates/package.json" => r#"{"name":"template"}"#,
                    "skills/example/templates/Cargo.toml" => {
                        "[package]\nname = \"template\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
                    }
                    "skills/example/templates/config.yaml" => "name: template\n",
                    "skills/example/templates/skill/SKILL.md" => {
                        "---\nname: <skill-name>\ndescription: >-\n  Template skill.\nargument-hint: \"<args>\"\nlicense: Apache-2.0\nuser-invocable: true\n---\n# Template\n"
                    }
                    "skills/example/helpers/package.json" => {
                        r#"{"name":"helper","packageManager":"bun@1.2.3","scripts":{"check":"bun test"}}"#
                    }
                    "skills/example/foo bar.md" => "# Space\n",
                    "skills/example/diagram_(v1).md" => "# Diagram\n",
                    "scripts/generate-docs.ts" | "scripts/helper.ts" => "console.log('ok');\n",
                    "scripts/template-helper/install.ts" => {
                        "const template = \"templates/Cargo.toml\";\nconsole.log(template);\n"
                    }
                    "scripts/template-helper/package.json" => {
                        r#"{"name":"template-helper","packageManager":"bun@1.2.3","scripts":{"check":"bun test"}}"#
                    }
                    "scripts/template-helper/templates/Cargo.toml" => {
                        "[package]\nname = \"helper-template\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
                    }
                    _ => unreachable!("fixture file has content"),
                };
                fixture.write(file, content);
            }
            fixture
        }

        fn write(&self, relative: &str, content: &str) {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                must(fs::create_dir_all(parent), "create fixture parent");
            }
            must(fs::write(path, content), "write fixture file");
        }

        fn run_detect(&self) -> Result<(BTreeSet<String>, RepositoryShape), GeneratorError> {
            let file_set = self.files.iter().cloned().collect::<BTreeSet<_>>();
            let context = ScanContext {
                root: &self.root,
                files: &self.files,
                file_set: &file_set,
            };
            let mut shape = empty_shape();
            let hidden = detect(&context, &mut shape)?;
            Ok((hidden, shape))
        }

        fn run_detect_failure(&self) -> (GeneratorError, RepositoryShape) {
            let file_set = self.files.iter().cloned().collect::<BTreeSet<_>>();
            let context = ScanContext {
                root: &self.root,
                files: &self.files,
                file_set: &file_set,
            };
            let mut shape = empty_shape();
            let error = detect(&context, &mut shape).expect_err("fixture must be rejected");
            (error, shape)
        }

        fn run_scan(&self) -> Result<RepositoryShape, GeneratorError> {
            super::super::scan_shape(
                &self.root,
                &ProviderSet::from([ProviderId::GithubHosted]),
                "main",
                &[],
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn empty_shape() -> RepositoryShape {
        RepositoryShape {
            files: Vec::new(),
            units: Vec::new(),
            detected: Vec::new(),
            limitations: Vec::new(),
            default_branch: "main".to_owned(),
            providers: ProviderSet::from([ProviderId::GithubHosted]),
        }
    }

    #[test]
    fn frontmatter_parser_accepts_folded_descriptions() {
        let parsed = parse_frontmatter(
            "---\nname: example\ndescription: >-\n  first line\n  second line\nargument-hint: \"<args>\"\nlicense: Apache-2.0\nuser-invocable: true\n---\n",
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        )
        .unwrap_or_else(|error| panic!("frontmatter fixture parses: {error}"));
        assert_eq!(parsed.values["name"], "example");
        assert_eq!(parsed.values["description"], "first line second line");
    }

    #[test]
    fn frontmatter_parser_rejects_missing_delimiter() {
        let error = match parse_frontmatter(
            "# not frontmatter\n",
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        ) {
            Ok(_) => panic!("missing frontmatter must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("opening frontmatter"));
    }

    #[test]
    fn frontmatter_parser_rejects_missing_closing_delimiter() {
        let error = match parse_frontmatter(
            "---\nname: example\ndescription: text\nargument-hint: args\nlicense: Apache-2.0\nuser-invocable: true\n",
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        ) {
            Ok(_) => panic!("missing closing frontmatter must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("closing frontmatter"));
    }

    #[test]
    fn malformed_yaml_frontmatter_is_rejected() {
        let error = match parse_frontmatter(
            "---\nname: example\ndescription: [unterminated\nargument-hint: args\nlicense: Apache-2.0\nuser-invocable: true\n---\n",
            "skills/example/SKILL.md",
            FrontmatterMode::LiveDefinition,
        ) {
            Ok(_) => panic!("malformed YAML frontmatter must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("invalid YAML frontmatter"));
    }

    fn reject_skill_frontmatter_variant(rewrite: impl FnOnce(String) -> String, needle: &str) {
        let fixture = Fixture::new();
        let skill = must(
            fs::read_to_string(fixture.root.join("skills/example/SKILL.md")),
            "read skill fixture",
        );
        fixture.write("skills/example/SKILL.md", &rewrite(skill));
        let (error, shape) = fixture.run_detect_failure();
        assert!(shape.units.is_empty(), "rejected input must emit no unit");
        assert!(
            error.to_string().contains(needle),
            "{error} does not contain {needle}"
        );
    }

    #[test]
    fn typed_frontmatter_rejects_wrong_yaml_types() {
        reject_skill_frontmatter_variant(
            |skill| {
                skill.replacen(
                    "argument-hint: \"<path>\"",
                    "argument-hint: [\"<path>\"]",
                    1,
                )
            },
            "argument-hint must be a YAML string",
        );
        reject_skill_frontmatter_variant(
            |skill| {
                skill.replacen(
                    "description: >-\n  Use the example skill.",
                    "description:\n  text: invalid",
                    1,
                )
            },
            "description must be a YAML string",
        );
        reject_skill_frontmatter_variant(
            |skill| skill.replacen("user-invocable: true", "user-invocable: \"true\"", 1),
            "user-invocable must be a YAML boolean",
        );
    }

    #[test]
    fn typed_frontmatter_rejects_duplicate_nested_keys_and_unknown_fields() {
        reject_skill_frontmatter_variant(
            |skill| {
                skill.replacen(
                    "description: >-\n  Use the example skill.",
                    "description:\n  text: one\n  text: two",
                    1,
                )
            },
            "duplicate key",
        );
        reject_skill_frontmatter_variant(
            |skill| {
                skill.replacen(
                    "user-invocable: true",
                    "user-invocable: true\nunknown-field: true",
                    1,
                )
            },
            "unsupported key",
        );
    }

    #[test]
    fn optional_frontmatter_policy_values_are_target_owned() {
        let rewrites: [fn(String) -> String; 2] = [
            |skill: String| skill.replacen("license: Apache-2.0", "license: MIT", 1),
            |skill: String| skill.replacen("user-invocable: true", "user-invocable: false", 1),
        ];
        for rewrite in rewrites {
            let fixture = Fixture::new();
            let skill = must(
                fs::read_to_string(fixture.root.join("skills/example/SKILL.md")),
                "read skill fixture",
            );
            fixture.write("skills/example/SKILL.md", &rewrite(skill));
            let (_, shape) = fixture
                .run_detect()
                .unwrap_or_else(|error| panic!("optional policy field must be accepted: {error}"));
            assert_eq!(shape.units.len(), 1);
        }
    }

    #[test]
    fn minimal_provider_valid_frontmatter_is_accepted() {
        let fixture = Fixture::new();
        let skill = must(
            fs::read_to_string(fixture.root.join("skills/example/SKILL.md")),
            "read skill fixture",
        );
        let skill = skill
            .lines()
            .filter(|line| {
                !line.starts_with("argument-hint:")
                    && !line.starts_with("license:")
                    && !line.starts_with("user-invocable:")
            })
            .collect::<Vec<_>>()
            .join("\n");
        fixture.write("skills/example/SKILL.md", &skill);
        let (_, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("minimal frontmatter must be accepted: {error}"));
        assert_eq!(shape.units.len(), 1);
    }

    #[test]
    fn typed_frontmatter_rejects_semantic_invalid_values() {
        reject_skill_frontmatter_variant(
            |skill| {
                skill.replacen(
                    "description: >-\n  Use the example skill.",
                    "description: \"\"",
                    1,
                )
            },
            "description must not be empty",
        );
        reject_skill_frontmatter_variant(
            |skill| {
                skill.replacen(
                    "user-invocable: true",
                    "user-invocable: true\ndisable-model-invocation: maybe",
                    1,
                )
            },
            "disable-model-invocation must be a YAML boolean",
        );
    }

    #[test]
    fn typed_frontmatter_rejects_aliases_and_tags() {
        reject_skill_frontmatter_variant(
            |skill| {
                skill
                    .replacen(
                        "description: >-\n  Use the example skill.",
                        "description: &description Use the example skill.",
                        1,
                    )
                    .replacen(
                        "argument-hint: \"<path>\"",
                        "argument-hint: *description",
                        1,
                    )
            },
            "invalid YAML frontmatter",
        );
        reject_skill_frontmatter_variant(
            |skill| skill.replacen("name: example", "name: !skill example", 1),
            "YAML tags or aliases",
        );
    }

    #[test]
    fn embedded_skill_template_is_validated_but_not_catalogued() {
        let fixture = Fixture::new();
        let (hidden, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("template fixture detects: {error}"));
        assert!(hidden.contains("skills/example/templates/skill/SKILL.md"));
        assert_eq!(shape.units.len(), 1);
        assert_eq!(shape.units[0].kind, UnitKind::Skills);
    }

    #[test]
    fn markdown_paths_stay_inside_repository() {
        assert_eq!(
            resolve_markdown_path("skills/example/SKILL.md", "references/policy.md"),
            Some("skills/example/references/policy.md".to_owned())
        );
        assert_eq!(
            resolve_markdown_path(
                "skills/example/references/policy.md",
                "../../../scripts/helper.ts"
            ),
            Some("scripts/helper.ts".to_owned())
        );
        assert_eq!(
            resolve_markdown_path("skills/example/SKILL.md", "../../../outside.md"),
            None
        );
    }

    #[test]
    fn markdown_destination_parser_handles_titles_nested_parentheses_and_queries() {
        let (target, consumed) = parse_markdown_destination("diagram_(v1).md \"title\") trailing")
            .expect("balanced Markdown destination parses");
        assert_eq!(target, "diagram_(v1).md");
        assert_eq!(consumed, "diagram_(v1).md \"title\")".len());
        assert_eq!(
            normalize_markdown_destination("foo%20bar.md?view=full#section"),
            Some("foo bar.md".to_owned())
        );
        assert!(normalize_markdown_destination("foo%2Fbar.md").is_none());
        assert!(normalize_markdown_destination("bad%2.md").is_none());
    }

    #[test]
    fn markdown_link_fixtures_resolve_and_skip_inline_code() {
        let fixture = Fixture::new();
        let (hidden, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("Markdown fixture detects: {error}"));
        assert!(hidden.contains("skills/example/templates/package.json"));
        assert_eq!(shape.units.len(), 1);
    }

    #[test]
    fn markdown_link_fixtures_skip_tilde_fenced_code() {
        let fixture = Fixture::new();
        fixture.write(
            "skills/example/references/policy.md",
            "~~~markdown\n[example](missing.md)\n~~~\n",
        );
        let (_, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("tilde-fenced Markdown must be ignored: {error}"));
        assert_eq!(shape.units.len(), 1);
    }

    #[test]
    fn provider_valid_plugin_without_generated_docs_is_accepted() {
        let mut fixture = Fixture::new();
        fixture
            .files
            .retain(|file| !file.starts_with("docs/") && file != "scripts/generate-docs.ts");
        let (_, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("docs-free plugin must detect: {error}"));
        assert_eq!(shape.units.len(), 1);
        assert!(shape.units[0]
            .pr_commands
            .iter()
            .all(|command| !command.contains("generate-docs.ts")));
    }

    #[test]
    fn provider_without_typescript_helpers_omits_helper_syntax_check() {
        let mut fixture = Fixture::new();
        fixture
            .files
            .retain(|file| !file.starts_with("docs/") && !file.starts_with("scripts/"));
        let (_, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("helper-free plugin must detect: {error}"));
        assert!(shape.units[0]
            .pr_commands
            .iter()
            .all(|command| !command.contains("bun build")));
    }

    #[test]
    fn markdown_fence_tracks_character_and_opening_length() {
        assert_eq!(markdown_fence("~~~rust"), Some((b'~', 3)));
        assert_eq!(markdown_fence("````rust"), Some((b'`', 4)));
        assert_eq!(markdown_fence("  prose"), None);
    }

    #[test]
    fn markdown_link_failures_are_not_bypassed() {
        for target in [
            "missing.md",
            "../../../outside.md",
            "/absolute.md",
            "//host/path",
            "..\\outside.md",
            "bad%2.md",
            "%2e%2e/%2foutside.md",
            "unknown-scheme:value",
            "topic<name>/README.md",
        ] {
            let fixture = Fixture::new();
            fixture.write(
                "skills/example/references/policy.md",
                &format!("[bad]({target})\n"),
            );
            let (error, shape) = fixture.run_detect_failure();
            assert!(shape.units.is_empty(), "unsafe link must emit no unit");
            assert!(error.to_string().contains("Markdown"), "{target}: {error}");
        }
    }

    #[test]
    fn valid_plugin_shape_hides_only_metadata_declared_templates() {
        let fixture = Fixture::new();
        let (hidden, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("valid skills fixture detects: {error}"));
        assert!(hidden.contains("skills/example/templates/package.json"));
        assert!(hidden.contains("skills/example/templates/Cargo.toml"));
        assert!(hidden.contains("skills/example/templates/config.yaml"));
        assert!(hidden.contains("skills/example/templates/skill/SKILL.md"));
        assert!(hidden.contains("scripts/template-helper/templates/Cargo.toml"));
        assert!(!hidden.contains("skills/example/helpers/package.json"));
        assert_eq!(shape.units.len(), 1);
        assert_eq!(shape.units[0].kind, UnitKind::Skills);
        assert_eq!(shape.units[0].tool_version.as_deref(), Some("1.2.3"));
        assert!(shape.units[0]
            .pr_commands
            .iter()
            .any(|command| command.contains("generate-docs.ts")));
        assert!(shape.units[0].pr_commands.iter().any(|command| {
            command.contains("find scripts")
                && command.contains("bun build")
                && command.contains("test \"$(bun --version)\" = \"1.2.3\"")
                && command.contains("-not -path '*/templates/*'")
        }));
    }

    #[test]
    fn full_scan_keeps_real_helper_package_and_drops_template_units() {
        let fixture = Fixture::new();
        let shape = fixture
            .run_scan()
            .unwrap_or_else(|error| panic!("scan skills fixture: {error}"));
        assert!(shape.units.iter().any(|unit| unit.kind == UnitKind::Skills));
        assert!(shape
            .units
            .iter()
            .any(|unit| { unit.kind == UnitKind::Bun && unit.root == "skills/example/helpers" }));
        assert!(shape
            .units
            .iter()
            .any(|unit| { unit.kind == UnitKind::Bun && unit.root == "scripts/template-helper" }));
        assert!(!shape.units.iter().any(|unit| {
            unit.root.contains("/templates") || unit.root.starts_with("skills/example/templates")
        }));
    }

    #[test]
    fn malformed_catalog_duplicate_key_is_rejected() {
        let fixture = Fixture::new();
        fixture.write(
            "catalog.json",
            r#"{"skills":["example"],"skills":["example"]}"#,
        );
        let error = fixture
            .run_detect()
            .expect_err("duplicate catalog key must fail");
        assert!(error.to_string().contains("duplicate JSON object key"));
    }

    #[test]
    fn provider_versions_use_strict_semver() {
        for version in [
            "0.28.0",
            "1.2.3-alpha.1",
            "1.2.3+build.7",
            "1.2.3-alpha+build.7",
        ] {
            assert!(
                validate_provider_version(version, "provider.json").is_ok(),
                "{version} should be valid SemVer"
            );
        }
        for version in [
            "1.0",
            "01.0.0",
            "1.0.0-",
            "1.0.0-alpha.01",
            "1.0.0+",
            "1.0.0+.",
            "1.0.0+build+extra",
        ] {
            assert!(
                validate_provider_version(version, "provider.json").is_err(),
                "{version} should be invalid SemVer"
            );
        }
    }

    #[test]
    fn duplicate_frontmatter_key_is_rejected() {
        let fixture = Fixture::new();
        let skill = must(
            fs::read_to_string(fixture.root.join("skills/example/SKILL.md")),
            "read skill fixture",
        );
        fixture.write(
            "skills/example/SKILL.md",
            &skill.replacen("name: example", "name: example\nname: duplicate", 1),
        );
        let error = fixture
            .run_detect()
            .expect_err("duplicate frontmatter key must fail");
        assert!(error.to_string().contains("duplicate key"));
    }

    #[test]
    fn missing_reference_is_rejected() {
        let fixture = Fixture::new();
        fixture.write(
            "skills/example/references/policy.md",
            "[missing](missing.md)\n",
        );
        let error = fixture
            .run_detect()
            .expect_err("missing Markdown reference must fail");
        assert!(error.to_string().contains("missing Markdown link"));
    }

    #[test]
    fn placeholder_links_require_an_explicit_component_marker() {
        assert!(is_markdown_placeholder("research/<topic>/README.md"));
        assert!(is_markdown_placeholder("<topic>"));
        assert!(!is_markdown_placeholder("pure-rust-macos-ui/README.md"));
        assert!(!is_markdown_placeholder("reference/topic<name>/README.md"));
        assert!(!is_markdown_placeholder("research/<topic.name>/README.md"));
        assert!(!is_markdown_placeholder("research/<1topic>/README.md"));
    }

    #[test]
    fn invalid_template_is_rejected() {
        let fixture = Fixture::new();
        fixture.write("skills/example/templates/package.json", "{\n");
        let error = fixture
            .run_detect()
            .expect_err("invalid template must fail");
        assert!(error.to_string().contains("invalid JSON template"));
    }

    #[test]
    fn plugin_marker_without_catalog_is_not_a_noop() {
        let root = std::env::temp_dir().join(format!(
            "velnor-skills-marker-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let file = ".codex-plugin/plugin.json".to_owned();
        must(
            fs::create_dir_all(root.join(".codex-plugin")),
            "create marker fixture",
        );
        must(
            fs::write(root.join(&file), r#"{"name":"broken"}"#),
            "write marker fixture",
        );
        let files = vec![file.clone()];
        let file_set = BTreeSet::from([file]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let mut shape = empty_shape();
        let error = detect(&context, &mut shape)
            .expect_err("plugin marker without catalog must fail closed");
        assert!(error.to_string().contains("catalog.json"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn catalog_skill_marker_without_provider_manifests_fails_closed() {
        let fixture = Fixture::new();
        let files = fixture
            .files
            .iter()
            .filter(|file| {
                !file.starts_with(".codex-plugin/")
                    && !file.starts_with(".kimi-plugin/")
                    && !file.starts_with(".claude-plugin/")
            })
            .cloned()
            .collect::<Vec<_>>();
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let context = ScanContext {
            root: &fixture.root,
            files: &files,
            file_set: &file_set,
        };
        let mut shape = empty_shape();
        let error = detect(&context, &mut shape)
            .expect_err("catalog plus direct skill marker must not silently no-op");
        assert!(error
            .to_string()
            .contains("at least one provider or marketplace manifest"));
    }

    #[test]
    fn provider_projection_validates_only_manifests_present() {
        let fixture = Fixture::new();
        let files = fixture
            .files
            .iter()
            .filter(|file| {
                !file.starts_with(".kimi-plugin/") && !file.starts_with(".claude-plugin/")
            })
            .cloned()
            .collect::<Vec<_>>();
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let context = ScanContext {
            root: &fixture.root,
            files: &files,
            file_set: &file_set,
        };
        let mut shape = empty_shape();
        let hidden = detect(&context, &mut shape)
            .unwrap_or_else(|error| panic!("Codex-only provider detects: {error}"));
        assert!(hidden.contains("skills/example/templates/package.json"));
        assert_eq!(shape.units.len(), 1);
        assert!(shape.detected.contains(&"skills-bun:1.2.3".to_owned()));
    }

    #[test]
    fn catalogued_template_directory_is_hidden_without_markdown_reference() {
        let fixture = Fixture::new();
        let skill = must(
            fs::read_to_string(fixture.root.join("skills/example/SKILL.md")),
            "read skill fixture",
        );
        fixture.write(
            "skills/example/SKILL.md",
            &skill.replace("[templates](templates/), ", ""),
        );
        let (hidden, shape) = fixture
            .run_detect()
            .unwrap_or_else(|error| panic!("unlinked template directory detects: {error}"));
        assert!(hidden.contains("skills/example/templates/package.json"));
        assert_eq!(shape.units.len(), 1);
    }

    #[test]
    fn target_bun_version_conflicts_fail_closed() {
        let fixture = Fixture::new();
        fixture.write(
            "scripts/template-helper/package.json",
            r#"{"name":"template-helper","packageManager":"bun@1.3.0","scripts":{"check":"bun test"}}"#,
        );
        let (error, shape) = fixture.run_detect_failure();
        assert!(shape.units.is_empty());
        assert!(error.to_string().contains("conflicting Bun versions"));
    }

    #[test]
    fn ordinary_rust_and_bun_packages_keep_their_language_units() {
        let rust = std::env::temp_dir().join(format!(
            "velnor-skills-rust-control-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        must(fs::create_dir_all(rust.join("src")), "create Rust control");
        must(
            fs::write(
                rust.join("Cargo.toml"),
                "[package]\nname = \"control\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            "write Rust control",
        );
        must(
            fs::write(
                rust.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"stable\"\n",
            ),
            "write Rust toolchain",
        );
        must(
            fs::write(rust.join("src/lib.rs"), "pub fn control() {}\n"),
            "write Rust source",
        );
        let rust_shape = super::super::scan_shape(
            &rust,
            &ProviderSet::from([ProviderId::GithubHosted]),
            "main",
            &[],
        )
        .unwrap_or_else(|error| panic!("scan ordinary Rust package: {error}"));
        assert!(rust_shape
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Rust));
        assert!(!rust_shape
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Skills));

        let bun = std::env::temp_dir().join(format!(
            "velnor-skills-bun-control-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        must(fs::create_dir_all(&bun), "create Bun control");
        must(
            fs::write(
                bun.join("package.json"),
                r#"{"name":"control","packageManager":"bun@1.2.3","scripts":{"test":"bun test"}}"#,
            ),
            "write Bun control",
        );
        must(fs::write(bun.join("bun.lock"), "{}\n"), "write Bun lock");
        let bun_shape = super::super::scan_shape(
            &bun,
            &ProviderSet::from([ProviderId::GithubHosted]),
            "main",
            &[],
        )
        .unwrap_or_else(|error| panic!("scan ordinary Bun package: {error}"));
        assert!(bun_shape
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Bun));
        assert!(!bun_shape
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::Skills));
        let _ = fs::remove_dir_all(rust);
        let _ = fs::remove_dir_all(bun);
    }
}

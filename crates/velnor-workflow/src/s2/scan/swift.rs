//! Swift detector: Swift packages, Xcode shared schemes, and `XcodeGen` specs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_yaml::{Mapping, Value};

use super::file_walk::{
    files_named, join_repo_path, path_prefix, resolve_repo_path, roots_for_manifests,
};
use super::{unit, RepositoryShape, ScanContext};
use crate::s2::{
    identifier_suffix, parent_path, shell_change_dir, shell_quote, CachePurpose, CacheSpec, Unit,
    UnitKind,
};

/// Toolchain pin files every Swift cache key hashes, mirroring how Rust keys
/// hash `rust-toolchain.toml`: a Swift/Xcode toolchain change must
/// invalidate dependency and intermediate state, never reuse it. `hashFiles`
/// ignores patterns that match nothing, so repos without these files keep
/// their existing keys; the Swift tools version is deliberately absent — it
/// is a minimum-version floor, not a toolchain identity.
const APPLE_TOOLCHAIN_PIN_KEY_FILES: [&str; 3] = ["mise.lock", ".swift-version", ".xcode-version"];

/// One `.binaryTarget` stanza: a local `path` artifact or a remote `url`
/// artifact. `None` means the stanza had no literal value for that key — a
/// variable or computed expression the static scan cannot resolve.
pub(crate) struct BinaryTarget {
    pub(crate) name: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) url: Option<String>,
}

/// Static `Package.swift` facts. The manifest is executable Swift; the scan
/// only reads its text, never evaluates it.
pub(crate) struct PackageFacts {
    pub(crate) tools_version: Option<String>,
    pub(crate) has_tests: bool,
    pub(crate) binary_targets: Vec<BinaryTarget>,
}

/// One local-path binary target awaiting the native-producer join: the
/// detector records it, and `super::join_native_producers` matches it
/// against `BoltFFI` producers after every detector has run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SwiftBinaryConsumer {
    pub(crate) manifest: String,
    pub(crate) package_root: String,
    pub(crate) unit: String,
    pub(crate) name: Option<String>,
    pub(crate) path: String,
}

impl Default for PackageFacts {
    /// An unreadable manifest keeps the historical contract: build plus test.
    /// Coverage narrows only on positive manifest evidence, never on a
    /// missing file.
    fn default() -> Self {
        PackageFacts {
            tools_version: None,
            has_tests: true,
            binary_targets: Vec::new(),
        }
    }
}

fn parse_tools_version(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let Some(value) = line.trim_start().strip_prefix("// swift-tools-version:") else {
            continue;
        };
        let token = value.split_whitespace().next().unwrap_or_default();
        if !token.is_empty()
            && token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
            && token.bytes().any(|byte| byte.is_ascii_digit())
        {
            return Some(token.to_owned());
        }
    }
    None
}

/// Whether `contents` calls `call` (`".testTarget"`) with only whitespace
/// between the name and the argument list.
fn call_present(contents: &str, call: &str) -> bool {
    let mut rest = contents;
    while let Some(found) = rest.find(call) {
        rest = &rest[found + call.len()..];
        if rest
            .trim_start_matches([' ', '\t', '\n', '\r'])
            .starts_with('(')
        {
            return true;
        }
    }
    false
}

/// The index just past the string literal opening at `bytes[start]` (`"` or
/// `"""`), or `None` when it never closes.
fn skip_string(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start + 1) == Some(&b'"') && bytes.get(start + 2) == Some(&b'"') {
        let mut index = start + 3;
        while index + 3 <= bytes.len() {
            if bytes[index] == b'"' && bytes[index + 1] == b'"' && bytes[index + 2] == b'"' {
                return Some(index + 3);
            }
            index += 1;
        }
        return None;
    }
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 2;
            }
            b'"' => {
                return Some(index + 1);
            }
            _ => {
                index += 1;
            }
        }
    }
    None
}

/// The index just past the block comment opening at `bytes[start]` (`/*`,
/// nestable in Swift), or `None` when it never closes.
fn skip_block_comment(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 2;
    let mut depth = 1;
    while index + 2 <= bytes.len() {
        if bytes[index] == b'/' && bytes[index + 1] == b'*' {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return Some(index);
            }
        } else {
            index += 1;
        }
    }
    None
}

/// Split `(...)` off the front of `text`, which must start with `(`: the
/// group's inner text plus the remainder. Strings and Swift comments do not
/// count toward nesting. Returns `None` when the group never closes. Every
/// cursor step lands on a UTF-8 boundary, so the returned slices are safe.
fn balanced_group(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    let mut index = 1;
    let mut depth = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return Some((&text[1..index - 1], &text[index..]));
                }
            }
            b'"' => {
                index = skip_string(bytes, index)?;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index)?;
            }
            _ => {
                let width = text[index..].chars().next().map_or(1, char::len_utf8);
                index += width;
            }
        }
    }
    None
}

/// The string content up to the closing unescaped `"`, or `None` when the
/// literal never closes. `literal` starts just after the opening quote.
fn read_quoted(literal: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = literal.chars();
    while let Some(char) = chars.next() {
        match char {
            '\\' => {
                let escaped = chars.next()?;
                out.push(match escaped {
                    'n' => '\n',
                    't' => '\t',
                    other => other,
                });
            }
            '"' => {
                return Some(out);
            }
            _ => {
                out.push(char);
            }
        }
    }
    None
}

/// The string literal passed as `key:` inside `group`, or `None` when the key
/// is absent, non-literal, or multiline.
fn string_arg(group: &str, key: &str) -> Option<String> {
    let mut rest = group;
    while let Some(found) = rest.find(key) {
        let before = rest[..found].chars().next_back();
        rest = &rest[found + key.len()..];
        if before.is_some_and(|char| char.is_alphanumeric() || char == '_') {
            continue;
        }
        let Some(value) = rest
            .trim_start_matches([' ', '\t', '\n', '\r'])
            .strip_prefix(':')
        else {
            continue;
        };
        let value = value.trim_start_matches([' ', '\t', '\n', '\r']);
        let Some(literal) = value.strip_prefix('"') else {
            continue;
        };
        if literal.starts_with("\"\"") {
            return None;
        }
        return read_quoted(literal);
    }
    None
}

fn parse_binary_targets(contents: &str) -> Vec<BinaryTarget> {
    let mut targets = Vec::new();
    let mut rest = contents;
    while let Some(found) = rest.find(".binaryTarget") {
        rest = &rest[found + ".binaryTarget".len()..];
        let group = rest.trim_start_matches([' ', '\t', '\n', '\r']);
        if !group.starts_with('(') {
            continue;
        }
        let Some((inner, after)) = balanced_group(group) else {
            break;
        };
        targets.push(BinaryTarget {
            name: string_arg(inner, "name"),
            path: string_arg(inner, "path"),
            url: string_arg(inner, "url"),
        });
        rest = after;
    }
    targets
}

fn parse_package_facts(contents: &str) -> PackageFacts {
    PackageFacts {
        tools_version: parse_tools_version(contents),
        has_tests: call_present(contents, ".testTarget"),
        binary_targets: parse_binary_targets(contents),
    }
}

/// One `XcodeGen` target: only the facts discovery needs. `target_type` and
/// `platform` stay optional because partial (included) documents may carry
/// fragments; the generator itself rejects an invalid final spec loudly.
struct XcodeGenTarget {
    name: String,
    target_type: Option<String>,
    platform: Option<String>,
}

/// One declared `XcodeGen` scheme: which targets its build and test actions
/// cover.
struct XcodeGenScheme {
    name: String,
    build_targets: Vec<String>,
    test_targets: Vec<String>,
}

/// A recognized `XcodeGen` specification plus its include closure.
struct XcodeGenSpec {
    path: String,
    name: String,
    minimum_version: Option<String>,
    files: Vec<String>,
    targets: Vec<XcodeGenTarget>,
    schemes: Vec<XcodeGenScheme>,
}

/// Bounds for include following: deep or broad chains report a limitation
/// instead of consuming the scan.
const XCODEGEN_INCLUDE_DEPTH_LIMIT: usize = 32;
const XCODEGEN_INCLUDE_FILE_LIMIT: usize = 128;

fn parse_yaml_mapping(contents: &str) -> Option<Mapping> {
    let config = serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
    let value: Value = serde_yaml::from_str_with_config(contents, &config).ok()?;
    value.as_mapping().cloned()
}

fn mapping_value<'a>(mapping: &'a Mapping, name: &str) -> Option<&'a Value> {
    mapping
        .iter()
        .find_map(|(key, value)| (key.as_str() == name).then_some(value))
}

fn mapping_text(mapping: &Mapping, name: &str) -> Option<String> {
    let text = mapping_value(mapping, name)?.as_str()?;
    (!text.is_empty()).then(|| text.to_owned())
}

/// Include entries: one path or a list of paths. Anything else is a
/// limitation, never a silent skip.
fn include_entries(mapping: &Mapping) -> Result<Vec<String>, &'static str> {
    let Some(value) = mapping_value(mapping, "include") else {
        return Ok(Vec::new());
    };
    match value {
        Value::String(path) => Ok(vec![path.clone()]),
        Value::Sequence(entries) => entries
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned)
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("include entries must be paths"),
        _ => Err("`include` must be a path or a list of paths"),
    }
}

struct SpecClosure<'a> {
    read: &'a dyn Fn(&str) -> Option<String>,
    ordered: Vec<(String, Mapping)>,
    limitations: Vec<String>,
    visiting: Vec<String>,
    visited: BTreeSet<String>,
}

fn visit_spec_file(path: &str, closure: &mut SpecClosure<'_>) {
    if closure.visited.contains(path) {
        return;
    }
    if closure.visiting.iter().any(|seen| seen == path) {
        let mut chain = closure.visiting.clone();
        chain.push(path.to_owned());
        closure
            .limitations
            .push(format!("XcodeGen include cycle: {}.", chain.join(" -> ")));
        return;
    }
    if closure.visiting.len() >= XCODEGEN_INCLUDE_DEPTH_LIMIT {
        closure.limitations.push(format!(
            "XcodeGen include chain at `{path}` exceeds {XCODEGEN_INCLUDE_DEPTH_LIMIT} levels; remaining includes are not followed."
        ));
        return;
    }
    if closure.visited.len() >= XCODEGEN_INCLUDE_FILE_LIMIT {
        closure.limitations.push(format!(
            "XcodeGen include closure exceeds {XCODEGEN_INCLUDE_FILE_LIMIT} files at `{path}`; remaining includes are not followed."
        ));
        return;
    }
    let Some(contents) = (closure.read)(path) else {
        closure.limitations.push(format!(
            "XcodeGen include `{path}` matches no tracked file."
        ));
        return;
    };
    let Some(mapping) = parse_yaml_mapping(&contents) else {
        closure
            .limitations
            .push(format!("XcodeGen include `{path}` is not a YAML mapping."));
        return;
    };
    closure.visiting.push(path.to_owned());
    match include_entries(&mapping) {
        Ok(entries) => {
            let parent = parent_path(path);
            for entry in entries {
                match resolve_repo_path(&parent, &entry) {
                    Some(resolved) => visit_spec_file(&resolved, closure),
                    None => closure.limitations.push(format!(
                        "XcodeGen include `{entry}` in `{path}` escapes the repository."
                    )),
                }
            }
        }
        Err(problem) => closure.limitations.push(format!(
            "XcodeGen include in `{path}` is unusable: {problem}."
        )),
    }
    closure.visiting.pop();
    closure.visited.insert(path.to_owned());
    closure.ordered.push((path.to_owned(), mapping));
}

/// Load the include closure in merge order: dependencies first, the entry
/// last so it wins. The scan only reads these files, never executes them.
fn load_spec_closure(
    entry: &str,
    read: &dyn Fn(&str) -> Option<String>,
) -> (Vec<(String, Mapping)>, Vec<String>) {
    let mut closure = SpecClosure {
        read,
        ordered: Vec::new(),
        limitations: Vec::new(),
        visiting: Vec::new(),
        visited: BTreeSet::new(),
    };
    visit_spec_file(entry, &mut closure);
    (closure.ordered, closure.limitations)
}

/// Whether the entry document is an `XcodeGen` spec: a `name` plus a
/// non-empty `targets` map. Recognition is structural; the filename alone
/// proves nothing.
fn is_xcodegen_spec(mapping: &Mapping) -> bool {
    mapping_text(mapping, "name").is_some()
        && mapping_value(mapping, "targets")
            .and_then(Value::as_mapping)
            .is_some_and(|targets| !targets.is_empty())
}

fn scheme_build_targets(detail: &Mapping) -> Vec<String> {
    mapping_value(detail, "build")
        .and_then(Value::as_mapping)
        .and_then(|build| mapping_value(build, "targets"))
        .and_then(Value::as_mapping)
        .map(|targets| {
            targets
                .keys()
                .map(String::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn scheme_test_targets(detail: &Mapping) -> Vec<String> {
    mapping_value(detail, "test")
        .and_then(Value::as_mapping)
        .and_then(|test| mapping_value(test, "targets"))
        .and_then(Value::as_sequence)
        .map(|targets| {
            targets
                .iter()
                .filter_map(|target| match target {
                    Value::String(name) => (!name.is_empty()).then(|| name.clone()),
                    Value::Mapping(detail) => mapping_text(detail, "name"),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn spec_minimum_version(mapping: &Mapping) -> Option<String> {
    if let Some(version) = mapping_text(mapping, "minimumXcodeGenVersion") {
        return Some(version);
    }
    let options = mapping_value(mapping, "options").and_then(Value::as_mapping)?;
    mapping_text(options, "minimumXcodeGenVersion")
}

/// Merge the closure into one spec. Later documents win, so the entry file
/// overrides its includes. Targets and schemes sort by name for a
/// deterministic shape.
fn merge_spec(entry: &str, ordered: &[(String, Mapping)]) -> Option<XcodeGenSpec> {
    let mut name: Option<String> = None;
    let mut minimum_version: Option<String> = None;
    let mut targets: BTreeMap<String, (Option<String>, Option<String>)> = BTreeMap::new();
    let mut schemes: BTreeMap<String, XcodeGenScheme> = BTreeMap::new();
    let mut files = Vec::new();
    for (path, mapping) in ordered {
        files.push(path.clone());
        if let Some(value) = mapping_text(mapping, "name") {
            name = Some(value);
        }
        if let Some(value) = spec_minimum_version(mapping) {
            minimum_version = Some(value);
        }
        if let Some(map) = mapping_value(mapping, "targets").and_then(Value::as_mapping) {
            for (key, value) in map {
                let target = key.as_str();
                let Some(detail) = value.as_mapping() else {
                    continue;
                };
                if target.is_empty() {
                    continue;
                }
                targets.insert(
                    target.to_owned(),
                    (
                        mapping_text(detail, "type"),
                        mapping_text(detail, "platform"),
                    ),
                );
            }
        }
        if let Some(map) = mapping_value(mapping, "schemes").and_then(Value::as_mapping) {
            for (key, value) in map {
                let scheme = key.as_str();
                let Some(detail) = value.as_mapping() else {
                    continue;
                };
                if scheme.is_empty() {
                    continue;
                }
                schemes.insert(
                    scheme.to_owned(),
                    XcodeGenScheme {
                        name: scheme.to_owned(),
                        build_targets: scheme_build_targets(detail),
                        test_targets: scheme_test_targets(detail),
                    },
                );
            }
        }
    }
    files.sort();
    files.dedup();
    Some(XcodeGenSpec {
        path: entry.to_owned(),
        name: name?,
        minimum_version,
        files,
        targets: targets
            .into_iter()
            .map(|(name, (target_type, platform))| XcodeGenTarget {
                name,
                target_type,
                platform,
            })
            .collect(),
        schemes: schemes.into_values().collect(),
    })
}

/// Build and test destinations mirror the committed-project units: macOS
/// builds on the host, iOS builds against the generic simulator. Anything
/// else stays generate-only with a limitation.
fn xcodegen_destinations(platform: Option<&str>) -> Option<(&'static str, &'static str)> {
    match platform {
        Some("macOS") => Some(("", "")),
        Some("iOS") => Some((
            " -destination 'generic/platform=iOS Simulator'",
            " -destination 'platform=iOS Simulator'",
        )),
        _ => None,
    }
}

/// How the app builds: a chosen scheme plus whether its test action runs,
/// or generate-only when no scheme can be selected honestly.
enum SchemePick {
    Build { scheme: String, testable: bool },
    GenerateOnly,
}

/// Select the scheme that builds `app`: a declared scheme merged over the
/// app's own name wins, else a lone candidate, else the generated
/// per-target scheme without tests. Several declared candidates stay
/// generate-only with a note naming them.
fn select_app_scheme(spec: &XcodeGenSpec, app: &XcodeGenTarget) -> (SchemePick, Option<String>) {
    let candidates = spec
        .schemes
        .iter()
        .filter(|scheme| {
            scheme
                .build_targets
                .iter()
                .any(|target| target == &app.name)
        })
        .collect::<Vec<_>>();
    let chosen = candidates
        .iter()
        .find(|scheme| scheme.name == app.name)
        .copied()
        .or_else(|| {
            if candidates.len() == 1 {
                Some(candidates[0])
            } else {
                None
            }
        });
    match (chosen, candidates.len()) {
        (Some(scheme), _) => (
            SchemePick::Build {
                scheme: scheme.name.clone(),
                testable: !scheme.test_targets.is_empty(),
            },
            None,
        ),
        (None, 0) => (
            SchemePick::Build {
                scheme: app.name.clone(),
                testable: false,
            },
            Some(format!(
                "No declared scheme builds `{}` in `{}`; using the generated per-target scheme while test execution waits on runtime refinement.",
                app.name, spec.path
            )),
        ),
        (None, _) => {
            let names = candidates
                .iter()
                .map(|scheme| scheme.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            (
                SchemePick::GenerateOnly,
                Some(format!(
                    "Schemes {names} all build `{}` in `{}`; declare the intended scheme to enable the app build.",
                    app.name, spec.path
                )),
            )
        }
    }
}

/// Build the `XcodeGen` unit for one recognized spec: generate the project,
/// then build (and, for a declared testable scheme, test) the app.
/// Returns `None` when a committed generated project with shared schemes
/// already covers the spec, so the two surfaces never double-verify.
fn xcodegen_unit(spec: &XcodeGenSpec, files: &[String]) -> (Option<Unit>, Vec<String>) {
    let mut notes = Vec::new();
    let root = parent_path(&spec.path);
    let generated = join_repo_path(&root, &format!("{}.xcodeproj", spec.name));
    let generated_prefix = format!("{generated}/");
    let covered_by_committed_project = files.iter().any(|file| {
        file.starts_with(&generated_prefix)
            && file.contains("/xcshareddata/xcschemes/")
            && file.ends_with(".xcscheme")
    });
    if covered_by_committed_project {
        notes.push(format!(
            "Committed `{generated}` covers `{}`; building the committed project while spec drift stays unverified statically.",
            spec.path
        ));
        return (None, notes);
    }
    let apps = spec
        .targets
        .iter()
        .filter(|target| target.target_type.as_deref() == Some("application"))
        .collect::<Vec<_>>();
    let command_prefix = shell_change_dir(&root);
    let spec_file = spec.path.rsplit('/').next().unwrap_or(&spec.path);
    let mut commands = vec![format!(
        "{command_prefix}xcodegen generate --spec {}",
        shell_quote(spec_file)
    )];
    let mut label = format!("Apple project ({}, XcodeGen generate)", spec.name);
    let mut id_part = "generate".to_owned();
    if apps.len() == 1 {
        let app = apps[0];
        let Some((build_destination, test_destination)) =
            xcodegen_destinations(app.platform.as_deref())
        else {
            let platform = app.platform.as_deref().map_or_else(
                || "no declared platform".to_owned(),
                |platform| format!("`{platform}`"),
            );
            notes.push(format!(
                "App `{}` in `{}` targets {platform}; static discovery builds macOS and iOS apps only.",
                app.name, spec.path
            ));
            return (
                Some(xcodegen_generate_unit(
                    spec, &root, spec_file, commands, label, &id_part,
                )),
                notes,
            );
        };
        let (pick, note) = select_app_scheme(spec, app);
        notes.extend(note);
        if let SchemePick::Build { scheme, testable } = pick {
            let project = shell_quote(&format!("{}.xcodeproj", spec.name));
            let scheme_quoted = shell_quote(&scheme);
            commands.push(format!(
                "{command_prefix}xcodebuild -project {project} -scheme {scheme_quoted}{build_destination} CODE_SIGNING_ALLOWED=NO build"
            ));
            if testable {
                commands.push(format!(
                    "{command_prefix}xcodebuild -project {project} -scheme {scheme_quoted}{test_destination} CODE_SIGNING_ALLOWED=NO test"
                ));
            }
            label = format!("Apple app ({scheme}, XcodeGen)");
            id_part = identifier_suffix(&scheme);
            if id_part.is_empty() {
                "app".clone_into(&mut id_part);
            }
        }
    } else if apps.is_empty() {
        notes.push(format!(
            "`{}` declares no application target; verifying generation only.",
            spec.path
        ));
    } else {
        let names = apps
            .iter()
            .map(|app| app.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(format!(
            "`{}` declares application targets {names}; declare the intended scheme to enable the app build.",
            spec.path
        ));
    }
    (
        Some(xcodegen_generate_unit(
            spec, &root, spec_file, commands, label, &id_part,
        )),
        notes,
    )
}

fn xcodegen_generate_unit(
    spec: &XcodeGenSpec,
    root: &str,
    spec_file: &str,
    commands: Vec<String>,
    label: String,
    id_part: &str,
) -> Unit {
    let root_part = {
        let part = identifier_suffix(root);
        if part.is_empty() {
            "root".to_owned()
        } else {
            part
        }
    };
    let spec_part = identifier_suffix(spec_file);
    let mut cache_key_files = spec.files.clone();
    cache_key_files.extend(
        APPLE_TOOLCHAIN_PIN_KEY_FILES
            .iter()
            .map(std::string::ToString::to_string),
    );
    cache_key_files.sort();
    cache_key_files.dedup();
    let root_watch = if root == "." {
        "**".to_owned()
    } else {
        format!("{root}/**")
    };
    let mut watch = vec![root_watch, "*.xcconfig".to_owned()];
    watch.extend(spec.files.iter().cloned());
    watch.sort();
    watch.dedup();
    Unit {
        id: format!("swift-xcodegen-{root_part}-{spec_part}-{id_part}"),
        label,
        kind: UnitKind::Swift,
        root: root.to_owned(),
        watch,
        pr_commands: commands.clone(),
        full_commands: commands,
        depends_on: Vec::new(),
        pinned_lockfile: false,
        cache: Some(CacheSpec {
            key_files: cache_key_files,
            paths: vec!["~/Library/Developer/Xcode/DerivedData".to_owned()],
            purpose: CachePurpose::XcodeIntermediates,
            mbx_output_cache_justification: None,
            mutable_mount_seed: false,
        }),
        tool_version: spec.minimum_version.clone(),
        mise_tools: Vec::new(),
        toolchain: None,
        services: Vec::new(),
        trust: crate::s2::provider::TrustReq::UntrustedOk,
        platform: crate::s2::provider::Platform::MacosArm64,
        capabilities: crate::s2::provider::Capabilities {
            native_macos_arm64: true,
            ..crate::s2::provider::Capabilities::default()
        },
        workspace_check: false,
        products: Vec::new(),
        prerequisites: Vec::new(),
        docker_contexts: Vec::new(),
        env: std::collections::BTreeMap::new(),
        mbx: None,
        prepared_tools: Vec::new(),
    }
}

fn swift_package_unit(package_root: &str, facts: &PackageFacts) -> Unit {
    let prefix = path_prefix(package_root);
    let command_prefix = shell_change_dir(package_root);
    let mut commands = vec![format!("{command_prefix}swift build")];
    if facts.has_tests {
        commands.push(format!("{command_prefix}swift test --parallel"));
    }
    let mut cache_key_files = vec![
        join_repo_path(package_root, "Package.swift"),
        join_repo_path(package_root, "Package.resolved"),
        join_repo_path(package_root, ".swiftpm/Package.resolved"),
    ];
    cache_key_files.extend(
        APPLE_TOOLCHAIN_PIN_KEY_FILES
            .iter()
            .map(std::string::ToString::to_string),
    );
    cache_key_files.sort();
    cache_key_files.dedup();
    let mut result = unit(
        UnitKind::Swift,
        package_root,
        vec![
            join_repo_path(package_root, "Package.swift"),
            join_repo_path(package_root, "Package.resolved"),
            join_repo_path(package_root, ".swiftpm/Package.resolved"),
            format!("{prefix}Sources/**"),
            format!("{prefix}Tests/**"),
            format!("{prefix}**/*.swift"),
        ],
        commands,
        Some(CacheSpec {
            key_files: cache_key_files,
            paths: vec!["~/.swiftpm".to_owned()],
            purpose: CachePurpose::SwiftPmSources,
            mbx_output_cache_justification: None,
            mutable_mount_seed: false,
        }),
    );
    result.id = format!("swift-package-{}", identifier_suffix(package_root));
    result.label = if package_root == "." {
        "Swift package".to_owned()
    } else {
        format!("Swift package ({package_root})")
    };
    // A SwiftPM package keeps the portable contract: it verifies wherever
    // its toolchain provisions. Only Xcode scheme work below and local
    // binary-target consumers carry an Apple need.
    result.tool_version.clone_from(&facts.tools_version);
    result
}

fn xcode_scheme_referenced_container(contents: &str) -> Option<String> {
    let marker = "ReferencedContainer=\"container:";
    let start = contents.find(marker)? + marker.len();
    Some(contents[start..].split('\"').next()?.to_owned())
}

fn xcode_project_is_ios(contents: &str) -> bool {
    contents.contains("IPHONEOS_DEPLOYMENT_TARGET") || contents.contains("SDKROOT = iphoneos")
}

fn xcode_scheme_units(root: &Path, files: &[String]) -> Vec<Unit> {
    let mut candidates = Vec::new();
    for scheme in files
        .iter()
        .filter(|file| file.ends_with(".xcscheme") && file.contains("/xcshareddata/xcschemes/"))
    {
        let Some((container, scheme_name)) = scheme.split_once("/xcshareddata/xcschemes/") else {
            continue;
        };
        let scheme_name = scheme_name.trim_end_matches(".xcscheme");
        let extension = if container.ends_with(".xcworkspace") {
            "xcworkspace"
        } else if container.ends_with(".xcodeproj") {
            "xcodeproj"
        } else {
            continue;
        };
        candidates.push((scheme, container, scheme_name, extension));
    }
    // Same-named schemes in different containers share a base id; qualify
    // every member of a colliding group with its container so the ids stay
    // stable and meaningful instead of order-dependent `-2` suffixes.
    let mut base_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, _, scheme_name, extension) in &candidates {
        *base_counts
            .entry(format!(
                "swift-{extension}-{}",
                identifier_suffix(scheme_name)
            ))
            .or_default() += 1;
    }
    candidates
        .into_iter()
        .map(|(scheme, container, scheme_name, extension)| {
            let base = format!("swift-{extension}-{}", identifier_suffix(scheme_name));
            let qualified = base_counts.get(&base).is_some_and(|count| *count > 1);
            xcode_scheme_unit(
                root,
                files,
                scheme,
                container,
                scheme_name,
                extension,
                qualified,
            )
        })
        .collect()
}

fn xcode_scheme_unit(
    root: &Path,
    files: &[String],
    scheme: &str,
    container: &str,
    scheme_name: &str,
    extension: &str,
    qualified: bool,
) -> Unit {
    let container_root = parent_path(container);
    let flag = if extension == "xcworkspace" {
        "-workspace"
    } else {
        "-project"
    };
    let scheme_contents = fs::read_to_string(root.join(scheme)).unwrap_or_default();
    let referenced_project = if extension == "xcworkspace" {
        xcode_scheme_referenced_container(&scheme_contents)
            .and_then(|path| resolve_repo_path(&container_root, &path))
    } else {
        Some(container.to_owned())
    };
    let project_contents = referenced_project
        .as_deref()
        .and_then(|project| {
            files
                .iter()
                .find(|file| file.as_str() == format!("{project}/project.pbxproj"))
        })
        .and_then(|project| fs::read_to_string(root.join(project)).ok())
        .unwrap_or_default();
    let ios_destination = xcode_project_is_ios(&project_contents);
    let build_destination = if ios_destination {
        " -destination 'generic/platform=iOS Simulator'"
    } else {
        ""
    };
    let test_destination = if ios_destination {
        " -destination 'platform=iOS Simulator'"
    } else {
        ""
    };
    let command_prefix = shell_change_dir(&container_root);
    let container_name = container.rsplit('/').next().unwrap_or(container);
    let container_quoted = shell_quote(container_name);
    let scheme_quoted = shell_quote(scheme_name);
    let commands = vec![
            format!(
                "{command_prefix}xcodebuild {flag} {container_quoted} -scheme {scheme_quoted}{build_destination} CODE_SIGNING_ALLOWED=NO build"
            ),
            format!(
                "{command_prefix}xcodebuild {flag} {container_quoted} -scheme {scheme_quoted}{test_destination} CODE_SIGNING_ALLOWED=NO test"
            ),
        ];
    let mut cache_key_files = vec![scheme.to_owned()];
    if extension == "xcodeproj" {
        cache_key_files.push(format!("{container}/project.pbxproj"));
    } else {
        cache_key_files.push(format!("{container}/contents.xcworkspacedata"));
        if let Some(project) = referenced_project {
            cache_key_files.push(format!("{project}/project.pbxproj"));
        }
    }
    cache_key_files.extend(
        APPLE_TOOLCHAIN_PIN_KEY_FILES
            .iter()
            .map(std::string::ToString::to_string),
    );
    cache_key_files.sort();
    cache_key_files.dedup();
    let root_watch = if container_root == "." {
        "**".to_owned()
    } else {
        format!("{container_root}/**")
    };
    let (id, label) = if qualified {
        (
            format!(
                "swift-{extension}-{}-{}",
                identifier_suffix(container),
                identifier_suffix(scheme_name)
            ),
            format!("Apple scheme ({scheme_name} in {container})"),
        )
    } else {
        (
            format!("swift-{extension}-{}", identifier_suffix(scheme_name)),
            format!("Apple scheme ({scheme_name})"),
        )
    };
    let mut unit = Unit {
        id,
        label,
        kind: UnitKind::Swift,
        root: container_root.clone(),
        watch: vec![
            format!("{container}/**"),
            root_watch,
            "*.xcconfig".to_owned(),
        ],
        pr_commands: commands.clone(),
        full_commands: commands,
        depends_on: Vec::new(),
        pinned_lockfile: false,
        cache: Some(CacheSpec {
            key_files: cache_key_files,
            paths: vec!["~/Library/Developer/Xcode/DerivedData".to_owned()],
            purpose: CachePurpose::XcodeIntermediates,
            mbx_output_cache_justification: None,
            mutable_mount_seed: false,
        }),
        tool_version: None,
        mise_tools: Vec::new(),
        toolchain: None,
        services: Vec::new(),
        trust: crate::s2::provider::TrustReq::UntrustedOk,
        platform: crate::s2::provider::Platform::MacosArm64,
        capabilities: crate::s2::provider::Capabilities {
            native_macos_arm64: true,
            ..crate::s2::provider::Capabilities::default()
        },
        workspace_check: false,
        products: Vec::new(),
        prerequisites: Vec::new(),
        docker_contexts: Vec::new(),
        env: std::collections::BTreeMap::new(),
        mbx: None,
        prepared_tools: Vec::new(),
    };
    unit.watch.sort();
    unit.watch.dedup();
    unit
}

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    for package_root in roots_for_manifests(&files_named(context.files, "Package.swift")) {
        shape.detected.push(format!("swift-package:{package_root}"));
        let manifest = join_repo_path(&package_root, "Package.swift");
        let facts = fs::read_to_string(context.root.join(&manifest))
            .ok()
            .map(|contents| parse_package_facts(&contents))
            .unwrap_or_default();
        if !facts.has_tests {
            shape.limitations.push(format!(
                "Swift package {manifest} declares no test targets; emitting build-only commands."
            ));
        }
        let mut unit = swift_package_unit(&package_root, &facts);
        for target in &facts.binary_targets {
            let name = target.name.as_deref().unwrap_or("<unnamed>");
            match (&target.path, &target.url) {
                (Some(path), _) => {
                    unit.platform = crate::s2::provider::Platform::MacosArm64;
                    unit.capabilities.native_macos_arm64 = true;
                    shape.swift_consumers.push(SwiftBinaryConsumer {
                        manifest: manifest.clone(),
                        package_root: package_root.clone(),
                        unit: unit.id.clone(),
                        name: target.name.clone(),
                        path: path.clone(),
                    });
                }
                (None, Some(_)) => {
                    shape.limitations.push(format!(
                        "Swift package {manifest} consumes remote binary target `{name}`; artifact provenance resolves at build time, not statically."
                    ));
                }
                (None, None) => {
                    shape.limitations.push(format!(
                        "Swift package {manifest} declares binary target `{name}` without a literal path or url; its provenance cannot be classified."
                    ));
                }
            }
        }
        shape.units.push(unit);
    }
    let mut xcode_units = xcode_scheme_units(context.root, context.files);
    let has_xcode_schemes = !xcode_units.is_empty();
    if has_xcode_schemes {
        shape
            .detected
            .push(format!("xcode-shared-schemes:{}", xcode_units.len()));
        shape.units.append(&mut xcode_units);
    }
    let has_xcode_container = context.files.iter().any(|file| {
        file.ends_with(".xcodeproj/project.pbxproj")
            || file.ends_with(".xcworkspace/contents.xcworkspacedata")
    });
    if has_xcode_container && !shape.units.iter().any(|unit| unit.kind == UnitKind::Swift) {
        shape.limitations.push(
            "Xcode projects were found without shared schemes; no build or test command was guessed. Share a scheme or add an explicit catalog profile.".to_owned(),
        );
    }
    if has_xcode_schemes {
        shape.limitations.push(
            "Apple test destinations use platform defaults; review the generated simulator destination when a project requires a named device or OS version.".to_owned(),
        );
    }
    detect_xcodegen_specs(context, shape);
}

fn detect_xcodegen_specs(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    let mut specs = files_named(context.files, "project.yml");
    specs.extend(files_named(context.files, "project.yaml"));
    specs.sort();
    for spec in &specs {
        let contents = fs::read_to_string(context.root.join(spec)).unwrap_or_default();
        let Some(mapping) = parse_yaml_mapping(&contents) else {
            shape.limitations.push(format!(
                "Project spec at `{spec}` is not a YAML mapping; no app surface derived."
            ));
            continue;
        };
        if !is_xcodegen_spec(&mapping) {
            shape.limitations.push(format!(
                "Project spec at `{spec}` is not a recognized XcodeGen document; it needs `name` plus a `targets` map."
            ));
            continue;
        }
        let read = |path: &str| {
            context
                .file_set
                .contains(path)
                .then(|| fs::read_to_string(context.root.join(path)).ok())
                .flatten()
        };
        let (ordered, closure_notes) = load_spec_closure(spec, &read);
        shape.limitations.extend(closure_notes);
        let Some(merged) = merge_spec(spec, &ordered) else {
            shape.limitations.push(format!(
                "Project spec at `{spec}` lost its project name while merging includes."
            ));
            continue;
        };
        shape.detected.push(format!("xcodegen:{}", merged.path));
        let (unit, unit_notes) = xcodegen_unit(&merged, context.files);
        shape.limitations.extend(unit_notes);
        if let Some(unit) = unit {
            shape.units.push(unit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_xcodegen_spec, load_spec_closure, merge_spec, parse_package_facts, parse_yaml_mapping,
        xcodegen_unit, PackageFacts, XcodeGenSpec,
    };
    use std::collections::BTreeMap;

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_some<T>(option: Option<T>, context: &str) -> T {
        match option {
            Some(value) => value,
            None => panic!("{context}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[test]
    fn tools_version_accepts_dotted_digits_only() {
        let facts = parse_package_facts("// swift-tools-version: 6.2\n");
        assert_eq!(facts.tools_version.as_deref(), Some("6.2"));
        let missing = parse_package_facts("let package = Package()\n");
        assert_eq!(missing.tools_version, None);
        let malformed = parse_package_facts("// swift-tools-version: 6.x\n");
        assert_eq!(malformed.tools_version, None);
    }

    #[test]
    fn test_targets_count_with_any_gap_before_parens() {
        assert!(parse_package_facts(".testTarget(name: \"App\")").has_tests);
        assert!(parse_package_facts(".testTarget (name: \"App\")").has_tests);
        assert!(!parse_package_facts(".target(name: \"App\")").has_tests);
        assert!(!parse_package_facts("// see .testTarget docs").has_tests);
    }

    #[test]
    fn unreadable_manifests_keep_the_test_command() {
        assert!(PackageFacts::default().has_tests);
    }

    #[test]
    fn binary_target_parses_single_line_stanzas() {
        let facts = parse_package_facts(
            ".binaryTarget(name: \"BridgeFFI\", path: \"../target/Bridge.xcframework\")",
        );
        assert_eq!(facts.binary_targets.len(), 1);
        assert_eq!(facts.binary_targets[0].name.as_deref(), Some("BridgeFFI"));
        assert_eq!(
            facts.binary_targets[0].path.as_deref(),
            Some("../target/Bridge.xcframework")
        );
        assert_eq!(facts.binary_targets[0].url, None);
    }

    #[test]
    fn binary_target_parses_multiline_stanzas() {
        let facts = parse_package_facts(
            ".binaryTarget(\n    name: \"BridgeFFI\",\n    path: \"../target/Bridge.xcframework\"\n)",
        );
        assert_eq!(facts.binary_targets.len(), 1);
        assert_eq!(facts.binary_targets[0].name.as_deref(), Some("BridgeFFI"));
        assert_eq!(
            facts.binary_targets[0].path.as_deref(),
            Some("../target/Bridge.xcframework")
        );
    }

    #[test]
    fn binary_target_parses_remote_urls() {
        let facts = parse_package_facts(
            ".binaryTarget(name: \"Remote\", url: \"https://example.com/Remote.zip\", checksum: \"abc\")",
        );
        assert_eq!(facts.binary_targets.len(), 1);
        assert_eq!(facts.binary_targets[0].name.as_deref(), Some("Remote"));
        assert_eq!(facts.binary_targets[0].path, None);
        assert_eq!(
            facts.binary_targets[0].url.as_deref(),
            Some("https://example.com/Remote.zip")
        );
    }

    #[test]
    fn binary_target_without_literals_yields_no_paths() {
        let facts = parse_package_facts(".binaryTarget(name: computedName, path: computedPath)");
        assert_eq!(facts.binary_targets.len(), 1);
        assert_eq!(facts.binary_targets[0].name, None);
        assert_eq!(facts.binary_targets[0].path, None);
        assert_eq!(facts.binary_targets[0].url, None);
    }

    #[test]
    fn parens_inside_strings_and_comments_do_not_break_groups() {
        let facts = parse_package_facts(
            ".binaryTarget(name: \"Foo (Bar)\", /* (c) */ path: \"a/b.xcframework\" // (tail)\n)",
        );
        assert_eq!(facts.binary_targets.len(), 1);
        assert_eq!(facts.binary_targets[0].name.as_deref(), Some("Foo (Bar)"));
        assert_eq!(
            facts.binary_targets[0].path.as_deref(),
            Some("a/b.xcframework")
        );
    }

    #[test]
    fn unbalanced_stanzas_stop_without_panic() {
        let facts = parse_package_facts(".binaryTarget(name: \"Broken\", path: \"a/b");
        assert!(facts.binary_targets.is_empty());
    }

    #[test]
    fn multiline_string_paths_stay_unresolved() {
        let facts = parse_package_facts(".binaryTarget(name: \"M\", path: \"\"\"a/b\"\"\")");
        assert_eq!(facts.binary_targets.len(), 1);
        assert_eq!(facts.binary_targets[0].path, None);
    }

    fn load_merged(
        entry: &str,
        files: &BTreeMap<String, String>,
    ) -> (Option<XcodeGenSpec>, Vec<String>) {
        let read = |path: &str| files.get(path).cloned();
        let (ordered, notes) = load_spec_closure(entry, &read);
        (merge_spec(entry, &ordered), notes)
    }

    fn spec_files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(path, contents)| (path.to_string(), contents.to_string()))
            .collect()
    }

    const MINIMAL_APP: &str =
        "name: Widget\ntargets:\n  WidgetApp:\n    type: application\n    platform: macOS\n";

    #[test]
    fn minimal_spec_yields_generate_plus_build() {
        let files = spec_files(&[("app/project.yml", MINIMAL_APP)]);
        let (spec, notes) = load_merged("app/project.yml", &files);
        assert!(notes.is_empty(), "{notes:?}");
        let spec = must_some(spec, "spec merges");
        assert_eq!(spec.name, "Widget");
        let (unit, notes) = xcodegen_unit(&spec, &[]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("No declared scheme"), "{}", notes[0]);
        let unit = must_some(unit, "unit is emitted");
        assert_eq!(unit.id, "swift-xcodegen-app-project-yml-widgetapp");
        assert_eq!(unit.label, "Apple app (WidgetApp, XcodeGen)");
        assert_eq!(unit.pr_commands.len(), 2, "{:?}", unit.pr_commands);
        assert!(unit.pr_commands[0].contains("xcodegen generate --spec 'project.yml'"));
        assert!(unit.pr_commands[1].contains("xcodebuild -project 'Widget.xcodeproj'"));
        assert!(unit.pr_commands[1].contains("-scheme 'WidgetApp'"));
        assert!(unit.pr_commands[1].contains("CODE_SIGNING_ALLOWED=NO build"));
    }

    #[test]
    fn recognition_needs_name_and_targets() {
        for (name, contents) in [
            ("no targets", "name: Widget\n"),
            ("no name", "targets:\n  App:\n    type: application\n"),
            ("empty targets", "name: Widget\ntargets: {}\n"),
            ("scalar", "just a string\n"),
        ] {
            let mapping = parse_yaml_mapping(contents).unwrap_or_default();
            assert!(!is_xcodegen_spec(&mapping), "{name} must not classify");
        }
        let mapping = must_some(parse_yaml_mapping(MINIMAL_APP), "minimal spec parses");
        assert!(is_xcodegen_spec(&mapping));
    }

    #[test]
    fn duplicate_keys_fail_the_mapping() {
        assert!(parse_yaml_mapping("name: A\nname: B\ntargets: {}\n").is_none());
    }

    #[test]
    fn includes_merge_with_entry_winning() {
        let files = spec_files(&[
            (
                "app/project.yml",
                "name: Widget\ninclude: base.yml\ntargets:\n  WidgetApp:\n    type: application\n    platform: macOS\n",
            ),
            (
                "app/base.yml",
                "targets:\n  WidgetApp:\n    type: application\n    platform: iOS\n  WidgetLib:\n    type: framework\n    platform: macOS\n",
            ),
        ]);
        let (spec, notes) = load_merged("app/project.yml", &files);
        assert!(notes.is_empty(), "{notes:?}");
        let spec = must_some(spec, "spec merges");
        assert_eq!(spec.files, vec!["app/base.yml", "app/project.yml"]);
        let app = must_some(
            spec.targets
                .iter()
                .find(|target| target.name == "WidgetApp"),
            "app survives the merge",
        );
        assert_eq!(app.platform.as_deref(), Some("macOS"));
        assert!(
            spec.targets.iter().any(|target| target.name == "WidgetLib"),
            "included targets join the merge"
        );
    }

    #[test]
    fn include_cycle_reports_a_closed_chain() {
        let files = spec_files(&[
            (
                "a.yml",
                "name: Widget\ninclude: b.yml\ntargets:\n  App:\n    type: application\n",
            ),
            ("b.yml", "include: a.yml\n"),
        ]);
        let (spec, notes) = load_merged("a.yml", &files);
        assert!(spec.is_some(), "the cycle must not drop the spec");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("a.yml -> b.yml -> a.yml"), "{}", notes[0]);
    }

    #[test]
    fn escaping_include_is_rejected() {
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\ninclude: ../../escape.yml\ntargets:\n  App:\n    type: application\n",
        )]);
        let (_, notes) = load_merged("app/project.yml", &files);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("escapes the repository"), "{}", notes[0]);
    }

    #[test]
    fn missing_and_malformed_includes_are_reported() {
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\ninclude: [gone.yml]\ntargets:\n  App:\n    type: application\n",
        )]);
        let (_, notes) = load_merged("app/project.yml", &files);
        assert!(
            notes
                .iter()
                .any(|note| note.contains("matches no tracked file")),
            "{notes:?}"
        );
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\ninclude: 42\ntargets:\n  App:\n    type: application\n",
        )]);
        let (_, notes) = load_merged("app/project.yml", &files);
        assert!(
            notes.iter().any(|note| note.contains("unusable")),
            "{notes:?}"
        );
    }

    #[test]
    fn scheme_selection_prefers_the_app_named_scheme() {
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\ntargets:\n  WidgetApp:\n    type: application\n    platform: macOS\n  WidgetAppTests:\n    type: bundle.unit-test\n    platform: macOS\nschemes:\n  Other:\n    build:\n      targets:\n        WidgetApp: all\n  WidgetApp:\n    build:\n      targets:\n        WidgetApp: all\n    test:\n      targets:\n        - WidgetAppTests\n",
        )]);
        let (spec, _) = load_merged("app/project.yml", &files);
        let (unit, notes) = xcodegen_unit(&must_some(spec, "spec merges"), &[]);
        assert!(notes.is_empty(), "{notes:?}");
        let unit = must_some(unit, "unit is emitted");
        assert_eq!(unit.pr_commands.len(), 3, "{:?}", unit.pr_commands);
        assert!(unit.pr_commands[1].contains("-scheme 'WidgetApp'"));
        assert!(unit.pr_commands[2].contains("-scheme 'WidgetApp'"));
        assert!(unit.pr_commands[2].contains("CODE_SIGNING_ALLOWED=NO test"));
    }

    #[test]
    fn lone_scheme_candidate_is_picked() {
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\ntargets:\n  WidgetApp:\n    type: application\n    platform: macOS\nschemes:\n  Nightly:\n    build:\n      targets:\n        WidgetApp: all\n",
        )]);
        let (spec, _) = load_merged("app/project.yml", &files);
        let (unit, _) = xcodegen_unit(&must_some(spec, "spec merges"), &[]);
        let unit = must_some(unit, "unit is emitted");
        assert!(unit.pr_commands[1].contains("-scheme 'Nightly'"));
        assert_eq!(
            unit.pr_commands.len(),
            2,
            "a scheme without tests stays build-only"
        );
    }

    #[test]
    fn ambiguous_schemes_stay_generate_only() {
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\ntargets:\n  WidgetApp:\n    type: application\n    platform: macOS\nschemes:\n  Alpha:\n    build:\n      targets:\n        WidgetApp: all\n  Beta:\n    build:\n      targets:\n        WidgetApp: all\n",
        )]);
        let (spec, _) = load_merged("app/project.yml", &files);
        let (unit, notes) = xcodegen_unit(&must_some(spec, "spec merges"), &[]);
        assert!(
            notes
                .iter()
                .any(|note| note.contains("Alpha") && note.contains("Beta")),
            "{notes:?}"
        );
        assert_eq!(must_some(unit, "unit").pr_commands.len(), 1);
    }

    #[test]
    fn multiple_apps_stay_generate_only() {
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\ntargets:\n  One:\n    type: application\n    platform: macOS\n  Two:\n    type: application\n    platform: macOS\n",
        )]);
        let (spec, _) = load_merged("app/project.yml", &files);
        let (unit, notes) = xcodegen_unit(&must_some(spec, "spec merges"), &[]);
        assert!(
            notes
                .iter()
                .any(|note| note.contains("One") && note.contains("Two")),
            "{notes:?}"
        );
        let unit = must_some(unit, "generate-only unit is emitted");
        assert_eq!(unit.pr_commands.len(), 1);
        assert!(unit.id.ends_with("-generate"), "{}", unit.id);
    }

    #[test]
    fn unsupported_platform_stays_generate_only() {
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\ntargets:\n  WidgetApp:\n    type: application\n    platform: tvOS\n",
        )]);
        let (spec, _) = load_merged("app/project.yml", &files);
        let (unit, notes) = xcodegen_unit(&must_some(spec, "spec merges"), &[]);
        assert!(notes.iter().any(|note| note.contains("tvOS")), "{notes:?}");
        assert_eq!(must_some(unit, "unit").pr_commands.len(), 1);
    }

    #[test]
    fn committed_project_with_schemes_skips_the_unit() {
        let files = spec_files(&[("app/project.yml", MINIMAL_APP)]);
        let (spec, _) = load_merged("app/project.yml", &files);
        let tracked =
            vec!["app/Widget.xcodeproj/xcshareddata/xcschemes/Widget.xcscheme".to_owned()];
        let (unit, notes) = xcodegen_unit(&must_some(spec, "spec merges"), &tracked);
        assert!(unit.is_none());
        assert!(
            notes.iter().any(|note| note.contains("Committed")),
            "{notes:?}"
        );
    }

    #[test]
    fn committed_project_without_schemes_keeps_the_unit() {
        let files = spec_files(&[("app/project.yml", MINIMAL_APP)]);
        let (spec, _) = load_merged("app/project.yml", &files);
        let tracked = vec!["app/Widget.xcodeproj/project.pbxproj".to_owned()];
        let (unit, _) = xcodegen_unit(&must_some(spec, "spec merges"), &tracked);
        assert!(unit.is_some());
    }

    #[test]
    fn minimum_version_lands_on_the_unit() {
        let files = spec_files(&[(
            "app/project.yml",
            "name: Widget\noptions:\n  minimumXcodeGenVersion: 2.46.0\ntargets:\n  WidgetApp:\n    type: application\n    platform: macOS\n",
        )]);
        let (spec, _) = load_merged("app/project.yml", &files);
        let (unit, _) = xcodegen_unit(&must_some(spec, "spec merges"), &[]);
        assert_eq!(
            must_some(unit, "unit").tool_version.as_deref(),
            Some("2.46.0")
        );
    }

    const NATIVE_CARGO: &str = "[package]\nname = \"bridge-core-ffi\"\nversion = \"0.1.0\"\n";
    const NATIVE_TOOLCHAIN: &str = "[toolchain]\nchannel = \"1.90.0\"\n";
    const NATIVE_BOLTFFI: &str = "[package]\nname = \"bridge-core\"\ncrate = \"bridge-core-ffi\"\n\n\
         [targets.apple.xcframework]\nname = \"BridgeCore\"\noutput = \"../../target/xcframework\"\n";

    fn native_package(binary_target: &str) -> String {
        format!(
            "// swift-tools-version: 6.0\nimport PackageDescription\n\nlet package = Package(\n    name: \"Desktop\",\n    targets: [\n        {binary_target},\n        .target(name: \"Bridge\"),\n        .testTarget(name: \"BridgeTests\", dependencies: [\"Bridge\"]),\n    ]\n)\n"
        )
    }

    fn native_fixture(entries: &[(&str, &str)]) -> std::path::PathBuf {
        use std::fs;
        let root =
            std::env::temp_dir().join(format!("velnor-swift-native-{}", crate::unique_suffix()));
        for (path, contents) in entries {
            let target = root.join(path);
            if let Some(parent) = target.parent() {
                must_some(fs::create_dir_all(parent).ok(), "create fixture directory");
            }
            must_some(fs::write(&target, contents).ok(), "write fixture file");
        }
        root
    }

    fn scan_native(root: &std::path::Path) -> super::super::RepositoryShape {
        must_ok(
            super::super::scan_shape(
                root,
                &std::collections::BTreeSet::from([crate::s2::provider::ProviderId::Velnor]),
                "main",
                &[],
            ),
            "scan fixture",
        )
    }

    #[test]
    fn native_join_wires_product_edge_on_renamed_fixture() {
        let root = native_fixture(&[
            ("libs/bridge-ffi/boltffi.toml", NATIVE_BOLTFFI),
            ("libs/bridge-ffi/Cargo.toml", NATIVE_CARGO),
            ("rust-toolchain.toml", NATIVE_TOOLCHAIN),
            (
                "clients/desktop/Package.swift",
                &native_package(
                    ".binaryTarget(name: \"BridgeCoreFFI\", path: \"../../target/xcframework/BridgeCore.xcframework\")",
                ),
            ),
        ]);
        let shape = scan_native(&root);
        let producer = must_some(
            shape.units.iter().find(|unit| {
                unit.kind == crate::s2::UnitKind::Rust && unit.root == "libs/bridge-ffi"
            }),
            "rust producer unit",
        );
        assert_eq!(producer.products.len(), 1);
        assert_eq!(producer.products[0].name, "xcframework-bridgecore");
        assert_eq!(producer.products[0].task, None);
        assert!(producer.products[0].env.is_empty());
        assert_eq!(
            producer.products[0].outputs,
            vec!["target/xcframework/BridgeCore.xcframework".to_owned()]
        );
        let consumer = must_some(
            shape
                .units
                .iter()
                .find(|unit| unit.id == "swift-package-clients-desktop"),
            "swift consumer unit",
        );
        assert_eq!(consumer.prerequisites.len(), 1);
        assert_eq!(consumer.prerequisites[0].producer, producer.id);
        assert_eq!(consumer.prerequisites[0].product, "xcframework-bridgecore");
        assert_eq!(consumer.prerequisites[0].task, None);
        assert!(
            shape
                .detected
                .iter()
                .any(|entry| entry == "boltffi-producer:libs/bridge-ffi"),
            "{:?}",
            shape.detected
        );
        assert!(
            shape.limitations.iter().any(|limitation| limitation.contains(
                "consumes binary target `BridgeCoreFFI` from BoltFFI manifest libs/bridge-ffi/boltffi.toml"
            )),
            "{:?}",
            shape.limitations
        );
        assert!(
            !shape
                .limitations
                .iter()
                .any(|limitation| limitation.contains("no tracked file provides")),
            "{:?}",
            shape.limitations
        );
        assert!(shape.swift_consumers.is_empty());
        let canonical = must_ok(shape.canonical_json(), "canonicalize shape");
        assert!(!canonical.contains("boltffi_producers"));
        assert!(!canonical.contains("swift_consumers"));
        let mut config = crate::s2::ProjectConfig::from(shape);
        must_ok(
            crate::s2::platform::resolve(&mut config),
            "product graph accepts the joined edge",
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_escalates_producer_and_appends_pack() {
        let root = native_fixture(&[
            ("libs/bridge-ffi/boltffi.toml", NATIVE_BOLTFFI),
            ("libs/bridge-ffi/Cargo.toml", NATIVE_CARGO),
            ("rust-toolchain.toml", NATIVE_TOOLCHAIN),
            (
                "clients/desktop/Package.swift",
                &native_package(
                    ".binaryTarget(name: \"BridgeCoreFFI\", path: \"../../target/xcframework/BridgeCore.xcframework\")",
                ),
            ),
        ]);
        let shape = scan_native(&root);
        let producer = must_some(
            shape.units.iter().find(|unit| {
                unit.kind == crate::s2::UnitKind::Rust && unit.root == "libs/bridge-ffi"
            }),
            "rust producer unit",
        );
        // The pack runs Apple tooling, so the producer inherits the macOS
        // requirement instead of staying a portable Linux unit.
        assert_eq!(producer.platform, crate::s2::provider::Platform::MacosArm64);
        assert!(producer.capabilities.native_macos_arm64);
        // The typed recipe lands after the unit's own checks, in both lanes:
        // wipe, two snapshots, pack, two drift diffs (the fixture keeps
        // the default generated `Package.swift`).
        for commands in [&producer.pr_commands, &producer.full_commands] {
            assert!(commands.len() > 6, "{commands:?}");
            let tail = &commands[commands.len() - 6..];
            assert_eq!(
                tail[0],
                "rm -rf 'target/xcframework/BridgeCore.xcframework'"
            );
            assert!(
                tail[1].contains("cp -R ") && tail[1].contains("velnor-boltffi-staging"),
                "bindings snapshot: {}",
                tail[1]
            );
            assert!(
                tail[2].contains("Package.swift") && tail[2].contains("cp "),
                "package snapshot: {}",
                tail[2]
            );
            assert_eq!(tail[3], "cd -- 'libs/bridge-ffi' && boltffi -v pack apple");
            assert!(
                tail[4].starts_with("diff -r ") && tail[5].starts_with("diff "),
                "drift diffs: {:?}",
                &tail[4..]
            );
        }
        assert!(
            producer
                .pr_commands
                .iter()
                .any(|command| command.contains("boltffi") && command.contains("pack apple")),
            "pack commands must trip the tool predicate: {:?}",
            producer.pr_commands
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_packs_once_for_two_consumers() {
        let target = ".binaryTarget(name: \"BridgeCoreFFI\", path: \"../../target/xcframework/BridgeCore.xcframework\")";
        let root = native_fixture(&[
            ("libs/bridge-ffi/boltffi.toml", NATIVE_BOLTFFI),
            ("libs/bridge-ffi/Cargo.toml", NATIVE_CARGO),
            ("rust-toolchain.toml", NATIVE_TOOLCHAIN),
            ("clients/desktop/Package.swift", &native_package(target)),
            ("clients/laptop/Package.swift", &native_package(target)),
        ]);
        let shape = scan_native(&root);
        let producer = must_some(
            shape.units.iter().find(|unit| {
                unit.kind == crate::s2::UnitKind::Rust && unit.root == "libs/bridge-ffi"
            }),
            "rust producer unit",
        );
        assert_eq!(producer.products.len(), 1);
        assert_eq!(
            producer
                .pr_commands
                .iter()
                .filter(|command| command.contains("pack apple"))
                .count(),
            1,
            "one pack serves every consumer: {:?}",
            producer.pr_commands
        );
        for id in [
            "swift-package-clients-desktop",
            "swift-package-clients-laptop",
        ] {
            let consumer = must_some(
                shape.units.iter().find(|unit| unit.id == id),
                "swift consumer unit",
            );
            assert_eq!(consumer.prerequisites.len(), 1);
            assert_eq!(consumer.prerequisites[0].product, "xcframework-bridgecore");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_rejects_module_mismatch() {
        let root = native_fixture(&[
            ("libs/bridge-ffi/boltffi.toml", NATIVE_BOLTFFI),
            ("libs/bridge-ffi/Cargo.toml", NATIVE_CARGO),
            ("rust-toolchain.toml", NATIVE_TOOLCHAIN),
            (
                "clients/desktop/Package.swift",
                &native_package(
                    ".binaryTarget(name: \"WrongName\", path: \"../../target/xcframework/BridgeCore.xcframework\")",
                ),
            ),
        ]);
        let shape = scan_native(&root);
        assert!(
            shape.limitations.iter().any(|limitation| limitation
                .contains("produces FFI module `BridgeCoreFFI`; the module disagrees")),
            "{:?}",
            shape.limitations
        );
        assert!(
            shape.units.iter().all(|unit| unit.products.is_empty()),
            "mismatched producer must not offer a product"
        );
        assert!(
            shape.units.iter().all(|unit| unit.prerequisites.is_empty()),
            "mismatched consumer must not require a product"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_keeps_untracked_limitation_without_producer() {
        let root = native_fixture(&[(
            "clients/desktop/Package.swift",
            &native_package(
                ".binaryTarget(name: \"BridgeCoreFFI\", path: \"../../target/xcframework/BridgeCore.xcframework\")",
            ),
        )]);
        let shape = scan_native(&root);
        assert!(
            shape
                .limitations
                .iter()
                .any(|limitation| limitation.contains(
                    "which no tracked file provides; the producing step must materialize it"
                )),
            "{:?}",
            shape.limitations
        );
        assert!(shape.units.iter().all(|unit| unit.prerequisites.is_empty()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_accepts_tracked_prebuilt_silently() {
        let root = native_fixture(&[
            (
                "clients/desktop/Package.swift",
                &native_package(
                    ".binaryTarget(name: \"BridgeCoreFFI\", path: \"Frameworks/BridgeCore.xcframework\")",
                ),
            ),
            (
                "clients/desktop/Frameworks/BridgeCore.xcframework/Info.plist",
                "<plist/>\n",
            ),
        ]);
        let shape = scan_native(&root);
        assert!(
            !shape
                .limitations
                .iter()
                .any(|limitation| limitation.contains("BridgeCoreFFI")),
            "{:?}",
            shape.limitations
        );
        assert!(shape.units.iter().all(|unit| unit.prerequisites.is_empty()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_rejects_escaping_consumer_path() {
        let root = native_fixture(&[(
            "clients/desktop/Package.swift",
            &native_package(
                ".binaryTarget(name: \"BridgeCoreFFI\", path: \"../../../../escape.xcframework\")",
            ),
        )]);
        let shape = scan_native(&root);
        assert!(
            shape.limitations.iter().any(|limitation| limitation
                .contains("absolute or escapes the repository; no producer can be joined")),
            "{:?}",
            shape.limitations
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn same_named_schemes_in_different_containers_get_qualified_ids() {
        let root = native_fixture(&[
            (
                "apps/one/One.xcodeproj/xcshareddata/xcschemes/App.xcscheme",
                "<Scheme/>\n",
            ),
            (
                "apps/two/Two.xcodeproj/xcshareddata/xcschemes/App.xcscheme",
                "<Scheme/>\n",
            ),
        ]);
        let shape = scan_native(&root);
        let mut ids: Vec<&str> = shape
            .units
            .iter()
            .filter(|unit| unit.kind == crate::s2::UnitKind::Swift)
            .map(|unit| unit.id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![
                "swift-xcodeproj-apps-one-one-xcodeproj-app",
                "swift-xcodeproj-apps-two-two-xcodeproj-app",
            ]
        );
        let mut labels: Vec<&str> = shape
            .units
            .iter()
            .filter(|unit| unit.kind == crate::s2::UnitKind::Swift)
            .map(|unit| unit.label.as_str())
            .collect();
        labels.sort_unstable();
        assert_eq!(
            labels,
            vec![
                "Apple scheme (App in apps/one/One.xcodeproj)",
                "Apple scheme (App in apps/two/Two.xcodeproj)",
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn same_named_scheme_across_project_and_workspace_stays_unqualified() {
        let root = native_fixture(&[
            (
                "app/App.xcodeproj/xcshareddata/xcschemes/App.xcscheme",
                "<Scheme/>\n",
            ),
            (
                "app/App.xcworkspace/xcshareddata/xcschemes/App.xcscheme",
                "<Scheme/>\n",
            ),
        ]);
        let shape = scan_native(&root);
        let mut ids: Vec<&str> = shape
            .units
            .iter()
            .filter(|unit| unit.kind == crate::s2::UnitKind::Swift)
            .map(|unit| unit.id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["swift-xcodeproj-app", "swift-xcworkspace-app"]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_accepts_unnamed_target_on_path_match() {
        let root = native_fixture(&[
            ("libs/bridge-ffi/boltffi.toml", NATIVE_BOLTFFI),
            ("libs/bridge-ffi/Cargo.toml", NATIVE_CARGO),
            ("rust-toolchain.toml", NATIVE_TOOLCHAIN),
            (
                "clients/desktop/Package.swift",
                &native_package(
                    ".binaryTarget(path: \"../../target/xcframework/BridgeCore.xcframework\")",
                ),
            ),
        ]);
        let shape = scan_native(&root);
        let consumer = must_some(
            shape
                .units
                .iter()
                .find(|unit| unit.id == "swift-package-clients-desktop"),
            "swift consumer unit",
        );
        assert_eq!(consumer.prerequisites.len(), 1);
        assert!(
            shape.limitations.iter().any(|limitation| limitation
                .contains("consumes binary target `<unnamed>` from BoltFFI manifest")),
            "{:?}",
            shape.limitations
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn swift_package_unit_caches_sources_with_toolchain_pins() {
        let root = native_fixture(&[(
            "clients/desktop/Package.swift",
            &native_package(".target(name: \"Bridge\")"),
        )]);
        let shape = scan_native(&root);
        let unit = must_some(
            shape
                .units
                .iter()
                .find(|unit| unit.id == "swift-package-clients-desktop"),
            "swift package unit",
        );
        let cache = must_some(unit.cache.as_ref(), "package unit declares a cache");
        assert_eq!(
            cache.purpose,
            crate::s2::CachePurpose::SwiftPmSources,
            "package downloads are a source layer, not intermediates"
        );
        assert_eq!(cache.paths, vec!["~/.swiftpm".to_owned()]);
        assert_eq!(
            cache.key_files,
            vec![
                ".swift-version".to_owned(),
                ".xcode-version".to_owned(),
                "clients/desktop/.swiftpm/Package.resolved".to_owned(),
                "clients/desktop/Package.resolved".to_owned(),
                "clients/desktop/Package.swift".to_owned(),
                "mise.lock".to_owned(),
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn xcodegen_unit_caches_intermediates_with_toolchain_pins() {
        let root = native_fixture(&[("app/project.yml", MINIMAL_APP)]);
        let shape = scan_native(&root);
        let unit = must_some(
            shape
                .units
                .iter()
                .find(|unit| unit.id.starts_with("swift-xcodegen-")),
            "xcodegen unit",
        );
        let cache = must_some(unit.cache.as_ref(), "xcodegen unit declares a cache");
        assert_eq!(
            cache.purpose,
            crate::s2::CachePurpose::XcodeIntermediates,
            "DerivedData is an intermediate seed, not a source bundle"
        );
        assert_eq!(
            cache.paths,
            vec!["~/Library/Developer/Xcode/DerivedData".to_owned()]
        );
        for key in [
            "app/project.yml",
            "mise.lock",
            ".swift-version",
            ".xcode-version",
        ] {
            assert!(
                cache.key_files.iter().any(|file| file == key),
                "key files contain {key}: {:?}",
                cache.key_files
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_populates_product_input_closure() {
        let root = native_fixture(&[
            ("libs/bridge-ffi/boltffi.toml", NATIVE_BOLTFFI),
            (
                "libs/bridge-ffi/Cargo.toml",
                "[package]\nname = \"bridge-core-ffi\"\nversion = \"0.1.0\"\n\n[dependencies]\nsibling = { path = \"../sibling\" }\n",
            ),
            ("libs/bridge-ffi/build.rs", "fn main() {}\n"),
            (
                "libs/sibling/Cargo.toml",
                "[package]\nname = \"sibling\"\nversion = \"0.1.0\"\n",
            ),
            ("rust-toolchain.toml", NATIVE_TOOLCHAIN),
            (
                "clients/desktop/Package.swift",
                &native_package(
                    ".binaryTarget(path: \"../../target/xcframework/BridgeCore.xcframework\")",
                ),
            ),
        ]);
        let shape = scan_native(&root);
        assert!(
            shape
                .limitations
                .iter()
                .any(|limitation| limitation.contains("producer step must materialize")),
            "{:?}",
            shape.limitations
        );
        let producer = must_some(
            shape.units.iter().find(|unit| {
                unit.id == "rust-libs-bridge-ffi" || unit.id.starts_with("rust-bridge")
            }),
            "rust producer unit",
        );
        assert_eq!(producer.products.len(), 1, "{:?}", producer.products);
        let product = &producer.products[0];
        for expected in [
            "libs/bridge-ffi/boltffi.toml",
            "libs/bridge-ffi/Cargo.toml",
            "libs/bridge-ffi/**/*.rs",
            "libs/bridge-ffi/build.rs",
            "libs/sibling/Cargo.toml",
            "libs/sibling/**/*.rs",
            "rust-toolchain.toml",
        ] {
            assert!(
                product.inputs.iter().any(|input| input == expected),
                "product inputs contain {expected}: {:?}",
                product.inputs
            );
        }
        assert!(
            product.inputs_unknown.is_empty(),
            "{:?}",
            product.inputs_unknown
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn digest_fixture(extra: &[(&str, &str)]) -> std::path::PathBuf {
        let package = native_package(
            ".binaryTarget(path: \"../../target/xcframework/BridgeCore.xcframework\")",
        );
        let mut entries = vec![
            ("libs/bridge-ffi/boltffi.toml", NATIVE_BOLTFFI),
            (
                "libs/bridge-ffi/Cargo.toml",
                "[package]\nname = \"bridge-core-ffi\"\nversion = \"0.1.0\"\n\n[dependencies]\nsibling = { path = \"../sibling\" }\n",
            ),
            ("libs/bridge-ffi/src/lib.rs", "pub fn bridge() {}\n"),
            (
                "libs/sibling/Cargo.toml",
                "[package]\nname = \"sibling\"\nversion = \"0.1.0\"\n",
            ),
            ("libs/sibling/src/lib.rs", "pub fn sibling() {}\n"),
            ("rust-toolchain.toml", NATIVE_TOOLCHAIN),
            ("clients/desktop/Package.swift", package.as_str()),
            (
                "clients/desktop/Sources/Bridge/Bridge.swift",
                "public func greet() {}\n",
            ),
        ];
        entries.extend_from_slice(extra);
        // `native_fixture` takes `&str` contents; leak nothing by building
        // owned pairs through a scratch vector of owned strings.
        let owned: Vec<(String, String)> = entries
            .iter()
            .map(|(path, contents)| ((*path).to_owned(), (*contents).to_owned()))
            .collect();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(path, contents)| (path.as_str(), contents.as_str()))
            .collect();
        native_fixture(&borrowed)
    }

    fn producer_product_digest(shape: &super::super::RepositoryShape) -> Option<String> {
        shape
            .units
            .iter()
            .find(|unit| unit.id == "rust-libs-bridge-ffi" || unit.id.starts_with("rust-bridge"))
            .and_then(|producer| producer.products.first())
            .and_then(|product| product.inputs_digest.clone())
    }

    fn is_hex_digest(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }

    #[test]
    fn native_join_computes_inputs_digest() {
        let root = digest_fixture(&[]);
        let shape = scan_native(&root);
        let digest = must_some(
            producer_product_digest(&shape),
            "producer product carries a digest",
        );
        assert!(is_hex_digest(&digest), "digest is hex SHA-256: {digest}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_digest_invalidates_on_transitive_source_edit() {
        let root = digest_fixture(&[]);
        let before = must_some(
            producer_product_digest(&scan_native(&root)),
            "digest before edit",
        );
        must_some(
            std::fs::write(
                root.join("libs/sibling/src/lib.rs"),
                "pub fn sibling2() {}\n",
            )
            .ok(),
            "rewrite transitive source",
        );
        let after = must_some(
            producer_product_digest(&scan_native(&root)),
            "digest after edit",
        );
        assert_ne!(before, after, "transitive edit must invalidate the digest");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_digest_ignores_consumer_only_edit() {
        let root = digest_fixture(&[]);
        let before = must_some(
            producer_product_digest(&scan_native(&root)),
            "digest before edit",
        );
        must_some(
            std::fs::write(
                root.join("clients/desktop/Sources/Bridge/Bridge.swift"),
                "public func greet2() {}\n",
            )
            .ok(),
            "rewrite consumer source",
        );
        let after = must_some(
            producer_product_digest(&scan_native(&root)),
            "digest after edit",
        );
        assert_eq!(before, after, "consumer-only edit must preserve the digest");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_digest_absent_on_closure_gap() {
        let root = digest_fixture(&[(
            "libs/bridge-ffi/Cargo.toml",
            "[package]\nname = \"bridge-core-ffi\"\nversion = \"0.1.0\"\n\n[dependencies]\nghost = { path = \"../ghost\" }\n",
        )]);
        // The duplicate Cargo.toml entry wins by write order: the ghost
        // dependency has no tracked manifest, so the closure stays open.
        let shape = scan_native(&root);
        let producer = must_some(
            shape.units.iter().find(|unit| {
                unit.id == "rust-libs-bridge-ffi" || unit.id.starts_with("rust-bridge")
            }),
            "rust producer unit",
        );
        let product = must_some(producer.products.first(), "producer product");
        assert!(
            !product.inputs_unknown.is_empty(),
            "gaps recorded: {:?}",
            product.inputs_unknown
        );
        assert_eq!(product.inputs_digest, None);
        assert!(
            shape
                .limitations
                .iter()
                .any(|limitation| limitation.contains("no exact inputs digest")),
            "{:?}",
            shape.limitations
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn native_join_carries_expected_output_files() {
        let boltffi = "[package]\nname = \"bridge-core\"\ncrate = \"bridge-core-ffi\"\n\n\
             [targets.apple]\ninclude_macos = true\nios_architectures = []\n\
             simulator_architectures = []\nmacos_architectures = [\"arm64\"]\n\n\
             [targets.apple.xcframework]\nname = \"BridgeCore\"\noutput = \"../../target/xcframework\"\n";
        let root = digest_fixture(&[("libs/bridge-ffi/boltffi.toml", boltffi)]);
        let shape = scan_native(&root);
        let producer = must_some(
            shape.units.iter().find(|unit| {
                unit.id == "rust-libs-bridge-ffi" || unit.id.starts_with("rust-bridge")
            }),
            "rust producer unit",
        );
        let product = must_some(producer.products.first(), "producer product");
        let out = "target/xcframework/BridgeCore.xcframework";
        assert_eq!(product.outputs, vec![out.to_owned()]);
        assert_eq!(
            product.output_files,
            vec![
                format!("{out}/Info.plist"),
                format!("{out}/macos-arm64/Headers/module.modulemap"),
                format!("{out}/macos-arm64/libbridge_core_ffi.a"),
            ]
        );
        assert_eq!(product.bindings_dir, "libs/bridge-ffi/dist/apple/Sources");
        assert_eq!(
            product.bindings_file,
            "libs/bridge-ffi/dist/apple/Sources/BridgeCoreFfiBoltFFI.swift"
        );
        assert_eq!(product.deployment_target, "16.0");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn xcode_scheme_unit_caches_intermediates_with_toolchain_pins() {
        let root = native_fixture(&[(
            "apps/one/One.xcodeproj/xcshareddata/xcschemes/App.xcscheme",
            "<Scheme/>\n",
        )]);
        let shape = scan_native(&root);
        let unit = must_some(
            shape
                .units
                .iter()
                .find(|unit| unit.id.starts_with("swift-xcodeproj-")),
            "xcode scheme unit",
        );
        let cache = must_some(unit.cache.as_ref(), "scheme unit declares a cache");
        assert_eq!(
            cache.purpose,
            crate::s2::CachePurpose::XcodeIntermediates,
            "DerivedData is an intermediate seed, not a source bundle"
        );
        for key in [
            "apps/one/One.xcodeproj/project.pbxproj",
            "mise.lock",
            ".swift-version",
            ".xcode-version",
        ] {
            assert!(
                cache.key_files.iter().any(|file| file == key),
                "key files contain {key}: {:?}",
                cache.key_files
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

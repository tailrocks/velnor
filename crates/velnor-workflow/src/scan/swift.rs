//! Swift detector: Swift packages and Xcode shared schemes.

use std::fs;
use std::path::Path;

use serde_yaml::Value;

use super::file_walk::{
    files_named, join_repo_path, path_prefix, resolve_repo_path, roots_for_manifests,
};
use super::{unit, RepositoryShape, ScanContext};
use crate::{
    identifier_suffix, parent_path, shell_change_dir, shell_quote, CachePurpose, CacheSpec,
    GeneratorError, Unit, UnitKind, ValidationPhase,
};

/// Toolchain identity pins shared by every Apple cache and watch contract.
/// `.swift-tools-version` is intentionally absent: it is only a minimum floor.
const APPLE_TOOLCHAIN_PIN_KEY_FILES: [&str; 3] = ["mise.lock", ".swift-version", ".xcode-version"];
const SWIFT_FORMAT_CONFIG: &str = ".swift-format";
const SWIFT_LINT_CONFIGS: [&str; 2] = [".swiftlint.yml", ".swiftlint.yaml"];

fn append_apple_toolchain_pin_paths(paths: &mut Vec<String>) {
    paths.extend(
        APPLE_TOOLCHAIN_PIN_KEY_FILES
            .iter()
            .map(std::string::ToString::to_string),
    );
}

fn swift_raw_string_quote(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    if bytes.get(start) != Some(&b'#') {
        return None;
    }
    let mut quote = start;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    (bytes.get(quote) == Some(&b'"')).then_some((quote, quote - start))
}

fn string_delimiter_is_escaped(bytes: &[u8], quote: usize, hashes: usize) -> bool {
    let mut slash = quote;
    while slash > 0 && bytes[slash - 1] == b'\\' {
        slash -= 1;
    }
    if (quote - slash) % 2 == 1 {
        return true;
    }
    hashes > 0
        && quote > hashes
        && bytes[quote - hashes..quote]
            .iter()
            .all(|byte| *byte == b'#')
        && bytes[quote - hashes - 1] == b'\\'
}

fn swift_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let (quote, hashes) = if bytes.get(start) == Some(&b'"') {
        (start, 0)
    } else {
        swift_raw_string_quote(bytes, start)?
    };
    let multiline = bytes.get(quote + 1) == Some(&b'"') && bytes.get(quote + 2) == Some(&b'"');
    let opening_len = if multiline { 3 } else { 1 };
    let closing_len = opening_len + hashes;
    let mut index = quote + opening_len;
    while index + closing_len <= bytes.len() {
        let closes = if multiline {
            bytes[index..].starts_with(b"\"\"\"")
        } else {
            bytes[index] == b'"'
        };
        if closes
            && !string_delimiter_is_escaped(bytes, index, hashes)
            && bytes[index + opening_len..index + closing_len]
                .iter()
                .all(|byte| *byte == b'#')
        {
            return Some(index + closing_len);
        }
        if hashes == 0 && !multiline && bytes[index] == b'\\' {
            index += 2;
        } else {
            index += 1;
        }
    }
    None
}

fn swift_string_has_interpolation(bytes: &[u8], start: usize, end: usize) -> bool {
    let (quote, hashes) = if bytes.get(start) == Some(&b'"') {
        (start, 0)
    } else if let Some((quote, hashes)) = swift_raw_string_quote(bytes, start) {
        (quote, hashes)
    } else {
        return false;
    };
    let multiline = bytes.get(quote + 1) == Some(&b'"') && bytes.get(quote + 2) == Some(&b'"');
    let opening_len = if multiline { 3 } else { 1 };
    let content_start = quote + opening_len;
    let content_end = end.saturating_sub(opening_len + hashes);
    let mut index = content_start;
    while index < content_end {
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        let slash_start = index;
        while index < content_end && bytes[index] == b'\\' {
            index += 1;
        }
        if (index - slash_start) % 2 != 1 {
            continue;
        }
        let marker_start = index;
        while index < content_end && bytes[index] == b'#' {
            index += 1;
        }
        if index - marker_start == hashes && bytes.get(index) == Some(&b'(') {
            return true;
        }
    }
    false
}

fn swift_block_comment_end(bytes: &[u8], start: usize) -> Option<usize> {
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

fn swift_trivia_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index) == Some(&b'/') && bytes.get(index + 1) == Some(&b'/') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes.get(index) == Some(&b'/') && bytes.get(index + 1) == Some(&b'*') {
            index = swift_block_comment_end(bytes, index)?;
            continue;
        }
        return Some(index);
    }
}

fn swift_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn qualified_swift_call(call: &str) -> Option<&'static str> {
    match call {
        ".testTarget" => Some("Target.testTarget"),
        ".binaryTarget" => Some("Target.binaryTarget"),
        _ => None,
    }
}

fn module_qualified_swift_call(call: &str) -> Option<&'static str> {
    match call {
        ".testTarget" => Some("PackageDescription.Target.testTarget"),
        ".binaryTarget" => Some("PackageDescription.Target.binaryTarget"),
        "Product.executable" => Some("PackageDescription.Product.executable"),
        _ => None,
    }
}

/// Detect a Swift call without treating comments or string contents as
/// package declarations. The scan only needs the call marker: it intentionally
/// refuses the whole schema-1 surface when a product might require a `swift
/// run` phase that this pipeline cannot model.
fn has_swift_call(contents: &str, call: &str) -> bool {
    has_swift_call_marker(contents, call)
        || qualified_swift_call(call)
            .is_some_and(|qualified| has_swift_call_marker(contents, qualified))
        || module_qualified_swift_call(call)
            .is_some_and(|qualified| has_swift_call_marker(contents, qualified))
}

fn has_swift_call_marker(contents: &str, call: &str) -> bool {
    let bytes = contents.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let Some(next) = swift_string_end(bytes, index) else {
                    return true;
                };
                index = next;
            }
            b'#' if swift_raw_string_quote(bytes, index).is_some() => {
                let Some(next) = swift_string_end(bytes, index) else {
                    return true;
                };
                index = next;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let Some(next) = swift_block_comment_end(bytes, index) else {
                    return true;
                };
                index = next;
            }
            _ => {
                let matches_call = contents[index..].starts_with(call)
                    && (index == 0
                        || (!swift_identifier_byte(bytes[index - 1]) && bytes[index - 1] != b'.'))
                    && (index + call.len() == bytes.len()
                        || !swift_identifier_byte(bytes[index + call.len()]));
                if matches_call {
                    let Some(next) = swift_trivia_end(bytes, index + call.len()) else {
                        return true;
                    };
                    if bytes.get(next) == Some(&b'(') {
                        return true;
                    }
                }
                let width = contents[index..].chars().next().map_or(1, char::len_utf8);
                index += width;
            }
        }
    }
    false
}

fn swift_group_has_literal(
    contents: &str,
    bytes: &[u8],
    group_start: usize,
    key: &str,
    literal: &str,
) -> Option<(bool, usize)> {
    let mut depth = 1;
    let mut cursor = group_start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => {
                let next = swift_string_end(bytes, cursor)?;
                cursor = next;
            }
            b'#' if swift_raw_string_quote(bytes, cursor).is_some() => {
                let next = swift_string_end(bytes, cursor)?;
                cursor = next;
            }
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                while cursor < bytes.len() && bytes[cursor] != b'\n' {
                    cursor += 1;
                }
            }
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                cursor = swift_block_comment_end(bytes, cursor)?;
            }
            b'(' => {
                depth += 1;
                cursor += 1;
            }
            b')' => {
                depth -= 1;
                cursor += 1;
                if depth == 0 {
                    return Some((false, cursor));
                }
            }
            _ => {
                let matches_key = depth == 1
                    && contents[cursor..].starts_with(key)
                    && (cursor == 0 || !swift_identifier_byte(bytes[cursor - 1]))
                    && (cursor + key.len() == bytes.len()
                        || !swift_identifier_byte(bytes[cursor + key.len()]));
                if matches_key {
                    let colon = swift_trivia_end(bytes, cursor + key.len())?;
                    if bytes.get(colon) == Some(&b':') {
                        let value = swift_trivia_end(bytes, colon + 1)?;
                        let is_string = bytes.get(value) == Some(&b'"')
                            || swift_raw_string_quote(bytes, value).is_some();
                        if is_string {
                            let end = swift_string_end(bytes, value)?;
                            if !swift_string_has_interpolation(bytes, value, end)
                                && contents[value..end].contains(literal)
                            {
                                return Some((true, end));
                            }
                            cursor = end;
                            continue;
                        }
                    }
                }
                let width = contents[cursor..].chars().next().map_or(1, char::len_utf8);
                cursor += width;
            }
        }
    }
    None
}

fn swift_call_has_literal(contents: &str, call: &str, key: &str, literal: &str) -> bool {
    swift_call_has_literal_marker(contents, call, key, literal)
        || qualified_swift_call(call).is_some_and(|qualified| {
            swift_call_has_literal_marker(contents, qualified, key, literal)
        })
        || module_qualified_swift_call(call).is_some_and(|qualified| {
            swift_call_has_literal_marker(contents, qualified, key, literal)
        })
}

fn swift_call_has_literal_marker(contents: &str, call: &str, key: &str, literal: &str) -> bool {
    let bytes = contents.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let Some(next) = swift_string_end(bytes, index) else {
                    return false;
                };
                index = next;
            }
            b'#' if swift_raw_string_quote(bytes, index).is_some() => {
                let Some(next) = swift_string_end(bytes, index) else {
                    return false;
                };
                index = next;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let Some(next) = swift_block_comment_end(bytes, index) else {
                    return false;
                };
                index = next;
            }
            _ => {
                let matches_call = contents[index..].starts_with(call)
                    && (index == 0
                        || (!swift_identifier_byte(bytes[index - 1]) && bytes[index - 1] != b'.'))
                    && (index + call.len() == bytes.len()
                        || !swift_identifier_byte(bytes[index + call.len()]));
                if !matches_call {
                    let width = contents[index..].chars().next().map_or(1, char::len_utf8);
                    index += width;
                    continue;
                }
                let Some(group_start) = swift_trivia_end(bytes, index + call.len()) else {
                    return false;
                };
                if bytes.get(group_start) != Some(&b'(') {
                    index += call.len();
                    continue;
                }
                let Some((found, next)) =
                    swift_group_has_literal(contents, bytes, group_start, key, literal)
                else {
                    return false;
                };
                if found {
                    return true;
                }
                index = next;
            }
        }
    }
    false
}

fn join_style_path(root: &str, name: &str) -> String {
    if root == "." {
        name.to_owned()
    } else {
        format!("{root}/{name}")
    }
}

fn parent_style_path(path: &str) -> Option<String> {
    if path == "." {
        None
    } else {
        Some(
            path.rsplit_once('/')
                .map_or_else(|| ".".to_owned(), |(parent, _)| parent.to_owned()),
        )
    }
}

/// Find the nearest repository config at the unit root or one of its
/// ancestors. A root unit never inherits a nested package's config.
fn nearest_style_config(unit_root: &str, files: &[String], names: &[&str]) -> Option<String> {
    let mut scope = unit_root.to_owned();
    loop {
        for name in names {
            let candidate = join_style_path(&scope, name);
            if files.iter().any(|file| file == &candidate) {
                return Some(candidate);
            }
        }
        let Some(parent) = parent_style_path(&scope) else {
            break;
        };
        scope = parent;
    }
    None
}

fn relative_style_path(from: &str, to: &str) -> String {
    let from_parts = if from == "." {
        Vec::new()
    } else {
        from.split('/').collect::<Vec<_>>()
    };
    let to_parts = if to == "." {
        Vec::new()
    } else {
        to.split('/').collect::<Vec<_>>()
    };
    let common = from_parts
        .iter()
        .zip(&to_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts = Vec::new();
    parts.extend(std::iter::repeat_n("..", from_parts.len() - common));
    parts.extend(to_parts[common..].iter().copied());
    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

fn swift_style_checks(unit_root: &str, files: &[String]) -> Vec<(String, ValidationPhase, String)> {
    let mut checks = Vec::new();
    if let Some(config) = nearest_style_config(unit_root, files, &[SWIFT_FORMAT_CONFIG]) {
        let config_arg = shell_quote(&relative_style_path(unit_root, &config));
        checks.push((
            format!(
                "{}swift format lint --configuration {config_arg} --recursive --strict .",
                shell_change_dir(unit_root)
            ),
            ValidationPhase::SwiftFormat,
            config,
        ));
    }
    if let Some(config) = nearest_style_config(unit_root, files, &SWIFT_LINT_CONFIGS) {
        let config_arg = shell_quote(&relative_style_path(unit_root, &config));
        checks.push((
            format!(
                "{}swiftlint lint --config {config_arg} --strict",
                shell_change_dir(unit_root)
            ),
            ValidationPhase::SwiftLint,
            config,
        ));
    }
    checks
}

fn apply_swift_style_checks(unit: &mut Unit, files: &[String]) {
    let checks = swift_style_checks(&unit.root, files);
    if checks.is_empty() {
        return;
    }
    let mut commands = checks
        .iter()
        .map(|(command, _, _)| command.clone())
        .collect::<Vec<_>>();
    commands.extend(std::mem::take(&mut unit.pr_commands));
    unit.pr_commands = commands.clone();
    unit.full_commands = commands;
    let mut phases = checks
        .iter()
        .map(|(_, phase, _)| *phase)
        .collect::<Vec<_>>();
    phases.extend(std::mem::take(&mut unit.phases));
    unit.phases = phases;
    unit.watch
        .extend(checks.into_iter().map(|(_, _, config)| config));
    unit.watch.sort();
    unit.watch.dedup();
}

fn is_xcodegen_spec(contents: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<Value>(contents) else {
        return false;
    };
    let Some(mapping) = value.as_mapping() else {
        return false;
    };
    let has_name = mapping.iter().any(|(key, value)| {
        key.as_str() == "name" && value.as_str().is_some_and(|name| !name.is_empty())
    });
    let has_targets = mapping.iter().any(|(key, value)| {
        key.as_str() == "targets"
            && value
                .as_mapping()
                .is_some_and(|targets| !targets.is_empty())
    });
    has_name && has_targets
}

fn xcode_scheme_has_test_action(contents: &str) -> bool {
    const MARKER: &str = "<TestAction";
    let mut index = 0;
    while let Some(found) = contents[index..].find('<') {
        let start = index + found;
        if contents[start..].starts_with("<!--") {
            let Some(end) = contents[start + 4..].find("-->") else {
                return false;
            };
            index = start + 4 + end + 3;
            continue;
        }
        if contents[start..].starts_with(MARKER) {
            let after = start + MARKER.len();
            if after == contents.len()
                || matches!(
                    contents.as_bytes().get(after),
                    Some(b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
                )
            {
                return true;
            }
        }
        index = start + 1;
    }
    false
}

fn swift_package_unit(package_root: &str, has_tests: bool) -> Unit {
    let prefix = path_prefix(package_root);
    let command_prefix = shell_change_dir(package_root);
    let mut commands = vec![format!("{command_prefix}swift build")];
    let mut phases = vec![ValidationPhase::SwiftBuild];
    if has_tests {
        commands.push(format!("{command_prefix}swift test --parallel"));
        phases.push(ValidationPhase::SwiftTest);
    }
    let mut watch = vec![
        join_repo_path(package_root, "Package.swift"),
        join_repo_path(package_root, "Package.resolved"),
        join_repo_path(package_root, ".swiftpm/Package.resolved"),
        format!("{prefix}Sources/**"),
        format!("{prefix}Tests/**"),
        format!("{prefix}**/*.swift"),
    ];
    append_apple_toolchain_pin_paths(&mut watch);
    let mut cache_key_files = vec![
        join_repo_path(package_root, "Package.swift"),
        join_repo_path(package_root, "Package.resolved"),
        join_repo_path(package_root, ".swiftpm/Package.resolved"),
    ];
    append_apple_toolchain_pin_paths(&mut cache_key_files);
    let mut result = unit(
        UnitKind::Swift,
        package_root,
        watch,
        commands,
        Some(CacheSpec {
            key_files: cache_key_files,
            paths: vec!["~/.swiftpm".to_owned()],
            purpose: CachePurpose::Generic,
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
    // A SwiftPM package is portable: it verifies wherever its toolchain
    // provisions, on the lane's default executor. Only Xcode scheme work
    // below carries an Apple need.
    result.platform = crate::platform::PlatformRequirement::swift_package();
    result.phases = phases;
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
    let mut units = Vec::new();
    for scheme in files
        .iter()
        .filter(|file| file.ends_with(".xcscheme") && file.contains("/xcshareddata/xcschemes/"))
    {
        let Some((container, scheme_name)) = scheme.split_once("/xcshareddata/xcschemes/") else {
            continue;
        };
        let scheme_name = scheme_name.trim_end_matches(".xcscheme");
        let container_root = parent_path(container);
        let (flag, extension) = if container.ends_with(".xcworkspace") {
            ("-workspace", "xcworkspace")
        } else if container.ends_with(".xcodeproj") {
            ("-project", "xcodeproj")
        } else {
            continue;
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
        let has_test_action = xcode_scheme_has_test_action(&scheme_contents);
        let command_prefix = shell_change_dir(&container_root);
        let container_name = container.rsplit('/').next().unwrap_or(container);
        let container_quoted = shell_quote(container_name);
        let scheme_quoted = shell_quote(scheme_name);
        let mut commands = vec![format!(
            "{command_prefix}xcodebuild {flag} {container_quoted} -scheme {scheme_quoted}{build_destination} CODE_SIGNING_ALLOWED=NO build"
        )];
        let mut phases = vec![ValidationPhase::SwiftBuild];
        if has_test_action {
            commands.push(format!(
                "{command_prefix}xcodebuild {flag} {container_quoted} -scheme {scheme_quoted}{test_destination} CODE_SIGNING_ALLOWED=NO test"
            ));
            phases.push(ValidationPhase::SwiftTest);
        }
        let mut cache_key_files = vec![scheme.to_owned()];
        if extension == "xcodeproj" {
            cache_key_files.push(format!("{container}/project.pbxproj"));
        } else {
            cache_key_files.push(format!("{container}/contents.xcworkspacedata"));
            if let Some(project) = referenced_project {
                cache_key_files.push(format!("{project}/project.pbxproj"));
            }
        }
        append_apple_toolchain_pin_paths(&mut cache_key_files);
        cache_key_files.sort();
        cache_key_files.dedup();
        let root_watch = if container_root == "." {
            "**".to_owned()
        } else {
            format!("{container_root}/**")
        };
        let mut watch = vec![
            format!("{container}/**"),
            root_watch,
            "*.xcconfig".to_owned(),
        ];
        append_apple_toolchain_pin_paths(&mut watch);
        watch.sort();
        watch.dedup();
        let mut unit = Unit {
            id: format!("swift-{extension}-{}", identifier_suffix(scheme_name)),
            label: format!("Apple scheme ({scheme_name})"),
            kind: UnitKind::Swift,
            root: container_root.clone(),
            watch,
            pr_commands: commands.clone(),
            full_commands: commands,
            github_pr_commands: None,
            github_full_commands: None,
            velnor_pr_commands: None,
            velnor_full_commands: None,
            phases,
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            pinned_lockfile: false,
            cache: Some(CacheSpec {
                key_files: cache_key_files,
                paths: vec!["~/Library/Developer/Xcode/DerivedData".to_owned()],
                purpose: CachePurpose::Generic,
                mbx_output_cache_justification: None,
                mutable_mount_seed: false,
            }),
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: None,
            services: Vec::new(),
            requires_trusted: false,
            workspace_check: false,
            reads_closed: false,
            platform: crate::platform::PlatformRequirement::apple_xcode(),
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        };
        apply_swift_style_checks(&mut unit, files);
        unit.watch.sort();
        unit.watch.dedup();
        units.push(unit);
    }
    units
}

/// Whether the package manifest consumes an `XCFramework` binary target: the
/// bundle only resolves where the Apple SDK exists, so the unit is
/// Apple-bound even without an Xcode project. A remote (URL) binary target
/// without an `.xcframework` reference carries no such need.
fn package_manifest_needs_xcframework(root: &Path, package_root: &str) -> bool {
    let manifest = root.join(join_repo_path(package_root, "Package.swift"));
    let contents = fs::read_to_string(manifest).unwrap_or_default();
    swift_call_has_literal(&contents, ".binaryTarget", "path", ".xcframework")
        || swift_call_has_literal(&contents, ".binaryTarget", "url", ".xcframework")
}

pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), GeneratorError> {
    for package_root in roots_for_manifests(&files_named(context.files, "Package.swift")) {
        shape.detected.push(format!("swift-package:{package_root}"));
        let manifest = join_repo_path(&package_root, "Package.swift");
        let contents = fs::read_to_string(context.root.join(&manifest)).ok();
        if contents.as_deref().is_some_and(|contents| {
            has_swift_call(contents, ".executable")
                || has_swift_call(contents, "Product.executable")
        }) {
            return Err(GeneratorError::usage(format!(
                "schema-1 Swift limitation: package {manifest} declares an executable product; refusing generation because the root scanner cannot emit the required `swift-run` phase without misleading phase selection. Use schema = 2 for executable-product support."
            )));
        }
        let has_tests = contents
            .as_deref()
            .is_none_or(|contents| has_swift_call(contents, ".testTarget"));
        if !has_tests {
            shape.limitations.push(format!(
                "Swift package {manifest} declares no test targets; emitting build-only commands."
            ));
        }
        let mut unit = swift_package_unit(&package_root, has_tests);
        apply_swift_style_checks(&mut unit, context.files);
        if package_manifest_needs_xcframework(context.root, &package_root) {
            unit.platform = crate::platform::PlatformRequirement::apple_xcframework();
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
    for spec in context.files.iter().filter(|file| {
        file.ends_with("/project.yml")
            || file.ends_with("/project.yaml")
            || file.as_str() == "project.yml"
            || file.as_str() == "project.yaml"
    }) {
        let contents = fs::read_to_string(context.root.join(spec)).unwrap_or_default();
        if is_xcodegen_spec(&contents) {
            return Err(GeneratorError::usage(format!(
                "schema-1 Swift limitation: XcodeGen spec `{spec}` is not modeled; refusing generation because emitting no XcodeGen unit would make phase and selection behavior misleading. Use schema = 2 for XcodeGen support."
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        apply_swift_style_checks, has_swift_call, is_xcodegen_spec, swift_call_has_literal,
        swift_package_unit, swift_style_checks, xcode_scheme_has_test_action,
        APPLE_TOOLCHAIN_PIN_KEY_FILES,
    };
    use crate::ValidationPhase;

    fn assert_apple_toolchain_pin_paths(unit: &crate::Unit) {
        assert!(
            unit.cache.is_some(),
            "Swift unit must have a cache contract"
        );
        let Some(cache) = unit.cache.as_ref() else {
            return;
        };
        for pin in APPLE_TOOLCHAIN_PIN_KEY_FILES {
            assert!(
                unit.watch.iter().any(|path| path == pin),
                "watch set must include {pin}: {:?}",
                unit.watch
            );
            assert!(
                cache.key_files.iter().any(|path| path == pin),
                "cache key files must include {pin}: {:?}",
                cache.key_files
            );
        }
    }

    #[test]
    fn executable_product_detection_skips_comments_and_strings() {
        assert!(has_swift_call(
            "products: [.executable(name: \"App\", targets: [\"App\"])]",
            ".executable"
        ));
        assert!(has_swift_call(
            "products: [Product.executable(name: \"App\", targets: [\"App\"])]",
            "Product.executable"
        ));
        assert!(has_swift_call(
            "products: [PackageDescription.Product.executable(name: \"App\", targets: [\"App\"])]",
            "Product.executable"
        ));
        assert!(has_swift_call(
            "products: [.executable /* comment */ (name: \"App\", targets: [\"App\"])]",
            ".executable"
        ));
        assert!(!has_swift_call(
            "// .executable(name: \"App\")\nlet note = \".executable(name: \\\"App\\\")\"",
            ".executable"
        ));
        assert!(!has_swift_call(
            "/* .executable(name: \"App\") */",
            ".executable"
        ));
        assert!(!has_swift_call(
            "let note = #\".executable(name: \"App\")\"#",
            ".executable"
        ));
        assert!(has_swift_call(
            "let note = \"unterminated .testTarget(name: ",
            ".testTarget"
        ));
    }

    #[test]
    fn test_target_detection_skips_comments_strings_and_lookalikes() {
        assert!(has_swift_call(
            ".testTarget /* gap */ (name: \"Tests\")",
            ".testTarget"
        ));
        assert!(has_swift_call(
            "Target.testTarget(name: \"Tests\")",
            ".testTarget"
        ));
        assert!(has_swift_call(
            "PackageDescription.Target.testTarget(name: \"Tests\")",
            ".testTarget"
        ));
        assert!(!has_swift_call(
            "// .testTarget(name: \"Comment\")\nlet text = \".testTarget(name: \\\"String\\\")\"",
            ".testTarget"
        ));
        assert!(!has_swift_call(
            ".not_testTarget(name: \"Lookalike\")\n.testTargeting(name: \"Lookalike\")\nOther.testTarget(name: \"Lookalike\")",
            ".testTarget"
        ));
    }

    #[test]
    fn xcframework_identity_requires_a_real_binary_target_literal() {
        assert!(swift_call_has_literal(
            ".binaryTarget /* gap */ (path: \"Build/App.xcframework\")",
            ".binaryTarget",
            "path",
            ".xcframework"
        ));
        assert!(swift_call_has_literal(
            "Target.binaryTarget(path: \"Build/App.xcframework\")",
            ".binaryTarget",
            "path",
            ".xcframework"
        ));
        assert!(swift_call_has_literal(
            "PackageDescription.Target.binaryTarget(path: \"Build/App.xcframework\")",
            ".binaryTarget",
            "path",
            ".xcframework"
        ));
        assert!(swift_call_has_literal(
            ".binaryTarget(url: \"https://example.com/App.xcframework\", checksum: \"abc\")",
            ".binaryTarget",
            "url",
            ".xcframework"
        ));
        assert!(!swift_call_has_literal(
            ".binaryTarget(path: \"Build/\\(name).xcframework\")",
            ".binaryTarget",
            "path",
            ".xcframework"
        ));
        assert!(!swift_call_has_literal(
            r##".binaryTarget(path: #"Build/\#(name).xcframework"#)"##,
            ".binaryTarget",
            "path",
            ".xcframework"
        ));
        assert!(!swift_call_has_literal(
            "let note = \".binaryTarget(path: \\\"Fake.xcframework\\\")\"\n\
             // .binaryTarget(path: \"Comment.xcframework\")",
            ".binaryTarget",
            "path",
            ".xcframework"
        ));
        assert!(!swift_call_has_literal(
            ".binaryTarget(path: computed)\nlet path = \"Build/App.xcframework\"",
            ".binaryTarget",
            "path",
            ".xcframework"
        ));
        assert!(!swift_call_has_literal(
            ".binaryTarget(name: \"Fake.xcframework\", path: computed)",
            ".binaryTarget",
            "path",
            ".xcframework"
        ));
    }

    #[test]
    fn swift_style_checks_use_nearest_real_configs() {
        let files = vec![
            "Packages/App/Package.swift".to_owned(),
            "Packages/.swift-format".to_owned(),
            "Packages/.swiftlint.yaml".to_owned(),
            "Packages/App/.swiftlint.yml".to_owned(),
            "Packages/App/notes.swift-format".to_owned(),
            "Packages/App/docs/.swiftlint.yml".to_owned(),
        ];
        let checks = swift_style_checks("Packages/App", &files);
        assert_eq!(checks.len(), 2);
        assert_eq!(
            checks[0],
            (
                "cd -- 'Packages/App' && swift format lint --configuration '../.swift-format' --recursive --strict ."
                    .to_owned(),
                ValidationPhase::SwiftFormat,
                "Packages/.swift-format".to_owned(),
            )
        );
        assert_eq!(
            checks[1],
            (
                "cd -- 'Packages/App' && swiftlint lint --config '.swiftlint.yml' --strict"
                    .to_owned(),
                ValidationPhase::SwiftLint,
                "Packages/App/.swiftlint.yml".to_owned(),
            )
        );
    }

    #[test]
    fn swift_style_checks_do_not_inherit_nested_configs_or_change_cache_keys() {
        let files = vec![
            "Package.swift".to_owned(),
            "Packages/App/.swift-format".to_owned(),
            "Packages/App/.swiftlint.yml".to_owned(),
        ];
        assert!(swift_style_checks(".", &files).is_empty());

        let mut unit = swift_package_unit("Packages/App", true);
        let cache_keys = unit.cache.as_ref().map(|cache| cache.key_files.clone());
        assert!(cache_keys.is_some());
        apply_swift_style_checks(&mut unit, &files);
        assert_eq!(
            unit.phases,
            vec![
                ValidationPhase::SwiftFormat,
                ValidationPhase::SwiftLint,
                ValidationPhase::SwiftBuild,
                ValidationPhase::SwiftTest,
            ]
        );
        assert_eq!(
            unit.pr_commands,
            vec![
                "cd -- 'Packages/App' && swift format lint --configuration '.swift-format' --recursive --strict ."
                    .to_owned(),
                "cd -- 'Packages/App' && swiftlint lint --config '.swiftlint.yml' --strict"
                    .to_owned(),
                "cd -- 'Packages/App' && swift build".to_owned(),
                "cd -- 'Packages/App' && swift test --parallel".to_owned(),
            ]
        );
        assert!(unit
            .watch
            .contains(&"Packages/App/.swift-format".to_owned()));
        assert!(unit
            .watch
            .contains(&"Packages/App/.swiftlint.yml".to_owned()));
        assert_eq!(
            unit.cache.as_ref().map(|cache| &cache.key_files),
            cache_keys.as_ref()
        );
    }

    #[test]
    fn xcodegen_detection_is_structural() {
        assert!(is_xcodegen_spec(
            "name: App\ntargets:\n  App:\n    type: application\n"
        ));
        assert!(!is_xcodegen_spec(
            "name: App\noptions:\n  bundleIdPrefix: org.example\n"
        ));
        assert!(!is_xcodegen_spec("targets: []\n"));
    }

    #[test]
    fn testless_swift_package_has_only_a_build_phase() {
        let unit = swift_package_unit("native", false);
        assert_eq!(
            unit.pr_commands,
            vec!["cd -- 'native' && swift build".to_owned()]
        );
        assert_eq!(unit.phases, vec![ValidationPhase::SwiftBuild]);
        assert_apple_toolchain_pin_paths(&unit);
    }

    #[test]
    fn commented_xcode_test_action_does_not_enable_testing() {
        assert!(!xcode_scheme_has_test_action(
            "<Scheme><!-- <TestAction/> --><BuildAction/></Scheme>"
        ));
        assert!(xcode_scheme_has_test_action(
            "<Scheme><BuildAction/><TestAction/></Scheme>"
        ));
    }
}

//! Swift detector: Swift packages and Xcode shared schemes.

use std::fs;
use std::path::Path;

use super::file_walk::{
    files_named, join_repo_path, path_prefix, resolve_repo_path, roots_for_manifests,
};
use super::{unit, RepositoryShape, ScanContext};
use crate::s2::{
    identifier_suffix, parent_path, shell_change_dir, shell_quote, CachePurpose, CacheSpec, Unit,
    UnitKind,
};

/// One `.binaryTarget` stanza: a local `path` artifact or a remote `url`
/// artifact. `None` means the stanza had no literal value for that key — a
/// variable or computed expression the static scan cannot resolve.
struct BinaryTarget {
    name: Option<String>,
    path: Option<String>,
    url: Option<String>,
}

/// Static `Package.swift` facts. The manifest is executable Swift; the scan
/// only reads its text, never evaluates it.
struct PackageFacts {
    tools_version: Option<String>,
    has_tests: bool,
    binary_targets: Vec<BinaryTarget>,
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
                let width = text[index..]
                    .chars()
                    .next()
                    .map_or(1, char::len_utf8);
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

fn swift_package_unit(package_root: &str, facts: &PackageFacts) -> Unit {
    let prefix = path_prefix(package_root);
    let command_prefix = shell_change_dir(package_root);
    let mut commands = vec![format!("{command_prefix}swift build")];
    if facts.has_tests {
        commands.push(format!("{command_prefix}swift test --parallel"));
    }
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
            key_files: vec![
                join_repo_path(package_root, "Package.swift"),
                join_repo_path(package_root, "Package.resolved"),
                join_repo_path(package_root, ".swiftpm/Package.resolved"),
            ],
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
        cache_key_files.sort();
        cache_key_files.dedup();
        let root_watch = if container_root == "." {
            "**".to_owned()
        } else {
            format!("{container_root}/**")
        };
        let mut unit = Unit {
            id: format!("swift-{extension}-{}", identifier_suffix(scheme_name)),
            label: format!("Apple scheme ({scheme_name})"),
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
                purpose: CachePurpose::Generic,
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
        units.push(unit);
    }
    units
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
                    let tracked = resolve_repo_path(&package_root, path).is_some_and(|resolved| {
                        context.file_set.contains(&resolved)
                            || context
                                .file_set
                                .iter()
                                .any(|file| file.starts_with(&format!("{resolved}/")))
                    });
                    if !tracked {
                        shape.limitations.push(format!(
                            "Swift package {manifest} references binary target `{name}` at `{path}`, which no tracked file provides; the producing step must materialize it before `swift build` consumes the package."
                        ));
                    }
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
}

#[cfg(test)]
mod tests {
    use super::{parse_package_facts, PackageFacts};

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
}

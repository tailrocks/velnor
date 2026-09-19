//! Static Swift package capability evidence.
//!
//! `SwiftPM`'s language name does not establish portability. This module reads
//! only manifests and tracked Swift source paths, then reports evidence that a
//! package needs an Apple SDK. It never evaluates Package.swift or project
//! code.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::native_contract::{AppleArch, AppleNativeContract, AppleSdkFamily, AppleVersion};

/// Apple frameworks/modules that cannot be provided by a Linux Swift SDK.
const APPLE_MODULES: &[&str] = &[
    "Accelerate",
    "ApplicationServices",
    "AVFoundation",
    "AppKit",
    "AudioToolbox",
    "CFNetwork",
    "CoreAudio",
    "CoreBluetooth",
    "CoreData",
    "CoreFoundation",
    "CoreGraphics",
    "CoreHaptics",
    "CoreImage",
    "CoreLocation",
    "CoreML",
    "CoreMedia",
    "CoreMotion",
    "CoreServices",
    "CoreSpotlight",
    "CoreText",
    "CoreVideo",
    "CryptoKit",
    "Darwin",
    "EventKit",
    "GameKit",
    "HealthKit",
    "ImageIO",
    "IOKit",
    "MapKit",
    "MachO",
    "MediaPlayer",
    "MediaToolbox",
    "Metal",
    "MetalKit",
    "MetricKit",
    "NetworkExtension",
    "OSLog",
    "OpenGL",
    "QuartzCore",
    "ReplayKit",
    "SceneKit",
    "Security",
    "Speech",
    "SpriteKit",
    "StoreKit",
    "SwiftUI",
    "SystemConfiguration",
    "UIKit",
    "UniformTypeIdentifiers",
    "UserNotifications",
    "VideoToolbox",
    "WatchKit",
    "WebKit",
    "os",
];

const APPLE_LINK_MARKERS: &[&str] = &[
    "-framework AppKit",
    "-framework AVFoundation",
    "-framework CoreAudio",
    "-framework CoreGraphics",
    "-framework CoreImage",
    "-framework CoreLocation",
    "-framework CoreML",
    "-framework Foundation",
    "-framework Metal",
    "-framework MetalKit",
    "-framework MetricKit",
    "-framework QuartzCore",
    "-framework SceneKit",
    "-framework Security",
    "-framework SpriteKit",
    "-framework StoreKit",
    "-framework SwiftUI",
    "-framework UIKit",
    "-framework WatchKit",
    "-sdk macosx",
    "-target arm64-apple-",
    "-target x86_64-apple-",
    "apple-darwin",
    "MACOSX_DEPLOYMENT_TARGET",
];

/// A package's statically provable Apple requirements.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SwiftPackageEvidence {
    /// The package needs an Apple SDK, even if it has no Xcode project.
    pub(crate) apple: bool,
    /// The package consumes an `XCFramework` binary target.
    pub(crate) xcframework: bool,
    /// Typed host/SDK/toolchain evidence, when the package is Apple-bound.
    pub(crate) native: Option<AppleNativeContract>,
}

/// Inspect one `SwiftPM` package without executing its manifest.
pub(crate) fn package_evidence(
    root: &Path,
    package_root: &str,
    files: &[String],
    package_roots: &[String],
) -> SwiftPackageEvidence {
    let manifest_path = if package_root == "." {
        root.join("Package.swift")
    } else {
        root.join(package_root).join("Package.swift")
    };
    let manifest = fs::read_to_string(manifest_path).unwrap_or_default();
    let source_roots = package_source_roots(&manifest, package_root);
    let xcframework = manifest_uses_xcframework(&manifest);
    let apple = xcframework
        || manifest_declares_apple_platform(&manifest)
        || manifest_uses_apple_linker(&manifest)
        || files
            .iter()
            .filter(|file| is_package_source(file, package_root, package_roots, &source_roots))
            .any(|file| {
                fs::read_to_string(root.join(file))
                    .is_ok_and(|contents| source_imports_apple_module(&contents))
            });
    let native = apple.then(|| manifest_native_contract(&manifest));
    SwiftPackageEvidence {
        apple,
        xcframework,
        native,
    }
}

fn manifest_native_contract(contents: &str) -> AppleNativeContract {
    let code = strip_swift_comments(contents);
    let family = if code.contains(".iOS(") {
        AppleSdkFamily::IosSimulator
    } else {
        AppleSdkFamily::Macos
    };
    let minimum = platform_version(&code, family);
    let mut contract = AppleNativeContract::new(family, minimum);
    if let Some(version) = swift_tools_version(contents) {
        contract.swift = crate::native_contract::AppleVersionConstraint::minimum(version);
    }
    let arches = architecture_evidence(&code);
    if !arches.is_empty() {
        contract.build_arches = arches;
    }
    contract
}

fn project_native_contract(contents: &str) -> Option<AppleNativeContract> {
    let code = strip_non_code_comments(contents);
    let family = if code.contains("iphonesimulator") {
        AppleSdkFamily::IosSimulator
    } else if code.contains("iphoneos") || code.contains("IPHONEOS_DEPLOYMENT_TARGET") {
        AppleSdkFamily::IosDevice
    } else if code.contains("macosx")
        || code.contains("macOS")
        || code.contains("MACOSX_DEPLOYMENT_TARGET")
    {
        AppleSdkFamily::Macos
    } else {
        return None;
    };
    let minimum = match family {
        AppleSdkFamily::Macos => {
            first_setting_version(&code, &["MACOSX_DEPLOYMENT_TARGET", "macOS", "macos"])
        }
        AppleSdkFamily::IosDevice | AppleSdkFamily::IosSimulator => {
            first_setting_version(&code, &["IPHONEOS_DEPLOYMENT_TARGET", "iOS", "ios"])
        }
    };
    let mut contract = AppleNativeContract::new(family, minimum);
    let arches = architecture_evidence(&code);
    if !arches.is_empty() {
        contract.build_arches = arches;
    }
    if let Some(version) = setting_version(&code, "xcodeVersion") {
        contract.xcode = crate::native_contract::AppleVersionConstraint::exact(version);
    }
    if let Some(version) = setting_version(&code, "SWIFT_VERSION") {
        contract.swift = crate::native_contract::AppleVersionConstraint::exact(version);
    }
    Some(contract)
}

fn swift_tools_version(contents: &str) -> Option<AppleVersion> {
    contents.lines().take(5).find_map(|line| {
        let line = line.trim();
        let value = line.strip_prefix("// swift-tools-version:")?.trim();
        AppleVersion::parse(value)
    })
}

/// Combine the generated Xcode project and its declarative generator input.
/// The latter is where XcodeGen pins facts such as xcodeVersion and universal
/// ARCHS; neither is guaranteed to survive into a generated project file.
pub(crate) fn xcode_native_contract(
    project: &str,
    project_config: Option<&str>,
) -> Option<AppleNativeContract> {
    let mut contract = project_native_contract(project);
    if let Some(project_config) = project_config {
        contract = match (contract, project_native_contract(project_config)) {
            (Some(left), Some(right)) => match left.merge(&right) {
                Ok(merged) => Some(merged),
                Err(error) => {
                    let mut conflicted = left;
                    conflicted.conflicts.push(error);
                    Some(conflicted)
                }
            },
            (left, right) => left.or(right),
        };
    }
    contract
}

fn platform_version(contents: &str, family: AppleSdkFamily) -> Option<AppleVersion> {
    let marker = match family {
        AppleSdkFamily::Macos => ".macOS(",
        AppleSdkFamily::IosDevice | AppleSdkFamily::IosSimulator => ".iOS(",
    };
    let start = contents.find(marker)? + marker.len();
    let rest = &contents[start..];
    let version_start = rest.find(".v")? + 2;
    let version = rest[version_start..]
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect::<String>();
    AppleVersion::parse(&version)
}

fn first_setting_version(contents: &str, keys: &[&str]) -> Option<AppleVersion> {
    keys.iter().find_map(|key| setting_version(contents, key))
}

fn setting_version(contents: &str, key: &str) -> Option<AppleVersion> {
    let position = contents.find(key)? + key.len();
    let value = contents[position..]
        .trim_start_matches(|character: char| matches!(character, ' ' | '\t' | ':' | '=' | '"'))
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '"' | '\'' | ',' | ';' | '#' | ')')
        })
        .next()?;
    AppleVersion::parse(value)
}

fn architecture_evidence(contents: &str) -> BTreeSet<AppleArch> {
    let mut arches = BTreeSet::new();
    if contents.contains("arm64") {
        arches.insert(AppleArch::Arm64);
    }
    if contents.contains("x86_64") {
        arches.insert(AppleArch::X86_64);
    }
    arches
}

fn strip_non_code_comments(contents: &str) -> String {
    contents
        .lines()
        .map(|line| line.split_once('#').map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

fn manifest_uses_xcframework(contents: &str) -> bool {
    let code = strip_swift_comments(contents);
    let marker = ".binaryTarget";
    let mut search_from = 0;
    while let Some(marker_start) = find_marker_outside_strings(&code, marker, search_from) {
        let after_marker = marker_start + marker.len();
        if call_argument_region(&code, after_marker)
            .is_some_and(|region| region.contains(".xcframework"))
        {
            return true;
        }
        search_from = after_marker;
    }
    false
}

fn manifest_declares_apple_platform(contents: &str) -> bool {
    // PackageDescription's platform spellings are stable API. Remove comments
    // and string literals so prose or a fixture string cannot change routing.
    let code = strip_swift_comments_and_strings(contents);
    [
        ".macOS(",
        ".iOS(",
        ".tvOS(",
        ".watchOS(",
        ".visionOS(",
        ".macCatalyst(",
    ]
    .iter()
    .any(|marker| code.contains(marker))
}

fn manifest_uses_apple_linker(contents: &str) -> bool {
    let code = strip_swift_comments(contents);
    for marker in ["linkerSettings", "unsafeFlags", ".linkedFramework"] {
        let mut search_from = 0;
        while let Some(marker_start) = find_marker_outside_strings(&code, marker, search_from) {
            let after_marker = marker_start + marker.len();
            if call_argument_region(&code, after_marker)
                .is_some_and(region_has_apple_linker_evidence)
            {
                return true;
            }
            search_from = after_marker;
        }
    }
    false
}

fn region_has_apple_linker_evidence(region: &str) -> bool {
    APPLE_LINK_MARKERS.iter().any(|flag| region.contains(flag))
        || APPLE_MODULES.iter().any(|module| {
            region.contains(&format!(".linkedFramework(\"{module}\""))
                || region.contains(&format!(".linkedFramework(name: \"{module}\""))
                || region.contains(&format!("\"{module}\""))
        })
}

fn find_marker_outside_strings(contents: &str, marker: &str, search_from: usize) -> Option<usize> {
    let mut index = search_from;
    let mut string = None;
    while index < contents.len() {
        if let Some((hashes, multiline)) = string {
            if let Some(length) = swift_string_end_at(contents, index, hashes, multiline) {
                index += length;
                string = None;
            } else {
                index += contents[index..].chars().next().map_or(1, char::len_utf8);
            }
            continue;
        }
        if let Some((hashes, multiline, length)) = swift_string_start_at(contents, index) {
            string = Some((hashes, multiline));
            index += length;
            continue;
        }
        if contents[index..].starts_with(marker) {
            return Some(index);
        }
        index += contents[index..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// Return the argument body after a manifest call/property marker. This keeps
/// linker evidence scoped to the actual `linkerSettings`/`unsafeFlags` call;
/// an unrelated prose string elsewhere in Package.swift cannot route a unit.
fn call_argument_region(contents: &str, after_marker: usize) -> Option<&str> {
    let open = contents[after_marker..]
        .char_indices()
        .find_map(|(offset, character)| {
            matches!(character, '(' | '[' | '{').then_some(after_marker + offset)
        })?;
    let opening = contents.as_bytes()[open];
    let closing = match opening {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _ => return None,
    };
    let mut depth = 0_u32;
    let mut index = open;
    let mut string = None;
    while index < contents.len() {
        if let Some((hashes, multiline)) = string {
            if let Some(length) = swift_string_end_at(contents, index, hashes, multiline) {
                index += length;
                string = None;
            } else {
                index += contents[index..].chars().next().map_or(1, char::len_utf8);
            }
            continue;
        }
        if let Some((hashes, multiline, length)) = swift_string_start_at(contents, index) {
            string = Some((hashes, multiline));
            index += length;
            continue;
        }
        let character = contents.as_bytes()[index];
        if character == opening {
            depth += 1;
        } else if contents.as_bytes()[index] == closing {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(&contents[open + 1..index]);
            }
        }
        index += 1;
    }
    None
}

fn swift_string_start_at(contents: &str, index: usize) -> Option<(usize, bool, usize)> {
    let bytes = contents.as_bytes();
    let (hashes, quote) = if bytes.get(index) == Some(&b'"') {
        (0, index)
    } else if bytes.get(index) == Some(&b'#') {
        let mut quote = index;
        while bytes.get(quote) == Some(&b'#') {
            quote += 1;
        }
        if bytes.get(quote) != Some(&b'"') {
            return None;
        }
        (quote - index, quote)
    } else {
        return None;
    };
    let multiline = bytes.get(quote..quote + 3) == Some(b"\"\"\"");
    Some((hashes, multiline, hashes + if multiline { 3 } else { 1 }))
}

fn swift_string_end_at(
    contents: &str,
    index: usize,
    hashes: usize,
    multiline: bool,
) -> Option<usize> {
    let bytes = contents.as_bytes();
    let quote_count = if multiline { 3 } else { 1 };
    let quotes_match = if multiline {
        bytes.get(index..index + 3) == Some(&b"\"\"\""[..])
    } else {
        bytes.get(index..index + 1) == Some(&b"\""[..])
    };
    if !quotes_match {
        return None;
    }
    if (0..hashes).any(|offset| bytes.get(index + quote_count + offset) != Some(&b'#')) {
        return None;
    }
    Some(quote_count + hashes)
}

fn source_imports_apple_module(contents: &str) -> bool {
    // Imports in block comments otherwise look like real declarations when
    // inspected line-by-line. Keep conditional compilation directives after
    // removing comments and literals as well, so fixture prose cannot route a
    // portable package to the Apple lane.
    let code = strip_swift_comments_and_strings(contents);
    code.lines().any(|line| {
        let line = line.trim_start();
        let imported = swift_import_module(line);
        imported.is_some_and(|module| APPLE_MODULES.contains(&module))
            || (line.starts_with("#if ") || line.starts_with("#elseif "))
                && APPLE_MODULES
                    .iter()
                    .any(|module| line.contains(&format!("canImport({module})")))
    })
}

fn swift_import_module(line: &str) -> Option<&str> {
    let mut rest = line;
    for attribute in ["@_exported ", "@testable ", "@_implementationOnly "] {
        if let Some(stripped) = rest.strip_prefix(attribute) {
            rest = stripped.trim_start();
        }
    }
    let rest = rest.strip_prefix("import ")?.trim_start();
    let mut words = rest.split_whitespace();
    let first = words.next()?;
    let module = if matches!(first, "class" | "struct" | "enum" | "func" | "var" | "let") {
        words.next()?
    } else {
        first
    };
    Some(module.split('.').next()?)
}

fn strip_swift_comments_and_strings(contents: &str) -> String {
    strip_swift_lexemes(contents, false)
}

fn strip_swift_comments(contents: &str) -> String {
    strip_swift_lexemes(contents, true)
}

/// Remove comments and optionally strings using Swift's nesting/raw-string
/// delimiters. Keeping this one lexer for manifest and source evidence avoids
/// the old false positives where `#"..."#` or `##"""..."""##` looked like
/// code after the first quote.
fn strip_swift_lexemes(contents: &str, keep_strings: bool) -> String {
    let chars = contents.chars().collect::<Vec<_>>();
    let mut block_depth = 0_u32;
    let mut string = None;
    let mut output = String::with_capacity(contents.len());
    let mut index = 0;
    while index < chars.len() {
        if let Some((hashes, multiline)) = string {
            if swift_string_terminator(&chars, index, hashes, multiline) {
                let length = string_delimiter_length(&chars, index, hashes, multiline);
                if keep_strings {
                    output.extend(&chars[index..index + length]);
                }
                index += length;
                string = None;
                continue;
            }
            if keep_strings {
                output.push(chars[index]);
            } else if chars[index] == '\n' {
                output.push('\n');
            }
            index += 1;
            continue;
        }
        if block_depth > 0 {
            if chars[index] == '/' && chars.get(index + 1) == Some(&'*') {
                index += 2;
                block_depth += 1;
            } else if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
                index += 2;
                block_depth -= 1;
            } else {
                if chars[index] == '\n' {
                    output.push('\n');
                }
                index += 1;
            }
            continue;
        }
        if chars[index] == '/' && chars.get(index + 1) == Some(&'/') {
            index += 2;
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            if index < chars.len() {
                output.push('\n');
                index += 1;
            }
            continue;
        }
        if chars[index] == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            block_depth = 1;
            continue;
        }
        if let Some((hashes, multiline, length)) = swift_string_start(&chars, index) {
            if keep_strings {
                output.extend(&chars[index..index + length]);
            }
            string = Some((hashes, multiline));
            index += length;
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

fn swift_string_start(chars: &[char], index: usize) -> Option<(usize, bool, usize)> {
    let (hashes, quote) = if chars[index] == '"' {
        (0, index)
    } else if chars[index] == '#' {
        let mut quote = index;
        while chars.get(quote) == Some(&'#') {
            quote += 1;
        }
        if chars.get(quote) != Some(&'"') {
            return None;
        }
        (quote - index, quote)
    } else {
        return None;
    };
    let multiline = quote + 3 <= chars.len()
        && chars[quote..quote + 3]
            .iter()
            .all(|character| *character == '"');
    let length = hashes + if multiline { 3 } else { 1 };
    Some((hashes, multiline, length))
}

fn swift_string_terminator(chars: &[char], index: usize, hashes: usize, multiline: bool) -> bool {
    string_delimiter_length(chars, index, hashes, multiline) > 0
}

fn string_delimiter_length(chars: &[char], index: usize, hashes: usize, multiline: bool) -> usize {
    let quote_count = if multiline { 3 } else { 1 };
    if index + quote_count > chars.len()
        || !chars[index..index + quote_count]
            .iter()
            .all(|character| *character == '"')
    {
        return 0;
    }
    if (0..hashes).any(|offset| chars.get(index + quote_count + offset) != Some(&'#')) {
        return 0;
    }
    quote_count + hashes
}

fn package_source_roots(manifest: &str, _package_root: &str) -> Vec<String> {
    let mut roots = vec!["Sources".to_owned(), "Tests".to_owned()];
    for line in manifest.lines() {
        let Some(path) = line.split_once("path:").and_then(|(_, value)| {
            value
                .trim()
                .strip_prefix('"')
                .and_then(|value| value.split('"').next())
        }) else {
            continue;
        };
        if !path.is_empty()
            && !path.starts_with('.')
            && !path.starts_with('/')
            && !path.contains("..")
            && !roots.iter().any(|root| root == path)
        {
            roots.push(path.to_owned());
        }
    }
    roots
}

fn is_package_source(
    file: &str,
    package_root: &str,
    package_roots: &[String],
    source_roots: &[String],
) -> bool {
    if !Path::new(file)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("swift"))
    {
        return false;
    }
    let package_prefix = if package_root == "." {
        String::new()
    } else {
        format!("{package_root}/")
    };
    if !file.starts_with(&package_prefix) {
        return false;
    }
    let relative = file.strip_prefix(&package_prefix).unwrap_or(file);
    let declared_source = source_roots
        .iter()
        .any(|source| relative == source || relative.starts_with(&format!("{source}/")));
    if !declared_source {
        return false;
    }
    // A repository can contain nested packages. Do not let a child package's
    // imports turn its parent package's unit Apple-bound (or vice versa).
    !package_roots.iter().any(|other| {
        other != package_root
            && other.starts_with(&package_prefix)
            && file.starts_with(&format!("{other}/"))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        manifest_declares_apple_platform, manifest_uses_apple_linker, manifest_uses_xcframework,
        source_imports_apple_module,
    };

    #[test]
    fn recognizes_real_native_shapes_without_repo_names() {
        assert!(manifest_declares_apple_platform(
            "let package = Package(platforms: [.macOS(.v26)])"
        ));
        assert!(source_imports_apple_module(
            "import Darwin\nimport MachO\nimport MetricKit\n"
        ));
        assert!(source_imports_apple_module("import SwiftUI\n"));
        assert!(source_imports_apple_module(
            "@_implementationOnly import CoreGraphics.CGFloat\n"
        ));
        assert!(source_imports_apple_module(
            "@_exported import SwiftUI\nimport class AppKit.NSView\n"
        ));
        assert!(manifest_uses_apple_linker(".linkedFramework(\"Security\")"));
        assert!(manifest_uses_apple_linker(
            ".target(name: \"app\", linkerSettings: [.unsafeFlags([\"-framework\", \"AppKit\"])])"
        ));
        assert!(manifest_uses_xcframework(
            ".binaryTarget(name: \"Bridge\", path: \"Bridge.xcframework\")"
        ));
    }

    #[test]
    fn keeps_portable_swift_sources_portable() {
        assert!(!manifest_declares_apple_platform(
            "let package = Package(name: \"portable\")"
        ));
        assert!(!manifest_uses_apple_linker(
            ".linkedLibrary(\"portable_ffi\")"
        ));
        assert!(!manifest_uses_apple_linker(
            "let note = \"-framework AppKit\"\n.target(name: \"app\", linkerSettings: [.linkedLibrary(\"portable_ffi\")])"
        ));
        assert!(!manifest_uses_apple_linker(
            "let note = \"\"\"\n.linkedFramework(\"AppKit\")\n\"\"\"\n.target(name: \"app\", linkerSettings: [.linkedLibrary(\"portable_ffi\")])"
        ));
        assert!(!manifest_uses_xcframework(
            "// .binaryTarget(name: \"Bridge\", path: \"Bridge.xcframework\")\nlet note = \".binaryTarget Bridge.xcframework\""
        ));
        assert!(!manifest_uses_xcframework(
            "let note = #\".binaryTarget(name: 'Bridge', path: 'Bridge.xcframework')\"#"
        ));
        assert!(!source_imports_apple_module(
            "import Foundation\nimport Logging\n"
        ));
    }

    #[test]
    fn comments_do_not_create_platform_evidence() {
        assert!(!manifest_declares_apple_platform(
            "// .macOS(.v26) is only documentation\n"
        ));
        assert!(!manifest_declares_apple_platform(
            "let note = \".macOS(.v26) is only data\"\n"
        ));
        assert!(!source_imports_apple_module(
            "let note = \"import SwiftUI\"\n"
        ));
        assert!(!source_imports_apple_module(
            "/*\nimport SwiftUI\n#if canImport(Darwin)\n*/\n"
        ));
        assert!(!source_imports_apple_module(
            "let note = ##\"\"\"\nimport SwiftUI\n\"\"\"##\n"
        ));
    }
}

//! Static Swift package capability evidence.
//!
//! `SwiftPM`'s language name does not establish portability. This module reads
//! only manifests and tracked Swift source paths, then reports evidence that a
//! package needs an Apple SDK. It never evaluates Package.swift or project
//! code.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::native_contract::{
    AppleArch, AppleNativeContract, AppleSdkFamily, AppleVersion,
};

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
    let xcframework = manifest_uses_xcframework(&manifest);
    let apple = xcframework
        || manifest_declares_apple_platform(&manifest)
        || manifest_uses_apple_linker(&manifest)
        || files
            .iter()
            .filter(|file| is_package_source(file, package_root, package_roots))
            .any(|file| {
                fs::read_to_string(root.join(file))
                    .is_ok_and(|contents| source_imports_apple_module(&contents))
            });
    let project_configs = files
        .iter()
        .filter(|file| is_package_project_config(file, package_root, package_roots))
        .filter_map(|file| fs::read_to_string(root.join(file)).ok())
        .collect::<Vec<_>>();
    let native = apple.then(|| {
        let mut contract = manifest_native_contract(&manifest);
        for project in &project_configs {
            contract = merge_contract(contract, project_native_contract(project));
        }
        contract
    });
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
        AppleSdkFamily::Macos => first_setting_version(
            &code,
            &["MACOSX_DEPLOYMENT_TARGET", "macOS", "macos"],
        ),
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
    Some(contract)
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

fn merge_contract(
    left: AppleNativeContract,
    right: Option<AppleNativeContract>,
) -> AppleNativeContract {
    let Some(right) = right else {
        return left;
    };
    match left.merge(&right) {
        Ok(merged) => merged,
        Err(error) => {
            let mut conflicted = left;
            conflicted.conflicts.push(error);
            conflicted
        }
    }
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
            character.is_whitespace() || matches!(character, '"' | ',' | '#')
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
    let mut in_string = false;
    let mut in_multiline_string = false;
    let mut escaped = false;
    let mut skip_quotes_until = 0;
    for (offset, character) in contents[search_from..].char_indices() {
        let index = search_from + offset;
        if index < skip_quotes_until {
            continue;
        }
        if in_multiline_string {
            if contents[index..].starts_with("\"\"\"") {
                in_multiline_string = false;
                skip_quotes_until = index + 3;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if contents[index..].starts_with("\"\"\"") {
            in_multiline_string = true;
            skip_quotes_until = index + 3;
            continue;
        }
        if character == '"' {
            in_string = true;
        } else if contents[index..].starts_with(marker) {
            return Some(index);
        }
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
    let mut in_string = false;
    let mut in_multiline_string = false;
    let mut escaped = false;
    let mut skip_quotes_until = 0;
    for (offset, character) in contents[open..].char_indices() {
        let index = open + offset;
        if index < skip_quotes_until {
            continue;
        }
        if in_multiline_string {
            if contents[index..].starts_with("\"\"\"") {
                in_multiline_string = false;
                skip_quotes_until = index + 3;
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if contents[index..].starts_with("\"\"\"") {
            in_multiline_string = true;
            skip_quotes_until = index + 3;
            continue;
        }
        if character == '"' {
            in_string = true;
            continue;
        }
        if contents.as_bytes()[index] == opening {
            depth += 1;
        } else if contents.as_bytes()[index] == closing {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(&contents[open + 1..index]);
            }
        }
    }
    None
}

fn source_imports_apple_module(contents: &str) -> bool {
    // Imports in block comments otherwise look like real declarations when
    // inspected line-by-line. Keep conditional compilation directives after
    // removing comments and literals as well, so fixture prose cannot route a
    // portable package to the Apple lane.
    let code = strip_swift_comments_and_strings(contents);
    code.lines().any(|line| {
        let line = line.trim_start();
        let imported = line
            .strip_prefix("import ")
            .or_else(|| line.strip_prefix("@testable import "))
            .or_else(|| line.strip_prefix("@_implementationOnly import "))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|module| module.split('.').next());
        imported.is_some_and(|module| APPLE_MODULES.contains(&module))
            || (line.starts_with("#if ") || line.starts_with("#elseif "))
                && APPLE_MODULES
                    .iter()
                    .any(|module| line.contains(&format!("canImport({module})")))
    })
}

fn strip_swift_comments_and_strings(contents: &str) -> String {
    let mut output = String::with_capacity(contents.len());
    let mut chars = contents.chars().peekable();
    let mut block_depth = 0_u32;
    let mut in_string = false;
    let mut in_multiline_string = false;
    while let Some(character) = chars.next() {
        if block_depth > 0 {
            if character == '/' && chars.peek() == Some(&'*') {
                chars.next();
                block_depth += 1;
            } else if character == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_depth -= 1;
            } else if character == '\n' {
                output.push('\n');
            }
            continue;
        }
        if in_multiline_string {
            if character == '"' && chars.peek() == Some(&'"') {
                let mut quotes = chars.clone();
                quotes.next();
                if quotes.peek() == Some(&'"') {
                    chars.next();
                    chars.next();
                    in_multiline_string = false;
                    output.push_str("\"\"\"");
                    continue;
                }
            }
            if character == '\n' {
                output.push('\n');
            }
            continue;
        }
        if in_string {
            if character == '\\' {
                chars.next();
            } else if character == '"' {
                in_string = false;
            } else if character == '\n' {
                output.push('\n');
            }
            continue;
        }
        if character == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    output.push('\n');
                    break;
                }
            }
        } else if character == '/' && chars.peek() == Some(&'*') {
            chars.next();
            block_depth = 1;
        } else if character == '"' && chars.peek() == Some(&'"') {
            let mut quotes = chars.clone();
            quotes.next();
            if quotes.peek() == Some(&'"') {
                chars.next();
                chars.next();
                in_multiline_string = true;
            } else {
                in_string = true;
            }
        } else if character == '"' {
            in_string = true;
        } else {
            output.push(character);
        }
    }
    output
}

fn strip_swift_comments(contents: &str) -> String {
    let mut output = String::with_capacity(contents.len());
    let mut chars = contents.chars().peekable();
    let mut block_depth = 0_u32;
    let mut in_string = false;
    let mut in_multiline_string = false;
    while let Some(character) = chars.next() {
        if block_depth > 0 {
            if character == '/' && chars.peek() == Some(&'*') {
                chars.next();
                block_depth += 1;
            } else if character == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_depth -= 1;
            } else if character == '\n' {
                output.push('\n');
            }
            continue;
        }
        if in_multiline_string {
            if character == '"' && chars.peek() == Some(&'"') {
                let mut quotes = chars.clone();
                quotes.next();
                if quotes.peek() == Some(&'"') {
                    chars.next();
                    chars.next();
                    in_multiline_string = false;
                    output.push_str("\"\"\"");
                    continue;
                }
            }
            output.push(character);
            continue;
        }
        if in_string {
            if character == '\\' {
                output.push(character);
                if let Some(escaped) = chars.next() {
                    output.push(escaped);
                }
            } else if character == '"' {
                in_string = false;
                output.push(character);
            } else {
                output.push(character);
            }
            continue;
        }
        if character == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    output.push('\n');
                    break;
                }
            }
        } else if character == '/' && chars.peek() == Some(&'*') {
            chars.next();
            block_depth = 1;
        } else if character == '"' && chars.peek() == Some(&'"') {
            let mut quotes = chars.clone();
            quotes.next();
            if quotes.peek() == Some(&'"') {
                chars.next();
                chars.next();
                in_multiline_string = true;
                output.push_str("\"\"\"");
            } else {
                in_string = true;
                output.push(character);
            }
        } else if character == '"' {
            in_string = true;
            output.push(character);
        } else {
            output.push(character);
        }
    }
    output
}

fn is_package_source(file: &str, package_root: &str, package_roots: &[String]) -> bool {
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
    // A repository can contain nested packages. Do not let a child package's
    // imports turn its parent package's unit Apple-bound (or vice versa).
    !package_roots.iter().any(|other| {
        other != package_root
            && other.starts_with(&package_prefix)
            && file.starts_with(&format!("{other}/"))
    })
}

fn is_package_project_config(file: &str, package_root: &str, package_roots: &[String]) -> bool {
    if !file.ends_with("project.yml") && !file.ends_with("project.yaml") {
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
    }
}

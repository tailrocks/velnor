//! Swift detector: Swift packages and Xcode shared schemes.

use std::fs;
use std::path::Path;

use super::file_walk::{
    files_named, join_repo_path, path_prefix, resolve_repo_path, roots_for_manifests,
};
use super::{unit, RepositoryShape, ScanContext};
use crate::{
    identifier_suffix, parent_path, shell_change_dir, shell_quote, CachePurpose, CacheSpec, Unit,
    UnitKind, ValidationPhase,
};

const SWIFT_FORMAT_CONFIG: &str = ".swift-format";
const SWIFT_LINT_CONFIGS: [&str; 2] = [".swiftlint.yml", ".swiftlint.yaml"];

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

fn swift_package_unit(package_root: &str) -> Unit {
    let prefix = path_prefix(package_root);
    let command_prefix = shell_change_dir(package_root);
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
        vec![
            format!("{command_prefix}swift build"),
            format!("{command_prefix}swift test --parallel"),
        ],
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
    result.phases = vec![ValidationPhase::SwiftBuild, ValidationPhase::SwiftTest];
    // A SwiftPM package is portable: it verifies wherever its toolchain
    // provisions, on the lane's default executor. Only Xcode scheme work
    // below carries an Apple need.
    result.platform = crate::platform::PlatformRequirement::swift_package();
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
            github_pr_commands: None,
            github_full_commands: None,
            velnor_pr_commands: None,
            velnor_full_commands: None,
            phases: vec![ValidationPhase::SwiftBuild, ValidationPhase::SwiftTest],
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
    contents.contains(".binaryTarget") && contents.contains(".xcframework")
}

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    for package_root in roots_for_manifests(&files_named(context.files, "Package.swift")) {
        shape.detected.push(format!("swift-package:{package_root}"));
        let mut unit = swift_package_unit(&package_root);
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
}

#[cfg(test)]
mod tests {
    use super::{
        apply_swift_style_checks, swift_package_unit, swift_style_checks, xcode_scheme_units,
    };
    use crate::ValidationPhase;
    use std::fs;

    #[test]
    fn swift_style_fixture_detects_nearest_configs_and_emits_commands() {
        let files = vec![
            ".swift-format".to_owned(),
            ".swiftlint.yml".to_owned(),
            "clients/app/Package.swift".to_owned(),
            "clients/app/Sources/App.swift".to_owned(),
            "clients/app/.swiftlint.yaml".to_owned(),
        ];
        let mut unit = swift_package_unit("clients/app");
        apply_swift_style_checks(&mut unit, &files);
        assert_eq!(
            unit.pr_commands,
            vec![
                "cd -- 'clients/app' && swift format lint --configuration '../../.swift-format' --recursive --strict .".to_owned(),
                "cd -- 'clients/app' && swiftlint lint --config '.swiftlint.yaml' --strict".to_owned(),
                "cd -- 'clients/app' && swift build".to_owned(),
                "cd -- 'clients/app' && swift test --parallel".to_owned(),
            ]
        );
        assert_eq!(
            unit.phases,
            vec![
                ValidationPhase::SwiftFormat,
                ValidationPhase::SwiftLint,
                ValidationPhase::SwiftBuild,
                ValidationPhase::SwiftTest,
            ]
        );
        assert!(unit.watch.contains(&".swift-format".to_owned()));
        assert!(unit
            .watch
            .contains(&"clients/app/.swiftlint.yaml".to_owned()));
        assert!(
            swift_style_checks(".", &["clients/app/.swift-format".to_owned()]).is_empty(),
            "a root unit must not inherit a nested package config"
        );
    }

    #[test]
    fn shared_scheme_style_fixture_adds_checks_before_xcodebuild() {
        let root =
            std::env::temp_dir().join(format!("velnor-root-swift-style-{}", std::process::id()));
        let scheme_dir = root.join("App.xcodeproj/xcshareddata/xcschemes");
        assert!(fs::create_dir_all(&scheme_dir).is_ok());
        assert!(fs::write(
            root.join("App.xcodeproj/project.pbxproj"),
            "// generic fixture\n"
        )
        .is_ok());
        assert!(fs::write(
            scheme_dir.join("App.xcscheme"),
            "<Scheme><BuildAction/><TestAction/></Scheme>\n"
        )
        .is_ok());
        let files = vec![
            ".swift-format".to_owned(),
            ".swiftlint.yml".to_owned(),
            "App.xcodeproj/project.pbxproj".to_owned(),
            "App.xcodeproj/xcshareddata/xcschemes/App.xcscheme".to_owned(),
        ];
        let units = xcode_scheme_units(&root, &files);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].pr_commands.len(), 4);
        assert!(units[0].pr_commands[0].contains("swift format lint"));
        assert!(units[0].pr_commands[1].contains("swiftlint lint"));
        assert!(units[0].pr_commands[2].contains("xcodebuild"));
        assert_eq!(
            units[0].phases,
            vec![
                ValidationPhase::SwiftFormat,
                ValidationPhase::SwiftLint,
                ValidationPhase::SwiftBuild,
                ValidationPhase::SwiftTest,
            ]
        );
        let _ = fs::remove_dir_all(root);
    }
}

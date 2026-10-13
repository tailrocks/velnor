//! Swift detector: Swift packages and Xcode shared schemes.

use std::fs;
use std::path::Path;

use super::file_walk::{
    files_named, join_repo_path, path_prefix, resolve_repo_path, roots_for_manifests,
};
use super::{unit, RepositoryShape, ScanContext};
use crate::{
    identifier_suffix, parent_path, shell_change_dir, shell_quote, CacheSpec, Unit, UnitKind,
};

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
        }),
    );
    result.id = format!("swift-package-{}", identifier_suffix(package_root));
    result.label = if package_root == "." {
        "Swift package".to_owned()
    } else {
        format!("Swift package ({package_root})")
    };
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
            depends_on: Vec::new(),
            cache: Some(CacheSpec {
                key_files: cache_key_files,
                paths: vec!["~/Library/Developer/Xcode/DerivedData".to_owned()],
            }),
            tool_version: None,
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
        shape.units.push(swift_package_unit(&package_root));
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

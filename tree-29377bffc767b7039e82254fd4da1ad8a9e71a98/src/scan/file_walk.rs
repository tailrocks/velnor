//! File walk and repository-path helpers shared by every detector.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};

use super::{RepositoryShape, ScanContext};
use crate::{parent_path, GeneratorError};

pub(crate) fn repository_files(root: &Path) -> Result<Vec<String>, GeneratorError> {
    if !root.is_dir() {
        return Err(GeneratorError::usage(format!(
            "not a repository directory: {}",
            root.display()
        )));
    }
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read directory", directory, &error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| GeneratorError::io("read directory entry", directory, &error))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let kind = entry
            .file_type()
            .map_err(|error| GeneratorError::io("read file type", &path, &error))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            // Generated `.github` content is output, not project input.
            if matches!(
                name.as_ref(),
                ".git"
                    | ".github"
                    | ".output"
                    | "target"
                    | "node_modules"
                    | ".build"
                    | ".gradle"
                    | ".terraform"
                    | "dist"
                    | "coverage"
            ) {
                continue;
            }
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                GeneratorError::usage(format!("make repository path relative: {error}"))
            })?;
            files.push(normalize_relative_path(relative)?);
        }
    }
    Ok(())
}

fn normalize_relative_path(path: &Path) -> Result<String, GeneratorError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part.chars().any(char::is_control) {
                    return Err(GeneratorError::usage(format!(
                        "unsafe control character in repository path: {}",
                        path.display()
                    )));
                }
                parts.push(part.into_owned());
            }
            Component::CurDir => (),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(GeneratorError::usage(format!(
                    "unsafe repository path: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

pub(crate) fn files_named(files: &[String], name: &str) -> Vec<String> {
    files
        .iter()
        .filter(|file| file.rsplit('/').next() == Some(name) && !is_test_support_path(file))
        .cloned()
        .collect()
}

/// Cargo target directories hold tests and fixtures, not shippable
/// packages: a manifest nested beneath one describes test support, never a
/// unit CI should verify on its own. Scanned relative to the repository
/// root, so manifests at a scanned root itself are unaffected.
pub(crate) fn is_test_support_path(path: &str) -> bool {
    path.split('/')
        .any(|segment| matches!(segment, "tests" | "benches"))
}

fn is_ignored_test_manifest(path: &str) -> bool {
    if !is_test_support_path(path) {
        return false;
    }
    match path.rsplit('/').next() {
        Some("Cargo.toml" | "package.json" | "Package.swift") => true,
        Some(name) if name.starts_with("Dockerfile") => true,
        Some("settings.gradle" | "settings.gradle.kts" | "build.gradle" | "build.gradle.kts") => {
            true
        }
        Some(_) | None => false,
    }
}

pub(crate) fn has_extension(file: &str, extension: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}

pub(crate) fn roots_for_manifests(manifests: &[String]) -> Vec<String> {
    let mut roots = BTreeSet::new();
    for manifest in manifests {
        roots.insert(parent_path(manifest));
    }
    roots.into_iter().collect()
}

pub(crate) fn join_repo_path(root: &str, child: &str) -> String {
    if root == "." {
        child.to_owned()
    } else {
        format!("{root}/{child}")
    }
}

pub(crate) fn path_prefix(root: &str) -> String {
    if root == "." {
        String::new()
    } else {
        format!("{root}/")
    }
}

pub(crate) fn resolve_repo_path(root: &str, relative: &str) -> Option<String> {
    let mut parts = if root == "." {
        Vec::new()
    } else {
        root.split('/').map(str::to_owned).collect::<Vec<_>>()
    };
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(component) => parts.push(component.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    })
}

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    if context
        .files
        .iter()
        .any(|file| is_ignored_test_manifest(file))
    {
        shape.limitations.push(
            "Manifests nested under tests/ or benches/ directories are treated as test fixtures and ignored.".to_owned(),
        );
    }
}

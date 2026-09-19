//! File walk and repository-path helpers shared by every detector.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};
use std::process::Command;

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::{RepositoryShape, ScanContext};
use crate::s2::{parent_path, GeneratorError};

/// Generator-owned artifacts must not feed back into the next scan pass.
const GENERATOR_OWNED_SCAN_FILES: &[&str] = &["config/fleet/velnor-host.env"];

pub(crate) fn repository_files(
    root: &Path,
    exclude: &[String],
) -> Result<Vec<String>, GeneratorError> {
    if !root.is_dir() {
        return Err(GeneratorError::usage(format!(
            "not a repository directory: {}",
            root.display()
        )));
    }
    // Generation must stay a function of the committed repository, not of the
    // checkout: untracked CI runtime artifacts, scratch files, and the `.git`
    // file of a linked worktree would otherwise enter the scan and make the
    // recorded scan input depend on the environment that ran the generator.
    // Prefer the git index and keep the physical walk only for directories
    // that are not inside a git repository (synthetic test targets).
    let mut files = if let Some(files) = tracked_files(root)? {
        files
    } else {
        let mut files = Vec::new();
        let mut action_roots = BTreeSet::new();
        discover_action_roots(root, root, &mut action_roots)?;
        collect_files(root, root, &mut files, &action_roots)?;
        files
    };
    let excludes = exclude_set(exclude)?;
    files.retain(|file| {
        !excludes.is_match(file) && !GENERATOR_OWNED_SCAN_FILES.contains(&file.as_str())
    });
    files.sort();
    Ok(files)
}

pub(crate) fn exclude_set(patterns: &[String]) -> Result<GlobSet, GeneratorError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|error| {
            GeneratorError::usage(format!(
                "[scan] exclude is not a valid glob: {pattern}: {error}"
            ))
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|error| {
        GeneratorError::usage(format!("[scan] exclude could not be compiled: {error}"))
    })
}

/// Tracked files under `root`, relative to it. `Ok(None)` means `root` is not
/// inside a git repository (no `git` binary, not a work tree, or a work tree
/// with no tracked files under `root`) and the physical walk should decide.
fn inside_git_work_tree(root: &Path) -> bool {
    Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true"
        })
}

fn tracked_files(root: &Path) -> Result<Option<Vec<String>>, GeneratorError> {
    let in_git = inside_git_work_tree(root);
    let Ok(output) = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z"])
        .output()
    else {
        if in_git {
            return Err(GeneratorError::usage(
                "git ls-files could not run inside a git work tree; scan will not walk the filesystem",
            ));
        }
        return Ok(None);
    };
    if !output.status.success() {
        if in_git {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(GeneratorError::usage(format!(
                "git ls-files failed inside a git work tree; scan will not walk the filesystem: {stderr}"
            )));
        }
        return Ok(None);
    }
    let mut tracked = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let relative = std::str::from_utf8(raw).map_err(|error| {
            GeneratorError::usage(format!("repository path is not utf-8: {error}"))
        })?;
        let relative = Path::new(relative);
        let Some(metadata) = tracked_file_metadata(root, relative) else {
            // Staged but deleted in the work tree: the detectors cannot read
            // it, so it cannot inform generation.
            continue;
        };
        if metadata.is_dir() {
            // Submodule git links and tracked symlinks are skipped exactly
            // like their walked counterparts.
            continue;
        }
        tracked.push(normalize_relative_path(relative)?);
    }
    let action_roots = tracked
        .iter()
        .filter(|file| {
            matches!(file.rsplit('/').next(), Some("action.yml" | "action.yaml"))
                && !is_test_support_path(file)
        })
        .map(|file| parent_path(file))
        .collect::<BTreeSet<_>>();
    let mut files = Vec::new();
    for normalized in tracked {
        // Generated `.github` content is output, not project input, and the
        // remaining directories are tool or package-manager output. The
        // `dist` exception is deliberately narrow: only a checked-in `dist`
        // subtree belonging to a detected local action is project input.
        if is_excluded_path(&normalized, &action_roots) {
            continue;
        }
        files.push(normalized);
    }
    if files.is_empty() && !in_git {
        return Ok(None);
    }
    Ok(Some(files))
}

/// Workflow paths are read only to discover local `uses` references. They are
/// deliberately not merged into the repository product file set.
pub(crate) fn workflow_reference_files(
    root: &Path,
    exclude: &[String],
) -> Result<Vec<String>, GeneratorError> {
    let excludes = exclude_set(exclude)?;
    let mut paths = if inside_git_work_tree(root) {
        tracked_reference_paths(root, ".github/workflows", true)?
    } else {
        physical_reference_paths(root, ".github/workflows", true)?
    };
    paths.retain(|path| !excludes.is_match(path));
    paths.retain(|path| {
        matches!(
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("yml" | "yaml")
        )
    });
    paths.sort();
    let mut workflows = Vec::new();
    for path in paths {
        let workflow_path = root.join(&path);
        let contents = fs::read_to_string(&workflow_path).map_err(|error| {
            GeneratorError::io("read workflow reference context", &workflow_path, &error)
        })?;
        if contents
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("# Generated by "))
        {
            continue;
        }
        workflows.push(path);
    }
    Ok(workflows)
}

/// Read only the tree of an explicitly selected local action. The ordinary
/// walk excludes `.github`, so workflow-selected actions there need this
/// narrow path without turning generated workflow files into scan inputs.
pub(crate) fn selected_action_files(
    root: &Path,
    action_root: &str,
    exclude: &[String],
) -> Result<Vec<String>, GeneratorError> {
    if action_root == "." {
        return Ok(Vec::new());
    }
    let relative = Path::new(action_root);
    let mut directory = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(GeneratorError::usage(format!(
                "unsafe local GitHub Action directory: {action_root}"
            )));
        };
        directory.push(component);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(GeneratorError::usage(format!(
                    "local GitHub Action directory `{action_root}` crosses a symlink"
                )));
            }
            Ok(metadata) if !metadata.is_dir() => return Ok(Vec::new()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(GeneratorError::io(
                    "inspect local GitHub Action directory",
                    &directory,
                    &error,
                ));
            }
        }
    }

    let excludes = exclude_set(exclude)?;
    let mut files = if inside_git_work_tree(root) {
        tracked_reference_paths(root, action_root, false)?
    } else {
        physical_reference_paths(root, action_root, false)?
    };
    files.retain(|file| file != action_root && !excludes.is_match(file));
    files.sort();
    Ok(files)
}

/// List regular, readable repository paths under a directory. With `direct`
/// set, only immediate children are returned; this matches GitHub's workflow
/// file layout. Symlinks are never followed.
fn tracked_reference_paths(
    root: &Path,
    directory: &str,
    direct: bool,
) -> Result<Vec<String>, GeneratorError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| GeneratorError::io("run git ls-files", root, &error))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(GeneratorError::usage(format!(
            "git ls-files failed while reading local action references: {stderr}"
        )));
    }
    let prefix = format!("{directory}/");
    let mut paths = Vec::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let raw = std::str::from_utf8(raw).map_err(|error| {
            GeneratorError::usage(format!("repository path is not utf-8: {error}"))
        })?;
        if !raw.starts_with(&prefix) {
            continue;
        }
        let suffix = &raw[prefix.len()..];
        if direct && suffix.contains('/') {
            continue;
        }
        let Some(metadata) = tracked_file_metadata(root, Path::new(raw)) else {
            continue;
        };
        if metadata.is_file() {
            let normalized = normalize_relative_path(Path::new(raw))?;
            if !direct && is_excluded_selected_action_path(&normalized, directory) {
                continue;
            }
            paths.push(normalized);
        }
    }
    Ok(paths)
}

/// Inspect every path component without traversing a symlinked parent.
fn tracked_file_metadata(root: &Path, relative: &Path) -> Option<fs::Metadata> {
    let mut path = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return None;
        };
        path.push(component);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if metadata.file_type().is_symlink() || components.peek().is_some() && !metadata.is_dir() {
            return None;
        }
        if components.peek().is_none() {
            return Some(metadata);
        }
    }
    None
}

fn physical_reference_paths(
    root: &Path,
    directory: &str,
    direct: bool,
) -> Result<Vec<String>, GeneratorError> {
    let mut directory_path = root.to_path_buf();
    for component in Path::new(directory).components() {
        let Component::Normal(component) = component else {
            return Err(GeneratorError::usage(format!(
                "unsafe local reference directory: {directory}"
            )));
        };
        directory_path.push(component);
        match fs::symlink_metadata(&directory_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(Vec::new()),
            Ok(metadata) if !metadata.is_dir() => return Ok(Vec::new()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(GeneratorError::io(
                    "inspect local reference directory",
                    &directory_path,
                    &error,
                ));
            }
        }
    }
    let entries = match fs::read_dir(&directory_path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(GeneratorError::io(
                "read local action reference directory",
                &directory_path,
                &error,
            ));
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            GeneratorError::io("read local action reference entry", &directory_path, &error)
        })?;
        let entry_path = entry.path();
        let kind = entry.file_type().map_err(|error| {
            GeneratorError::io(
                "read local action reference entry type",
                &entry_path,
                &error,
            )
        })?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_file() {
            let relative = entry_path.strip_prefix(root).map_err(|error| {
                GeneratorError::usage(format!("make local reference path relative: {error}"))
            })?;
            paths.push(normalize_relative_path(relative)?);
        } else if kind.is_dir() && !direct {
            let name = entry.file_name();
            if is_excluded_directory(&name.to_string_lossy()) && name.to_string_lossy() != "dist" {
                continue;
            }
            collect_reference_tree(root, &entry_path, &mut paths)?;
        }
    }
    Ok(paths)
}

fn collect_reference_tree(
    root: &Path,
    directory: &Path,
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read selected local action", directory, &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            GeneratorError::io("read selected local action entry", directory, &error)
        })?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| {
            GeneratorError::io("read selected local action entry type", &path, &error)
        })?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            let name = entry.file_name();
            if is_excluded_directory(&name.to_string_lossy()) && name.to_string_lossy() != "dist" {
                continue;
            }
            collect_reference_tree(root, &path, files)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                GeneratorError::usage(format!("make selected local action path relative: {error}"))
            })?;
            files.push(normalize_relative_path(relative)?);
        }
    }
    Ok(())
}

fn is_excluded_selected_action_path(path: &str, action_root: &str) -> bool {
    let prefix = format!("{action_root}/");
    let relative = path.strip_prefix(&prefix).unwrap_or(path);
    let components = relative.split('/').collect::<Vec<_>>();
    components
        .iter()
        .take(components.len().saturating_sub(1))
        .any(|component| is_excluded_directory(component) && *component != "dist")
}

/// Generated `.github` content is output, not project input; the remaining
/// directories are tool or package-manager output.
fn is_excluded_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".github"
            | ".github-gen"
            | ".output"
            | "target"
            | "build"
            | "node_modules"
            | ".build"
            | ".gradle"
            | ".terraform"
            | "dist"
            | "coverage"
    )
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<String>,
    action_roots: &BTreeSet<String>,
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
            let relative = path
                .strip_prefix(root)
                .map_err(|error| {
                    GeneratorError::usage(format!("make directory path relative: {error}"))
                })
                .and_then(normalize_relative_path)?;
            if is_excluded_directory(name.as_ref())
                && !(name == "dist" && path_is_inside_action(&relative, action_roots))
            {
                continue;
            }
            collect_files(root, &path, files, action_roots)?;
        } else if kind.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                GeneratorError::usage(format!("make repository path relative: {error}"))
            })?;
            let normalized = normalize_relative_path(relative)?;
            if !is_excluded_path(&normalized, action_roots) {
                files.push(normalized);
            }
        }
    }
    Ok(())
}

/// Find local action roots before the physical walk applies package-output
/// exclusions. This permits an action's checked-in `dist/` entrypoint without
/// making every repository `dist/` file generation input.
fn discover_action_roots(
    root: &Path,
    directory: &Path,
    action_roots: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read directory", directory, &error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| GeneratorError::io("read directory entry", directory, &error))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|error| GeneratorError::io("read file type", &path, &error))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            let name = entry.file_name();
            // `.github` is intentionally never scanned as project input, but
            // `dist` must be traversed here so a local action there can opt in
            // its own metadata and entrypoints.
            if is_excluded_directory(&name.to_string_lossy()) && name != "dist" {
                continue;
            }
            discover_action_roots(root, &path, action_roots)?;
        } else if kind.is_file()
            && matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("action.yml" | "action.yaml")
            )
        {
            let relative = path.strip_prefix(root).map_err(|error| {
                GeneratorError::usage(format!("make action path relative: {error}"))
            })?;
            let relative = normalize_relative_path(relative)?;
            if !is_test_support_path(&relative) {
                action_roots.insert(parent_path(&relative));
            }
        }
    }
    Ok(())
}

fn path_is_inside_action(path: &str, action_roots: &BTreeSet<String>) -> bool {
    action_roots.iter().any(|root| {
        root == "."
            || path == root
            || path
                .strip_prefix(root)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn is_excluded_path(path: &str, action_roots: &BTreeSet<String>) -> bool {
    let components = path.split('/').collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        if index + 1 == components.len() {
            break;
        }
        if is_excluded_directory(component)
            && !(component == &"dist" && path_is_inside_action(path, action_roots))
        {
            return true;
        }
    }
    false
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

#[cfg(test)]
mod tests {
    use super::{repository_files, selected_action_files, workflow_reference_files};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_fail<T: std::fmt::Debug, E: std::fmt::Display>(
        result: Result<T, E>,
        context: &str,
    ) -> String {
        match result {
            Err(error) => error.to_string(),
            Ok(value) => panic!("{context}: {value:?}"),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-file-walk-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        must(fs::create_dir_all(&root), "create scratch directory");
        root
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "velnor-workflow")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "velnor-workflow")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .unwrap_or_else(|error| panic!("run git: {error}"));
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn untracked_files_stay_out_of_a_git_repository_scan() {
        let root = scratch("tracked");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        must(
            fs::write(root.join("tracked.txt"), "tracked"),
            "write tracked file",
        );
        git(&root, &["add", "tracked.txt"]);
        must(
            fs::write(root.join("untracked.txt"), "untracked"),
            "write untracked file",
        );
        must(
            fs::create_dir_all(root.join("scratch")),
            "create untracked directory",
        );
        must(
            fs::write(root.join("scratch/note.txt"), "note"),
            "write nested untracked file",
        );
        git(&root, &["commit", "-qm", "tracked"]);

        let files = must(repository_files(&root, &[]), "scan tracked repository");
        assert_eq!(files, vec!["tracked.txt".to_owned()]);
    }

    #[test]
    fn git_work_tree_does_not_filesystem_walk_when_ls_files_fails() {
        let root = scratch("ls-files-fail");
        git(&root, &["init", "-q"]);
        must(
            fs::write(root.join("tracked.txt"), "tracked"),
            "write tracked",
        );
        git(&root, &["add", "tracked.txt"]);
        git(&root, &["commit", "-qm", "tracked"]);
        must(
            fs::write(root.join("untracked.txt"), "untracked"),
            "write untracked",
        );
        // Mode 000 does not stop root from reading the index, and Velnor
        // job containers run as root. Corrupt bytes fail `ls-files` for
        // every uid while `rev-parse --is-inside-work-tree` still succeeds.
        let index = root.join(".git/index");
        must(fs::write(&index, b"not-a-git-index"), "corrupt git index");
        let result = repository_files(&root, &[]);
        let error = must_fail(result, "filesystem walk ran after git ls-files failed");
        assert!(
            error.contains("git ls-files"),
            "must fail closed on the index: {error}"
        );
    }

    #[test]
    fn directories_outside_git_fall_back_to_the_physical_walk() {
        let root = scratch("untracked");
        must(fs::write(root.join("present.txt"), "present"), "write file");
        must(
            fs::create_dir_all(root.join("nested")),
            "create nested directory",
        );
        must(
            fs::write(root.join("nested/deep.txt"), "deep"),
            "write nested file",
        );

        let mut files = must(repository_files(&root, &[]), "scan plain directory");
        files.sort();
        assert_eq!(
            files,
            vec!["nested/deep.txt".to_owned(), "present.txt".to_owned()]
        );
    }

    #[test]
    fn only_local_action_dist_is_project_input() {
        let root = scratch("action-dist");
        must(
            fs::create_dir_all(root.join("actions/example/dist")),
            "create action dist",
        );
        must(
            fs::write(
                root.join("actions/example/action.yml"),
                "runs:\n  using: node20\n  main: dist/index.js\n",
            ),
            "write action metadata",
        );
        must(
            fs::write(root.join("actions/example/dist/index.js"), "entrypoint\n"),
            "write action dist",
        );
        must(
            fs::create_dir_all(root.join("dist")),
            "create repository dist",
        );
        must(
            fs::write(root.join("dist/generated.js"), "generated\n"),
            "write repository dist",
        );
        must(
            fs::create_dir_all(root.join(".github-gen/sources/actions/example")),
            "create generated action source",
        );
        must(
            fs::write(
                root.join(".github-gen/sources/actions/example/action.yml"),
                "runs:\n  using: composite\n  steps:\n    - run: echo generated\n",
            ),
            "write generated action source",
        );

        let files = must(repository_files(&root, &[]), "scan action dist fixture");
        assert!(files.contains(&"actions/example/dist/index.js".to_owned()));
        assert!(!files.contains(&"dist/generated.js".to_owned()));
        assert!(!files.iter().any(|file| file.starts_with(".github-gen/")));
    }

    #[test]
    fn tracked_reference_walks_skip_untracked_workflows_and_build_outputs() {
        let root = scratch("tracked-action-reference-context");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        for directory in [
            ".github/workflows",
            ".github/actions/foo/dist",
            ".github/actions/foo/node_modules/package",
            ".github/actions/foo/target/debug",
            ".github/actions/foo/build/output",
            ".github/actions/foo/coverage",
        ] {
            must(
                fs::create_dir_all(root.join(directory)),
                "create tracked reference fixture directory",
            );
        }
        must(
            fs::write(
                root.join(".github/workflows/ci.yml"),
                "jobs:\n  build:\n    steps:\n      - uses: ./.github/actions/foo\n",
            ),
            "write tracked workflow",
        );
        must(
            fs::write(
                root.join(".github/workflows/generated.yml"),
                "# Generated by velnor-workflow.\njobs: {}\n",
            ),
            "write tracked generated workflow",
        );
        for (path, contents) in [
            (".github/actions/foo/action.yml", "runs: {}\n"),
            (".github/actions/foo/dist/index.js", "entrypoint\n"),
            (
                ".github/actions/foo/node_modules/package/index.js",
                "generated dependency\n",
            ),
            (".github/actions/foo/target/debug/output", "build output\n"),
            (".github/actions/foo/build/output/cache", "build output\n"),
            (".github/actions/foo/coverage/report", "coverage output\n"),
        ] {
            must(
                fs::write(root.join(path), contents),
                "write tracked action tree file",
            );
        }
        git(&root, &["add", ".github"]);
        git(&root, &["commit", "-qm", "tracked reference context"]);

        must(
            fs::write(
                root.join(".github/workflows/untracked.yml"),
                "jobs:\n  build:\n    steps:\n      - uses: ./untracked-action\n",
            ),
            "write untracked workflow decoy",
        );
        must(
            fs::write(
                root.join(".github/actions/foo/untracked.js"),
                "untracked action file\n",
            ),
            "write untracked action decoy",
        );

        let workflows = must(
            workflow_reference_files(&root, &[]),
            "read tracked workflow reference files",
        );
        assert_eq!(workflows, [".github/workflows/ci.yml"]);
        let selected = must(
            selected_action_files(&root, ".github/actions/foo", &[]),
            "read explicitly selected action tree",
        );
        assert!(selected.contains(&".github/actions/foo/action.yml".to_owned()));
        assert!(selected.contains(&".github/actions/foo/dist/index.js".to_owned()));
        for ignored in [
            ".github/actions/foo/node_modules/package/index.js",
            ".github/actions/foo/target/debug/output",
            ".github/actions/foo/build/output/cache",
            ".github/actions/foo/coverage/report",
            ".github/actions/foo/untracked.js",
        ] {
            assert!(
                !selected.iter().any(|file| file == ignored),
                "included {ignored}"
            );
        }
        let scanned = must(repository_files(&root, &[]), "read ordinary project inputs");
        assert!(!scanned.iter().any(|file| file.starts_with(".github/")));
    }

    #[cfg(unix)]
    #[test]
    fn tracked_walks_skip_files_below_symlinked_parent_directories() {
        use std::os::unix::fs::symlink;

        let root = scratch("symlinked-reference-parent");
        let outside = scratch("symlinked-reference-target");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        for directory in [".github/workflows", "actions/foo/scripts"] {
            must(
                fs::create_dir_all(root.join(directory)),
                "create symlink fixture directory",
            );
        }
        for directory in [outside.join("workflows"), outside.join("scripts")] {
            must(
                fs::create_dir_all(directory),
                "create external fixture directory",
            );
        }
        must(
            fs::write(
                root.join(".github/workflows/ci.yml"),
                "jobs:\n  build:\n    steps:\n      - uses: ./actions/foo\n",
            ),
            "write tracked workflow before symlinking",
        );
        must(
            fs::write(
                root.join("actions/foo/action.yml"),
                "runs:\n  using: node20\n  main: scripts/main.js\n",
            ),
            "write tracked action metadata",
        );
        must(
            fs::write(root.join("actions/foo/scripts/main.js"), "tracked target\n"),
            "write tracked action entrypoint",
        );
        must(
            fs::write(outside.join("workflows/ci.yml"), "jobs: {}\n"),
            "write external workflow target",
        );
        must(
            fs::write(outside.join("scripts/main.js"), "outside target\n"),
            "write external action target",
        );
        git(&root, &["add", ".github", "actions/foo"]);
        git(&root, &["commit", "-qm", "tracked action and workflow"]);

        must(
            fs::remove_dir_all(root.join(".github/workflows")),
            "remove workflow directory before symlinking",
        );
        must(
            fs::remove_dir_all(root.join("actions/foo/scripts")),
            "remove action script directory before symlinking",
        );
        must(
            symlink(outside.join("workflows"), root.join(".github/workflows")),
            "symlink workflow parent",
        );
        must(
            symlink(outside.join("scripts"), root.join("actions/foo/scripts")),
            "symlink action script parent",
        );

        assert!(
            must(
                workflow_reference_files(&root, &[]),
                "skip symlinked workflow tree"
            )
            .is_empty(),
            "workflow reader crossed a symlinked parent"
        );
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "skip symlinked action subtree",
        );
        assert!(selected.contains(&"actions/foo/action.yml".to_owned()));
        assert!(!selected.contains(&"actions/foo/scripts/main.js".to_owned()));
        let scanned = must(repository_files(&root, &[]), "skip symlinked tracked file");
        assert!(!scanned.contains(&"actions/foo/scripts/main.js".to_owned()));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}

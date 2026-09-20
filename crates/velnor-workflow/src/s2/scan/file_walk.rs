//! File walk and repository-path helpers shared by every detector.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
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
    let mut indexed = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let relative = std::str::from_utf8(raw).map_err(|error| {
            GeneratorError::usage(format!("repository path is not utf-8: {error}"))
        })?;
        let relative = Path::new(relative);
        indexed.push(normalize_relative_path(relative)?);
    }
    let intent_to_add = intent_to_add_paths(root)?;
    indexed.retain(|path| !intent_to_add.contains(path));
    let indexed_paths = indexed.iter().cloned().collect::<BTreeSet<_>>();
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| GeneratorError::io("resolve repository root", root, &error))?;
    let mut tracked = Vec::new();
    for relative in indexed {
        if indexed_repository_file_target(&canonical_root, &relative, &indexed_paths)?.is_none() {
            // The alias or one of its symlink targets is absent from the
            // index, so a clean checkout cannot reproduce this path.
            continue;
        }
        let Some(metadata) = tracked_file_metadata(root, Path::new(&relative)) else {
            // Staged but deleted in the work tree.
            continue;
        };
        if metadata.is_dir() {
            // Git submodules and symlinked directories are not flat file
            // inputs. Explicitly selected action trees resolve safe aliases.
            continue;
        }
        tracked.push(relative);
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
/// Tracked `build` and `node_modules` files stay visible inside that selected
/// tree because action metadata can name those files as entrypoints. A root
/// action supplements those otherwise-pruned directories without rescanning
/// the full repository. Git-backed walks require each symlink component and
/// its canonical file target to be indexed.
pub(crate) fn selected_action_files(
    root: &Path,
    action_root: &str,
    exclude: &[String],
) -> Result<Vec<String>, GeneratorError> {
    let excludes = exclude_set(exclude)?;
    let tracked = if inside_git_work_tree(root) {
        Some(tracked_path_set(root)?)
    } else {
        None
    };
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| GeneratorError::io("resolve repository root", root, &error))?;
    let directories = if action_root == "." {
        vec!["dist", "build", "node_modules"]
    } else {
        let relative = Path::new(action_root);
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(GeneratorError::usage(format!(
                "unsafe local GitHub Action directory: {action_root}"
            )));
        }
        vec![action_root]
    };
    let mut files = Vec::new();
    let walk = SelectedActionWalk {
        root: &canonical_root,
        tracked: tracked.as_ref(),
        excludes: &excludes,
    };
    for directory in directories {
        let logical_directory = canonical_root.join(directory);
        let physical_directory = match fs::canonicalize(&logical_directory) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(GeneratorError::io(
                    "resolve local GitHub Action directory",
                    &logical_directory,
                    &error,
                ));
            }
        };
        if !physical_directory.starts_with(&canonical_root) {
            return Err(GeneratorError::usage(format!(
                "local GitHub Action directory `{directory}` symlink escapes the checkout"
            )));
        }
        if let Some(tracked) = tracked.as_ref()
            && !symlink_components_are_tracked(&canonical_root, directory, tracked)?
        {
            continue;
        }
        if !physical_directory.is_dir() {
            continue;
        }
        let mut active_directories = vec![physical_directory.clone()];
        walk.collect_tree(
            &logical_directory,
            &physical_directory,
            &mut active_directories,
            &mut files,
        )?;
    }
    files.retain(|file| file != action_root && !excludes.is_match(file));
    files.sort();
    files.dedup();
    Ok(files)
}

fn tracked_path_set(root: &Path) -> Result<BTreeSet<String>, GeneratorError> {
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
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path).map_err(|error| {
                GeneratorError::usage(format!("repository path is not utf-8: {error}"))
            })?;
            normalize_relative_path(Path::new(path))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for path in intent_to_add_paths(root)? {
        paths.remove(&path);
    }
    Ok(paths)
}

/// Git includes intent-to-add paths in ls-files, but does not put their
/// contents in the index tree that a clean checkout would reproduce.
fn intent_to_add_paths(root: &Path) -> Result<BTreeSet<String>, GeneratorError> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=no"])
        .output()
        .map_err(|error| GeneratorError::io("run git status", root, &error))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(GeneratorError::usage(format!(
            "git status failed while checking the index: {stderr}"
        )));
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| record.starts_with(b" A "))
        .map(|record| {
            let path = std::str::from_utf8(&record[3..]).map_err(|error| {
                GeneratorError::usage(format!("repository path is not utf-8: {error}"))
            })?;
            normalize_relative_path(Path::new(path))
        })
        .collect()
}

struct SelectedActionWalk<'a> {
    root: &'a Path,
    tracked: Option<&'a BTreeSet<String>>,
    excludes: &'a GlobSet,
}

impl SelectedActionWalk<'_> {
    fn collect_tree(
        &self,
        logical_directory: &Path,
        physical_directory: &Path,
        active_directories: &mut Vec<std::path::PathBuf>,
        files: &mut Vec<String>,
    ) -> Result<(), GeneratorError> {
        let entries = fs::read_dir(physical_directory).map_err(|error| {
            GeneratorError::io(
                "read selected local GitHub Action",
                physical_directory,
                &error,
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                GeneratorError::io(
                    "read selected local GitHub Action entry",
                    physical_directory,
                    &error,
                )
            })?;
            self.collect_entry(logical_directory, &entry, active_directories, files)?;
        }
        Ok(())
    }

    fn collect_entry(
        &self,
        logical_directory: &Path,
        entry: &fs::DirEntry,
        active_directories: &mut Vec<std::path::PathBuf>,
        files: &mut Vec<String>,
    ) -> Result<(), GeneratorError> {
        let name = entry.file_name();
        let logical_path = logical_directory.join(&name);
        let logical_relative = repository_relative_path(self.root, &logical_path)?;
        if self.excludes.is_match(&logical_relative) {
            return Ok(());
        }
        let physical_path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            GeneratorError::io(
                "inspect selected local GitHub Action entry",
                &physical_path,
                &error,
            )
        })?;
        if file_type.is_symlink() {
            return self.collect_symlink(
                &logical_path,
                &logical_relative,
                &physical_path,
                &name,
                active_directories,
                files,
            );
        }
        if file_type.is_dir() {
            if is_excluded_selected_action_directory(&name.to_string_lossy()) {
                return Ok(());
            }
            let target = fs::canonicalize(&physical_path).map_err(|error| {
                GeneratorError::io(
                    "resolve selected GitHub Action directory",
                    &physical_path,
                    &error,
                )
            })?;
            if !target.starts_with(self.root) {
                return Err(GeneratorError::usage(format!(
                    "selected GitHub Action directory `{logical_relative}` escapes the checkout"
                )));
            }
            active_directories.push(target.clone());
            self.collect_tree(&logical_path, &target, active_directories, files)?;
            active_directories.pop();
        } else if file_type.is_file() {
            let target = fs::canonicalize(&physical_path).map_err(|error| {
                GeneratorError::io(
                    "resolve selected GitHub Action file",
                    &physical_path,
                    &error,
                )
            })?;
            self.append_file(&logical_path, &target, files)?;
        }
        Ok(())
    }

    fn collect_symlink(
        &self,
        logical_path: &Path,
        logical_relative: &str,
        physical_path: &Path,
        name: &std::ffi::OsStr,
        active_directories: &mut Vec<std::path::PathBuf>,
        files: &mut Vec<String>,
    ) -> Result<(), GeneratorError> {
        let target = match fs::canonicalize(physical_path) {
            Ok(target) => target,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                // Runner sees a dangling selected entrypoint as missing and
                // continues to the next manifest candidate.
                return Ok(());
            }
            Err(error) => {
                return Err(GeneratorError::io(
                    "resolve selected GitHub Action symlink",
                    physical_path,
                    &error,
                ));
            }
        };
        if !target.starts_with(self.root) {
            return Err(GeneratorError::usage(format!(
                "selected GitHub Action symlink `{logical_relative}` escapes the checkout"
            )));
        }
        let metadata = fs::metadata(&target).map_err(|error| {
            GeneratorError::io(
                "inspect selected GitHub Action symlink target",
                &target,
                &error,
            )
        })?;
        if metadata.is_dir() {
            if is_excluded_selected_action_directory(&name.to_string_lossy()) {
                return Ok(());
            }
            if let Some(tracked) = self.tracked
                && !symlink_components_are_tracked(self.root, logical_relative, tracked)?
            {
                return Ok(());
            }
            if active_directories.contains(&target) {
                return Err(GeneratorError::usage(format!(
                    "selected GitHub Action symlink cycle at `{logical_relative}`"
                )));
            }
            active_directories.push(target.clone());
            self.collect_tree(logical_path, &target, active_directories, files)?;
            active_directories.pop();
        } else if metadata.is_file() {
            self.append_file(logical_path, &target, files)?;
        }
        Ok(())
    }

    fn append_file(
        &self,
        logical_path: &Path,
        target_path: &Path,
        files: &mut Vec<String>,
    ) -> Result<(), GeneratorError> {
        let logical_relative = repository_relative_path(self.root, logical_path)?;
        let target_relative = repository_relative_path(self.root, target_path)?;
        if self.excludes.is_match(&logical_relative) || self.excludes.is_match(&target_relative) {
            return Ok(());
        }
        if let Some(tracked) = self.tracked {
            let Some(indexed_target) =
                indexed_repository_file_target(self.root, &logical_relative, tracked)?
            else {
                return Ok(());
            };
            if indexed_target != target_relative {
                return Ok(());
            }
        }
        files.push(logical_relative.clone());
        if target_relative != logical_relative {
            files.push(target_relative);
        }
        Ok(())
    }
}

fn repository_relative_path(root: &Path, path: &Path) -> Result<String, GeneratorError> {
    let relative = path.strip_prefix(root).map_err(|error| {
        GeneratorError::usage(format!(
            "selected action path escaped repository root: {error}"
        ))
    })?;
    normalize_relative_path(relative)
}

/// Check a file named explicitly by action metadata even when the ordinary
/// repository walk pruned its directory. For Git repositories every symlink
/// component and the canonical file target must be tracked. This keeps
/// generated `.github` content out of broad scans while accounting for
/// explicit action inputs.
pub(crate) fn explicitly_referenced_repository_file(
    root: &Path,
    relative: &str,
    exclude: &[String],
) -> Result<bool, GeneratorError> {
    let path = Path::new(relative);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Ok(false);
    }
    let excludes = exclude_set(exclude)?;
    if excludes.is_match(relative) {
        return Ok(false);
    }
    let Some(target) = canonical_repository_target(root, relative)? else {
        return Ok(false);
    };
    if excludes.is_match(&target) {
        return Ok(false);
    }
    if inside_git_work_tree(root) {
        let indexed_paths = tracked_path_set(root)?;
        let canonical_root = fs::canonicalize(root)
            .map_err(|error| GeneratorError::io("resolve repository root", root, &error))?;
        let Some(indexed_target) =
            indexed_repository_file_target(&canonical_root, relative, &indexed_paths)?
        else {
            return Ok(false);
        };
        if indexed_target != target {
            return Ok(false);
        }
    }
    Ok(tracked_file_metadata(root, path).is_some_and(|metadata| metadata.is_file()))
}

/// Resolve a readable repository path as Runner does while keeping symlink
/// targets inside the checkout. The returned path is the canonical target used
/// to watch changes that arrive through an in-repository alias.
pub(crate) fn canonical_repository_target(
    root: &Path,
    relative: &str,
) -> Result<Option<String>, GeneratorError> {
    let path = root.join(relative);
    let mut current = root.to_path_buf();
    let mut has_symlink = false;
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return Ok(None);
        };
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(GeneratorError::io(
                    "inspect repository path",
                    &current,
                    &error,
                ))
            }
        };
        has_symlink |= metadata.file_type().is_symlink();
    }
    if !has_symlink {
        return Ok(Some(relative.to_owned()));
    }
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| GeneratorError::io("resolve repository root", root, &error))?;
    let target = match fs::canonicalize(&path) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GeneratorError::io("resolve repository file", &path, &error)),
    };
    if !target.starts_with(&canonical_root) {
        return Err(GeneratorError::usage(format!(
            "repository symlink `{relative}` escapes the checkout"
        )));
    }
    let target_relative = target.strip_prefix(&canonical_root).map_err(|error| {
        GeneratorError::usage(format!("resolve repository symlink target: {error}"))
    })?;
    normalize_relative_path(target_relative).map(Some)
}

/// List readable, tracked repository paths under a directory. With `direct`
/// set, only immediate children are returned; this matches GitHub's workflow
/// file layout. Safe in-checkout file symlinks are resolved for readability.
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
    let mut indexed_paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .map(|raw| {
            let raw = std::str::from_utf8(raw).map_err(|error| {
                GeneratorError::usage(format!("repository path is not utf-8: {error}"))
            })?;
            normalize_relative_path(Path::new(raw))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for path in intent_to_add_paths(root)? {
        indexed_paths.remove(&path);
    }
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| GeneratorError::io("resolve repository root", root, &error))?;
    let prefix = format!("{directory}/");
    let mut paths = Vec::new();
    for normalized in &indexed_paths {
        if !normalized.starts_with(&prefix) {
            continue;
        }
        let suffix = &normalized[prefix.len()..];
        if direct && suffix.contains('/') {
            continue;
        }
        let Some(target) =
            indexed_repository_file_target(&canonical_root, normalized, &indexed_paths)?
        else {
            continue;
        };
        if !fs::metadata(canonical_root.join(target)).is_ok_and(|metadata| metadata.is_file()) {
            continue;
        }
        if !direct && is_excluded_selected_action_path(normalized, directory) {
            continue;
        }
        paths.push(normalized.clone());
    }
    Ok(paths)
}

/// Inspect every path component and follow only symlinks whose canonical
/// target stays inside the checkout.
fn tracked_file_metadata(root: &Path, relative: &Path) -> Option<fs::Metadata> {
    let mut path = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(component) = component else {
            return None;
        };
        path.push(component);
        let metadata = fs::symlink_metadata(&path).ok()?;
        let metadata = if metadata.file_type().is_symlink() {
            canonical_path_under_root(root, &path)?;
            fs::metadata(&path).ok()?
        } else {
            metadata
        };
        if components.peek().is_some() && !metadata.is_dir() {
            return None;
        }
        if components.peek().is_none() {
            return Some(metadata);
        }
    }
    None
}

fn canonical_path_under_root(root: &Path, path: &Path) -> Option<std::path::PathBuf> {
    let canonical_root = fs::canonicalize(root).ok()?;
    let canonical_target = fs::canonicalize(path).ok()?;
    canonical_target
        .starts_with(&canonical_root)
        .then_some(canonical_target)
}

#[derive(Debug)]
enum RepositoryPathComponent {
    Normal(OsString),
    CurrentDirectory,
    ParentDirectory,
}

fn repository_path_components(path: &Path) -> Option<VecDeque<RepositoryPathComponent>> {
    path.components()
        .map(|component| match component {
            Component::Normal(component) => {
                Some(RepositoryPathComponent::Normal(component.to_owned()))
            }
            Component::CurDir => Some(RepositoryPathComponent::CurrentDirectory),
            Component::ParentDir => Some(RepositoryPathComponent::ParentDirectory),
            Component::RootDir | Component::Prefix(_) => None,
        })
        .collect()
}

struct RepositoryPathResolution {
    target_relative: String,
    symlink_components: BTreeSet<String>,
}

/// Resolve each symlink component while retaining its indexed path. Git stores
/// a symlink as one index entry; children reached through a directory alias
/// remain indexed under the real target path.
#[expect(
    clippy::too_many_lines,
    reason = "the resolver follows every symlink component so each alias can be checked against the Git index"
)]
fn resolve_repository_path(
    canonical_root: &Path,
    relative: &str,
) -> Result<Option<RepositoryPathResolution>, GeneratorError> {
    let relative_path = Path::new(relative);
    if relative_path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Ok(None);
    }
    let Some(mut pending) = repository_path_components(relative_path) else {
        return Ok(None);
    };
    let mut current = canonical_root.to_path_buf();
    let mut symlinks = BTreeSet::new();
    let mut link_count = 0;
    while let Some(component) = pending.pop_front() {
        match component {
            RepositoryPathComponent::CurrentDirectory => {}
            RepositoryPathComponent::ParentDirectory => {
                if current == canonical_root {
                    return Ok(None);
                }
                current.pop();
            }
            RepositoryPathComponent::Normal(component) => {
                match fs::metadata(&current) {
                    Ok(metadata) if metadata.is_dir() => {}
                    Ok(_) => return Ok(None),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        ) =>
                    {
                        return Ok(None);
                    }
                    Err(error) => {
                        return Err(GeneratorError::io(
                            "inspect repository symlink parent",
                            &current,
                            &error,
                        ));
                    }
                }
                let path = current.join(component);
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        ) =>
                    {
                        return Ok(None);
                    }
                    Err(error) => {
                        return Err(GeneratorError::io("inspect repository path", &path, &error));
                    }
                };
                if metadata.file_type().is_symlink() {
                    let relative_link = repository_relative_path(canonical_root, &path)?;
                    symlinks.insert(relative_link);
                    link_count += 1;
                    if link_count > 40 {
                        return Ok(None);
                    }
                    let target = fs::read_link(&path).map_err(|error| {
                        GeneratorError::io("read repository symlink", &path, &error)
                    })?;
                    let target_components = if target.is_absolute() {
                        let Ok(target) = target.strip_prefix(canonical_root) else {
                            return Ok(None);
                        };
                        current = canonical_root.to_path_buf();
                        repository_path_components(target)
                    } else {
                        repository_path_components(&target)
                    };
                    let Some(mut target_components) = target_components else {
                        return Ok(None);
                    };
                    target_components.append(&mut pending);
                    pending = target_components;
                } else {
                    if !pending.is_empty() && !metadata.is_dir() {
                        return Ok(None);
                    }
                    current = path;
                }
            }
        }
    }
    let target_relative = if symlinks.is_empty() {
        repository_relative_path(canonical_root, &current)?
    } else {
        let canonical_target = match fs::canonicalize(&current) {
            Ok(path) if path.starts_with(canonical_root) => path,
            Ok(_) => return Ok(None),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(GeneratorError::io(
                    "resolve repository symlink path",
                    &current,
                    &error,
                ));
            }
        };
        if !canonical_target.starts_with(canonical_root) {
            return Ok(None);
        }
        repository_relative_path(canonical_root, &canonical_target)?
    };
    Ok(Some(RepositoryPathResolution {
        target_relative,
        symlink_components: symlinks,
    }))
}

fn symlink_components_are_tracked(
    canonical_root: &Path,
    relative: &str,
    indexed_paths: &BTreeSet<String>,
) -> Result<bool, GeneratorError> {
    let Some(resolution) = resolve_repository_path(canonical_root, relative)? else {
        return Ok(false);
    };
    Ok(resolution
        .symlink_components
        .iter()
        .all(|component| indexed_paths.contains(component)))
}

fn indexed_repository_file_target(
    canonical_root: &Path,
    relative: &str,
    indexed_paths: &BTreeSet<String>,
) -> Result<Option<String>, GeneratorError> {
    let Some(resolution) = resolve_repository_path(canonical_root, relative)? else {
        return Ok(None);
    };
    if !indexed_paths.contains(&resolution.target_relative)
        || resolution
            .symlink_components
            .iter()
            .any(|component| !indexed_paths.contains(component))
    {
        return Ok(None);
    }
    Ok(Some(resolution.target_relative))
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
            if is_excluded_selected_action_directory(&name.to_string_lossy()) {
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
            if is_excluded_selected_action_directory(&name.to_string_lossy()) {
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
        .any(|component| is_excluded_selected_action_directory(component))
}

fn is_excluded_selected_action_directory(name: &str) -> bool {
    is_excluded_directory(name) && !matches!(name, ".github" | "dist" | "build" | "node_modules")
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
            if canonical_path_under_root(root, &path)
                .is_some_and(|target| fs::metadata(target).is_ok_and(|metadata| metadata.is_file()))
            {
                let relative = path.strip_prefix(root).map_err(|error| {
                    GeneratorError::usage(format!("make repository symlink path relative: {error}"))
                })?;
                let normalized = normalize_relative_path(relative)?;
                if !is_excluded_path(&normalized, action_roots) {
                    files.push(normalized);
                }
            }
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
        let is_safe_file_symlink = kind.is_symlink()
            && canonical_path_under_root(root, &path).is_some_and(|target| {
                fs::metadata(target).is_ok_and(|metadata| metadata.is_file())
            });
        if kind.is_symlink() && !is_safe_file_symlink {
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
        } else if (kind.is_file() || is_safe_file_symlink)
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
    use super::{
        canonical_repository_target, explicitly_referenced_repository_file, repository_files,
        selected_action_files, workflow_reference_files,
    };
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

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn git_output(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("run git: {error}"));
        assert!(output.status.success(), "git {args:?} failed");
        must(String::from_utf8(output.stdout), "decode git output")
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
    #[expect(
        clippy::too_many_lines,
        reason = "the selected-action tree fixture keeps tracked boundary paths and expected files auditable together"
    )]
    fn tracked_reference_walks_include_selected_action_build_and_dependency_files() {
        let root = scratch("tracked-action-reference-context");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        for directory in [
            ".github/workflows",
            ".github/actions/foo/dist",
            ".github/actions/foo/.github",
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
                ".github/actions/foo/.github/entry.js",
                "nested action entrypoint\n",
            ),
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
        assert!(selected.contains(&".github/actions/foo/.github/entry.js".to_owned()));
        for included in [
            ".github/actions/foo/.github/entry.js",
            ".github/actions/foo/node_modules/package/index.js",
            ".github/actions/foo/build/output/cache",
        ] {
            assert!(
                selected.iter().any(|file| file == included),
                "missing {included}"
            );
        }
        for ignored in [
            ".github/actions/foo/target/debug/output",
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

    #[test]
    fn physical_selected_action_walk_keeps_build_and_dependency_files() {
        let root = scratch("physical-action-build-and-dependencies");
        for (path, contents) in [
            ("actions/foo/action.yml", "runs: {}\n"),
            ("actions/foo/build/output.js", "build entrypoint\n"),
            (
                "actions/foo/node_modules/pkg/index.js",
                "dependency entrypoint\n",
            ),
            ("actions/foo/target/debug/output", "generated\n"),
        ] {
            let path = root.join(path);
            if let Some(parent) = path.parent() {
                must(fs::create_dir_all(parent), "create physical action path");
            }
            must(fs::write(path, contents), "write physical action file");
        }

        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "read physical selected action tree",
        );
        assert!(selected.contains(&"actions/foo/build/output.js".to_owned()));
        assert!(selected.contains(&"actions/foo/node_modules/pkg/index.js".to_owned()));
        assert!(!selected.contains(&"actions/foo/target/debug/output".to_owned()));
        let _ = fs::remove_dir_all(root);
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
        let error = selected_action_files(&root, "actions/foo", &[])
            .err()
            .unwrap_or_else(|| panic!("external action symlink target must fail closed"));
        assert!(
            error.to_string().contains("escapes the checkout"),
            "{error}"
        );
        let scanned = must(repository_files(&root, &[]), "skip symlinked tracked file");
        assert!(!scanned.contains(&"actions/foo/scripts/main.js".to_owned()));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn tracked_selected_action_walk_follows_contained_symlink_targets() {
        use std::os::unix::fs::symlink;

        let root = scratch("tracked-action-symlink-targets");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        for directory in ["actions/real", "actions/entry", "shared"] {
            must(
                fs::create_dir_all(root.join(directory)),
                "create symlink action tree",
            );
        }
        for (path, contents) in [
            (
                "actions/real/action.yml",
                "runs:\n  using: docker\n  image: Dockerfile\n",
            ),
            ("actions/real/Dockerfile", "FROM scratch\n"),
            (
                "actions/entry/action.yml",
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            ("shared/main.js", "process.exit(0)\n"),
        ] {
            must(
                fs::write(root.join(path), contents),
                "write tracked symlink target",
            );
        }
        must(
            symlink("real", root.join("actions/linked")),
            "create tracked action directory symlink",
        );
        must(
            symlink("../../shared/main.js", root.join("actions/entry/index.js")),
            "create tracked entrypoint symlink",
        );
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "tracked symlink actions"]);

        let linked = must(
            selected_action_files(&root, "actions/linked", &[]),
            "walk contained action directory symlink",
        );
        for path in [
            "actions/linked/action.yml",
            "actions/linked/Dockerfile",
            "actions/real/action.yml",
            "actions/real/Dockerfile",
        ] {
            assert!(
                linked.iter().any(|file| file == path),
                "missing {path}: {linked:?}"
            );
        }

        let entry = must(
            selected_action_files(&root, "actions/entry", &[]),
            "walk contained action entrypoint symlink",
        );
        assert!(entry.contains(&"actions/entry/index.js".to_owned()));
        assert!(entry.contains(&"shared/main.js".to_owned()));
        assert_eq!(
            canonical_repository_target(&root, "actions/entry/index.js").unwrap(),
            Some("shared/main.js".to_owned())
        );
        assert!(must(
            explicitly_referenced_repository_file(&root, "actions/entry/index.js", &[]),
            "resolve tracked symlink entrypoint"
        ));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "this fixture proves both sides of Git index membership against Runner's manifest fallback"
    )]
    fn selected_action_manifest_fallback_is_not_shadowed_by_untracked_preferred_symlink() {
        use std::os::unix::fs::symlink;

        let root = scratch("action-manifest-symlink-index");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        for directory in ["actions/foo", "shared"] {
            must(
                fs::create_dir_all(root.join(directory)),
                "create action fixture",
            );
        }
        for (path, contents) in [
            (
                "actions/foo/action.yaml",
                "runs:\n  using: node20\n  main: fallback.js\n",
            ),
            ("actions/foo/fallback.js", "process.exit(0)\n"),
            (
                "shared/action.yml",
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            ("shared/main.js", "process.exit(0)\n"),
        ] {
            must(
                fs::write(root.join(path), contents),
                "write manifest target",
            );
        }
        must(
            symlink(
                "../../shared/action.yml",
                root.join("actions/foo/action.yml"),
            ),
            "create preferred manifest alias",
        );
        must(
            symlink("../../shared/main.js", root.join("actions/foo/index.js")),
            "create action entrypoint alias",
        );
        git(
            &root,
            &[
                "add",
                "actions/foo/action.yaml",
                "actions/foo/fallback.js",
                "shared/action.yml",
                "shared/main.js",
            ],
        );
        git(&root, &["commit", "-qm", "track targets and fallback only"]);

        assert!(git_output(
            &root,
            &["ls-files", "--stage", "--", "actions/foo/action.yml"]
        )
        .is_empty());
        assert!(
            git_output(&root, &["ls-files", "--stage", "--", "shared/action.yml"])
                .starts_with("100644 ")
        );
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "select fallback action tree with untracked preferred alias",
        );
        assert!(selected.contains(&"actions/foo/action.yaml".to_owned()));
        assert!(selected.contains(&"actions/foo/fallback.js".to_owned()));
        for absent in ["actions/foo/action.yml", "actions/foo/index.js"] {
            assert!(!selected.contains(&absent.to_owned()), "included {absent}");
        }
        for alias in ["actions/foo/action.yml", "actions/foo/index.js"] {
            assert!(
                !must(
                    explicitly_referenced_repository_file(&root, alias, &[]),
                    "reject untracked action alias"
                ),
                "accepted untracked alias {alias}"
            );
        }

        must(
            fs::remove_file(root.join("shared/action.yml")),
            "remove untracked preferred manifest target",
        );
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "fall back from dangling untracked preferred manifest",
        );
        assert!(selected.contains(&"actions/foo/action.yaml".to_owned()));
        assert!(selected.contains(&"actions/foo/fallback.js".to_owned()));
        assert!(!selected.contains(&"actions/foo/action.yml".to_owned()));
        must(
            fs::write(
                root.join("shared/action.yml"),
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            "restore preferred manifest target",
        );

        git(&root, &["add", "-N", "actions/foo/action.yml"]);
        assert!(
            git_output(&root, &["status", "--porcelain=v1", "--untracked-files=no"])
                .starts_with(" A actions/foo/action.yml"),
            "intent-to-add status must remain distinct from a staged file"
        );
        assert!(
            git_output(
                &root,
                &["ls-files", "--stage", "--", "actions/foo/action.yml"]
            )
            .starts_with("120000 "),
            "intent-to-add symlink still appears in ls-files"
        );
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "select fallback action tree with an intent-to-add preferred alias",
        );
        assert!(selected.contains(&"actions/foo/action.yaml".to_owned()));
        assert!(!selected.contains(&"actions/foo/action.yml".to_owned()));
        assert!(!must(
            explicitly_referenced_repository_file(&root, "actions/foo/action.yml", &[]),
            "reject intent-to-add action alias"
        ));
        let scanned = must(
            repository_files(&root, &[]),
            "scan action tree with intent-to-add preferred alias",
        );
        assert!(scanned.contains(&"actions/foo/action.yaml".to_owned()));
        assert!(!scanned.contains(&"actions/foo/action.yml".to_owned()));

        git(
            &root,
            &["add", "actions/foo/action.yml", "actions/foo/index.js"],
        );
        git(&root, &["commit", "-qm", "track both aliases"]);
        assert!(git_output(
            &root,
            &["ls-files", "--stage", "--", "actions/foo/action.yml"]
        )
        .starts_with("120000 "));
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "select action tree with tracked aliases and targets",
        );
        for present in [
            "actions/foo/action.yml",
            "actions/foo/action.yaml",
            "actions/foo/index.js",
            "shared/action.yml",
            "shared/main.js",
        ] {
            assert!(selected.contains(&present.to_owned()), "missing {present}");
        }
        assert!(must(
            explicitly_referenced_repository_file(&root, "actions/foo/index.js", &[]),
            "accept tracked action entrypoint alias and target"
        ));

        must(
            fs::remove_file(root.join("shared/action.yml")),
            "remove tracked preferred manifest target",
        );
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "fall back from dangling tracked preferred manifest",
        );
        assert!(selected.contains(&"actions/foo/action.yaml".to_owned()));
        assert!(selected.contains(&"actions/foo/fallback.js".to_owned()));
        assert!(!selected.contains(&"actions/foo/action.yml".to_owned()));
        assert!(!must(
            explicitly_referenced_repository_file(&root, "actions/foo/action.yml", &[]),
            "reject tracked but dangling manifest alias"
        ));
        must(
            fs::write(
                root.join("shared/action.yml"),
                "runs:\n  using: node20\n  main: index.js\n",
            ),
            "restore tracked preferred manifest target",
        );

        git(
            &root,
            &["rm", "--cached", "shared/action.yml", "shared/main.js"],
        );
        git(&root, &["commit", "-qm", "leave targets untracked"]);
        assert!(
            git_output(
                &root,
                &["ls-files", "--stage", "--", "actions/foo/action.yml"]
            )
            .starts_with("120000 "),
            "the symlink alias remains indexed"
        );
        assert!(git_output(&root, &["ls-files", "--stage", "--", "shared/action.yml"]).is_empty());
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "select fallback action tree with tracked aliases and untracked targets",
        );
        assert!(selected.contains(&"actions/foo/action.yaml".to_owned()));
        assert!(selected.contains(&"actions/foo/fallback.js".to_owned()));
        for absent in [
            "actions/foo/action.yml",
            "actions/foo/index.js",
            "shared/action.yml",
            "shared/main.js",
        ] {
            assert!(!selected.contains(&absent.to_owned()), "included {absent}");
        }
        assert!(!must(
            explicitly_referenced_repository_file(&root, "actions/foo/index.js", &[]),
            "reject tracked action alias with untracked target"
        ));
        let scanned = must(repository_files(&root, &[]), "scan action fallback fixture");
        assert!(!scanned.contains(&"actions/foo/action.yml".to_owned()));
        assert!(scanned.contains(&"actions/foo/action.yaml".to_owned()));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn selected_action_walk_requires_each_nested_symlink_component_to_be_tracked() {
        use std::os::unix::fs::symlink;

        let root = scratch("nested-action-symlink-components");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        must(
            fs::create_dir_all(root.join("actions/foo")),
            "create selected action directory",
        );
        must(
            fs::create_dir_all(root.join("shared/actual")),
            "create nested symlink target",
        );
        must(
            fs::write(
                root.join("actions/foo/action.yml"),
                "runs:\n  using: node20\n  main: linked/inner/entry.js\n",
            ),
            "write selected action metadata",
        );
        must(
            fs::write(root.join("shared/actual/entry.js"), "process.exit(0)\n"),
            "write canonical nested entrypoint",
        );
        must(
            symlink("../../shared", root.join("actions/foo/linked")),
            "create tracked outer directory alias",
        );
        must(
            symlink("actual", root.join("shared/inner")),
            "create untracked nested directory alias",
        );
        git(
            &root,
            &[
                "add",
                "actions/foo/action.yml",
                "actions/foo/linked",
                "shared/actual/entry.js",
            ],
        );
        git(&root, &["commit", "-qm", "leave nested alias untracked"]);

        assert!(
            git_output(&root, &["ls-files", "--stage", "--", "actions/foo/linked"])
                .starts_with("120000 ")
        );
        assert!(git_output(&root, &["ls-files", "--stage", "--", "shared/inner"]).is_empty());
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "walk action with an untracked nested alias",
        );
        assert!(selected.contains(&"actions/foo/action.yml".to_owned()));
        assert!(!selected.contains(&"actions/foo/linked/inner/entry.js".to_owned()));
        // The canonical file is reachable through the tracked outer alias;
        // only the path through the untracked inner alias must be omitted.
        assert!(selected.contains(&"shared/actual/entry.js".to_owned()));
        assert!(!must(
            explicitly_referenced_repository_file(&root, "actions/foo/linked/inner/entry.js", &[]),
            "reject reference with untracked nested alias"
        ));

        git(&root, &["add", "shared/inner"]);
        git(&root, &["commit", "-qm", "track nested alias"]);
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "walk action with every alias tracked",
        );
        assert!(selected.contains(&"actions/foo/linked/inner/entry.js".to_owned()));
        assert!(selected.contains(&"shared/actual/entry.js".to_owned()));
        assert!(must(
            explicitly_referenced_repository_file(&root, "actions/foo/linked/inner/entry.js", &[]),
            "accept reference with every symlink component tracked"
        ));

        git(&root, &["rm", "--cached", "shared/actual/entry.js"]);
        git(&root, &["commit", "-qm", "leave canonical file untracked"]);
        let selected = must(
            selected_action_files(&root, "actions/foo", &[]),
            "walk action with untracked canonical file",
        );
        assert!(!selected.contains(&"actions/foo/linked/inner/entry.js".to_owned()));
        assert!(!selected.contains(&"shared/actual/entry.js".to_owned()));
        assert!(!must(
            explicitly_referenced_repository_file(&root, "actions/foo/linked/inner/entry.js", &[]),
            "reject reference with untracked canonical file"
        ));

        let _ = fs::remove_dir_all(root);
    }
}

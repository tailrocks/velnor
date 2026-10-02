//! File walk and repository-path helpers shared by every detector.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::{RepositoryShape, ScanContext};
use crate::s2::{parent_path, GeneratorError};

/// Validate the physical repository tree before any repository-controlled
/// file is read by config discovery or a detector.
///
/// The walk never follows a symlinked directory. A symlink is accepted only
/// when its complete target resolves inside the repository and ends at a
/// regular file or directory. The preflight uses the same boundary as the
/// physical walk: root-level tool/output directories and `.git`/`node_modules`
/// at every depth are outside the scanner's input boundary.
/// Check only the caller-supplied root boundary without following the root or
/// any of its parent components. Callers that bind an identity handle use this
/// before opening that handle, then run the complete tree validation.
pub(crate) fn validate_repository_root_boundary(root: &Path) -> Result<(), GeneratorError> {
    validated_repository_root_path(root).map(|_| ())
}

fn validated_repository_root_path(root: &Path) -> Result<PathBuf, GeneratorError> {
    let root = absolute_normalized_path(root)?;
    reject_symlinked_root_components(&root)?;
    let root_metadata = fs::symlink_metadata(&root)
        .map_err(|error| GeneratorError::io("inspect repository root", &root, &error))?;
    if root_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "refusing symlinked repository root: {}",
            root.display()
        )));
    }
    if !root_metadata.is_dir() {
        return Err(GeneratorError::usage(format!(
            "repository root is not a directory: {}",
            root.display()
        )));
    }
    Ok(root)
}

pub(crate) fn validate_repository_tree(root: &Path) -> Result<(), GeneratorError> {
    let root = validated_repository_root_path(root)?;
    let canonical_root = normalize_macos_system_alias(
        root.canonicalize()
            .map_err(|error| GeneratorError::io("canonicalize repository root", &root, &error))?,
    );
    validate_directory(&root, &canonical_root, &root)
}

/// Make a path absolute while normalizing lexical `.` and `..` components.
/// No filesystem component is resolved here; that is the job of the explicit
/// `symlink_metadata` checks below.
pub(crate) fn absolute_normalized_path(path: &Path) -> Result<PathBuf, GeneratorError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| GeneratorError::usage(format!("read current directory: {error}")))?
            .join(path)
    };
    // Inspect the caller-supplied path before lexical normalization. A
    // symlink followed by `..` can otherwise disappear from the normalized
    // path and redirect a later filesystem operation through an untrusted
    // parent. Normalize macOS's `/tmp` and `/var` aliases first so those
    // system aliases are not mistaken for repository-controlled symlinks.
    let absolute = normalize_macos_system_alias(absolute);
    reject_symlinked_root_components(&absolute)?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(_) | Component::Prefix(_) | Component::RootDir => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalize_macos_system_alias(normalized))
}

#[cfg(target_os = "macos")]
fn normalize_macos_system_alias(path: PathBuf) -> PathBuf {
    for (alias, target) in [
        (Path::new("/var"), Path::new("/private/var")),
        (Path::new("/tmp"), Path::new("/private/tmp")),
    ] {
        if let Ok(suffix) = path.strip_prefix(alias) {
            return target.join(suffix);
        }
    }
    path
}

#[cfg(not(target_os = "macos"))]
fn normalize_macos_system_alias(path: PathBuf) -> PathBuf {
    path
}

/// Check every root path component without allowing a symlinked parent to
/// redirect the root outside the caller's intended tree.
fn reject_symlinked_root_components(root: &Path) -> Result<(), GeneratorError> {
    let mut current = PathBuf::new();
    for component in root.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            GeneratorError::io("inspect repository root component", &current, &error)
        })?;
        if metadata.file_type().is_symlink() {
            return Err(GeneratorError::usage(format!(
                "refusing symlinked repository root component: {}",
                current.display()
            )));
        }
        if !metadata.is_dir() {
            return Err(GeneratorError::usage(format!(
                "repository root component is not a directory: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

fn validate_directory(
    root: &Path,
    canonical_root: &Path,
    directory: &Path,
) -> Result<(), GeneratorError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read repository directory", directory, &error))?;
    let at_root = directory == root;
    for entry in entries {
        let entry = entry.map_err(|error| {
            GeneratorError::io("read repository directory entry", directory, &error)
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect repository entry", &path, &error))?;
        let name = entry.file_name();
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            if is_preflight_excluded_directory(directory, &name, at_root)
                && !is_opaque_root_output(directory, &name, at_root)
            {
                return Err(GeneratorError::usage(format!(
                    "repository contains a symlinked excluded directory: {}",
                    path.display()
                )));
            }
            if is_opaque_root_output(directory, &name, at_root) {
                // CI may materialize the root build cache as a symlink. It is
                // outside the scan boundary, so lstat it and prune it without
                // resolving or reading its target.
                continue;
            }
            validate_symlink(root, canonical_root, &path)?;
        } else if metadata.is_dir() {
            if is_preflight_excluded_directory(directory, &name, at_root) {
                continue;
            }
            validate_directory(root, canonical_root, &path)?;
        } else if !metadata.is_file() {
            return Err(GeneratorError::usage(format!(
                "repository contains a special file: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_symlink(root: &Path, canonical_root: &Path, path: &Path) -> Result<(), GeneratorError> {
    let target = fs::read_link(path)
        .map_err(|error| GeneratorError::io("read repository symlink", path, &error))?;
    let resolved = normalize_macos_system_alias(path.canonicalize().map_err(|error| {
        GeneratorError::usage(format!(
            "repository symlink is dangling or unreadable: {} -> {} ({error})",
            path.display(),
            target.display()
        ))
    })?);
    if !resolved.starts_with(canonical_root) {
        return Err(GeneratorError::usage(format!(
            "repository symlink escapes the repository: {} -> {} (root {})",
            path.display(),
            target.display(),
            root.display()
        )));
    }
    let metadata = fs::symlink_metadata(&resolved).map_err(|error| {
        GeneratorError::io("inspect repository symlink target", &resolved, &error)
    })?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(GeneratorError::usage(format!(
            "repository symlink targets a special file: {} -> {}",
            path.display(),
            target.display()
        )));
    }
    Ok(())
}

fn filesystem_path_alias(left: &Path, right: &Path) -> bool {
    let Ok(left) = fs::canonicalize(left) else {
        return false;
    };
    let Ok(right) = fs::canonicalize(right) else {
        return false;
    };
    left == right
}

fn is_preflight_excluded_directory(parent: &Path, name: &std::ffi::OsStr, at_root: bool) -> bool {
    let name = name.to_string_lossy();
    if name == ".git" || name == "node_modules" || at_root && is_excluded_directory(&name) {
        return true;
    }
    let path = parent.join(&*name);
    [".git", "node_modules"]
        .into_iter()
        .chain(at_root.then_some("target"))
        .chain(at_root.then_some(".output"))
        .chain(at_root.then_some(".build"))
        .chain(at_root.then_some(".gradle"))
        .chain(at_root.then_some(".terraform"))
        .chain(at_root.then_some("dist"))
        .chain(at_root.then_some("coverage"))
        .any(|expected| filesystem_path_alias(&path, &parent.join(expected)))
}

/// Root build output is an opaque scanner exclusion. In particular, CI may
/// mount a cache at `target` through a symlink to a location outside the
/// checkout; checking only its link metadata keeps the scanner from following
/// or reading that cache.
fn is_opaque_root_output(parent: &Path, name: &std::ffi::OsStr, at_root: bool) -> bool {
    at_root
        && (name == "target" || filesystem_path_alias(&parent.join(name), &parent.join("target")))
}

pub(crate) fn repository_files(
    root: &Path,
    exclude: &[String],
) -> Result<Vec<String>, GeneratorError> {
    repository_files_with_static_sources(root, exclude, &[])
}

pub(crate) fn repository_files_with_static_sources(
    root: &Path,
    exclude: &[String],
    static_sources: &[String],
) -> Result<Vec<String>, GeneratorError> {
    repository_files_with_static_files(root, exclude, static_sources, &[])
}

pub(crate) fn repository_files_with_static_files(
    root: &Path,
    exclude: &[String],
    static_sources: &[String],
    static_outputs: &[String],
) -> Result<Vec<String>, GeneratorError> {
    repository_files_with_static_files_and_owned_paths(
        root,
        exclude,
        static_sources,
        static_outputs,
        &[],
        None,
    )
}

pub(crate) fn repository_files_with_static_files_and_owned_paths(
    root: &Path,
    exclude: &[String],
    static_sources: &[String],
    static_outputs: &[String],
    generated_aliases: &[crate::s2::GeneratedAliasPath],
    verified_owned_paths: Option<&BTreeSet<PathBuf>>,
) -> Result<Vec<String>, GeneratorError> {
    validate_repository_tree(root)?;
    if !root.is_dir() {
        return Err(GeneratorError::usage(format!(
            "not a repository directory: {}",
            root.display()
        )));
    }
    let generator_owned = match verified_owned_paths {
        Some(verified_owned_paths) => {
            let mut paths = crate::s2::generator_fixed_output_paths_with_static_files(
                root,
                static_sources,
                static_outputs,
                generated_aliases,
            )?;
            paths.extend(verified_owned_paths.iter().cloned());
            paths
        }
        // A sidecar is evidence only after the current renderer has proved
        // each recorded path.  The first scan has no such proof, so it may
        // exclude only fixed generator paths, trusted generated aliases, and
        // declared static outputs.
        None => crate::s2::generator_fixed_output_paths_with_static_files(
            root,
            static_sources,
            static_outputs,
            generated_aliases,
        )?,
    };
    let owned_paths = generator_owned
        .iter()
        .filter_map(|path| path.to_str().map(str::to_owned))
        .chain(static_outputs.iter().cloned())
        .collect::<Vec<_>>();
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
        collect_files(root, root, &mut files)?;
        files
    };
    let excludes = exclude_set(exclude)?;
    // Dependency trees are never repository inputs, regardless of whether a
    // committed index entry or a physical walk supplied the path. Keep this
    // boundary here so every detector sees the same filtered file set.
    let mut retained = Vec::with_capacity(files.len());
    for file in files {
        if excludes.is_match(&file)
            || crate::scanner_path_matches_any_owned_path(root, &file, &owned_paths)
                .map_err(|error| GeneratorError::usage(error.to_string()))?
            || is_node_modules_path(&file)
        {
            continue;
        }
        retained.push(file);
    }
    files = retained;
    files.sort();
    Ok(files)
}

fn exclude_set(patterns: &[String]) -> Result<GlobSet, GeneratorError> {
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
    let mut files = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let relative = std::str::from_utf8(raw).map_err(|error| {
            GeneratorError::usage(format!("repository path is not utf-8: {error}"))
        })?;
        let relative = Path::new(relative);
        // Generated `.github` content is output, not project input, and the
        // remaining directories are tool or package-manager output.
        if path_has_scan_pruned_component(root, relative) {
            continue;
        }
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
        files.push(normalize_relative_path(relative)?);
    }
    if files.is_empty() && !in_git {
        return Ok(None);
    }
    Ok(Some(files))
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

/// Tool and package-manager output is not project input. `.github` is
/// intentionally absent: handwritten workflows/actions are real scan inputs.
fn is_excluded_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".output"
            | "target"
            | "node_modules"
            | ".build"
            | ".gradle"
            | ".terraform"
            | "dist"
            | "coverage"
    )
}

fn path_has_scan_pruned_component(root: &Path, relative: &Path) -> bool {
    let mut parent = root.to_path_buf();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(name) = component else {
            return false;
        };
        let at_root = index == 0;
        if is_preflight_excluded_directory(&parent, name, at_root) {
            return true;
        }
        parent.push(name);
    }
    false
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
        let kind = entry
            .file_type()
            .map_err(|error| GeneratorError::io("read file type", &path, &error))?;
        let at_root = directory == root;
        if is_preflight_excluded_directory(directory, &name, at_root) {
            continue;
        }
        if kind.is_symlink() {
            continue;
        }
        // Parity with the git-index walk, which filters tool output at the
        // repository root. Dependency trees are excluded at every depth;
        // committed fixtures under nested `dist/` and similar directories
        // remain scan inputs. `.git` metadata is never an input at any depth —
        // the index never lists it, and a linked worktree's `.git` pointer file
        // must not enter the scan either.
        if kind.is_dir() {
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

pub(crate) fn normalize_relative_path(path: &Path) -> Result<String, GeneratorError> {
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
        .filter(|file| {
            file.rsplit('/').next() == Some(name)
                && !is_test_support_path(file)
                && !is_node_modules_path(file)
        })
        .cloned()
        .collect()
}

/// Dependency manifests are never project packages, regardless of nesting.
pub(crate) fn is_node_modules_path(path: &str) -> bool {
    path.split('/').any(|segment| segment == "node_modules")
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
    use super::{files_named, repository_files, validate_repository_tree};
    use std::collections::BTreeSet;
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

    #[cfg(unix)]
    fn short_scratch(name: &str) -> PathBuf {
        let root = PathBuf::from("/tmp").join(format!("vwr-{name}-{}", crate::unique_suffix()));
        must(fs::create_dir_all(&root), "create short scratch directory");
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
            fs::create_dir_all(root.join("web/node_modules/vite")),
            "create tracked dependency directory",
        );
        must(
            fs::write(root.join("web/node_modules/vite/package.json"), "{}\n"),
            "write tracked dependency manifest",
        );
        git(&root, &["add", "web/node_modules/vite/package.json"]);
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
    fn ownership_sidecar_excludes_only_recorded_outputs() {
        let root = scratch("ownership-aware-github");
        must(
            fs::create_dir_all(root.join(".github/ci")),
            "create ownership directory",
        );
        must(
            fs::create_dir_all(root.join(".github/workflows")),
            "create workflow directory",
        );
        must(
            fs::write(
                root.join(".github/workflows/handwritten.yml"),
                "name: hand\n",
            ),
            "write handwritten workflow",
        );
        must(
            fs::write(
                root.join(".github/workflows/forged.yml"),
                "# Generated by velnor-workflow. Regenerate; do not hand-edit.\nname: forged\n",
            ),
            "write forged workflow",
        );
        must(
            fs::write(
                root.join(".github/workflows/Generated.yml"),
                "name: generated\n",
            ),
            "write recorded output",
        );
        must(
            fs::create_dir_all(root.join("state")),
            "create cache config directory",
        );
        must(
            fs::write(root.join("state/cache.env"), "MANUAL=1\n"),
            "write manual cache config",
        );
        must(
            fs::write(
                root.join(crate::s2::OWNERSHIP_STATE),
                format!(
                    "# Generated ownership state; do not edit.\nschema = 2\n[inputs]\nconfig\t0000000000000000\nscan\t0000000000000000\ngenerator\t{}\n[outputs]\n.github/workflows/generated.yml\t0000000000000000\nstate/cache.env\t0000000000000000\n",
                    crate::s2::GENERATOR_REVISION
                ),
            ),
            "write ownership sidecar",
        );

        let files = must(
            repository_files(&root, &[]),
            "scan ownership-aware repository",
        );
        assert!(files.contains(&".github/workflows/handwritten.yml".to_owned()));
        assert!(files.contains(&".github/workflows/forged.yml".to_owned()));
        assert!(files.contains(&"state/cache.env".to_owned()));
        // The initial scan has no renderer-bound sidecar proof. A recorded
        // dynamic path therefore remains an input until the convergence pass.
        assert!(files.contains(&".github/workflows/Generated.yml".to_owned()));
        assert!(!files.contains(&crate::s2::OWNERSHIP_STATE.to_owned()));

        let verified = BTreeSet::from([PathBuf::from(".github/workflows/generated.yml")]);
        let files = must(
            super::repository_files_with_static_files_and_owned_paths(
                &root,
                &[],
                &[],
                &[],
                &[],
                Some(&verified),
            ),
            "scan with renderer-bound ownership",
        );
        let recorded_path_is_alias = match (
            fs::canonicalize(root.join(".github/workflows/generated.yml")),
            fs::canonicalize(root.join(".github/workflows/Generated.yml")),
        ) {
            (Ok(recorded), Ok(candidate)) => recorded == candidate,
            _ => false,
        };
        assert_eq!(
            files.contains(&".github/workflows/Generated.yml".to_owned()),
            !recorded_path_is_alias,
            "a distinct case path stays an input on case-sensitive filesystems"
        );

        must(
            fs::write(
                root.join(crate::s2::OWNERSHIP_STATE),
                format!(
                    "# Generated ownership state; do not edit.\nschema = 2\n[inputs]\nconfig\t0000000000000000\nscan\t0000000000000000\ngenerator\t{}\n[outputs]\n.github/workflows/generated.yml\t0000000000000000\n",
                    crate::s2::GENERATOR_REVISION
                ),
            ),
            "record fleet config as generated",
        );
        let files = must(
            repository_files(&root, &[]),
            "scan after fleet config ownership is recorded",
        );
        assert!(files.contains(&"state/cache.env".to_owned()));

        let declared_source = vec!["state/cache.env".to_owned()];
        let files = must(
            super::repository_files_with_static_sources(&root, &[], &declared_source),
            "scan declared fleet config source",
        );
        assert!(files.contains(&"state/cache.env".to_owned()));

        let unrelated_source = vec!["state/other.env".to_owned()];
        let files = must(
            super::repository_files_with_static_sources(&root, &[], &unrelated_source),
            "scan without a matching static source",
        );
        assert!(files.contains(&"state/cache.env".to_owned()));
    }

    #[test]
    fn declared_static_outputs_are_excluded_on_the_first_scan() {
        let root = scratch("declared-static-output");
        let output = root.join(".github/workflows/static.yml");
        must(
            fs::create_dir_all(output.parent().unwrap_or(&root)),
            "create static output directory",
        );
        must(fs::write(&output, "name: static\n"), "write static output");
        let outputs = vec![".github/workflows/static.yml".to_owned()];
        let files = must(
            super::repository_files_with_static_files(&root, &[], &[], &outputs),
            "scan declared static output",
        );
        assert!(!files.contains(&outputs[0]));
        must(fs::remove_dir_all(root), "remove static-output root");
    }

    #[test]
    fn foreign_ownership_state_without_outputs_fails_closed() {
        let root = scratch("foreign-ownership-without-outputs");
        must(
            fs::create_dir_all(root.join(".github/ci")),
            "create ownership directory",
        );
        must(
            fs::write(
                root.join(crate::s2::OWNERSHIP_STATE),
                "# Generated ownership state; do not edit.\nschema = 99\n[inputs]\nconfig\t0000000000000000\n",
            ),
            "write foreign ownership state",
        );

        let error = must_fail(
            repository_files(&root, &[]),
            "foreign ownership state without outputs must fail closed",
        );
        assert!(
            error.contains("no parseable `[outputs]` section"),
            "error must identify missing ownership evidence: {error}"
        );
    }

    #[test]
    fn foreign_ownership_state_with_empty_outputs_fails_closed() {
        let root = scratch("foreign-ownership-empty-outputs");
        must(
            fs::create_dir_all(root.join(".github/ci")),
            "create ownership directory",
        );
        must(
            fs::write(
                root.join(crate::s2::OWNERSHIP_STATE),
                "# Generated ownership state; do not edit.\nschema = 99\n[inputs]\nconfig\t0000000000000000\n[outputs]\n",
            ),
            "write foreign ownership state",
        );

        let error = must_fail(
            repository_files(&root, &[]),
            "foreign ownership state with empty outputs must fail closed",
        );
        assert!(
            error.contains("empty `[outputs]` section"),
            "error must identify missing ownership rows: {error}"
        );
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
    fn physical_walk_matches_git_index_exclusion_depth() {
        let root = scratch("exclusion-depth");
        must(
            fs::create_dir_all(root.join("dist")),
            "create root dist directory",
        );
        must(
            fs::write(root.join("dist/bundle.js"), "bundle"),
            "write root dist file",
        );
        must(
            fs::create_dir_all(root.join("target")),
            "create root target directory",
        );
        must(
            fs::write(root.join("target/app"), "app"),
            "write root target file",
        );
        must(
            fs::create_dir_all(root.join("pkg/dist")),
            "create nested dist directory",
        );
        must(
            fs::write(root.join("pkg/dist/data.txt"), "data"),
            "write nested dist file",
        );
        must(
            fs::create_dir_all(root.join("pkg/target")),
            "create nested target directory",
        );
        must(
            fs::write(root.join("pkg/target/lib.rlib"), "lib"),
            "write nested target file",
        );
        must(
            fs::write(root.join("pkg/main.rs"), "main"),
            "write nested source",
        );
        must(
            fs::create_dir_all(root.join("pkg/node_modules/vite")),
            "create nested dependency directory",
        );
        must(
            fs::write(root.join("pkg/node_modules/vite/package.json"), "{}\n"),
            "write nested dependency manifest",
        );
        must(
            fs::create_dir_all(root.join("pkg/.git/objects")),
            "create nested git directory",
        );
        must(
            fs::write(root.join("pkg/.git/objects/pack"), "pack"),
            "write nested git file",
        );
        must(
            fs::write(root.join(".git"), "gitdir: elsewhere"),
            "write worktree pointer file",
        );

        let mut files = must(repository_files(&root, &[]), "scan plain directory");
        files.sort();
        assert_eq!(
            files,
            vec![
                "pkg/dist/data.txt".to_owned(),
                "pkg/main.rs".to_owned(),
                "pkg/target/lib.rlib".to_owned(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_external_links_inside_nested_build_directories() {
        use std::os::unix::fs::symlink;

        for directory in ["dist", "target"] {
            let root = short_scratch(&format!("nested-{directory}-external"));
            let outside = short_scratch(&format!("nested-{directory}-external-target"));
            must(
                fs::create_dir_all(root.join(format!("pkg/{directory}"))),
                "create nested build directory",
            );
            must(
                fs::write(outside.join("secret.txt"), "outside\n"),
                "write external target",
            );
            must(
                symlink(&outside, root.join(format!("pkg/{directory}/escape"))),
                "create external nested build link",
            );

            let error = must_fail(
                validate_repository_tree(&root),
                "external nested build link must fail repository preflight",
            );
            assert!(
                error.contains("escapes the repository"),
                "unexpected error: {error}"
            );

            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outside);
        }
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_dangling_links_inside_nested_build_directories() {
        use std::os::unix::fs::symlink;

        for directory in ["dist", "target"] {
            let root = short_scratch(&format!("nested-{directory}-dangling"));
            must(
                fs::create_dir_all(root.join(format!("pkg/{directory}"))),
                "create nested build directory",
            );
            must(
                symlink(
                    "missing.txt",
                    root.join(format!("pkg/{directory}/dangling")),
                ),
                "create dangling nested build link",
            );

            let error = must_fail(
                validate_repository_tree(&root),
                "dangling nested build link must fail repository preflight",
            );
            assert!(error.contains("dangling"), "unexpected error: {error}");

            let _ = fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_special_links_inside_nested_build_directories() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        for (directory, hidden_directory) in [("dist", "target"), ("target", "dist")] {
            let root = short_scratch(&format!("nested-{directory}-special"));
            let nested = root.join(format!("pkg/{directory}"));
            let hidden = root.join(hidden_directory).join("special.sock");
            must(fs::create_dir_all(&nested), "create nested build directory");
            must(
                fs::create_dir_all(must(
                    hidden.parent().ok_or("hidden socket parent"),
                    "hidden socket parent",
                )),
                "create hidden special target directory",
            );
            let listener = must(UnixListener::bind(&hidden), "create special target");
            must(
                symlink(
                    format!("../../{hidden_directory}/special.sock"),
                    nested.join("special-link"),
                ),
                "create nested build link to special target",
            );

            let error = must_fail(
                validate_repository_tree(&root),
                "nested build link to a special target must fail repository preflight",
            );
            assert!(error.contains("special file"), "unexpected error: {error}");

            drop(listener);
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn named_files_keep_web_package_and_ignore_nested_node_modules_manifests() {
        let files = [
            "package.json",
            "web/package.json",
            "web/node_modules/vite/package.json",
            "web/node_modules/vite/node_modules/esbuild/package.json",
            "node_modules/root-package/package.json",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();

        assert_eq!(
            files_named(&files, "package.json"),
            vec!["package.json".to_owned(), "web/package.json".to_owned()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_files_rejects_files_below_symlinked_parent_directories() {
        use std::os::unix::fs::symlink;

        let root = scratch("symlinked-tracked-parent");
        let outside = scratch("symlinked-tracked-target");
        git(&root, &["init", "-q"]);
        git(&root, &["commit", "--allow-empty", "-qm", "seed"]);
        must(
            fs::create_dir_all(root.join("actions/foo/scripts")),
            "create symlink fixture directory",
        );
        must(
            fs::create_dir_all(outside.join("scripts")),
            "create external fixture directory",
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
            fs::write(outside.join("scripts/main.js"), "outside target\n"),
            "write external action target",
        );
        git(&root, &["add", "actions/foo"]);
        git(&root, &["commit", "-qm", "tracked action"]);

        must(
            fs::remove_dir_all(root.join("actions/foo/scripts")),
            "remove action script directory before symlinking",
        );
        must(
            symlink(outside.join("scripts"), root.join("actions/foo/scripts")),
            "symlink action script parent",
        );

        let error = must_fail(
            repository_files(&root, &[]),
            "reject symlinked tracked file",
        );
        assert!(error.contains("escapes the repository"), "{error}");

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_allows_symlinks_confined_to_regular_files_and_directories() {
        use std::os::unix::fs::symlink;

        let root = scratch("preflight-confined-symlinks");
        must(
            fs::create_dir_all(root.join("real-dir/nested")),
            "create confined symlink directory",
        );
        must(
            fs::write(root.join("real-file.txt"), "inside\n"),
            "write confined symlink file",
        );
        must(
            fs::write(root.join("real-dir/nested/file.txt"), "inside\n"),
            "write confined symlink directory file",
        );
        must(
            symlink("real-file.txt", root.join("file-link")),
            "create confined file symlink",
        );
        must(
            symlink("real-dir", root.join("directory-link")),
            "create confined directory symlink",
        );

        must(
            validate_repository_tree(&root),
            "confined symlinks must pass repository preflight",
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_external_and_dangling_symlinks() {
        use std::os::unix::fs::symlink;

        let root = scratch("preflight-external-symlink");
        let outside = scratch("preflight-external-target");
        must(
            fs::write(outside.join("outside.txt"), "outside\n"),
            "write external target",
        );
        must(
            symlink(&outside, root.join("escape-parent")),
            "create external directory symlink",
        );
        let error = must_fail(
            validate_repository_tree(&root),
            "external symlink must fail repository preflight",
        );
        assert!(error.contains("escapes the repository"), "{error}");
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);

        let root = scratch("preflight-dangling-symlink");
        must(
            symlink("missing.txt", root.join("dangling")),
            "create dangling symlink",
        );
        let error = must_fail(
            validate_repository_tree(&root),
            "dangling symlink must fail repository preflight",
        );
        assert!(error.contains("dangling"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_a_symlinked_root_parent() {
        use std::os::unix::fs::symlink;

        let outside = scratch("preflight-root-parent-target");
        let launch = scratch("preflight-root-parent-launch");
        let root = outside.join("repository");
        must(fs::create_dir_all(&root), "create root parent target");
        must(
            symlink(&outside, launch.join("redirect")),
            "create symlinked root parent",
        );
        let redirected_root = launch.join("redirect/repository");
        let error = must_fail(
            validate_repository_tree(&redirected_root),
            "symlinked root parent must fail repository preflight",
        );
        assert!(error.contains("root component"), "{error}");
        let _ = fs::remove_dir_all(outside);
        let _ = fs::remove_dir_all(launch);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_symlinked_root_component_before_parent_normalization() {
        use std::os::unix::fs::symlink;

        let root = short_scratch("preflight-root-parent-component");
        let outside = short_scratch("preflight-root-parent-component-target");
        must(
            fs::create_dir_all(root.join("repo")),
            "create normalized repository target",
        );
        must(
            symlink(&outside, root.join("link")),
            "create symlinked path component",
        );

        let error = must_fail(
            validate_repository_tree(&root.join("link/../repo")),
            "symlink before parent normalization must fail repository preflight",
        );
        assert!(
            error.contains("root component"),
            "unexpected error: {error}"
        );
        assert!(error.contains("link"), "unexpected error: {error}");

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn preflight_rejects_special_files() {
        use std::os::unix::net::UnixListener;

        let root = short_scratch("special");
        let socket = root.join("socket");
        let listener = must(UnixListener::bind(&socket), "create unix socket");
        let error = must_fail(
            validate_repository_tree(&root),
            "special file must fail repository preflight",
        );
        assert!(error.contains("special file"), "{error}");
        drop(listener);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn repository_files_treats_a_symlinked_root_target_as_opaque() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let root = short_scratch("preflight-root-target-link");
        let outside = short_scratch("preflight-root-target-cache");
        let _listener = must(
            UnixListener::bind(outside.join("must-not-be-read.sock")),
            "create special file below external cache",
        );
        must(
            symlink(&outside, root.join("target")),
            "create symlinked root target",
        );
        let files = must(
            repository_files(&root, &[]),
            "scan with symlinked root target cache",
        );
        assert!(
            files.is_empty(),
            "root target cache entered scan: {files:?}"
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn repository_files_rejects_symlinked_git_and_dependency_directories() {
        use std::os::unix::fs::symlink;

        for name in [".git", "node_modules"] {
            let root = short_scratch("preflight-excluded-link");
            let outside = short_scratch("preflight-excluded-link-target");
            must(
                symlink(&outside, root.join(name)),
                "create symlinked excluded directory",
            );
            let error = must_fail(
                repository_files(&root, &[]),
                "symlinked excluded directory must fail preflight",
            );
            assert!(error.contains("symlinked excluded directory"), "{error}");
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(outside);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_and_short_excluded_directory_aliases_are_pruned() {
        let root = scratch("windows-excluded-aliases");
        for name in [".GIT", "TARGET", "node_modules"] {
            must(
                fs::create_dir_all(root.join(name)),
                "create excluded directory",
            );
            must(
                fs::write(root.join(name).join("must-not-scan.txt"), "cache\n"),
                "write excluded sentinel",
            );
        }
        let short_alias = root.join("NODE_M~1");
        if fs::canonicalize(&short_alias).is_ok() {
            assert!(super::is_preflight_excluded_directory(
                &root,
                std::ffi::OsStr::new("NODE_M~1"),
                true
            ));
        }
        let files = must(repository_files(&root, &[]), "scan Windows aliases");
        assert!(files.is_empty(), "excluded aliases entered scan: {files:?}");
        let _ = fs::remove_dir_all(root);
    }
}

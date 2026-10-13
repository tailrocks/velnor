//! File walk and repository-path helpers shared by every detector.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::{RepositoryShape, ScanContext};
use crate::{parent_path, GeneratorError};

pub(crate) fn repository_files(
    root: &Path,
    exclude: &[String],
) -> Result<Vec<String>, GeneratorError> {
    validate_scan_root(root)?;
    let generator_owned = crate::s2::generator_owned_output_paths(root)
        .map_err(|error| GeneratorError::usage(error.to_string()))?;
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
    files.retain(|file| {
        !excludes.is_match(file)
            && !generator_owned.contains(Path::new(file))
            && !is_node_modules_path(file)
    });
    files.sort();
    Ok(files)
}

/// Validate the tree before any scanner or generation-config reader follows a
/// repository path. The walk uses `symlink_metadata` so it never mistakes a
/// link for its target while checking the tree; link targets are resolved only
/// for the confined-target check below.
///
/// Links to regular files or directories inside `root` are valid repository
/// inputs. A dangling or escaping link would let a later direct reader inspect
/// bytes outside the repository, and a special file could block or provide a
/// device-backed read, so both fail closed. The preflight uses the same
/// boundary as the physical walk: root-level tool/output directories and
/// `.git`/`node_modules` at every depth are outside the scan boundary.
pub(crate) fn validate_scan_root(root: &Path) -> Result<(), GeneratorError> {
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
    let canonical_root = normalize_macos_system_alias(
        root.canonicalize()
            .map_err(|error| GeneratorError::io("canonicalize repository root", &root, &error))?,
    );
    validate_scan_directory(&root, &canonical_root, &root)
}

/// Make a path absolute while normalizing lexical `.` and `..` components.
/// No filesystem component is resolved here; explicit `symlink_metadata`
/// checks below enforce the no-follow boundary.
fn absolute_normalized_path(path: &Path) -> Result<PathBuf, GeneratorError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| GeneratorError::usage(format!("read current directory: {error}")))?
            .join(path)
    };
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

fn validate_scan_directory(
    root: &Path,
    canonical_root: &Path,
    directory: &Path,
) -> Result<(), GeneratorError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read repository directory", directory, &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            GeneratorError::io("read repository directory entry", directory, &error)
        })?;
        let path = entry.path();
        let name = entry.file_name();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect repository entry", &path, &error))?;
        let at_root = directory == root;

        if metadata.file_type().is_symlink() {
            if is_scan_pruned_directory(&name, at_root) {
                continue;
            }
            validate_confined_symlink(&path, canonical_root)?;
            continue;
        }

        if metadata.is_dir() {
            if is_scan_pruned_directory(&name, at_root) {
                continue;
            }
            validate_scan_directory(root, canonical_root, &path)?;
        } else if !metadata.is_file() {
            return Err(GeneratorError::usage(format!(
                "repository contains a special file: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_confined_symlink(path: &Path, canonical_root: &Path) -> Result<(), GeneratorError> {
    let target = fs::read_link(path)
        .map_err(|error| GeneratorError::io("read repository symlink", path, &error))?;
    let canonical_target =
        normalize_macos_system_alias(fs::canonicalize(path).map_err(|error| {
            GeneratorError::usage(format!(
                "repository symlink is dangling or unreadable: {} -> {} ({error})",
                path.display(),
                target.display()
            ))
        })?);
    if !canonical_target.starts_with(canonical_root) {
        return Err(GeneratorError::usage(format!(
            "repository symlink escapes the repository: {} -> {}",
            path.display(),
            target.display()
        )));
    }
    let metadata = fs::symlink_metadata(&canonical_target).map_err(|error| {
        GeneratorError::io(
            "inspect repository symlink target",
            &canonical_target,
            &error,
        )
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

fn is_scan_pruned_directory(name: &std::ffi::OsStr, at_root: bool) -> bool {
    let name = name.to_string_lossy();
    name == ".git" || name == "node_modules" || at_root && is_excluded_directory(&name)
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
        let Some(Component::Normal(leading)) = relative.components().next() else {
            return Ok(None);
        };
        // Generated `.github` content is output, not project input, and the
        // remaining directories are tool or package-manager output.
        if is_excluded_directory(&leading.to_string_lossy()) {
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
        // Parity with the git-index walk, which filters tool output at the
        // repository root. Dependency trees are excluded at every depth;
        // committed fixtures under nested `dist/` and similar directories
        // remain scan inputs. `.git` metadata is never an input at any depth —
        // the index never lists it, and a linked worktree's `.git` pointer file
        // must not enter the scan either.
        if name.as_ref() == ".git" {
            continue;
        }
        let at_root = directory == root;
        if kind.is_dir() {
            if name.as_ref() == "node_modules" || at_root && is_excluded_directory(name.as_ref()) {
                continue;
            }
            collect_files(root, &path, files)?;
        } else if kind.is_file() {
            if at_root && is_excluded_directory(name.as_ref()) {
                continue;
            }
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
    use super::{files_named, repository_files, validate_scan_root};
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

    fn short_scratch(name: &str) -> PathBuf {
        let base = if Path::new("/private/tmp").is_dir() {
            PathBuf::from("/private/tmp")
        } else {
            PathBuf::from("/tmp")
        };
        let root = base.join(format!(
            "vfw-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
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
                root.join(".github/workflows/generated.yml"),
                "name: generated\n",
            ),
            "write recorded output",
        );
        must(
            fs::create_dir_all(root.join("config/fleet")),
            "create fleet config directory",
        );
        must(
            fs::write(root.join("config/fleet/velnor-host.env"), "MANUAL=1\n"),
            "write manual fleet config",
        );
        must(
            fs::write(
                root.join(crate::s2::OWNERSHIP_STATE),
                format!(
                    "# Generated ownership state; do not edit.\nschema = 2\n[inputs]\nconfig\t0000000000000000\nscan\t0000000000000000\ngenerator\t{}\n[outputs]\n.github/workflows/generated.yml\t0000000000000000\n",
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
        assert!(files.contains(&"config/fleet/velnor-host.env".to_owned()));
        assert!(!files.contains(&".github/workflows/generated.yml".to_owned()));
        assert!(!files.contains(&crate::s2::OWNERSHIP_STATE.to_owned()));

        must(
            fs::write(
                root.join(crate::s2::OWNERSHIP_STATE),
                format!(
                    "# Generated ownership state; do not edit.\nschema = 2\n[inputs]\nconfig\t0000000000000000\nscan\t0000000000000000\ngenerator\t{}\n[outputs]\n.github/workflows/generated.yml\t0000000000000000\nconfig/fleet/velnor-host.env\t0000000000000000\n",
                    crate::s2::GENERATOR_REVISION
                ),
            ),
            "record fleet config as generated",
        );
        let files = must(
            repository_files(&root, &[]),
            "scan after fleet config ownership is recorded",
        );
        assert!(!files.contains(&"config/fleet/velnor-host.env".to_owned()));
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
    fn scan_preflight_rejects_external_links_inside_nested_build_directories() {
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
                validate_scan_root(&root),
                "external nested build link must fail scan preflight",
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
    fn scan_preflight_rejects_dangling_links_inside_nested_build_directories() {
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
                validate_scan_root(&root),
                "dangling nested build link must fail scan preflight",
            );
            assert!(error.contains("dangling"), "unexpected error: {error}");

            let _ = fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_rejects_special_links_inside_nested_build_directories() {
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
                validate_scan_root(&root),
                "nested build link to a special target must fail scan preflight",
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
    fn scan_preflight_rejects_an_external_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let root = short_scratch("external-parent");
        let outside = short_scratch("external-parent-target");
        must(
            fs::write(outside.join("secret.txt"), "outside\n"),
            "write external target",
        );
        must(
            symlink(&outside, root.join("linked")),
            "create external parent symlink",
        );

        let error = must_fail(
            validate_scan_root(&root),
            "external symlinked parent must fail scan preflight",
        );
        assert!(
            error.contains("escapes the repository"),
            "unexpected error: {error}"
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_skips_symlinked_git_metadata_entry() {
        use std::os::unix::fs::symlink;

        let root = short_scratch("symlinked-git");
        let outside = short_scratch("symlinked-git-target");
        must(
            symlink(&outside, root.join(".git")),
            "create symlinked git metadata entry",
        );
        must(
            validate_scan_root(&root),
            "symlinked git metadata is outside scan inputs",
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_skips_a_symlinked_excluded_directory() {
        use std::os::unix::fs::symlink;

        let root = short_scratch("symlinked-skipped-directory");
        let outside = short_scratch("symlinked-skipped-target");
        must(
            symlink(&outside, root.join("target")),
            "create symlinked skipped directory",
        );
        must(
            validate_scan_root(&root),
            "symlinked excluded directory is outside scan inputs",
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_skips_symlinks_at_pruned_boundaries() {
        use std::os::unix::fs::symlink;

        for name in [".git", "target", "node_modules"] {
            let root = short_scratch("confined-pruned-link");
            must(
                fs::create_dir(root.join("real-target")),
                "create confined pruned target",
            );
            must(
                symlink("real-target", root.join(name)),
                "create confined pruned symlink",
            );
            must(
                validate_scan_root(&root),
                "symlink at a pruned boundary is outside scan inputs",
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_allows_confined_symlinks_to_files_and_directories() {
        use std::os::unix::fs::symlink;

        let root = short_scratch("confined-links");
        must(
            fs::create_dir_all(root.join("real/nested")),
            "create confined target directory",
        );
        must(
            fs::write(root.join("real/file.txt"), "inside\n"),
            "write confined target file",
        );
        must(
            fs::write(root.join("real/nested/deep.txt"), "deep\n"),
            "write confined nested target file",
        );
        must(
            symlink("real/file.txt", root.join("file-link")),
            "create confined file symlink",
        );
        must(
            symlink("real", root.join("directory-link")),
            "create confined directory symlink",
        );

        must(
            validate_scan_root(&root),
            "confined regular symlinks must pass scan preflight",
        );
        let files = must(repository_files(&root, &[]), "scan confined symlink tree");
        assert!(files.contains(&"real/file.txt".to_owned()));
        assert!(!files.contains(&"file-link".to_owned()));
        assert!(!files.contains(&"directory-link".to_owned()));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_rejects_dangling_symlinks() {
        use std::os::unix::fs::symlink;

        let root = short_scratch("dangling-link");
        must(
            symlink("missing.txt", root.join("dangling")),
            "create dangling symlink",
        );
        let error = must_fail(
            validate_scan_root(&root),
            "dangling symlink must fail scan preflight",
        );
        assert!(error.contains("dangling"), "unexpected error: {error}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_rejects_symlink_loops() {
        use std::os::unix::fs::symlink;

        let root = short_scratch("symlink-loop");
        must(
            symlink("loop-b", root.join("loop-a")),
            "create first loop link",
        );
        must(
            symlink("loop-a", root.join("loop-b")),
            "create second loop link",
        );
        let error = must_fail(
            validate_scan_root(&root),
            "symlink loop must fail scan preflight",
        );
        assert!(
            error.contains("dangling or unreadable"),
            "unexpected error: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_rejects_special_files() {
        use std::os::unix::net::UnixListener;

        let root = short_scratch("special-file");
        let socket = root.join("control.sock");
        let _listener = must(UnixListener::bind(&socket), "create special socket");
        let error = must_fail(
            validate_scan_root(&root),
            "special file must fail scan preflight",
        );
        assert!(error.contains("special file"), "unexpected error: {error}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_rejects_a_symlinked_root() {
        use std::os::unix::fs::symlink;

        let root = short_scratch("root-link");
        let target = short_scratch("root-target");
        must(
            symlink(&target, root.join("repository")),
            "create symlinked root",
        );
        let error = must_fail(
            validate_scan_root(&root.join("repository")),
            "symlinked root must fail scan preflight",
        );
        assert!(
            error.contains("repository root"),
            "unexpected error: {error}"
        );
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(target);
    }

    #[cfg(unix)]
    #[test]
    fn scan_preflight_rejects_a_symlinked_root_ancestor() {
        use std::os::unix::fs::symlink;

        let outside = short_scratch("root-ancestor-target");
        let launch = short_scratch("root-ancestor-launch");
        let root = outside.join("repository");
        must(fs::create_dir_all(&root), "create root ancestor target");
        must(
            symlink(&outside, launch.join("redirect")),
            "create symlinked root ancestor",
        );
        let redirected_root = launch.join("redirect/repository");
        let error = must_fail(
            validate_scan_root(&redirected_root),
            "symlinked root ancestor must fail scan preflight",
        );
        assert!(
            error.contains("root component"),
            "unexpected error: {error}"
        );
        let _ = fs::remove_dir_all(outside);
        let _ = fs::remove_dir_all(launch);
    }

    #[cfg(unix)]
    #[test]
    fn tracked_walk_rejects_external_symlinked_parent_directories() {
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
            "external symlinked tracked parent must fail preflight",
        );
        assert!(
            error.contains("escapes the repository"),
            "unexpected error: {error}"
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}

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
    let generator_owned = crate::s2::generator_owned_output_paths(root)?;
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
    files.retain(|file| {
        !excludes.is_match(file)
            && !GENERATOR_OWNED_SCAN_FILES.contains(&file.as_str())
            && !generator_owned.contains(Path::new(file))
    });
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
        // Parity with the git-index walk, which filters on the leading path
        // component only: tool-output names stay excluded at the repository
        // root, but nested content (committed fixtures under a nested `dist/`,
        // ...) scans like any other input. `.git` metadata is never an input
        // at any depth — the index never lists it, and a linked worktree's
        // `.git` pointer file must not enter the scan either.
        if name.as_ref() == ".git" {
            continue;
        }
        let at_root = directory == root;
        if kind.is_dir() {
            if at_root && is_excluded_directory(name.as_ref()) {
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
    use super::repository_files;
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
        assert!(!files.contains(&".github/workflows/generated.yml".to_owned()));
        assert!(!files.contains(&crate::s2::OWNERSHIP_STATE.to_owned()));
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
    fn tracked_walks_skip_files_below_symlinked_parent_directories() {
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

        let scanned = must(repository_files(&root, &[]), "skip symlinked tracked file");
        assert!(!scanned.contains(&"actions/foo/scripts/main.js".to_owned()));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}

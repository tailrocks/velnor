//! File walk and repository-path helpers shared by every detector.

#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
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
        collect_files(root, root, &mut files)?;
        files
    };
    let excludes = exclude_set(exclude)?;
    files.retain(|file| {
        !excludes.is_match(file) && !GENERATOR_OWNED_SCAN_FILES.contains(&file.as_str())
    });
    files.sort();
    Ok(files)
}

/// Codex's legacy component scan has tighter bounds than the generic scan.
const CODEX_MAX_WALK_DIRECTORIES: usize = 2_000;
const CODEX_MAX_WALK_ENTRIES: usize = 20_000;
const CODEX_MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const CODEX_RESPONSE_ITEM_OVERHEAD_BYTES: usize = 64;
const CODEX_MAX_ALIAS_FALLBACK_CHECKS: usize = 20_000;
#[cfg(target_vendor = "apple")]
const CODEX_MAX_SYMLINK_HOPS: usize = 31;
#[cfg(not(target_vendor = "apple"))]
const CODEX_MAX_SYMLINK_HOPS: usize = 40;

#[cfg(test)]
thread_local! {
    static TRACKED_INDEX_SNAPSHOTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy)]
struct CodexWalkLimits {
    directories: usize,
    entries: usize,
    response_bytes: usize,
    fallback_checks: usize,
}

impl Default for CodexWalkLimits {
    fn default() -> Self {
        Self {
            directories: CODEX_MAX_WALK_DIRECTORIES,
            entries: CODEX_MAX_WALK_ENTRIES,
            response_bytes: CODEX_MAX_RESPONSE_BYTES,
            fallback_checks: CODEX_MAX_ALIAS_FALLBACK_CHECKS,
        }
    }
}

/// Restore Codex plugin files below roots that the generic repository walk
/// pruned. These files remain private to Skills validation. Codex's bounded
/// walk follows in-repository directory links, while retaining alias paths in
/// its result. Generic `SafeRoot` users continue to reject every symlink.
#[cfg(test)]
pub(crate) fn add_codex_component_files(
    root: &Path,
    declared_roots: &[String],
    exclude: &[String],
    max_depth: usize,
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    add_codex_component_files_with_limits(
        root,
        declared_roots,
        exclude,
        max_depth,
        CodexWalkLimits::default(),
        files,
    )
}

#[cfg(test)]
fn add_codex_component_files_with_limits(
    root: &Path,
    declared_roots: &[String],
    exclude: &[String],
    max_depth: usize,
    limits: CodexWalkLimits,
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    let repository = CodexRepository::new(root)?;
    add_codex_component_files_from_repository(
        &repository,
        declared_roots,
        exclude,
        max_depth,
        limits,
        files,
    )
}

pub(crate) fn add_codex_component_files_with_repository(
    repository: &CodexRepository,
    declared_roots: &[String],
    exclude: &[String],
    max_depth: usize,
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    add_codex_component_files_from_repository(
        repository,
        declared_roots,
        exclude,
        max_depth,
        CodexWalkLimits::default(),
        files,
    )
}

fn add_codex_component_files_from_repository(
    repository: &CodexRepository,
    declared_roots: &[String],
    exclude: &[String],
    max_depth: usize,
    limits: CodexWalkLimits,
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    if declared_roots.is_empty() {
        return Ok(());
    }

    let codex_root = &repository.root;
    let excludes = exclude_set(exclude)?;
    let existing = files.iter().cloned().collect::<BTreeSet<_>>();
    let declared_roots = declared_roots.iter().collect::<BTreeSet<_>>();
    let mut added = BTreeSet::new();
    let walk_context = CodexWalkContext {
        root: codex_root,
        max_depth,
        prune_hidden_directories: true,
        excludes: &excludes,
        existing: &existing,
    };

    for declared_root in declared_roots {
        let Some(resolved) = codex_root.resolve(declared_root, "Codex Skills root")? else {
            continue;
        };
        // Codex's plugin walker treats a non-directory root as empty. Keep
        // file-root link lookup in `codex_repository_path_exists`; discovery
        // must never promote a declared SKILL.md file to a skill root.
        if !resolved.is_directory {
            continue;
        }
        let initial_directory = resolved.canonical_path.as_path();
        let mut state = CodexWalkState::new(&codex_root.canonical_root, initial_directory, limits)?;
        let mut queue = VecDeque::new();
        let mut root_added = BTreeSet::new();
        state.enqueue_directory(resolved, 0, true, &mut queue)?;

        while let Some(directory) = queue.pop_front() {
            codex_walk_directory(
                &walk_context,
                &directory,
                &mut state,
                &mut queue,
                &mut root_added,
            )?;
        }

        state.emit_fallback_files(codex_root, &excludes, &existing, max_depth, &mut root_added)?;
        added.extend(root_added);
    }

    files.extend(added);
    files.sort();
    files.dedup();
    Ok(())
}

#[derive(Clone)]
struct CodexResolvedPath {
    alias_relative: String,
    canonical_relative: String,
    canonical_path: PathBuf,
    is_directory: bool,
    symlink_entries: Vec<String>,
}

#[derive(Clone, Debug)]
enum CodexPathComponent {
    Normal(String),
    Current,
    Parent,
}

enum CodexResolveOutcome {
    Resolved(CodexResolvedPath),
    Missing,
    Outside,
}

struct CodexResolveState {
    alias_relative: String,
    pending: VecDeque<CodexPathComponent>,
    resolved_components: Vec<String>,
    symlink_entries: Vec<String>,
    final_component_was_symlink: bool,
    symlink_hops: usize,
    followed_symlink: bool,
}

fn codex_relative_components(components: &[String]) -> String {
    if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    }
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(name) => normalized.push(name),
        }
    }
    normalized
}

#[derive(Clone)]
struct CodexWalkDirectory {
    alias_relative: String,
    canonical_relative: String,
    canonical_path: PathBuf,
    depth: usize,
    symlink_entries: Vec<String>,
}

#[derive(Clone)]
struct CodexDirectoryAliasFallback {
    primary: CodexWalkDirectory,
    alternative: CodexWalkDirectory,
}

#[derive(Clone)]
struct CodexCachedFile {
    resolved: CodexResolvedPath,
    excluded: bool,
    emitted: bool,
}

fn relative_suffix<'a>(path: &'a str, directory: &str) -> Option<&'a str> {
    if directory == "." {
        return Some(path);
    }
    path.strip_prefix(directory)?.strip_prefix('/')
}

struct CodexWalkContext<'a> {
    root: &'a CodexRoot,
    max_depth: usize,
    prune_hidden_directories: bool,
    excludes: &'a GlobSet,
    existing: &'a BTreeSet<String>,
}

struct CodexRoot {
    canonical_root: PathBuf,
    root_alias: PathBuf,
    tracked: Option<CodexTrackedIndex>,
    #[cfg(test)]
    followed_symlink_target_lookups: Cell<usize>,
}

struct CodexTrackedIndex {
    paths: BTreeSet<String>,
    immediate_children: BTreeMap<String, BTreeSet<String>>,
}

impl CodexTrackedIndex {
    fn new(paths: BTreeSet<String>) -> Self {
        let mut immediate_children: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for path in &paths {
            let mut parent = ".".to_owned();
            let mut components = path.split('/').peekable();
            while let Some(name) = components.next() {
                immediate_children
                    .entry(parent.clone())
                    .or_default()
                    .insert(name.to_owned());
                if components.peek().is_some() {
                    parent = join_repo_path(&parent, name);
                }
            }
        }
        Self {
            paths,
            immediate_children,
        }
    }
}

impl CodexRoot {
    fn new(root: &Path) -> Result<Self, GeneratorError> {
        let root_absolute = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| GeneratorError::io("resolve repository root", root, &error))?
                .join(root)
        };
        let root_alias = normalize_absolute_path(&root_absolute);
        let canonical_root = fs::canonicalize(root)
            .map_err(|error| GeneratorError::io("resolve repository root", root, &error))?;
        let metadata = fs::metadata(&canonical_root).map_err(|error| {
            GeneratorError::io("inspect repository root", &canonical_root, &error)
        })?;
        if !metadata.is_dir() {
            return Err(GeneratorError::usage(format!(
                "not a repository directory: {}",
                root.display()
            )));
        }
        Ok(Self {
            canonical_root,
            root_alias,
            tracked: tracked_index_paths(root)?.map(CodexTrackedIndex::new),
            #[cfg(test)]
            followed_symlink_target_lookups: Cell::new(0),
        })
    }

    fn resolve(
        &self,
        relative: &str,
        purpose: &str,
    ) -> Result<Option<CodexResolvedPath>, GeneratorError> {
        match self.resolve_confined(relative, purpose)? {
            CodexResolveOutcome::Resolved(resolved) => Ok(Some(resolved)),
            CodexResolveOutcome::Missing => Ok(None),
            CodexResolveOutcome::Outside => Err(Self::outside_error(relative, purpose)),
        }
    }

    fn resolve_confined(
        &self,
        relative: &str,
        purpose: &str,
    ) -> Result<CodexResolveOutcome, GeneratorError> {
        let components = safe_relative_components(relative, purpose)?;
        let alias_relative = if components.is_empty() {
            ".".to_owned()
        } else {
            components.join("/")
        };
        let state = CodexResolveState {
            alias_relative,
            pending: components
                .into_iter()
                .map(|component| CodexPathComponent::Normal(component.to_owned()))
                .collect(),
            resolved_components: Vec::new(),
            symlink_entries: Vec::new(),
            final_component_was_symlink: false,
            symlink_hops: 0,
            followed_symlink: false,
        };
        self.resolve_components(state)
    }

    fn resolve_components(
        &self,
        mut state: CodexResolveState,
    ) -> Result<CodexResolveOutcome, GeneratorError> {
        while let Some(component) = state.pending.pop_front() {
            match component {
                CodexPathComponent::Current => {}
                CodexPathComponent::Parent => {
                    if state.resolved_components.pop().is_none() {
                        return Ok(CodexResolveOutcome::Outside);
                    }
                }
                CodexPathComponent::Normal(component) => {
                    if let Some(outcome) = self.resolve_normal_component(&mut state, component)? {
                        return Ok(outcome);
                    }
                }
            }
        }

        // Empty and dot-only symlink targets resolve to the containing
        // directory. Every component reached here was checked without
        // following an unchecked path.
        Ok(Self::resolved_path(
            &self.canonical_root,
            &state.alias_relative,
            &state.resolved_components,
            true,
            state.symlink_entries,
        ))
    }

    fn resolve_normal_component(
        &self,
        state: &mut CodexResolveState,
        component: String,
    ) -> Result<Option<CodexResolveOutcome>, GeneratorError> {
        let parent_relative = codex_relative_components(&state.resolved_components);
        let mut candidate = self.canonical_root.clone();
        for parent in &state.resolved_components {
            candidate.push(parent);
        }
        candidate.push(&component);
        Self::note_followed_symlink_target_lookup(self, state.followed_symlink);
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if codex_is_missing_or_symlink_loop(&error) => {
                return Ok(Some(CodexResolveOutcome::Missing));
            }
            Err(error) => {
                return Err(GeneratorError::io(
                    "inspect Codex repository path",
                    &candidate,
                    &error,
                ));
            }
        };

        if metadata.file_type().is_symlink() {
            return self.resolve_symlink_component(state, &component, &candidate, &parent_relative);
        }
        if !state.pending.is_empty() && !metadata.is_dir() {
            return Ok(Some(CodexResolveOutcome::Missing));
        }
        state.resolved_components.push(component);
        if state.pending.is_empty() {
            if state.final_component_was_symlink && !metadata.is_dir() {
                // Codex follows directory symlinks only.
                return Ok(Some(CodexResolveOutcome::Missing));
            }
            return Ok(Some(Self::resolved_path(
                &self.canonical_root,
                &state.alias_relative,
                &state.resolved_components,
                metadata.is_dir(),
                std::mem::take(&mut state.symlink_entries),
            )));
        }
        Ok(None)
    }

    fn resolve_symlink_component(
        &self,
        state: &mut CodexResolveState,
        component: &str,
        path: &Path,
        parent_relative: &str,
    ) -> Result<Option<CodexResolveOutcome>, GeneratorError> {
        let index_entry = join_repo_path(parent_relative, component);
        if self
            .tracked
            .as_ref()
            .is_some_and(|tracked| !tracked.paths.contains(&index_entry))
        {
            return Ok(Some(CodexResolveOutcome::Missing));
        }
        state.symlink_hops += 1;
        if state.symlink_hops > CODEX_MAX_SYMLINK_HOPS {
            return Ok(Some(CodexResolveOutcome::Missing));
        }
        if state.pending.is_empty() {
            state.final_component_was_symlink = true;
        }
        state.symlink_entries.push(index_entry);
        state.followed_symlink = true;
        let target = match fs::read_link(path) {
            Ok(target) => target,
            Err(error) if codex_is_missing_or_symlink_loop(&error) => {
                return Ok(Some(CodexResolveOutcome::Missing));
            }
            Err(error) => {
                return Err(GeneratorError::io(
                    "read Codex repository symlink",
                    path,
                    &error,
                ));
            }
        };
        let Some(target_components) =
            self.symlink_target_components(&target, &mut state.resolved_components)
        else {
            return Ok(Some(CodexResolveOutcome::Outside));
        };
        let mut expanded = target_components;
        expanded.append(&mut state.pending);
        state.pending = expanded;
        Ok(None)
    }

    fn symlink_target_components(
        &self,
        target: &Path,
        resolved_components: &mut Vec<String>,
    ) -> Option<VecDeque<CodexPathComponent>> {
        let target = if target.is_absolute() {
            let relative = target
                .strip_prefix(&self.root_alias)
                .or_else(|_| target.strip_prefix(&self.canonical_root))
                .ok()?;
            resolved_components.clear();
            relative
        } else {
            target
        };

        target
            .components()
            .map(|component| match component {
                Component::Normal(name) => {
                    Some(CodexPathComponent::Normal(name.to_str()?.to_owned()))
                }
                Component::CurDir => Some(CodexPathComponent::Current),
                Component::ParentDir => Some(CodexPathComponent::Parent),
                Component::RootDir | Component::Prefix(_) => None,
            })
            .collect()
    }

    fn resolved_path(
        canonical_root: &Path,
        alias_relative: &str,
        resolved_components: &[String],
        is_directory: bool,
        symlink_entries: Vec<String>,
    ) -> CodexResolveOutcome {
        let canonical_relative = codex_relative_components(resolved_components);
        let mut canonical_path = canonical_root.to_path_buf();
        for component in resolved_components {
            canonical_path.push(component);
        }
        CodexResolveOutcome::Resolved(CodexResolvedPath {
            alias_relative: alias_relative.to_owned(),
            canonical_relative,
            canonical_path,
            is_directory,
            symlink_entries,
        })
    }

    fn outside_error(alias_relative: &str, purpose: &str) -> GeneratorError {
        GeneratorError::usage(format!(
            "{purpose} path {alias_relative} resolves outside the repository"
        ))
    }

    #[cfg(test)]
    fn note_followed_symlink_target_lookup(root: &Self, followed_symlink: bool) {
        if followed_symlink {
            root.followed_symlink_target_lookups
                .set(root.followed_symlink_target_lookups.get() + 1);
        }
    }

    #[cfg(not(test))]
    fn note_followed_symlink_target_lookup(_root: &Self, _followed_symlink: bool) {}

    fn child_names(
        &self,
        directory: &CodexWalkDirectory,
        remaining_entries: usize,
    ) -> Result<Vec<std::ffi::OsString>, GeneratorError> {
        if let Some(tracked) = &self.tracked {
            return Ok(codex_indexed_child_names(
                tracked,
                &directory.canonical_relative,
                remaining_entries,
            )?
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect());
        }

        let entries = fs::read_dir(&directory.canonical_path).map_err(|error| {
            GeneratorError::io(
                "read Codex repository directory",
                &directory.canonical_path,
                &error,
            )
        })?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                GeneratorError::io(
                    "read Codex repository directory entry",
                    &directory.canonical_path,
                    &error,
                )
            })?;
            if names.len() >= remaining_entries {
                return Err(GeneratorError::usage(
                    "Codex component scan exceeded the entry limit",
                ));
            }
            names.push(entry.file_name());
        }
        names.sort();
        Ok(names)
    }

    fn file_is_allowed(&self, path: &CodexResolvedPath) -> bool {
        !path.is_directory
            && self.tracked.as_ref().is_none_or(|tracked| {
                tracked.paths.contains(&path.canonical_relative)
                    && path
                        .symlink_entries
                        .iter()
                        .all(|entry| tracked.paths.contains(entry))
            })
    }
}

fn codex_indexed_child_names(
    tracked: &CodexTrackedIndex,
    canonical_directory: &str,
    remaining_entries: usize,
) -> Result<BTreeSet<String>, GeneratorError> {
    let Some(children) = tracked.immediate_children.get(canonical_directory) else {
        return Ok(BTreeSet::new());
    };
    if children.len() > remaining_entries {
        return Err(GeneratorError::usage(
            "Codex component scan exceeded the entry limit",
        ));
    }
    Ok(children.clone())
}

fn codex_is_missing_or_symlink_loop(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(unix)]
    {
        // ELOOP is errno 40 on Linux/Android and 62 on Darwin/BSD. Rust's
        // stable ErrorKind API does not expose this case yet.
        matches!(
            (std::env::consts::OS, error.raw_os_error()),
            ("linux" | "android", Some(40))
                | (
                    "macos" | "ios" | "freebsd" | "openbsd" | "netbsd" | "dragonfly",
                    Some(62)
                )
        )
    }
    #[cfg(not(unix))]
    {
        false
    }
}

struct CodexWalkState {
    limits: CodexWalkLimits,
    repository_root: PathBuf,
    visited_directories: BTreeSet<PathBuf>,
    queued_directories: BTreeSet<PathBuf>,
    emitted_paths: BTreeSet<String>,
    first_directory_aliases: BTreeMap<PathBuf, CodexWalkDirectory>,
    duplicate_directory_aliases: Vec<CodexDirectoryAliasFallback>,
    duplicate_directory_alias_keys: BTreeSet<(PathBuf, String)>,
    cached_files: BTreeMap<PathBuf, CodexCachedFile>,
    fallback_checks: usize,
    examined_entries: usize,
    response_bytes: usize,
    initial_directory: PathBuf,
}

impl CodexWalkState {
    fn new(
        repository_root: &Path,
        initial_directory: &Path,
        limits: CodexWalkLimits,
    ) -> Result<Self, GeneratorError> {
        if limits.directories == 0 {
            return Err(GeneratorError::usage(
                "Codex component scan exceeded the directory limit",
            ));
        }
        Ok(Self {
            limits,
            repository_root: repository_root.to_path_buf(),
            visited_directories: [initial_directory.to_path_buf()].into_iter().collect(),
            queued_directories: BTreeSet::new(),
            emitted_paths: BTreeSet::new(),
            first_directory_aliases: BTreeMap::new(),
            duplicate_directory_aliases: Vec::new(),
            duplicate_directory_alias_keys: BTreeSet::new(),
            cached_files: BTreeMap::new(),
            fallback_checks: 0,
            examined_entries: 0,
            response_bytes: 0,
            initial_directory: initial_directory.to_path_buf(),
        })
    }

    fn enqueue_directory(
        &mut self,
        resolved: CodexResolvedPath,
        depth: usize,
        initial_seed: bool,
        queue: &mut VecDeque<CodexWalkDirectory>,
    ) -> Result<(), GeneratorError> {
        if !resolved.is_directory {
            return Ok(());
        }

        let canonical_path = resolved.canonical_path.clone();
        if self.queued_directories.contains(&canonical_path)
            || self.first_directory_aliases.contains_key(&canonical_path)
        {
            self.record_duplicate_directory_alias(resolved, depth);
            return Ok(());
        }

        // The initial directory is counted from the start so a cycle cannot
        // evade the directory cap. Its declared alias still needs one chance
        // to seed the walk, including when it resolves through a symlink.
        let initial_alias_seed = initial_seed && canonical_path == self.initial_directory;
        if self.visited_directories.contains(&canonical_path) {
            if !initial_alias_seed {
                return Ok(());
            }
        } else {
            self.visited_directories.insert(canonical_path.clone());
        }
        if self.visited_directories.len() > self.limits.directories {
            return Err(GeneratorError::usage(
                "Codex component scan exceeded the directory limit",
            ));
        }

        let directory = CodexWalkDirectory {
            alias_relative: resolved.alias_relative,
            canonical_relative: resolved.canonical_relative,
            canonical_path: resolved.canonical_path,
            depth,
            symlink_entries: resolved.symlink_entries,
        };
        self.first_directory_aliases
            .insert(canonical_path.clone(), directory.clone());
        self.queued_directories.insert(canonical_path);
        queue.push_back(directory);
        Ok(())
    }

    fn record_duplicate_directory_alias(&mut self, resolved: CodexResolvedPath, depth: usize) {
        let Some(primary) = self
            .first_directory_aliases
            .get(&resolved.canonical_path)
            .cloned()
        else {
            return;
        };
        let alternative = CodexWalkDirectory {
            alias_relative: resolved.alias_relative,
            canonical_relative: resolved.canonical_relative,
            canonical_path: resolved.canonical_path.clone(),
            depth,
            symlink_entries: resolved.symlink_entries,
        };
        if primary.alias_relative == alternative.alias_relative
            || !self.duplicate_directory_alias_keys.insert((
                alternative.canonical_path.clone(),
                alternative.alias_relative.clone(),
            ))
        {
            return;
        }
        self.duplicate_directory_aliases
            .push(CodexDirectoryAliasFallback {
                primary,
                alternative,
            });
    }

    fn cache_file(&mut self, resolved: &CodexResolvedPath, excluded: bool) {
        self.cached_files
            .entry(resolved.canonical_path.clone())
            .or_insert_with(|| CodexCachedFile {
                resolved: resolved.clone(),
                excluded,
                emitted: false,
            });
    }

    fn mark_file_emitted(&mut self, canonical_path: &Path) {
        if let Some(candidate) = self.cached_files.get_mut(canonical_path) {
            candidate.emitted = true;
        }
    }

    fn emit_fallback_files(
        &mut self,
        root: &CodexRoot,
        excludes: &GlobSet,
        existing: &BTreeSet<String>,
        max_depth: usize,
        added: &mut BTreeSet<String>,
    ) -> Result<(), GeneratorError> {
        let aliases = self.duplicate_directory_aliases.clone();
        let mut candidates = self
            .cached_files
            .iter()
            .filter(|(_, candidate)| candidate.excluded && !candidate.emitted)
            .map(|(canonical_path, candidate)| (canonical_path.clone(), candidate.clone()))
            .collect::<Vec<_>>();

        for alias in aliases {
            if candidates.is_empty() {
                break;
            }
            for (canonical_path, candidate) in &mut candidates {
                self.examine_fallback_candidate()?;
                let Some(suffix) = relative_suffix(
                    &candidate.resolved.alias_relative,
                    &alias.primary.alias_relative,
                ) else {
                    continue;
                };
                let directory_depth = suffix.split('/').count().saturating_sub(1);
                if alias.alternative.depth.saturating_add(directory_depth) > max_depth {
                    continue;
                }

                let alias_relative = join_repo_path(&alias.alternative.alias_relative, suffix);
                if excludes.is_match(&alias_relative) {
                    continue;
                }
                let Some(symlink_suffix) = candidate
                    .resolved
                    .symlink_entries
                    .as_slice()
                    .strip_prefix(alias.primary.symlink_entries.as_slice())
                else {
                    continue;
                };
                let mut resolved = candidate.resolved.clone();
                resolved.alias_relative.clone_from(&alias_relative);
                resolved
                    .symlink_entries
                    .clone_from(&alias.alternative.symlink_entries);
                resolved.symlink_entries.extend_from_slice(symlink_suffix);
                if !root.file_is_allowed(&resolved) {
                    continue;
                }

                let newly_emitted = self.emit_file(&alias_relative)?;
                if newly_emitted && !existing.contains(&alias_relative) {
                    added.insert(alias_relative);
                }
                candidate.emitted = true;
                self.mark_file_emitted(canonical_path);
            }
            candidates.retain(|(_, candidate)| !candidate.emitted);
        }
        Ok(())
    }

    fn examine_fallback_candidate(&mut self) -> Result<(), GeneratorError> {
        if self.fallback_checks >= self.limits.fallback_checks {
            return Err(GeneratorError::usage(
                "Codex component scan exceeded the alias fallback limit",
            ));
        }
        self.fallback_checks += 1;
        Ok(())
    }

    fn examine_entry(&mut self) -> Result<(), GeneratorError> {
        if self.examined_entries >= self.limits.entries {
            return Err(GeneratorError::usage(
                "Codex component scan exceeded the entry limit",
            ));
        }
        self.examined_entries += 1;
        Ok(())
    }

    fn remaining_entries(&self) -> usize {
        self.limits.entries - self.examined_entries
    }

    fn account_path(&mut self, alias_relative: &str) -> Result<(), GeneratorError> {
        let absolute_alias = self.repository_root.join(alias_relative);
        let path_bytes = absolute_alias.to_string_lossy().len();
        let size = path_bytes
            .checked_add(CODEX_RESPONSE_ITEM_OVERHEAD_BYTES)
            .and_then(|size| self.response_bytes.checked_add(size))
            .ok_or_else(|| {
                GeneratorError::usage("Codex component scan exceeded the response-size limit")
            })?;
        if size > self.limits.response_bytes {
            return Err(GeneratorError::usage(
                "Codex component scan exceeded the response-size limit",
            ));
        }
        self.response_bytes = size;
        Ok(())
    }

    fn emit_file(&mut self, path: &str) -> Result<bool, GeneratorError> {
        if self.emitted_paths.contains(path) {
            return Ok(false);
        }
        self.account_path(path)?;
        self.emitted_paths.insert(path.to_owned());
        Ok(true)
    }
}

fn codex_walk_directory(
    context: &CodexWalkContext<'_>,
    directory: &CodexWalkDirectory,
    state: &mut CodexWalkState,
    queue: &mut VecDeque<CodexWalkDirectory>,
    added: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    for child_name in context
        .root
        .child_names(directory, state.remaining_entries())?
    {
        state.examine_entry()?;
        let Some(name) = child_name.to_str() else {
            continue;
        };
        let candidate = directory.canonical_path.join(name);
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(GeneratorError::io(
                    "inspect Codex repository entry",
                    &candidate,
                    &error,
                ));
            }
        };
        let alias_relative = join_repo_path(&directory.alias_relative, name);
        let pruned = (context.prune_hidden_directories && name.starts_with('.'))
            || directory.depth >= context.max_depth;

        if metadata.file_type().is_symlink() {
            let resolved = match context
                .root
                .resolve_confined(&alias_relative, "Codex component")?
            {
                CodexResolveOutcome::Resolved(resolved) => resolved,
                CodexResolveOutcome::Missing => continue,
                CodexResolveOutcome::Outside if pruned => continue,
                CodexResolveOutcome::Outside => {
                    return Err(CodexRoot::outside_error(&alias_relative, "Codex component"));
                }
            };
            if !resolved.is_directory {
                continue;
            }
            state.account_path(&alias_relative)?;
            if pruned {
                continue;
            }
            state.enqueue_directory(resolved, directory.depth + 1, false, queue)?;
            continue;
        }

        if metadata.is_dir() {
            let Some(resolved) = context.root.resolve(&alias_relative, "Codex component")? else {
                continue;
            };
            if !resolved.is_directory {
                continue;
            }
            state.account_path(&alias_relative)?;
            if pruned {
                continue;
            }
            state.enqueue_directory(resolved, directory.depth + 1, false, queue)?;
            continue;
        }

        if !metadata.is_file() {
            continue;
        }
        let Some(resolved) = context.root.resolve(&alias_relative, "Codex component")? else {
            continue;
        };
        if resolved.is_directory {
            continue;
        }
        codex_add_file(
            context.root,
            &resolved,
            context.excludes,
            context.existing,
            state,
            added,
        )?;
    }
    Ok(())
}

fn codex_add_file(
    root: &CodexRoot,
    resolved: &CodexResolvedPath,
    excludes: &GlobSet,
    existing: &BTreeSet<String>,
    state: &mut CodexWalkState,
    added: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    if !root.file_is_allowed(resolved) {
        return Ok(());
    }

    let filtered_by_exclude = excludes.is_match(&resolved.alias_relative);
    state.cache_file(resolved, filtered_by_exclude);
    if filtered_by_exclude {
        return Ok(());
    }

    let newly_emitted = state.emit_file(&resolved.alias_relative)?;
    state.mark_file_emitted(&resolved.canonical_path);
    if newly_emitted && !existing.contains(&resolved.alias_relative) {
        added.insert(resolved.alias_relative.clone());
    }
    Ok(())
}

/// Add Kimi Skills that the generic repository walk intentionally prunes.
///
/// Kimi checks a directory's immediate `SKILL.md` before pruning its child
/// directories. It prunes only dot-prefixed directories and `node_modules`;
/// output directories skipped by the generic walk remain traversable here.
/// `max_depth` is the greatest parent depth whose immediate child Skills are
/// checked, so candidates may sit one level deeper. Both the index and
/// physical walks follow only real directories.
pub(crate) fn add_kimi_node_modules_skill_files(
    root: &Path,
    declared_roots: &[String],
    exclude: &[String],
    max_depth: usize,
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    if declared_roots.is_empty() {
        return Ok(());
    }

    let excludes = exclude_set(exclude)?;
    let mut added = BTreeSet::new();
    let safe_root = SafeRoot { root };
    let roots = declared_roots
        .iter()
        .map(|declared_root| normalize_declared_root(declared_root))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if let Some(tracked) = tracked_index_paths(root)? {
        for declared_root in roots {
            for candidate in &tracked {
                if is_kimi_skill_in_declared_root(candidate, &declared_root, max_depth)
                    && !excludes.is_match(candidate)
                    && safe_root.is_regular_file(candidate, "Kimi SKILL.md")?
                {
                    added.insert(candidate.clone());
                }
            }
        }
    } else {
        for declared_root in roots {
            let Some(_) = safe_root.directory(&declared_root, "Kimi Skills root")? else {
                continue;
            };
            collect_kimi_declared_root_skills(
                &safe_root,
                &declared_root,
                0,
                max_depth,
                &excludes,
                &mut added,
            )?;
        }
    }

    if added.is_empty() {
        return Ok(());
    }
    files.extend(added);
    files.sort();
    files.dedup();
    Ok(())
}

/// Restore Markdown omitted by the generic walk beneath declared Kimi roots.
///
/// Kimi Skills scan each declared root, including roots under generic-excluded
/// output/package directories. Kimi commands recurse through their declared
/// trees. These paths stay in the Skills detector's private input and never
/// feed generic project detection.
pub(crate) fn add_kimi_component_files(
    root: &Path,
    skill_roots: &[String],
    command_roots: &[String],
    exclude: &[String],
    max_depth: usize,
    files: &mut Vec<String>,
) -> Result<BTreeSet<String>, GeneratorError> {
    let original = files.iter().cloned().collect::<BTreeSet<_>>();
    add_kimi_node_modules_skill_files(root, skill_roots, exclude, max_depth, files)?;

    let excludes = exclude_set(exclude)?;
    let safe_root = SafeRoot { root };
    let tracked = tracked_index_paths(root)?;
    let mut added = BTreeSet::new();

    for declared_root in skill_roots {
        let declared_root = normalize_declared_root(declared_root)?;
        add_kimi_flat_skill_markdown(
            &safe_root,
            &declared_root,
            tracked.as_ref(),
            &excludes,
            &mut added,
        )?;
    }

    for command_root in command_roots {
        let command_root = normalize_declared_root(command_root)?;
        add_kimi_command_markdown(
            &safe_root,
            &command_root,
            tracked.as_ref(),
            &excludes,
            &mut added,
        )?;
    }

    files.extend(added);
    files.sort();
    files.dedup();
    Ok(files
        .iter()
        .filter(|file| !original.contains(*file))
        .cloned()
        .collect())
}

/// Restore provider-owned Skill definitions omitted only because their path
/// contains a generic output directory (`target`, `dist`, or `coverage`).
/// Each `(root, requires_skill_directory)` pair mirrors the provider's normal
/// discovery depth: direct `SKILL.md` is allowed for explicit roots, while
/// fixed `skills/` roots require one child directory.
pub(crate) fn add_provider_skill_definition_files(
    root: &Path,
    skill_roots: &[(String, bool)],
    exclude: &[String],
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    let excludes = exclude_set(exclude)?;
    let safe_root = SafeRoot { root };
    let tracked = tracked_index_paths(root)?;
    let mut added = BTreeSet::new();
    for (declared_root, requires_skill_directory) in skill_roots {
        let declared_root = normalize_declared_root(declared_root)?;
        if let Some(tracked) = &tracked {
            for candidate in tracked {
                let Some(relative) = component_relative(candidate, &declared_root) else {
                    continue;
                };
                let components = relative.split('/').collect::<Vec<_>>();
                if components.last() != Some(&"SKILL.md")
                    || (components.len() != 2 && *requires_skill_directory)
                    || components.len() > 2
                    || components.is_empty()
                    || !has_provider_output_directory(candidate)
                    || excludes.is_match(candidate)
                {
                    continue;
                }
                if safe_root.is_regular_file(candidate, "provider Skill definition")? {
                    added.insert(candidate.clone());
                }
            }
            continue;
        }

        if safe_root
            .directory(&declared_root, "provider Skills root")?
            .is_none()
        {
            continue;
        }
        let root_skill = join_repo_path(&declared_root, "SKILL.md");
        if !requires_skill_directory
            && has_provider_output_directory(&root_skill)
            && !excludes.is_match(&root_skill)
            && safe_root.is_regular_file(&root_skill, "provider Skill definition")?
        {
            added.insert(root_skill);
        }
        for (_, child) in safe_root.child_directories(&declared_root)? {
            let skill_file = join_repo_path(&child, "SKILL.md");
            if has_provider_output_directory(&skill_file)
                && !excludes.is_match(&skill_file)
                && safe_root.is_regular_file(&skill_file, "provider Skill definition")?
            {
                added.insert(skill_file);
            }
        }
    }
    files.extend(added);
    files.sort();
    files.dedup();
    Ok(())
}

/// Restore provider-owned Claude command definitions omitted by the generic
/// output-directory pruning. Only direct `.md` commands and nested `SKILL.md`
/// command skills admitted by the normal adapter are added.
pub(crate) fn add_provider_command_files(
    root: &Path,
    command_roots: &[String],
    exclude: &[String],
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    let excludes = exclude_set(exclude)?;
    let safe_root = SafeRoot { root };
    let tracked = tracked_index_paths(root)?;
    let mut added = BTreeSet::new();
    for declared_root in command_roots {
        let declared_root = normalize_declared_root(declared_root)?;
        if let Some(tracked) = &tracked {
            for candidate in tracked {
                if provider_command_candidate(candidate, &declared_root)
                    && has_provider_output_directory(candidate)
                    && !excludes.is_match(candidate)
                    && safe_root.is_regular_file(candidate, "provider command")?
                {
                    added.insert(candidate.clone());
                }
            }
        } else {
            if safe_root.is_regular_file(&declared_root, "provider command")?
                && provider_command_candidate(&declared_root, &declared_root)
                && has_provider_output_directory(&declared_root)
                && !excludes.is_match(&declared_root)
            {
                added.insert(declared_root.clone());
                continue;
            }
            let mut examined_entries = 0usize;
            if safe_root
                .directory(&declared_root, "provider commands root")?
                .is_some()
            {
                collect_provider_command_files(
                    &safe_root,
                    &declared_root,
                    &declared_root,
                    &excludes,
                    &mut added,
                    &mut examined_entries,
                )?;
            }
        }
    }
    files.extend(added);
    files.sort();
    files.dedup();
    Ok(())
}

/// Restore files beneath discovered non-Codex/Kimi component roots when the
/// generic walk pruned a valid output-named resource directory. Returned
/// paths stay in the Skills detector's private input.
pub(crate) fn add_provider_component_resource_files(
    root: &Path,
    component_roots: &[String],
    exclude: &[String],
    files: &mut Vec<String>,
) -> Result<(), GeneratorError> {
    let excludes = exclude_set(exclude)?;
    let safe_root = SafeRoot { root };
    let tracked = tracked_index_paths(root)?;
    let mut added = BTreeSet::new();
    if let Some(tracked) = &tracked {
        for component_root in component_roots {
            let component_root = normalize_declared_root(component_root)?;
            for candidate in tracked {
                if is_path_within(candidate, &component_root)
                    && has_provider_output_directory(candidate)
                    && !excludes.is_match(candidate)
                    && safe_root.is_regular_file(candidate, "provider component resource")?
                {
                    added.insert(candidate.clone());
                }
            }
        }
    } else {
        let mut examined_entries = 0usize;
        for component_root in component_roots {
            let component_root = normalize_declared_root(component_root)?;
            if safe_root
                .directory(&component_root, "provider component root")?
                .is_some()
            {
                collect_provider_output_files(
                    &safe_root,
                    &component_root,
                    &component_root,
                    &excludes,
                    &mut added,
                    &mut examined_entries,
                )?;
            }
        }
    }
    files.extend(added);
    files.sort();
    files.dedup();
    Ok(())
}

fn component_relative<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if root == "." {
        Some(path)
    } else {
        path.strip_prefix(root)?.strip_prefix('/')
    }
}

fn has_provider_output_directory(path: &str) -> bool {
    Path::new(path).parent().is_some_and(|parent| {
        parent.components().any(|component| {
            matches!(
                component,
                Component::Normal(name)
                    if matches!(name.to_string_lossy().as_ref(), "target" | "dist" | "coverage")
            )
        })
    })
}

fn provider_command_candidate(path: &str, root: &str) -> bool {
    if path == root {
        return Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("md");
    }
    let Some(relative) = component_relative(path, root) else {
        return false;
    };
    let components = relative.split('/').collect::<Vec<_>>();
    (components.len() == 1
        && Path::new(path).extension().and_then(|ext| ext.to_str()) == Some("md"))
        || (components.len() >= 2 && components.last() == Some(&"SKILL.md"))
}

fn collect_provider_command_files(
    safe_root: &SafeRoot<'_>,
    component_root: &str,
    directory: &str,
    excludes: &GlobSet,
    added: &mut BTreeSet<String>,
    examined_entries: &mut usize,
) -> Result<(), GeneratorError> {
    *examined_entries += 1;
    if *examined_entries > 20_000 {
        return Err(GeneratorError::usage(
            "provider command recovery exceeded its 20,000-entry limit",
        ));
    }
    for candidate in safe_root.child_files(directory)? {
        *examined_entries += 1;
        if *examined_entries > 20_000 {
            return Err(GeneratorError::usage(
                "provider command recovery exceeded its 20,000-entry limit",
            ));
        }
        if provider_command_candidate(&candidate, component_root)
            && has_provider_output_directory(&candidate)
            && !excludes.is_match(&candidate)
        {
            added.insert(candidate);
        }
    }
    for (name, child) in safe_root.child_directories(directory)? {
        if is_never_walked_directory(&name) {
            continue;
        }
        collect_provider_command_files(
            safe_root,
            component_root,
            &child,
            excludes,
            added,
            examined_entries,
        )?;
    }
    Ok(())
}

fn collect_provider_output_files(
    safe_root: &SafeRoot<'_>,
    component_root: &str,
    directory: &str,
    excludes: &GlobSet,
    added: &mut BTreeSet<String>,
    examined_entries: &mut usize,
) -> Result<(), GeneratorError> {
    *examined_entries += 1;
    if *examined_entries > 20_000 {
        return Err(GeneratorError::usage(
            "provider resource recovery exceeded its 20,000-entry limit",
        ));
    }
    for candidate in safe_root.child_files(directory)? {
        *examined_entries += 1;
        if *examined_entries > 20_000 {
            return Err(GeneratorError::usage(
                "provider resource recovery exceeded its 20,000-entry limit",
            ));
        }
        if component_relative(&candidate, component_root).is_some()
            && has_provider_output_directory(&candidate)
            && !excludes.is_match(&candidate)
        {
            added.insert(candidate);
        }
    }
    for (name, child) in safe_root.child_directories(directory)? {
        if is_never_walked_directory(&name) {
            continue;
        }
        collect_provider_output_files(
            safe_root,
            component_root,
            &child,
            excludes,
            added,
            examined_entries,
        )?;
    }
    Ok(())
}

fn is_never_walked_directory(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".github" | ".output" | "node_modules" | ".build" | ".gradle" | ".terraform"
    )
}

/// Add local Markdown and template resources beneath approved Kimi scopes.
///
/// This is separate from candidate discovery so callers can first apply
/// nested-Skill permissions, then pass accepted component roots and exact
/// blocked roots. Skill scopes check direct child `SKILL.md` files before
/// pruning dot-prefixed and `node_modules` directories. Command scopes recurse
/// through those directories, matching Kimi's command walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KimiResourceKind {
    Skill,
    PrunedSkill,
    Command,
}

pub(crate) fn add_kimi_component_resource_files(
    root: &Path,
    resource_roots: &[String],
    kind: KimiResourceKind,
    exclude: &[String],
    skip_roots: &[String],
    files: &mut Vec<String>,
) -> Result<BTreeSet<String>, GeneratorError> {
    if resource_roots.is_empty() {
        return Ok(BTreeSet::new());
    }

    let original = files.iter().cloned().collect::<BTreeSet<_>>();
    let excludes = exclude_set(exclude)?;
    let safe_root = SafeRoot { root };
    let tracked = tracked_index_paths(root)?;
    let resource_roots = resource_roots
        .iter()
        .map(|resource_root| normalize_declared_root(resource_root))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let skip_roots = skip_roots
        .iter()
        .map(|skip_root| normalize_declared_root(skip_root))
        .collect::<Result<BTreeSet<_>, _>>()?;
    let mut added = BTreeSet::new();

    for resource_root in resource_roots {
        if is_within_any_root(&resource_root, &skip_roots) {
            continue;
        }
        if let Some(tracked) = &tracked {
            for candidate in tracked {
                if is_kimi_resource_in_root(candidate, &resource_root, kind)
                    && (kind != KimiResourceKind::PrunedSkill
                        || is_kimi_pruned_skill_resource(
                            candidate,
                            &resource_root,
                            tracked,
                            &safe_root,
                        )?)
                    && !is_within_any_root(candidate, &skip_roots)
                    && is_kimi_component_resource(candidate)
                    && !excludes.is_match(candidate)
                    && safe_root.is_regular_file(candidate, "Kimi component resource")?
                {
                    added.insert(candidate.clone());
                }
            }
            continue;
        }

        match safe_root.path_kind(&resource_root, "Kimi component resource root")? {
            Some(false) => {
                if is_kimi_resource_in_root(&resource_root, &resource_root, kind)
                    && !is_within_any_root(&resource_root, &skip_roots)
                    && is_kimi_component_resource(&resource_root)
                    && !excludes.is_match(&resource_root)
                    && safe_root.is_regular_file(&resource_root, "Kimi component resource")?
                {
                    added.insert(resource_root);
                }
            }
            Some(true) => collect_kimi_component_resources(
                &safe_root,
                &resource_root,
                &resource_root,
                kind,
                &skip_roots,
                &excludes,
                &mut added,
            )?,
            None => {}
        }
    }

    files.extend(added.iter().cloned());
    files.sort();
    files.dedup();
    Ok(added
        .into_iter()
        .filter(|file| !original.contains(file))
        .collect())
}

fn collect_kimi_component_resources(
    safe_root: &SafeRoot<'_>,
    directory: &str,
    resource_root: &str,
    kind: KimiResourceKind,
    skip_roots: &BTreeSet<String>,
    excludes: &GlobSet,
    added: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    for candidate in safe_root.child_files(directory)? {
        if is_kimi_component_resource(&candidate)
            && !excludes.is_match(&candidate)
            && !is_within_any_root(&candidate, skip_roots)
        {
            added.insert(candidate);
        }
    }
    for (name, child) in safe_root.child_directories(directory)? {
        if is_within_any_root(&child, skip_roots) {
            continue;
        }
        let in_template_tree = kind == KimiResourceKind::PrunedSkill
            && is_kimi_template_resource_path(&child, resource_root);
        if kind == KimiResourceKind::PrunedSkill
            && !in_template_tree
            && (is_kimi_pruned_directory(&name)
                || safe_root.is_regular_file(
                    &join_repo_path(&child, "SKILL.md"),
                    "nested Kimi Skill resource",
                )?)
        {
            continue;
        }
        if kind == KimiResourceKind::Skill && is_kimi_pruned_directory(&name) {
            let skill_file = join_repo_path(&child, "SKILL.md");
            if !excludes.is_match(&skill_file)
                && !is_within_any_root(&skill_file, skip_roots)
                && safe_root.is_regular_file(&skill_file, "Kimi nested Skill resource")?
            {
                added.insert(skill_file);
            }
            continue;
        }
        collect_kimi_component_resources(
            safe_root,
            &child,
            resource_root,
            kind,
            skip_roots,
            excludes,
            added,
        )?;
    }
    Ok(())
}

fn is_kimi_resource_in_root(path: &str, root: &str, kind: KimiResourceKind) -> bool {
    if kind == KimiResourceKind::Command || path == root {
        return is_path_within(path, root);
    }
    let Some(relative) = path.strip_prefix(&path_prefix(root)) else {
        return false;
    };
    let components = relative.split('/').collect::<Vec<_>>();
    let Some(file_name) = components.last().copied() else {
        return false;
    };
    let directories = &components[..components.len() - 1];
    let ancestors = &directories[..directories.len().saturating_sub(1)];
    ancestors
        .iter()
        .all(|directory| !is_kimi_pruned_directory(directory))
        && directories
            .last()
            .is_none_or(|directory| !is_kimi_pruned_directory(directory) || file_name == "SKILL.md")
}

fn is_kimi_pruned_skill_resource(
    path: &str,
    root: &str,
    tracked: &BTreeSet<String>,
    safe_root: &SafeRoot<'_>,
) -> Result<bool, GeneratorError> {
    let Some(relative) = path.strip_prefix(&path_prefix(root)) else {
        return Ok(false);
    };
    let components = relative.split('/').collect::<Vec<_>>();
    let directories = &components[..components.len().saturating_sub(1)];
    let in_template_tree = components
        .first()
        .is_some_and(|component| *component == "templates");
    let mut ancestor = root.to_owned();
    for directory in directories {
        if !in_template_tree && is_kimi_pruned_directory(directory) {
            return Ok(false);
        }
        ancestor = join_repo_path(&ancestor, directory);
        let marker = join_repo_path(&ancestor, "SKILL.md");
        if !in_template_tree
            && tracked.contains(&marker)
            && safe_root.is_regular_file(&marker, "nested Kimi Skill resource")?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_kimi_template_resource_path(path: &str, root: &str) -> bool {
    path.strip_prefix(&path_prefix(root))
        .and_then(|relative| relative.split('/').next())
        .is_some_and(|component| component == "templates")
}

fn is_within_any_root(path: &str, roots: &BTreeSet<String>) -> bool {
    roots.iter().any(|root| is_path_within(path, root))
}

fn is_kimi_component_resource(path: &str) -> bool {
    ["md", "json", "toml", "yaml", "yml"]
        .iter()
        .any(|extension| has_extension(path, extension))
}

fn add_kimi_flat_skill_markdown(
    safe_root: &SafeRoot<'_>,
    declared_root: &str,
    tracked: Option<&BTreeSet<String>>,
    excludes: &GlobSet,
    added: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    if let Some(tracked) = tracked {
        for candidate in tracked {
            if is_direct_child(candidate, declared_root)
                && has_lowercase_md_extension(candidate)
                && !excludes.is_match(candidate)
                && safe_root.is_regular_file(candidate, "Kimi flat skill")?
            {
                added.insert(candidate.clone());
            }
        }
        return Ok(());
    }

    for candidate in safe_root.child_files(declared_root)? {
        if has_lowercase_md_extension(&candidate) && !excludes.is_match(&candidate) {
            added.insert(candidate);
        }
    }
    Ok(())
}

fn add_kimi_command_markdown(
    safe_root: &SafeRoot<'_>,
    command_root: &str,
    tracked: Option<&BTreeSet<String>>,
    excludes: &GlobSet,
    added: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    if let Some(tracked) = tracked {
        for candidate in tracked {
            let in_root = candidate == command_root || is_path_within(candidate, command_root);
            if in_root
                && has_lowercase_md_extension(candidate)
                && !excludes.is_match(candidate)
                && safe_root.is_regular_file(candidate, "Kimi command")?
            {
                added.insert(candidate.clone());
            }
        }
        return Ok(());
    }

    if safe_root.is_regular_file(command_root, "Kimi command")? {
        if has_lowercase_md_extension(command_root) && !excludes.is_match(command_root) {
            added.insert(command_root.to_owned());
        }
        return Ok(());
    }
    if safe_root
        .directory(command_root, "Kimi commands root")?
        .is_none()
    {
        return Ok(());
    }
    collect_kimi_command_markdown(safe_root, command_root, excludes, added)
}

fn collect_kimi_command_markdown(
    safe_root: &SafeRoot<'_>,
    directory: &str,
    excludes: &GlobSet,
    added: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    for candidate in safe_root.child_files(directory)? {
        if has_lowercase_md_extension(&candidate) && !excludes.is_match(&candidate) {
            added.insert(candidate);
        }
    }
    for (_, child) in safe_root.child_directories(directory)? {
        collect_kimi_command_markdown(safe_root, &child, excludes, added)?;
    }
    Ok(())
}

fn is_direct_child(path: &str, directory: &str) -> bool {
    let relative = if directory == "." {
        path
    } else if let Some(relative) = path.strip_prefix(&format!("{directory}/")) {
        relative
    } else {
        return false;
    };
    !relative.contains('/')
}

fn is_path_within(path: &str, directory: &str) -> bool {
    directory == "." || path == directory || path.starts_with(&format!("{directory}/"))
}

fn has_lowercase_md_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension == "md")
}

fn normalize_declared_root(root: &str) -> Result<String, GeneratorError> {
    if root == "." {
        return Ok(root.to_owned());
    }
    if root.is_empty()
        || root.starts_with('/')
        || root.contains('\\')
        || root.chars().any(char::is_control)
        || is_windows_drive_path(root)
        || root
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
    {
        return Err(GeneratorError::usage(format!(
            "Kimi Skills root {root} must stay inside the repository"
        )));
    }
    Ok(root.to_owned())
}

fn is_kimi_skill_in_declared_root(file: &str, declared_root: &str, max_depth: usize) -> bool {
    let Some(relative) = file.strip_prefix(&path_prefix(declared_root)) else {
        return false;
    };
    let components = relative.split('/').collect::<Vec<_>>();
    let Some("SKILL.md") = components.last().copied() else {
        return false;
    };
    let directories = &components[..components.len() - 1];
    directories.len() <= max_depth.saturating_add(1)
        && directories
            .iter()
            .take(directories.len().saturating_sub(1))
            .all(|directory| !is_kimi_pruned_directory(directory))
}

fn is_kimi_pruned_directory(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules"
}

fn collect_kimi_declared_root_skills(
    safe_root: &SafeRoot<'_>,
    directory: &str,
    depth: usize,
    max_depth: usize,
    excludes: &GlobSet,
    added: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    add_skill_at_directory(safe_root, directory, None, excludes, added)?;
    for (name, child) in safe_root.child_directories(directory)? {
        // Match the pinned Kimi ordering: inspect this directory's Skill
        // before deciding whether its contents can be traversed.
        add_skill_at_directory(safe_root, &child, None, excludes, added)?;
        if is_kimi_pruned_directory(&name) || depth >= max_depth {
            continue;
        }
        collect_kimi_declared_root_skills(
            safe_root,
            &child,
            depth + 1,
            max_depth,
            excludes,
            added,
        )?;
    }
    Ok(())
}

fn add_skill_at_directory(
    safe_root: &SafeRoot<'_>,
    directory: &str,
    tracked: Option<&BTreeSet<String>>,
    excludes: &GlobSet,
    added: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    let skill_file = join_repo_path(directory, "SKILL.md");
    if excludes.is_match(&skill_file) || tracked.is_some_and(|paths| !paths.contains(&skill_file)) {
        return Ok(());
    }
    if safe_root.is_regular_file(&skill_file, "Kimi SKILL.md")? {
        added.insert(skill_file);
    }
    Ok(())
}

struct SafeRoot<'a> {
    root: &'a Path,
}

impl SafeRoot<'_> {
    fn directory(&self, relative: &str, purpose: &str) -> Result<Option<PathBuf>, GeneratorError> {
        let components = safe_relative_components(relative, purpose)?;
        let mut path = self.root.to_path_buf();
        if components.is_empty() {
            return Ok(path.is_dir().then_some(path));
        }
        for component in &components {
            path.push(component);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(GeneratorError::io("inspect repository path", &path, &error));
                }
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Ok(None);
            }
        }
        Ok(Some(path))
    }

    fn is_regular_file(&self, relative: &str, purpose: &str) -> Result<bool, GeneratorError> {
        let components = safe_relative_components(relative, purpose)?;
        let Some((leaf, parents)) = components.split_last() else {
            return Ok(false);
        };
        let mut path = self.root.to_path_buf();
        for parent in parents {
            path.push(parent);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => {
                    return Err(GeneratorError::io("inspect repository path", &path, &error));
                }
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Ok(false);
            }
        }
        path.push(leaf);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(GeneratorError::io("inspect repository file", &path, &error)),
        }
    }

    fn child_directories(&self, relative: &str) -> Result<Vec<(String, String)>, GeneratorError> {
        let Some(directory) = self.directory(relative, "Kimi Skills directory")? else {
            return Ok(Vec::new());
        };
        let entries = fs::read_dir(&directory)
            .map_err(|error| GeneratorError::io("read repository directory", &directory, &error))?;
        let mut children = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                GeneratorError::io("read repository directory entry", &directory, &error)
            })?;
            let file_type = entry.file_type().map_err(|error| {
                GeneratorError::io("read repository entry type", &entry.path(), &error)
            })?;
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let child = join_repo_path(relative, &name);
            if self
                .directory(&child, "Kimi Skills child directory")?
                .is_none()
            {
                continue;
            }
            children.push((name, child));
        }
        children.sort();
        Ok(children)
    }

    fn child_files(&self, relative: &str) -> Result<Vec<String>, GeneratorError> {
        let Some(directory) = self.directory(relative, "repository directory")? else {
            return Ok(Vec::new());
        };
        let entries = fs::read_dir(&directory)
            .map_err(|error| GeneratorError::io("read repository directory", &directory, &error))?;
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                GeneratorError::io("read repository directory entry", &directory, &error)
            })?;
            let file_type = entry.file_type().map_err(|error| {
                GeneratorError::io("read repository entry type", &entry.path(), &error)
            })?;
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            files.push(join_repo_path(relative, &name));
        }
        files.sort();
        Ok(files)
    }

    fn path_kind(&self, relative: &str, purpose: &str) -> Result<Option<bool>, GeneratorError> {
        let components = safe_relative_components(relative, purpose)?;
        if components.is_empty() {
            return Ok(self.root.is_dir().then_some(true));
        }
        let mut path = self.root.to_path_buf();
        for (index, component) in components.iter().enumerate() {
            path.push(component);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => {
                    return Err(GeneratorError::io("inspect repository path", &path, &error));
                }
            };
            if metadata.file_type().is_symlink() {
                return Ok(None);
            }
            let is_leaf = index + 1 == components.len();
            if !is_leaf && !metadata.is_dir() {
                return Ok(None);
            }
            if is_leaf {
                if metadata.is_file() {
                    return Ok(Some(false));
                }
                if metadata.is_dir() {
                    return Ok(Some(true));
                }
                return Ok(None);
            }
        }
        Ok(None)
    }
}

fn safe_relative_components<'a>(
    relative: &'a str,
    purpose: &str,
) -> Result<Vec<&'a str>, GeneratorError> {
    if relative == "." {
        return Ok(Vec::new());
    }
    if relative.is_empty()
        || relative.starts_with('/')
        || relative.contains('\\')
        || relative.chars().any(char::is_control)
        || is_windows_drive_path(relative)
    {
        return Err(GeneratorError::usage(format!(
            "{purpose} path {relative} must stay inside the repository"
        )));
    }
    let components = relative.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| matches!(*component, "" | "." | ".."))
    {
        return Err(GeneratorError::usage(format!(
            "{purpose} path {relative} must stay inside the repository"
        )));
    }
    Ok(components)
}

fn tracked_index_paths(root: &Path) -> Result<Option<BTreeSet<String>>, GeneratorError> {
    #[cfg(test)]
    TRACKED_INDEX_SNAPSHOTS.with(|count| count.set(count.get() + 1));
    if !inside_git_work_tree(root) {
        return Ok(None);
    }
    let output = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|_| {
            GeneratorError::usage(
                "git ls-files could not run inside a git work tree; scan will not walk the filesystem",
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(GeneratorError::usage(format!(
            "git ls-files failed inside a git work tree; scan will not walk the filesystem: {stderr}"
        )));
    }
    let mut files = BTreeSet::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let relative = std::str::from_utf8(raw).map_err(|error| {
            GeneratorError::usage(format!("repository path is not utf-8: {error}"))
        })?;
        let relative = Path::new(relative);
        if !matches!(relative.components().next(), Some(Component::Normal(_))) {
            return Ok(None);
        }
        files.insert(normalize_relative_path(relative)?);
    }
    Ok(Some(files))
}

fn is_windows_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

pub(crate) fn excluded_existing_files(
    root: &Path,
    candidates: &BTreeSet<String>,
    exclude: &[String],
) -> Result<BTreeSet<String>, GeneratorError> {
    let excludes = exclude_set(exclude)?;
    let tracked = tracked_index_paths(root)?;
    let safe_root = SafeRoot { root };
    let mut excluded_candidates = BTreeSet::new();
    for candidate in candidates {
        if !excludes.is_match(candidate)
            || tracked
                .as_ref()
                .is_some_and(|paths| !paths.contains(candidate))
        {
            continue;
        }
        if safe_root.is_regular_file(candidate, "excluded Kimi skill definition")? {
            excluded_candidates.insert(candidate.clone());
        }
    }
    Ok(excluded_candidates)
}

/// A scan-local Codex repository view. It snapshots the Git index once and
/// reuses that state for every discovered definition and Markdown link.
pub(crate) struct CodexRepository {
    root: CodexRoot,
}

impl CodexRepository {
    pub(crate) fn new(root: &Path) -> Result<Self, GeneratorError> {
        Ok(Self {
            root: CodexRoot::new(root)?,
        })
    }

    pub(crate) fn path(
        &self,
        relative: &str,
        exclude: &[String],
        purpose: &str,
    ) -> Result<Option<PathBuf>, GeneratorError> {
        let excludes = exclude_set(exclude)?;
        let Some(resolved) = self.root.resolve(relative, purpose)? else {
            return Ok(None);
        };
        if !self.root.file_is_allowed(&resolved) || excludes.is_match(&resolved.alias_relative) {
            return Ok(None);
        }
        Ok(Some(resolved.canonical_path))
    }

    /// Check a Codex link target through the repository view, while applying
    /// the hidden-directory policy of any declared root that contains it.
    /// The lookup is depth-unbounded and reuses the scan's tracked index.
    pub(crate) fn link_exists(
        &self,
        relative: &str,
        exclude: &[String],
        scan_roots: &[String],
    ) -> Result<bool, GeneratorError> {
        let excludes = exclude_set(exclude)?;
        safe_relative_components(relative, "Codex Markdown link")?;
        if codex_alias_has_hidden_parent(relative, scan_roots) {
            return Ok(false);
        }
        let resolved = match self
            .root
            .resolve_confined(relative, "Codex Markdown link")?
        {
            CodexResolveOutcome::Resolved(resolved) => resolved,
            CodexResolveOutcome::Missing => return Ok(false),
            CodexResolveOutcome::Outside
                if codex_alias_has_hidden_component(relative, scan_roots) =>
            {
                return Ok(false);
            }
            CodexResolveOutcome::Outside => {
                return Err(CodexRoot::outside_error(relative, "Codex Markdown link"));
            }
        };
        if !codex_alias_is_visible_from_scan_roots(
            &resolved.alias_relative,
            resolved.is_directory,
            scan_roots,
        ) {
            return Ok(false);
        }
        if !resolved.is_directory {
            return Ok(self.root.file_is_allowed(&resolved)
                && !excludes.is_match(&resolved.alias_relative));
        }

        let mut state = CodexWalkState::new(
            &self.root.canonical_root,
            &resolved.canonical_path,
            CodexWalkLimits::default(),
        )?;
        let mut queue = VecDeque::new();
        state.enqueue_directory(resolved, 0, true, &mut queue)?;
        let mut files = BTreeSet::new();
        let existing = BTreeSet::new();
        let walk_context = CodexWalkContext {
            root: &self.root,
            max_depth: usize::MAX,
            prune_hidden_directories: true,
            excludes: &excludes,
            existing: &existing,
        };
        while let Some(directory) = queue.pop_front() {
            codex_walk_directory(
                &walk_context,
                &directory,
                &mut state,
                &mut queue,
                &mut files,
            )?;
        }
        state.emit_fallback_files(&self.root, &excludes, &existing, usize::MAX, &mut files)?;
        Ok(!files.is_empty())
    }
}

/// Compatibility wrapper for callers that need one-off Codex resolution.
#[cfg(test)]
pub(crate) fn codex_repository_path(
    root: &Path,
    relative: &str,
    exclude: &[String],
    purpose: &str,
) -> Result<Option<PathBuf>, GeneratorError> {
    CodexRepository::new(root)?.path(relative, exclude, purpose)
}

/// Check whether a Codex Markdown target is available through the same
/// canonical root, tracked-index, hidden-directory, and exclude rules used by
/// component scans. This walk is depth-unbounded: links can point beyond the
/// skill discovery depth without expanding discovery itself.
#[cfg(test)]
pub(crate) fn codex_repository_path_exists(
    root: &Path,
    relative: &str,
    exclude: &[String],
) -> Result<bool, GeneratorError> {
    codex_repository_link_exists(root, relative, exclude, &[])
}

/// Check a Codex link target through the repository view, while applying the
/// hidden-directory policy of any declared root that contains the target.
/// The lookup is not limited by skill discovery depth.
#[cfg(test)]
pub(crate) fn codex_repository_link_exists(
    root: &Path,
    relative: &str,
    exclude: &[String],
    scan_roots: &[String],
) -> Result<bool, GeneratorError> {
    CodexRepository::new(root)?.link_exists(relative, exclude, scan_roots)
}

fn codex_alias_is_visible_from_scan_roots(
    path: &str,
    is_directory: bool,
    scan_roots: &[String],
) -> bool {
    let mut matched_scan_root = false;
    for scan_root in scan_roots {
        let relative = if path == scan_root {
            ""
        } else if scan_root == "." {
            path
        } else if let Some(relative) = path.strip_prefix(&format!("{scan_root}/")) {
            relative
        } else {
            continue;
        };
        matched_scan_root = true;
        if relative.is_empty() {
            return true;
        }
        let components = relative.split('/').collect::<Vec<_>>();
        let directory_count = if is_directory {
            components.len()
        } else {
            components.len().saturating_sub(1)
        };
        if components[..directory_count]
            .iter()
            .all(|component| !component.starts_with('.'))
        {
            return true;
        }
    }
    !matched_scan_root
}

fn codex_alias_has_hidden_parent(path: &str, scan_roots: &[String]) -> bool {
    codex_alias_suffixes(path, scan_roots).any(|suffix| {
        let components = suffix.split('/').collect::<Vec<_>>();
        components
            .get(..components.len().saturating_sub(1))
            .is_some_and(|parents| parents.iter().any(|component| component.starts_with('.')))
    })
}

fn codex_alias_has_hidden_component(path: &str, scan_roots: &[String]) -> bool {
    codex_alias_suffixes(path, scan_roots).any(|suffix| {
        suffix
            .split('/')
            .any(|component| component.starts_with('.'))
    })
}

fn codex_alias_suffixes<'a>(
    path: &'a str,
    scan_roots: &'a [String],
) -> impl Iterator<Item = &'a str> {
    scan_roots.iter().filter_map(move |scan_root| {
        if path == scan_root {
            Some("")
        } else if scan_root == "." {
            Some(path)
        } else {
            path.strip_prefix(&format!("{scan_root}/"))
        }
    })
}

pub(crate) fn repository_path_exists(
    root: &Path,
    relative: &str,
    exclude: &[String],
) -> Result<bool, GeneratorError> {
    let excludes = exclude_set(exclude)?;
    let safe_root = SafeRoot { root };
    let Some(is_directory) = safe_root.path_kind(relative, "Markdown link")? else {
        return Ok(false);
    };
    if !is_directory {
        if excludes.is_match(relative) {
            return Ok(false);
        }
        let tracked = tracked_index_paths(root)?;
        return Ok(tracked.is_none_or(|files| files.contains(relative)));
    }

    let prefix = if relative == "." {
        String::new()
    } else {
        format!("{relative}/")
    };
    if let Some(tracked) = tracked_index_paths(root)? {
        for file in tracked {
            if file.starts_with(&prefix)
                && !excludes.is_match(&file)
                && safe_root.is_regular_file(&file, "Markdown link target")?
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    directory_has_nonexcluded_file(&safe_root, relative, &excludes)
}

pub(crate) fn repository_path_is_file(
    root: &Path,
    relative: &str,
    exclude: &[String],
) -> Result<bool, GeneratorError> {
    let excludes = exclude_set(exclude)?;
    if excludes.is_match(relative) {
        return Ok(false);
    }
    let safe_root = SafeRoot { root };
    if safe_root.path_kind(relative, "Kimi Skills root")? != Some(false) {
        return Ok(false);
    }
    Ok(tracked_index_paths(root)?.is_none_or(|files| files.contains(relative)))
}

fn directory_has_nonexcluded_file(
    safe_root: &SafeRoot<'_>,
    directory: &str,
    excludes: &GlobSet,
) -> Result<bool, GeneratorError> {
    for file in safe_root.child_files(directory)? {
        if !excludes.is_match(&file) {
            return Ok(true);
        }
    }
    for (_, child) in safe_root.child_directories(directory)? {
        if directory_has_nonexcluded_file(safe_root, &child, excludes)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn is_git_work_tree(root: &Path) -> bool {
    inside_git_work_tree(root)
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
        let Some(Component::Normal(_)) = relative.components().next() else {
            return Ok(None);
        };
        // Generated `.github` content is output, not project input, and the
        // remaining directories are tool or package-manager output. Match
        // every directory component, just as the physical walk does.
        if path_has_excluded_directory_component(relative) {
            continue;
        }
        let absolute = root.join(relative);
        let Ok(metadata) = fs::symlink_metadata(&absolute) else {
            // Staged but deleted in the work tree: the detectors cannot read
            // it, so it cannot inform generation.
            continue;
        };
        if metadata.is_dir() || metadata.file_type().is_symlink() {
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

/// Generated `.github` content is output, not project input; the remaining
/// directories are tool or package-manager output.
fn is_excluded_directory(name: &str) -> bool {
    matches!(
        name,
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
    )
}

fn path_has_excluded_directory_component(path: &Path) -> bool {
    path.parent().is_some_and(|parent| {
        parent.components().any(|component| {
            matches!(
                component,
                Component::Normal(name) if is_excluded_directory(&name.to_string_lossy())
            )
        })
    })
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
            if is_excluded_directory(name.as_ref()) {
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

#[cfg(test)]
mod tests {
    use super::{
        add_codex_component_files, add_codex_component_files_with_limits, add_kimi_component_files,
        add_kimi_component_resource_files, add_kimi_node_modules_skill_files,
        codex_indexed_child_names, codex_repository_path, codex_repository_path_exists,
        repository_files, repository_path_exists, repository_path_is_file, CodexRepository,
        CodexResolveOutcome, CodexRoot, CodexTrackedIndex, CodexWalkLimits, KimiResourceKind,
        CODEX_RESPONSE_ITEM_OVERHEAD_BYTES, TRACKED_INDEX_SNAPSHOTS,
    };
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

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            must(fs::create_dir_all(parent), "create fixture parent");
        }
        must(fs::write(path, contents), "write fixture file");
    }

    #[cfg(unix)]
    fn symlink(target: &Path, link: &Path) {
        must(
            std::os::unix::fs::symlink(target, link),
            "create fixture symlink",
        );
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
    fn tracked_repository_files_prune_excluded_directories_at_any_depth() {
        let root = scratch("tracked-excluded-components");
        git(&root, &["init", "-q"]);
        for (path, contents) in [
            ("src/main.rs", "source\n"),
            ("packages/target/debug/out.txt", "build output\n"),
            ("packages/dist/assets/out.css", "generated output\n"),
            ("packages/coverage/report.json", "coverage output\n"),
            ("packages/node_modules/pkg/index.js", "package output\n"),
            ("packages/.github/workflows/generated.yml", "generated CI\n"),
            ("packages/targeted/keep.txt", "not target\n"),
            ("packages/docs/target", "file named target\n"),
        ] {
            write(&root, path, contents);
        }
        git(&root, &["add", "."]);

        let files = must(
            repository_files(&root, &[]),
            "scan index with nested excluded directories",
        );
        assert_eq!(
            files,
            vec![
                "packages/docs/target".to_owned(),
                "packages/targeted/keep.txt".to_owned(),
                "src/main.rs".to_owned(),
            ]
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

    #[cfg(unix)]
    #[test]
    fn codex_walk_follows_in_root_directory_alias_and_keeps_alias_paths() {
        let root = scratch("codex-in-root-alias");
        write(&root, "target/SKILL.md", "---\nname: alias\n---\n");
        symlink(Path::new("target"), &root.join("skills"));
        let mut files = Vec::new();

        must(
            add_codex_component_files(&root, &["skills".to_owned()], &[], 6, &mut files),
            "walk Codex directory alias",
        );
        assert_eq!(files, vec!["skills/SKILL.md"]);
        assert_eq!(
            must(
                codex_repository_path(&root, "skills/SKILL.md", &[], "Codex resource"),
                "resolve Codex file through alias",
            ),
            Some(must(root.canonicalize(), "canonicalize fixture root").join("target/SKILL.md"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn codex_walk_accepts_a_declared_alias_to_the_repository_root() {
        let root = scratch("codex-root-alias");
        write(&root, "SKILL.md", "---\nname: root\n---\n");
        symlink(Path::new("."), &root.join("root-alias"));
        let mut files = Vec::new();

        must(
            add_codex_component_files(&root, &["root-alias".to_owned()], &[], 0, &mut files),
            "walk declared root alias",
        );
        assert_eq!(files, vec!["root-alias/SKILL.md"]);
        assert!(must(
            codex_repository_path_exists(&root, "root-alias", &[]),
            "check root alias directory target",
        ));
    }

    #[cfg(unix)]
    #[test]
    fn codex_walk_uses_later_alias_when_first_alias_is_excluded() {
        let root = scratch("codex-excluded-first-alias");
        write(&root, "target/SKILL.md", "---\nname: fallback\n---\n");
        must(fs::create_dir_all(root.join("skills")), "create Codex root");
        symlink(Path::new("../target"), &root.join("skills/a"));
        symlink(Path::new("../target"), &root.join("skills/b"));
        symlink(Path::new("../target"), &root.join("skills/c"));
        let excludes = vec!["skills/a/SKILL.md".to_owned()];
        let mut files = Vec::new();

        must(
            add_codex_component_files(&root, &["skills".to_owned()], &excludes, 6, &mut files),
            "walk excluded primary alias with fallback",
        );
        assert_eq!(files, vec!["skills/b/SKILL.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn codex_walk_prunes_hidden_aliases_and_excludes_alias_paths() {
        let root = scratch("codex-hidden-excluded-alias");
        write(&root, "hidden-target/SKILL.md", "hidden alias target\n");
        write(&root, "excluded-target/SKILL.md", "excluded alias target\n");
        must(fs::create_dir_all(root.join("skills")), "create Codex root");
        symlink(Path::new("../hidden-target"), &root.join("skills/.hidden"));
        symlink(
            Path::new("../excluded-target"),
            &root.join("skills/excluded"),
        );
        let excludes = vec!["skills/excluded/SKILL.md".to_owned()];
        let mut files = Vec::new();

        must(
            add_codex_component_files(&root, &["skills".to_owned()], &excludes, 6, &mut files),
            "walk Codex aliases with hidden and excluded paths",
        );
        assert!(files.is_empty());
        assert!(must(
            codex_repository_path_exists(&root, "skills/.hidden/SKILL.md", &[]),
            "check hidden alias target",
        ));
        assert!(!must(
            codex_repository_path_exists(&root, "skills/excluded/SKILL.md", &excludes),
            "check excluded alias target",
        ));
    }

    #[cfg(unix)]
    #[test]
    fn codex_path_resolver_skips_file_symlinks() {
        let root = scratch("codex-file-symlink");
        write(&root, "skills/SKILL.md", "---\nname: root\n---\n");
        symlink(Path::new("SKILL.md"), &root.join("skills/linked.md"));
        let mut files = Vec::new();

        must(
            add_codex_component_files(&root, &["skills".to_owned()], &[], 6, &mut files),
            "walk Codex root with file symlink",
        );
        assert_eq!(files, vec!["skills/SKILL.md"]);
        assert_eq!(
            must(
                codex_repository_path(&root, "skills/linked.md", &[], "Codex resource"),
                "resolve Codex file symlink",
            ),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn codex_skipped_file_symlinks_do_not_consume_response_bytes() {
        for use_git_index in [false, true] {
            let root = scratch("codex-file-symlink-response-budget");
            write(&root, "target.md", "target\n");
            must(
                fs::create_dir_all(root.join("skills")),
                "create Codex skill root",
            );
            symlink(Path::new("../target.md"), &root.join("skills/file-link.md"));
            if use_git_index {
                git(&root, &["init", "-q"]);
                git(&root, &["add", "skills/file-link.md"]);
            }

            let mut files = Vec::new();
            must(
                add_codex_component_files_with_limits(
                    &root,
                    &["skills".to_owned()],
                    &[],
                    6,
                    CodexWalkLimits {
                        directories: 1,
                        entries: 1,
                        response_bytes: 0,
                        fallback_checks: 0,
                    },
                    &mut files,
                ),
                "skip Codex file symlink before response-byte accounting",
            );
            assert!(files.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn codex_walk_deduplicates_cycles_and_multiple_directory_aliases() {
        let root = scratch("codex-alias-cycle");
        write(&root, "skills/target/SKILL.md", "---\nname: target\n---\n");
        symlink(Path::new("target"), &root.join("skills/a"));
        symlink(Path::new("target"), &root.join("skills/b"));
        symlink(Path::new(".."), &root.join("skills/target/loop"));
        let mut files = Vec::new();

        must(
            add_codex_component_files(&root, &["skills".to_owned()], &[], 6, &mut files),
            "walk Codex cyclic aliases",
        );
        assert_eq!(files, vec!["skills/a/SKILL.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn codex_walk_skips_symlink_loops() {
        let root = scratch("codex-symlink-loop");
        must(fs::create_dir_all(root.join("skills")), "create Codex root");
        symlink(Path::new("loop"), &root.join("skills/loop"));
        let mut files = Vec::new();

        must(
            add_codex_component_files(&root, &["skills".to_owned()], &[], 6, &mut files),
            "skip filesystem symlink loop",
        );
        assert!(files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn codex_walk_rejects_external_directory_symlinks() {
        let fixture = scratch("codex-external-alias");
        let root = fixture.join("repo");
        must(
            fs::create_dir_all(root.join("skills")),
            "create repository root",
        );
        write(&fixture, "outside/SKILL.md", "outside\n");
        symlink(&fixture.join("outside"), &root.join("skills/external"));
        let error = must_fail(
            add_codex_component_files(&root, &["skills".to_owned()], &[], 6, &mut Vec::new()),
            "reject external Codex directory target",
        );
        assert!(error.contains("outside the repository"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn codex_resolver_rejects_external_symlink_before_target_metadata() {
        let fixture = scratch("codex-external-target-resolver");
        let root = fixture.join("repo");
        let outside = fixture.join("outside");
        must(
            fs::create_dir_all(root.join("skills")),
            "create repository root",
        );
        must(fs::create_dir_all(&outside), "create external directory");
        write(&outside, "guide.md", "external data\n");
        must(
            fs::create_dir_all(root.join("internal-target")),
            "create internal target directory",
        );
        symlink(
            Path::new("../internal-target"),
            &root.join("skills/internal"),
        );
        symlink(&outside, &root.join("skills/external"));
        let codex_root = must(CodexRoot::new(&root), "open Codex root");

        assert!(matches!(
            must(
                codex_root.resolve_confined("skills/internal", "Codex test path"),
                "resolve in-repository alias"
            ),
            CodexResolveOutcome::Resolved(resolved) if resolved.is_directory
        ));
        let inside_target_lookups = codex_root.followed_symlink_target_lookups.get();
        assert!(inside_target_lookups > 0);
        assert!(matches!(
            must(
                codex_root.resolve_confined("skills/external", "Codex test path"),
                "resolve external alias within root"
            ),
            CodexResolveOutcome::Outside
        ));
        assert_eq!(
            codex_root.followed_symlink_target_lookups.get(),
            inside_target_lookups,
            "external target must not be inspected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn codex_walk_prunes_hidden_and_too_deep_external_symlinks_before_confinement() {
        for use_git_index in [false, true] {
            let fixture = scratch("codex-pruned-external-alias");
            let root = fixture.join("repo");
            let outside_skill = fixture.join("outside");
            let blocked_target = fixture.join("not-a-directory").join("child");
            must(
                fs::create_dir_all(root.join("skills/one/two/three/four/five/six")),
                "create depth-six Codex parent",
            );
            write(
                &fixture,
                "outside/SKILL.md",
                "---\nname: external\ndescription: Outside target.\n---\n",
            );
            write(
                &fixture,
                "not-a-directory",
                "block symlink target traversal\n",
            );
            symlink(&outside_skill, &root.join("skills/.hidden"));
            symlink(&blocked_target, &root.join("skills/.unreadable"));
            symlink(
                &outside_skill,
                &root.join("skills/one/two/three/four/five/six/external"),
            );
            symlink(
                &blocked_target,
                &root.join("skills/one/two/three/four/five/six/unreadable"),
            );
            if use_git_index {
                git(&root, &["init", "-q"]);
                git(&root, &["add", "-f", "."]);
            }

            let mut files = Vec::new();
            must(
                add_codex_component_files(&root, &["skills".to_owned()], &[], 6, &mut files),
                "prune hidden and over-depth Codex aliases without resolving them",
            );
            assert!(files.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn codex_git_walk_requires_tracked_aliases_and_canonical_files() {
        let root = scratch("codex-indexed-alias");
        git(&root, &["init", "-q"]);
        write(&root, "target/SKILL.md", "---\nname: tracked\n---\n");
        symlink(Path::new("target"), &root.join("skills"));
        git(&root, &["add", "target/SKILL.md", "skills"]);
        write(&root, "target/untracked.md", "untracked\n");
        symlink(Path::new("target"), &root.join("untracked-skills"));

        let mut files = Vec::new();
        must(
            add_codex_component_files(
                &root,
                &["skills".to_owned(), "untracked-skills".to_owned()],
                &[],
                6,
                &mut files,
            ),
            "walk indexed Codex alias",
        );
        assert_eq!(files, vec!["skills/SKILL.md"]);
        assert_eq!(
            must(
                codex_repository_path(&root, "skills/SKILL.md", &[], "Codex resource"),
                "resolve tracked canonical file",
            ),
            Some(must(root.canonicalize(), "canonicalize fixture root").join("target/SKILL.md"))
        );
        assert_eq!(
            must(
                codex_repository_path(&root, "skills/untracked.md", &[], "Codex resource"),
                "resolve untracked canonical file",
            ),
            None
        );
        assert!(!must(
            codex_repository_path_exists(&root, "skills/untracked.md", &[]),
            "check untracked canonical link target",
        ));
    }

    #[test]
    fn codex_git_index_preindexes_direct_children() {
        let mut paths = (0..5_000)
            .map(|index| format!("unrelated/dir-{index}/nested/file.md"))
            .collect::<BTreeSet<_>>();
        paths.insert("target/direct.md".to_owned());
        paths.insert("target/nested/guide.md".to_owned());
        let index = CodexTrackedIndex::new(paths);

        let children = must(
            codex_indexed_child_names(&index, "target", 2),
            "lookup direct children from large index",
        );
        assert_eq!(
            children,
            ["direct.md".to_owned(), "nested".to_owned()]
                .into_iter()
                .collect()
        );
        let error = must_fail(
            codex_indexed_child_names(&index, "target", 1),
            "enforce child entry cap against the directory index",
        );
        assert!(error.contains("entry limit"), "{error}");
    }

    #[test]
    fn codex_repository_reuses_one_git_index_snapshot_for_many_links() {
        let root = scratch("codex-index-snapshot-reuse");
        git(&root, &["init", "-q"]);
        for index in 0..12 {
            write(&root, &format!("resources/{index}.md"), "# Resource\n");
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "resources"]);

        TRACKED_INDEX_SNAPSHOTS.with(|count| count.set(0));
        let repository = must(CodexRepository::new(&root), "open Codex repository view");
        for index in 0..12 {
            assert!(must(
                repository.link_exists(&format!("resources/{index}.md"), &[], &[]),
                "check Codex link through cached index",
            ));
        }
        let snapshots = TRACKED_INDEX_SNAPSHOTS.with(std::cell::Cell::get);
        assert_eq!(snapshots, 1, "one tracked-index snapshot per view");
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn codex_symlink_resolution_matches_the_apple_hop_limit() {
        let root = scratch("codex-apple-symlink-hop-limit");
        write(&root, "payload/file", "readable\n");
        for count in [31, 32] {
            let prefix = if count == 31 { "inside" } else { "over" };
            for index in 0..count {
                let link = root.join(format!("{prefix}{index}"));
                let target = if index + 1 == count {
                    PathBuf::from("payload")
                } else {
                    PathBuf::from(format!("{prefix}{}", index + 1))
                };
                symlink(&target, &link);
            }
        }

        assert!(fs::metadata(root.join("inside0/file")).is_ok());
        assert!(fs::metadata(root.join("over0/file")).is_err());
        let repository = must(CodexRepository::new(&root), "open Codex root");
        assert!(must(
            repository.path("inside0/file", &[], "Codex hop-limit fixture"),
            "resolve 31-link Codex path",
        )
        .is_some());
        assert!(must(
            repository.path("over0/file", &[], "Codex hop-limit fixture"),
            "resolve 32-link Codex path",
        )
        .is_none());
    }

    #[test]
    fn codex_walk_caps_fail_closed_without_mutating_inputs() {
        let root = scratch("codex-caps");
        write(&root, "skills/child/one.md", "one\n");
        write(&root, "skills/child/two.md", "two\n");

        let mut files = vec!["existing.md".to_owned()];
        let directory_error = must_fail(
            add_codex_component_files_with_limits(
                &root,
                &["skills".to_owned()],
                &[],
                6,
                CodexWalkLimits {
                    directories: 1,
                    entries: 10,
                    response_bytes: 10_000,
                    fallback_checks: 10,
                },
                &mut files,
            ),
            "enforce Codex directory cap",
        );
        assert!(
            directory_error.contains("directory limit"),
            "{directory_error}"
        );
        assert_eq!(files, vec!["existing.md"]);

        let entry_error = must_fail(
            add_codex_component_files_with_limits(
                &root,
                &["skills".to_owned()],
                &[],
                6,
                CodexWalkLimits {
                    directories: 10,
                    entries: 1,
                    response_bytes: 10_000,
                    fallback_checks: 10,
                },
                &mut files,
            ),
            "enforce Codex entry cap",
        );
        assert!(entry_error.contains("entry limit"), "{entry_error}");
        assert_eq!(files, vec!["existing.md"]);

        let response_error = must_fail(
            add_codex_component_files_with_limits(
                &root,
                &["skills".to_owned()],
                &[],
                6,
                CodexWalkLimits {
                    directories: 10,
                    entries: 10,
                    response_bytes: 1,
                    fallback_checks: 10,
                },
                &mut files,
            ),
            "enforce Codex response-size cap",
        );
        assert!(
            response_error.contains("response-size limit"),
            "{response_error}"
        );
        assert_eq!(files, vec!["existing.md"]);
    }

    #[test]
    fn codex_walk_accounts_for_depth_pruned_directory_paths() {
        let root = scratch("codex-directory-response-cap");
        let long_name = "long-directory-name-".repeat(12);
        let relative = format!("skills/{long_name}");
        must(
            fs::create_dir_all(root.join(&relative)),
            "create long Codex directory",
        );
        let absolute_path = must(root.canonicalize(), "canonicalize fixture root").join(&relative);
        let exact_response_bytes =
            absolute_path.to_string_lossy().len() + CODEX_RESPONSE_ITEM_OVERHEAD_BYTES;
        let mut files = Vec::new();

        let error = must_fail(
            add_codex_component_files_with_limits(
                &root,
                &["skills".to_owned()],
                &[],
                0,
                CodexWalkLimits {
                    directories: 10,
                    entries: 10,
                    response_bytes: exact_response_bytes - 1,
                    fallback_checks: 10,
                },
                &mut files,
            ),
            "count a depth-pruned Codex directory response item",
        );
        assert!(error.contains("response-size limit"), "{error}");
        assert!(files.is_empty(), "directory-only fixture emitted files");

        must(
            add_codex_component_files_with_limits(
                &root,
                &["skills".to_owned()],
                &[],
                0,
                CodexWalkLimits {
                    directories: 10,
                    entries: 10,
                    response_bytes: exact_response_bytes,
                    fallback_checks: 10,
                },
                &mut files,
            ),
            "accept the exact Codex directory response boundary",
        );
        assert!(files.is_empty(), "directory-only fixture emitted files");
    }

    #[test]
    fn codex_walk_applies_response_caps_per_declared_root() {
        let root = scratch("codex-per-root-caps");
        write(&root, "one/a.md", "one\n");
        write(&root, "two/b.md", "two\n");
        let canonical_root = must(root.canonicalize(), "canonicalize fixture root");
        let response_bytes = canonical_root
            .join("one/a.md")
            .to_string_lossy()
            .len()
            .max(canonical_root.join("two/b.md").to_string_lossy().len())
            + CODEX_RESPONSE_ITEM_OVERHEAD_BYTES;
        let mut files = Vec::new();

        must(
            add_codex_component_files_with_limits(
                &root,
                &["one".to_owned(), "two".to_owned()],
                &[],
                6,
                CodexWalkLimits {
                    directories: 1,
                    entries: 1,
                    response_bytes,
                    fallback_checks: 0,
                },
                &mut files,
            ),
            "apply Codex response cap per declared root",
        );
        assert_eq!(files, vec!["one/a.md", "two/b.md"]);
    }

    #[cfg(unix)]
    #[test]
    fn codex_alias_fallback_work_cap_fails_closed_without_mutating_inputs() {
        let root = scratch("codex-fallback-cap");
        write(&root, "target/SKILL.md", "skill\n");
        must(fs::create_dir_all(root.join("skills")), "create Codex root");
        symlink(Path::new("../target"), &root.join("skills/a"));
        symlink(Path::new("../target"), &root.join("skills/b"));
        let mut files = vec!["existing.md".to_owned()];

        let error = must_fail(
            add_codex_component_files_with_limits(
                &root,
                &["skills".to_owned()],
                &["skills/a/SKILL.md".to_owned()],
                6,
                CodexWalkLimits {
                    directories: 10,
                    entries: 10,
                    response_bytes: 10_000,
                    fallback_checks: 0,
                },
                &mut files,
            ),
            "enforce Codex alias fallback work cap",
        );
        assert!(error.contains("alias fallback limit"), "{error}");
        assert_eq!(files, vec!["existing.md"]);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one fixture covers Kimi's child-before-prune traversal boundary"
    )]
    fn kimi_recovery_checks_candidates_before_pruning_and_walks_generic_output_dirs() {
        let root = scratch("kimi-node-modules-plain");
        write(
            &root,
            "plugins/example/skills/node_modules/SKILL.md",
            "---\nname: direct\n---\n",
        );
        write(
            &root,
            "plugins/example/skills/node_modules/nested/SKILL.md",
            "ignored nested skill\n",
        );
        write(
            &root,
            "plugins/example/skills/group/node_modules/SKILL.md",
            "---\nname: grouped\n---\n",
        );
        write(
            &root,
            "plugins/example/skills/group/node_modules/deeper/SKILL.md",
            "ignored nested skill\n",
        );
        write(
            &root,
            "plugins/example/skills/.hidden/SKILL.md",
            "already in ordinary walk\n",
        );
        write(
            &root,
            "plugins/example/skills/.hidden/node_modules/SKILL.md",
            "ignored hidden subtree\n",
        );
        write(
            &root,
            "plugins/example/skills/.hidden/nested/SKILL.md",
            "ignored hidden descendant\n",
        );
        write(
            &root,
            "plugins/example/skills/.github/SKILL.md",
            "direct hidden candidate\n",
        );
        write(
            &root,
            "plugins/example/skills/.github/nested/SKILL.md",
            "ignored hidden descendant\n",
        );
        write(
            &root,
            "plugins/example/skills/target/SKILL.md",
            "output directory direct candidate\n",
        );
        write(
            &root,
            "plugins/example/skills/target/nested/SKILL.md",
            "walkable output descendant\n",
        );
        write(
            &root,
            "plugins/example/skills/dist/nested/SKILL.md",
            "walkable output descendant\n",
        );
        write(
            &root,
            "plugins/example/skills/coverage/SKILL.md",
            "output directory direct candidate\n",
        );
        write(
            &root,
            "outside/node_modules/SKILL.md",
            "outside declared root\n",
        );

        let generic_files = must(repository_files(&root, &[]), "scan plain fixture");
        assert!(generic_files
            .iter()
            .any(|file| { file == "plugins/example/skills/.hidden/SKILL.md" }));
        for pruned_by_generic_walk in [
            "plugins/example/skills/node_modules/SKILL.md",
            "plugins/example/skills/.github/SKILL.md",
            "plugins/example/skills/target/SKILL.md",
        ] {
            assert!(!generic_files
                .iter()
                .any(|file| file == pruned_by_generic_walk));
        }

        let mut kimi_files = Vec::new();

        must(
            add_kimi_node_modules_skill_files(
                &root,
                &["plugins/example/skills".to_owned()],
                &[],
                8,
                &mut kimi_files,
            ),
            "recover Kimi Skills from generic walk exclusions",
        );
        assert_eq!(
            kimi_files,
            vec![
                "plugins/example/skills/.github/SKILL.md".to_owned(),
                "plugins/example/skills/.hidden/SKILL.md".to_owned(),
                "plugins/example/skills/coverage/SKILL.md".to_owned(),
                "plugins/example/skills/dist/nested/SKILL.md".to_owned(),
                "plugins/example/skills/group/node_modules/SKILL.md".to_owned(),
                "plugins/example/skills/node_modules/SKILL.md".to_owned(),
                "plugins/example/skills/target/SKILL.md".to_owned(),
                "plugins/example/skills/target/nested/SKILL.md".to_owned(),
            ]
        );
    }

    #[test]
    fn kimi_node_modules_skill_walk_obeys_declared_root_depth() {
        let root = scratch("kimi-node-modules-depth");
        write(
            &root,
            "skills/one/node_modules/SKILL.md",
            "---\nname: within\n---\n",
        );
        write(
            &root,
            "skills/one/two/node_modules/SKILL.md",
            "outside depth\n",
        );
        let mut files = Vec::new();

        must(
            add_kimi_node_modules_skill_files(&root, &["skills".to_owned()], &[], 1, &mut files),
            "add Kimi Skills through depth one",
        );
        assert_eq!(files, vec!["skills/one/node_modules/SKILL.md".to_owned()]);
    }

    #[test]
    fn kimi_git_walk_recovers_tracked_skills_from_generic_output_directories() {
        let root = scratch("kimi-node-modules-index");
        git(&root, &["init", "-q"]);
        write(&root, "node_modules/SKILL.md", "---\nname: tracked\n---\n");
        write(
            &root,
            "package/target/SKILL.md",
            "---\nname: target-root\n---\n",
        );
        write(
            &root,
            "package/target/nested/SKILL.md",
            "---\nname: target-nested\n---\n",
        );
        write(
            &root,
            "package/target/excluded/SKILL.md",
            "---\nname: excluded\n---\n",
        );
        git(&root, &["add", "node_modules/SKILL.md", "package/target"]);
        write(&root, "package/node_modules/SKILL.md", "untracked skill\n");
        let excludes = vec!["package/target/excluded/SKILL.md".to_owned()];

        let mut files = must(repository_files(&root, &excludes), "scan indexed fixture");
        assert!(files.is_empty());
        must(
            add_kimi_node_modules_skill_files(
                &root,
                &[".".to_owned(), "package".to_owned()],
                &excludes,
                8,
                &mut files,
            ),
            "recover tracked Kimi Skills and respect excludes",
        );
        assert_eq!(
            files,
            vec![
                "node_modules/SKILL.md".to_owned(),
                "package/target/SKILL.md".to_owned(),
                "package/target/nested/SKILL.md".to_owned(),
            ]
        );
    }

    #[test]
    fn kimi_supplement_respects_excludes_for_physical_walk_candidates() {
        let root = scratch("kimi-node-modules-excluded-physical");
        write(
            &root,
            "plugins/example/skills/node_modules/SKILL.md",
            "---\nname: excluded\n---\n",
        );
        let excludes = vec!["plugins/example/skills/node_modules/SKILL.md".to_owned()];

        let mut files = must(
            repository_files(&root, &excludes),
            "scan physical fixture with excludes",
        );
        assert!(files.is_empty());
        must(
            add_kimi_node_modules_skill_files(
                &root,
                &["plugins/example/skills".to_owned()],
                &excludes,
                8,
                &mut files,
            ),
            "add Kimi Skills without excluded candidates",
        );
        assert!(files.is_empty());
    }

    #[test]
    fn kimi_supplement_respects_excludes_for_indexed_candidates() {
        let root = scratch("kimi-node-modules-excluded-index");
        git(&root, &["init", "-q"]);
        write(
            &root,
            "plugins/example/skills/node_modules/SKILL.md",
            "---\nname: excluded\n---\n",
        );
        git(
            &root,
            &["add", "plugins/example/skills/node_modules/SKILL.md"],
        );
        let excludes = vec!["plugins/example/skills/node_modules/SKILL.md".to_owned()];

        let mut files = must(
            repository_files(&root, &excludes),
            "scan indexed fixture with excludes",
        );
        assert!(files.is_empty());
        must(
            add_kimi_node_modules_skill_files(
                &root,
                &["plugins/example/skills".to_owned()],
                &excludes,
                8,
                &mut files,
            ),
            "add Kimi Skills without excluded indexed candidates",
        );
        assert!(files.is_empty());
    }

    #[test]
    fn kimi_declared_root_inside_node_modules_enumerates_cli_candidates_safely() {
        let root = scratch("kimi-node-modules-declared-root");
        let declared_root = "node_modules/example-plugin/skills";
        write(
            &root,
            "node_modules/example-plugin/skills/SKILL.md",
            "---\nname: root\n---\n",
        );
        write(
            &root,
            "node_modules/example-plugin/skills/child/SKILL.md",
            "---\nname: child\n---\n",
        );
        write(
            &root,
            "node_modules/example-plugin/skills/node_modules/SKILL.md",
            "---\nname: package-manager-child\n---\n",
        );
        write(
            &root,
            "node_modules/example-plugin/skills/node_modules/package/SKILL.md",
            "nested package-manager skill\n",
        );
        write(
            &root,
            "node_modules/example-plugin/skills/child/nested/SKILL.md",
            "nested child skill\n",
        );
        write(
            &root,
            "node_modules/example-plugin/skills/node_modules/.hidden/SKILL.md",
            "pruned package skill\n",
        );
        write(
            &root,
            "node_modules/example-plugin/skills/.hidden/SKILL.md",
            "direct hidden candidate\n",
        );
        write(
            &root,
            "node_modules/example-plugin/skills/.hidden/nested/SKILL.md",
            "pruned hidden skill\n",
        );
        write(
            &root,
            "node_modules/example-plugin/skills/excluded/SKILL.md",
            "excluded candidate\n",
        );
        let excludes = vec![format!("{declared_root}/excluded/SKILL.md")];
        let mut files = must(
            repository_files(&root, &excludes),
            "scan repository with node_modules root",
        );

        must(
            add_kimi_node_modules_skill_files(
                &root,
                &[declared_root.to_owned()],
                &excludes,
                8,
                &mut files,
            ),
            "add direct Kimi Skills from declared node_modules root",
        );
        assert_eq!(
            files,
            vec![
                format!("{declared_root}/.hidden/SKILL.md"),
                format!("{declared_root}/SKILL.md"),
                format!("{declared_root}/child/SKILL.md"),
                format!("{declared_root}/child/nested/SKILL.md"),
                "node_modules/example-plugin/skills/node_modules/SKILL.md".to_owned(),
            ]
        );
    }

    #[test]
    fn kimi_git_declared_node_modules_root_uses_only_tracked_candidates() {
        let root = scratch("kimi-node-modules-declared-root-index");
        git(&root, &["init", "-q"]);
        write(
            &root,
            "node_modules/pkg/skills/SKILL.md",
            "---\nname: root\n---\n",
        );
        write(
            &root,
            "node_modules/pkg/skills/child/SKILL.md",
            "---\nname: child\n---\n",
        );
        write(
            &root,
            "node_modules/pkg/skills/child/nested/SKILL.md",
            "---\nname: nested\n---\n",
        );
        git(&root, &["add", "node_modules/pkg/skills"]);
        write(
            &root,
            "node_modules/pkg/skills/untracked/SKILL.md",
            "untracked skill\n",
        );

        let mut files = must(repository_files(&root, &[]), "scan indexed repository");
        assert!(files.is_empty());
        must(
            add_kimi_node_modules_skill_files(
                &root,
                &["node_modules/pkg/skills".to_owned()],
                &[],
                8,
                &mut files,
            ),
            "add only indexed Kimi candidates from declared node_modules root",
        );
        assert_eq!(
            files,
            vec![
                "node_modules/pkg/skills/SKILL.md".to_owned(),
                "node_modules/pkg/skills/child/SKILL.md".to_owned(),
                "node_modules/pkg/skills/child/nested/SKILL.md".to_owned(),
            ]
        );
    }

    #[test]
    fn kimi_component_candidate_recovery_defers_support_resources_until_scope_approval() {
        let root = scratch("kimi-node-modules-components");
        write(&root, "node_modules/pkg/skills/quick.md", "# Flat skill\n");
        write(
            &root,
            "node_modules/pkg/skills/nested/readme.md",
            "not a top-level flat skill or owned support file\n",
        );
        write(
            &root,
            "node_modules/pkg/skills/directory/SKILL.md",
            "---\nname: directory\ndescription: Skill.\n---\n",
        );
        write(
            &root,
            "node_modules/pkg/skills/directory/references/guide.md",
            "# Support\n",
        );
        write(&root, "node_modules/pkg/commands/run.md", "# Run\n");
        write(
            &root,
            "node_modules/pkg/commands/.hidden/review.md",
            "# Hidden command\n",
        );
        write(
            &root,
            "node_modules/pkg/commands/node_modules/tool.md",
            "# Nested command\n",
        );
        let mut files = must(repository_files(&root, &[]), "scan component fixture");

        let recovered = must(
            add_kimi_component_files(
                &root,
                &["node_modules/pkg/skills".to_owned()],
                &["node_modules/pkg/commands".to_owned()],
                &[],
                8,
                &mut files,
            ),
            "recover Kimi component paths",
        );
        assert!(!files
            .iter()
            .any(|file| file == "node_modules/pkg/skills/directory/references/guide.md"));
        assert_eq!(
            files,
            vec![
                "node_modules/pkg/commands/.hidden/review.md".to_owned(),
                "node_modules/pkg/commands/node_modules/tool.md".to_owned(),
                "node_modules/pkg/commands/run.md".to_owned(),
                "node_modules/pkg/skills/directory/SKILL.md".to_owned(),
                "node_modules/pkg/skills/quick.md".to_owned(),
            ]
        );
        assert!(!recovered.contains("node_modules/pkg/skills/nested/readme.md"));

        let resources = must(
            add_kimi_component_resource_files(
                &root,
                &["node_modules/pkg/skills/directory".to_owned()],
                KimiResourceKind::Skill,
                &[],
                &[],
                &mut files,
            ),
            "recover approved Kimi Skill resources",
        );
        assert_eq!(
            resources,
            ["node_modules/pkg/skills/directory/references/guide.md".to_owned()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        assert!(files
            .iter()
            .any(|file| file == "node_modules/pkg/skills/directory/references/guide.md"));
    }

    #[cfg(unix)]
    #[test]
    fn kimi_component_resource_recovery_respects_index_excludes_and_symlinks() {
        use std::os::unix::fs::symlink;

        let root = scratch("kimi-component-resources");
        git(&root, &["init", "-q"]);
        for (path, contents) in [
            (
                "plugins/example/skills/tool/SKILL.md",
                "---\nname: tool\n---\n",
            ),
            (
                "plugins/example/skills/tool/references/guide.md",
                "# Guide\n",
            ),
            ("plugins/example/skills/tool/templates/config.json", "{}\n"),
            (
                "plugins/example/skills/tool/templates/settings.toml",
                "key = true\n",
            ),
            (
                "plugins/example/skills/tool/templates/settings.yaml",
                "key: true\n",
            ),
            (
                "plugins/example/skills/tool/templates/settings.yml",
                "key: true\n",
            ),
            (
                "plugins/example/skills/tool/templates/excluded.json",
                "{}\n",
            ),
            (
                "plugins/example/skills/tool/templates/ignored.txt",
                "not a supported resource\n",
            ),
            ("outside/leak.md", "external\n"),
        ] {
            write(&root, path, contents);
        }
        must(
            symlink(
                root.join("outside/leak.md"),
                root.join("plugins/example/skills/tool/references/leak.md"),
            ),
            "create resource file symlink",
        );
        must(
            symlink(
                root.join("outside"),
                root.join("plugins/example/skills/tool/references/external"),
            ),
            "create resource directory symlink",
        );
        git(&root, &["add", "plugins/example/skills/tool"]);
        write(
            &root,
            "plugins/example/skills/tool/references/untracked.md",
            "untracked\n",
        );
        let excludes = vec!["plugins/example/skills/tool/templates/excluded.json".to_owned()];
        let mut files = Vec::new();

        let recovered = must(
            add_kimi_component_resource_files(
                &root,
                &["plugins/example/skills/tool".to_owned()],
                KimiResourceKind::Skill,
                &excludes,
                &[],
                &mut files,
            ),
            "recover indexed Kimi resources",
        );
        assert_eq!(
            recovered,
            [
                "plugins/example/skills/tool/SKILL.md".to_owned(),
                "plugins/example/skills/tool/references/guide.md".to_owned(),
                "plugins/example/skills/tool/templates/config.json".to_owned(),
                "plugins/example/skills/tool/templates/settings.toml".to_owned(),
                "plugins/example/skills/tool/templates/settings.yaml".to_owned(),
                "plugins/example/skills/tool/templates/settings.yml".to_owned(),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
        );
        assert_eq!(files, recovered.iter().cloned().collect::<Vec<_>>());
    }

    #[cfg(unix)]
    #[test]
    fn kimi_pruned_skill_resources_ignore_missing_or_symlink_nested_skill_markers() {
        use std::os::unix::fs::symlink;

        let root = scratch("kimi-pruned-skill-resource-marker");
        git(&root, &["init", "-q"]);
        write(&root, "skills/node_modules/SKILL.md", "root Skill\n");
        write(&root, "skills/node_modules/pkg/SKILL.md", "nested Skill\n");
        write(
            &root,
            "skills/node_modules/pkg/references/guide.md",
            "resource\n",
        );
        git(&root, &["add", "."]);

        let mut files = Vec::new();
        must(
            add_kimi_component_resource_files(
                &root,
                &["skills/node_modules".to_owned()],
                KimiResourceKind::PrunedSkill,
                &[],
                &[],
                &mut files,
            ),
            "exclude resources owned by a nested Skill",
        );
        assert!(!files
            .iter()
            .any(|file| { file == "skills/node_modules/pkg/references/guide.md" }));

        must(
            fs::remove_file(root.join("skills/node_modules/pkg/SKILL.md")),
            "remove nested Skill marker",
        );
        files.clear();
        must(
            add_kimi_component_resource_files(
                &root,
                &["skills/node_modules".to_owned()],
                KimiResourceKind::PrunedSkill,
                &[],
                &[],
                &mut files,
            ),
            "recover resources after indexed Skill marker deletion",
        );
        assert!(files
            .iter()
            .any(|file| file == "skills/node_modules/pkg/references/guide.md"));

        write(&root, "outside/SKILL.md", "outside Skill\n");
        must(
            symlink(
                root.join("outside/SKILL.md"),
                root.join("skills/node_modules/pkg/SKILL.md"),
            ),
            "replace nested Skill marker with symlink",
        );
        files.clear();
        must(
            add_kimi_component_resource_files(
                &root,
                &["skills/node_modules".to_owned()],
                KimiResourceKind::PrunedSkill,
                &[],
                &[],
                &mut files,
            ),
            "recover resources without following a nested Skill marker symlink",
        );
        assert!(files
            .iter()
            .any(|file| file == "skills/node_modules/pkg/references/guide.md"));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one fixture contrasts Kimi Skill pruning with recursive commands"
    )]
    fn kimi_skill_resources_prune_hidden_trees_but_commands_recurse_them() {
        let root = scratch("kimi-resource-walk-kinds");
        let skill_root = "node_modules/pkg/skills/.root";
        let command_root = "node_modules/pkg/commands";
        for (path, contents) in [
            ("node_modules/pkg/skills/.root/SKILL.md", "skill\n"),
            (
                "node_modules/pkg/skills/.root/references/root.md",
                "root resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/.hidden/SKILL.md",
                "direct hidden candidate\n",
            ),
            (
                "node_modules/pkg/skills/.root/.hidden/guide.md",
                "pruned hidden resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/.hidden/nested/guide.md",
                "pruned hidden descendant\n",
            ),
            (
                "node_modules/pkg/skills/.root/node_modules/SKILL.md",
                "direct package candidate\n",
            ),
            (
                "node_modules/pkg/skills/.root/node_modules/guide.md",
                "pruned package resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/node_modules/pkg/guide.md",
                "pruned package descendant\n",
            ),
            (
                "node_modules/pkg/skills/.root/normal/guide.md",
                "ordinary nested resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/normal/node_modules/SKILL.md",
                "direct nested candidate\n",
            ),
            (
                "node_modules/pkg/skills/.root/normal/node_modules/guide.md",
                "pruned nested package resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/target/guide.md",
                "generic output directory resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/blocked[1]/guide.md",
                "blocked by exact root\n",
            ),
            (
                "node_modules/pkg/skills/.root/blocked1/guide.md",
                "glob-like sibling remains\n",
            ),
            ("node_modules/pkg/commands/root.md", "command root\n"),
            (
                "node_modules/pkg/commands/.hidden/review.md",
                "hidden command resource\n",
            ),
            (
                "node_modules/pkg/commands/.hidden/node_modules/inside.md",
                "nested hidden command resource\n",
            ),
            (
                "node_modules/pkg/commands/node_modules/tool.md",
                "package command resource\n",
            ),
        ] {
            write(&root, path, contents);
        }
        let skip_roots = ["node_modules/pkg/skills/.root/blocked[1]".to_owned()];
        let mut files = Vec::new();

        let skill_resources = must(
            add_kimi_component_resource_files(
                &root,
                &[skill_root.to_owned()],
                KimiResourceKind::Skill,
                &[],
                &skip_roots,
                &mut files,
            ),
            "recover scoped Kimi Skill resources",
        );
        assert_eq!(
            skill_resources,
            [
                "node_modules/pkg/skills/.root/SKILL.md".to_owned(),
                "node_modules/pkg/skills/.root/.hidden/SKILL.md".to_owned(),
                "node_modules/pkg/skills/.root/node_modules/SKILL.md".to_owned(),
                "node_modules/pkg/skills/.root/normal/guide.md".to_owned(),
                "node_modules/pkg/skills/.root/normal/node_modules/SKILL.md".to_owned(),
                "node_modules/pkg/skills/.root/references/root.md".to_owned(),
                "node_modules/pkg/skills/.root/target/guide.md".to_owned(),
                "node_modules/pkg/skills/.root/blocked1/guide.md".to_owned(),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
        );

        let command_resources = must(
            add_kimi_component_resource_files(
                &root,
                &[command_root.to_owned()],
                KimiResourceKind::Command,
                &[],
                &[],
                &mut files,
            ),
            "recover recursive Kimi command resources",
        );
        assert_eq!(
            command_resources,
            [
                "node_modules/pkg/commands/.hidden/node_modules/inside.md".to_owned(),
                "node_modules/pkg/commands/.hidden/review.md".to_owned(),
                "node_modules/pkg/commands/node_modules/tool.md".to_owned(),
                "node_modules/pkg/commands/root.md".to_owned(),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "tracked-path fixture verifies the same Skill and command traversal rules"
    )]
    fn kimi_tracked_resources_match_skill_and_command_walk_kinds() {
        let root = scratch("kimi-resource-walk-kinds-index");
        let skill_root = "node_modules/pkg/skills/.root";
        let command_root = "node_modules/pkg/commands";
        git(&root, &["init", "-q"]);
        for (path, contents) in [
            ("node_modules/pkg/skills/.root/SKILL.md", "skill\n"),
            (
                "node_modules/pkg/skills/.root/references/guide.md",
                "root resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/.hidden/SKILL.md",
                "direct hidden candidate\n",
            ),
            (
                "node_modules/pkg/skills/.root/.hidden/guide.md",
                "pruned hidden resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/.hidden/nested/guide.md",
                "pruned hidden descendant\n",
            ),
            (
                "node_modules/pkg/skills/.root/node_modules/SKILL.md",
                "direct package candidate\n",
            ),
            (
                "node_modules/pkg/skills/.root/node_modules/guide.md",
                "pruned package resource\n",
            ),
            (
                "node_modules/pkg/skills/.root/normal/node_modules/SKILL.md",
                "direct nested candidate\n",
            ),
            (
                "node_modules/pkg/skills/.root/normal/node_modules/guide.md",
                "pruned nested package resource\n",
            ),
            ("node_modules/pkg/commands/root.md", "command root\n"),
            (
                "node_modules/pkg/commands/.hidden/review.md",
                "hidden command resource\n",
            ),
            (
                "node_modules/pkg/commands/.hidden/node_modules/inside.md",
                "nested hidden command resource\n",
            ),
            (
                "node_modules/pkg/commands/node_modules/tool.md",
                "package command resource\n",
            ),
        ] {
            write(&root, path, contents);
        }
        git(&root, &["add", "."]);
        write(
            &root,
            "node_modules/pkg/skills/.root/untracked.md",
            "untracked resource\n",
        );
        let mut files = Vec::new();

        let skill_resources = must(
            add_kimi_component_resource_files(
                &root,
                &[skill_root.to_owned()],
                KimiResourceKind::Skill,
                &[],
                &[],
                &mut files,
            ),
            "recover tracked Kimi Skill resources",
        );
        assert_eq!(
            skill_resources,
            [
                "node_modules/pkg/skills/.root/SKILL.md".to_owned(),
                "node_modules/pkg/skills/.root/.hidden/SKILL.md".to_owned(),
                "node_modules/pkg/skills/.root/node_modules/SKILL.md".to_owned(),
                "node_modules/pkg/skills/.root/normal/node_modules/SKILL.md".to_owned(),
                "node_modules/pkg/skills/.root/references/guide.md".to_owned(),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
        );

        let command_resources = must(
            add_kimi_component_resource_files(
                &root,
                &[command_root.to_owned()],
                KimiResourceKind::Command,
                &[],
                &[],
                &mut files,
            ),
            "recover tracked Kimi command resources",
        );
        assert_eq!(
            command_resources,
            [
                "node_modules/pkg/commands/.hidden/node_modules/inside.md".to_owned(),
                "node_modules/pkg/commands/.hidden/review.md".to_owned(),
                "node_modules/pkg/commands/node_modules/tool.md".to_owned(),
                "node_modules/pkg/commands/root.md".to_owned(),
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn repository_path_existence_checks_index_and_excludes() {
        let root = scratch("repository-path-exists");
        write(&root, "node_modules/pkg/tracked.md", "tracked\n");
        write(&root, "node_modules/pkg/untracked.md", "untracked\n");
        let excludes = vec!["node_modules/pkg/tracked.md".to_owned()];

        git(&root, &["init", "-q"]);
        git(&root, &["add", "node_modules/pkg/tracked.md"]);
        assert!(must(
            repository_path_exists(&root, "node_modules/pkg/tracked.md", &[]),
            "check tracked target"
        ));
        assert!(must(
            repository_path_is_file(&root, "node_modules/pkg/tracked.md", &[]),
            "check tracked file root"
        ));
        assert!(!must(
            repository_path_exists(&root, "node_modules/pkg/untracked.md", &[]),
            "check untracked target"
        ));
        assert!(!must(
            repository_path_is_file(&root, "node_modules/pkg/untracked.md", &[]),
            "check untracked file root"
        ));
        assert!(!must(
            repository_path_exists(&root, "node_modules/pkg/tracked.md", &excludes),
            "check excluded target"
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            must(
                symlink(
                    root.join("node_modules/pkg/tracked.md"),
                    root.join("node_modules/pkg/linked.md"),
                ),
                "create symlink target",
            );
            assert!(!must(
                repository_path_exists(&root, "node_modules/pkg/linked.md", &[]),
                "reject symlink target"
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn kimi_node_modules_skill_walk_skips_symlink_directories_and_files() {
        use std::os::unix::fs::symlink;

        let root = scratch("kimi-node-modules-symlink");
        write(&root, "outside/SKILL.md", "external skill\n");
        let skills = root.join("skills");
        must(
            fs::create_dir_all(skills.join("linked")),
            "create Skills root",
        );
        must(
            symlink(root.join("outside"), skills.join("linked/node_modules")),
            "link node_modules directory",
        );
        must(
            fs::create_dir_all(skills.join("node_modules")),
            "create real node_modules directory",
        );
        must(
            symlink(
                root.join("outside/SKILL.md"),
                skills.join("node_modules/SKILL.md"),
            ),
            "link skill file",
        );

        let mut files = Vec::new();
        must(
            add_kimi_node_modules_skill_files(&root, &["skills".to_owned()], &[], 8, &mut files),
            "skip symlinked Kimi files",
        );
        assert!(files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn kimi_declared_node_modules_root_skips_symlinked_skills_and_directories() {
        use std::os::unix::fs::symlink;

        let root = scratch("kimi-node-modules-declared-symlink");
        write(&root, "outside/SKILL.md", "external skill\n");
        let declared_root = root.join("node_modules/pkg/skills");
        must(
            fs::create_dir_all(&declared_root),
            "create declared Skills root",
        );
        must(
            symlink(
                root.join("outside/SKILL.md"),
                declared_root.join("SKILL.md"),
            ),
            "link root Skill file",
        );
        must(
            symlink(root.join("outside"), declared_root.join("linked")),
            "link child Skill directory",
        );

        let mut files = Vec::new();
        must(
            add_kimi_node_modules_skill_files(
                &root,
                &["node_modules/pkg/skills".to_owned()],
                &[],
                8,
                &mut files,
            ),
            "skip symlinks under declared node_modules root",
        );
        assert!(files.is_empty());
    }
}

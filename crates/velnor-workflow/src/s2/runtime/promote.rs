//! Atomic D19 pin promotion: one command producing the pin+metadata+full-tree commit.
//!
//! `velnor-workflow promote --rev <pin>` advances a repository's generator pin
//! and regenerates its entire tree with the stamped generator in a single
//! commit. It replaces the manual "bump `[generator] revision` and regenerate"
//! flow, which let a tree rendered with generator X be stamped with pin Y (a
//! fleet sync rendered with an in-flight generator while stamping a mainline
//! pin, and merged with red validators).
//!
//! The command binds the render to the stamp before writing anything: the
//! running binary's own stamped source closure must equal the closure of the
//! pin's tree under the binary's own build identity (render with X ⇒ stamp
//! X). A binary built from any other source fails closed without touching the
//! tree.
//!
//! After rendering, the command proves determinism (a fresh re-render is
//! byte-identical), proves write integrity (disk matches the render,
//! including the regenerated ownership metadata), stages exactly the paths it
//! wrote or deleted, and creates one signed-off commit. Any failure after the first
//! write restores the recorded preimages, so the tree is either promoted or
//! untouched — never half-rendered.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::closure::{closure_of_tree, is_full_closure};
use super::super::policy::GENERATION_CONFIG;
use super::super::provider::ProviderSet;
use super::super::{
    capture_file_preimage, is_full_revision, ownership_state_content, parse_ownership_state,
    render_tree, stale_owned_files, write_generated_with_options, GeneratorError,
    OwnershipStateFile, OWNERSHIP_STATE, SOURCE_CLOSURE,
};

const SOURCE_FEATURES: &str = env!("VELNOR_WORKFLOW_FEATURES");
const SOURCE_PROFILE: &str = env!("VELNOR_WORKFLOW_PROFILE");
static NEXT_STAGED_CONFIG: AtomicUsize = AtomicUsize::new(0);

/// Promotion intent, separated from the CLI surface for testing.
#[derive(Clone)]
pub(crate) struct PromoteOptions {
    /// The generator commit to stamp into `[generator] revision`: a full
    /// SHA, or `HEAD` for the generator checkout's own head.
    pub(crate) rev: String,
    /// The repository root whose tree is promoted.
    pub(crate) repo: PathBuf,
    /// The repository holding generator history for the render≡stamp
    /// binding. Defaults to `repo` (owner self-promotion); fleet promotion
    /// of a consumer tree points it at a product checkout containing the pin.
    pub(crate) generator_repo: Option<PathBuf>,
    /// Default branch for branch gates. Defaults to the repository's own.
    pub(crate) default_branch: Option<String>,
    /// Provider override. When absent, the schema-2 config or all declared
    /// providers determine the rendered tree, exactly like normal generation.
    pub(crate) providers: Option<ProviderSet>,
    /// Commit message override. Defaults to the `bump D19 pin` subject the
    /// repository history uses.
    pub(crate) message: Option<String>,
    /// Verify binding, render, and report without writing or committing.
    pub(crate) dry_run: bool,
}

/// What one promotion did, for the CLI report and tests.
pub(crate) struct PromoteReport {
    pub(crate) old_pin: String,
    pub(crate) new_pin: String,
    /// The source closure the binding proved (render ≡ stamp).
    pub(crate) closure: String,
    /// Every promoted path, relative to the repository root.
    pub(crate) changed: Vec<PathBuf>,
    /// The promotion commit, or `None` on dry runs and no-op promotions.
    pub(crate) commit: Option<String>,
    /// True when the tree already matched the pin render and no commit was
    /// needed.
    pub(crate) noop: bool,
}

/// Advance `options.repo` to `options.rev` and commit the pin, the ownership
/// metadata, and the regenerated tree atomically.
///
/// # Errors
/// When the pin is malformed, the binding fails (this binary renders with a
/// different source closure than the pin names), the tree is not promotion
/// clean, rendering or verification fails, or the commit cannot be created.
/// A failure after the first write restores the tree to its prior bytes.
pub(crate) fn run_promote(options: &PromoteOptions) -> Result<PromoteReport, GeneratorError> {
    run_promote_with_config_writer(options, atomic_write_generation_config)
}

fn run_promote_with_config_writer(
    options: &PromoteOptions,
    write_config: impl FnOnce(&Path, &Path, &[u8]) -> Result<(), GeneratorError>,
) -> Result<PromoteReport, GeneratorError> {
    let repo = canonicalize(&options.repo, "promote --repo")?;
    require_repository_root(&repo)?;
    let generator_repo = match &options.generator_repo {
        Some(path) => canonicalize(path, "promote --generator-repo")?,
        None => repo.clone(),
    };
    // `HEAD` names the generator checkout's own head, so the policy
    // remediation command runs as written; everything else must already be a
    // full SHA. The resolved SHA is what gets stamped and bound.
    let rev = if options.rev == "HEAD" {
        git(&generator_repo, &["rev-parse", "HEAD"])?
    } else {
        options.rev.clone()
    };
    if !is_full_revision(&rev) {
        return Err(GeneratorError::usage(format!(
            "promote --rev must be a full 40-character commit SHA or HEAD, got {:?}",
            options.rev
        )));
    }
    let options = PromoteOptions {
        rev,
        ..options.clone()
    };
    let options = &options;
    // The binding fires before anything is read for mutation, let alone
    // written: only the generator at the pin (or a closure-identical twin)
    // may stamp it.
    let closure = verify_render_stamp_binding(&generator_repo, &options.rev)?;
    require_promotion_clean(&repo)?;
    let pin_path = checked_generation_config_path(&repo)?;
    let pin_content = std::fs::read_to_string(&pin_path)
        .map_err(|error| GeneratorError::io("read generation config", &pin_path, &error))?;
    let (stamped, old_pin) = stamp_pin(&pin_content, &options.rev)?;
    let default_branch = match &options.default_branch {
        Some(branch) => branch.clone(),
        None => resolve_default_branch(&repo)?,
    };
    let mut snapshot = Snapshot::capture(&repo, [PathBuf::from(GENERATION_CONFIG)])?;
    let outcome = (|| {
        checked_generation_config_path(&repo)?;
        write_config(&repo, &pin_path, stamped.as_bytes())?;
        promote_rendered_tree(
            options,
            &repo,
            &default_branch,
            &mut snapshot,
            &old_pin,
            &closure,
        )
    })();
    match outcome {
        Ok(report) => Ok(report),
        Err(error) => {
            if let Err(restore) = snapshot.restore(&repo) {
                return Err(GeneratorError::usage(format!(
                    "{error}; then restoring the pre-promotion tree failed: {restore}"
                )));
            }
            // The index was clean on entry, so anything staged is ours;
            // unstage it so the tree is untouched, not half-staged.
            let _ = git(&repo, &["reset", "--quiet"]);
            Err(error)
        }
    }
}

/// Resolve the checked-in generation config without following any symlink in
/// its path. The repository root is canonical, so checking every component
/// below it keeps promotion inside the worktree.
fn checked_generation_config_path(repo: &Path) -> Result<PathBuf, GeneratorError> {
    let relative = Path::new(GENERATION_CONFIG);
    let component_count = relative.components().count();
    if component_count == 0 {
        return Err(GeneratorError::usage(
            "the generation config path is empty; refusing to promote".to_owned(),
        ));
    }

    let mut path = repo.to_path_buf();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(part) = component else {
            return Err(GeneratorError::usage(format!(
                "the generation config path is not repository-relative: {GENERATION_CONFIG}"
            )));
        };
        path.push(part);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect generation config path", &path, &error))?;
        if metadata.file_type().is_symlink() {
            return Err(GeneratorError::usage(format!(
                "refusing to promote through a symlinked generation config path: {}",
                path.display()
            )));
        }
        if index + 1 < component_count && !metadata.is_dir() {
            return Err(GeneratorError::usage(format!(
                "generation config ancestor is not a directory: {}",
                path.display()
            )));
        }
        if index + 1 == component_count && !metadata.is_file() {
            return Err(GeneratorError::usage(format!(
                "generation config is not a regular file: {}",
                path.display()
            )));
        }
    }
    Ok(path)
}

/// Write the pin through a same-directory temporary file, then atomically
/// replace the checked config. A failed stage never truncates the live config.
fn atomic_write_generation_config(
    repo: &Path,
    path: &Path,
    content: &[u8],
) -> Result<(), GeneratorError> {
    if checked_generation_config_path(repo)?.as_path() != path {
        return Err(GeneratorError::usage(
            "generation config path changed during promotion; refusing to write".to_owned(),
        ));
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| GeneratorError::io("inspect generation config", path, &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(GeneratorError::usage(format!(
            "generation config is not a regular file: {}",
            path.display()
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        GeneratorError::usage("generation config has no parent directory".to_owned())
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        GeneratorError::usage("generation config has no file name".to_owned())
    })?;

    let (staged_path, mut staged) = loop {
        let serial = NEXT_STAGED_CONFIG.fetch_add(1, Ordering::Relaxed);
        let mut staged_name = std::ffi::OsString::from(".");
        staged_name.push(file_name);
        staged_name.push(format!(".promote-{}-{serial}.tmp", std::process::id()));
        let staged_path = parent.join(staged_name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged_path)
        {
            Ok(file) => break (staged_path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(GeneratorError::io(
                    "create staged generation config",
                    &staged_path,
                    &error,
                ));
            }
        }
    };

    if let Err(error) = staged.write_all(content) {
        drop(staged);
        let _ = std::fs::remove_file(&staged_path);
        return Err(GeneratorError::io(
            "write staged generation config",
            &staged_path,
            &error,
        ));
    }
    if let Err(error) = staged.set_permissions(metadata.permissions()) {
        drop(staged);
        let _ = std::fs::remove_file(&staged_path);
        return Err(GeneratorError::io(
            "preserve generation config permissions",
            &staged_path,
            &error,
        ));
    }
    if let Err(error) = staged.sync_all() {
        drop(staged);
        let _ = std::fs::remove_file(&staged_path);
        return Err(GeneratorError::io(
            "sync staged generation config",
            &staged_path,
            &error,
        ));
    }
    drop(staged);

    match checked_generation_config_path(repo) {
        Ok(current) if current.as_path() == path => {}
        Ok(_) => {
            let _ = std::fs::remove_file(&staged_path);
            return Err(GeneratorError::usage(
                "generation config path changed during promotion; refusing to replace it"
                    .to_owned(),
            ));
        }
        Err(error) => {
            let _ = std::fs::remove_file(&staged_path);
            return Err(error);
        }
    }
    if let Err(error) = std::fs::rename(&staged_path, path) {
        let _ = std::fs::remove_file(&staged_path);
        return Err(GeneratorError::io(
            "replace generation config",
            path,
            &error,
        ));
    }
    Ok(())
}

/// Render, verify, stage, and commit after the pin file is stamped. Every
/// error path restores `snapshot` in the caller.
fn promote_rendered_tree(
    options: &PromoteOptions,
    repo: &Path,
    default_branch: &str,
    snapshot: &mut Snapshot,
    old_pin: &str,
    closure: &str,
) -> Result<PromoteReport, GeneratorError> {
    let rendered = render_tree(repo, options.providers.clone(), default_branch)?;
    let previous_state_preimage =
        capture_file_preimage(&repo.join(OWNERSHIP_STATE), Path::new(OWNERSHIP_STATE))?;
    let previous_state = parse_ownership_state(repo, &previous_state_preimage)?;
    // Only stale paths whose schema-2 ledger digest matches their live bytes
    // enter the promotion transaction. The generator checks them again before
    // deleting; retaining this reviewed preimage lets promotion commit or
    // restore the same paths if a later step fails.
    let stale_files = match &previous_state {
        OwnershipStateFile::Present(state) => {
            stale_owned_files(repo, &rendered.files, Some(&state.outputs))?
        }
        OwnershipStateFile::Absent | OwnershipStateFile::ForeignSchema { .. } => Vec::new(),
    };
    let stale_paths = stale_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    snapshot.extend(
        repo,
        rendered
            .files
            .keys()
            .cloned()
            .chain([PathBuf::from(OWNERSHIP_STATE)]),
    )?;
    snapshot.extend_reviewed_stale(stale_files)?;
    // Promotion overwrites generator-owned files by design: every genuine pin
    // advance changes existing rendered bytes, so the conflicts guard would
    // refuse every real promotion. Force is the intended semantic here — the
    // promotion already requires a tracked-clean tree, snapshots every
    // preimage for restore, and proves determinism plus write integrity after
    // the write. Force bypasses only the conflicts guard: ownership proof
    // still rejects manually modified files, so unowned workflows are never
    // deleted.
    write_generated_with_options(
        repo,
        &rendered.files,
        &rendered.inputs,
        false,
        false,
        true,
    )?;
    verify_promoted_tree(
        repo,
        &rendered.files,
        options.providers.clone(),
        default_branch,
    )?;
    let expected_state = ownership_state_content(&rendered.files, &rendered.inputs);
    let state_path = repo.join(OWNERSHIP_STATE);
    let state_disk = std::fs::read_to_string(&state_path)
        .map_err(|error| GeneratorError::io("read ownership state", &state_path, &error))?;
    if state_disk != expected_state {
        return Err(GeneratorError::usage(
            "the regenerated ownership metadata does not match the render; refusing to promote"
                .to_owned(),
        ));
    }
    let owned: BTreeSet<PathBuf> = rendered
        .files
        .keys()
        .cloned()
        .chain(stale_paths)
        .chain([
            PathBuf::from(GENERATION_CONFIG),
            PathBuf::from(OWNERSHIP_STATE),
        ])
        .collect();
    let changed = promotion_changes(repo, &owned)?;
    if changed.is_empty() {
        return Ok(PromoteReport {
            old_pin: old_pin.to_owned(),
            new_pin: options.rev.clone(),
            closure: closure.to_owned(),
            changed: Vec::new(),
            commit: None,
            noop: true,
        });
    }
    if options.dry_run {
        let report = PromoteReport {
            old_pin: old_pin.to_owned(),
            new_pin: options.rev.clone(),
            closure: closure.to_owned(),
            changed,
            commit: None,
            noop: false,
        };
        snapshot.restore(repo)?;
        return Ok(report);
    }
    stage(repo, &changed)?;
    let message = promotion_message(options, old_pin);
    git(repo, &["commit", "-s", "-m", &message])?;
    let commit = git(repo, &["rev-parse", "HEAD"])?;
    Ok(PromoteReport {
        old_pin: old_pin.to_owned(),
        new_pin: options.rev.clone(),
        closure: closure.to_owned(),
        changed,
        commit: Some(commit),
        noop: false,
    })
}

/// Prove the running binary renders with exactly the source it is about to
/// stamp: its own stamped closure must equal the pin tree's closure under
/// the binary's own build identity. Returns the proven closure.
fn verify_render_stamp_binding(generator_repo: &Path, rev: &str) -> Result<String, GeneratorError> {
    if !is_full_closure(SOURCE_CLOSURE) {
        return Err(GeneratorError::usage(
            "promotion requires a binary that proves its own source: this velnor-workflow stamps no closure (built without git); rebuild it from a git checkout"
                .to_owned(),
        ));
    }
    let pin_closure = closure_of_tree(
        generator_repo,
        rev,
        SOURCE_FEATURES,
        SOURCE_PROFILE,
    )
    .map_err(|error| {
        GeneratorError::usage(format!(
            "{error}; promote needs the stamped pin's generator history (pass --generator-repo at a checkout containing it)"
        ))
    })?;
    if pin_closure != SOURCE_CLOSURE {
        return Err(GeneratorError::usage(format!(
            "refusing to stamp {rev}: this binary renders with source closure {SOURCE_CLOSURE} but the pin names {pin_closure}; promotion renders with exactly the generator it stamps"
        )));
    }
    Ok(pin_closure)
}

/// Resolve the default branch the same way S2 generation does. Promotion
/// stays inside the runtime module, so the private schema-2 validator remains
/// available without exporting a legacy helper from the generator root.
fn resolve_default_branch(repository: &Path) -> Result<String, GeneratorError> {
    let symbolic_ref = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output();
    if let Ok(output) = symbolic_ref
        && output.status.success()
    {
        let reference = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if let Some(branch) = reference.strip_prefix("origin/") {
            return super::super::validate_default_branch(branch).map(str::to_owned);
        }
    }

    let remote_head = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["ls-remote", "--symref", "origin", "HEAD"])
        .output()
        .map_err(|error| {
            GeneratorError::usage(format!(
                "resolve repository default branch from Git metadata: {error}; pass --default-branch explicitly"
            ))
        })?;
    if remote_head.status.success() {
        let output = String::from_utf8_lossy(&remote_head.stdout);
        if let Some(branch) = super::super::parse_remote_head(&output) {
            return Ok(branch.to_owned());
        }
    }
    Err(GeneratorError::usage(
        "repository default branch is unavailable from Git metadata; pass --default-branch explicitly",
    ))
}

/// Prove the write is faithful: a fresh re-render of the stamped tree must be
/// byte-identical to the render just written (determinism), and every disk
/// byte must match it (write integrity).
fn verify_promoted_tree(
    repo: &Path,
    rendered: &BTreeMap<PathBuf, String>,
    providers: Option<ProviderSet>,
    default_branch: &str,
) -> Result<(), GeneratorError> {
    let again = render_tree(repo, providers, default_branch)?;
    if again.files != *rendered {
        let divergent: Vec<String> = rendered
            .keys()
            .chain(again.files.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|path| rendered.get(*path) != again.files.get(*path))
            .map(|path| path.display().to_string())
            .collect();
        return Err(GeneratorError::usage(format!(
            "the stamped tree does not render deterministically; refusing to promote: {}",
            divergent.join(", ")
        )));
    }
    for (relative, wanted) in rendered {
        let path = repo.join(relative);
        let disk = std::fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read promoted file", &path, &error))?;
        if disk != *wanted {
            return Err(GeneratorError::usage(format!(
                "the written tree differs from the render at {}; refusing to promote",
                relative.display()
            )));
        }
    }
    Ok(())
}

/// Replace the `[generator] revision` value, preserving every other byte
/// (comments, spacing, trailing newline). Returns the stamped content and
/// the previous pin.
pub(crate) fn stamp_pin(content: &str, new_rev: &str) -> Result<(String, String), GeneratorError> {
    let mut section = String::new();
    let mut stamped = false;
    let mut old: Option<String> = None;
    let mut lines: Vec<String> = content.split('\n').map(str::to_owned).collect();
    for line in &mut lines {
        let trimmed = line.trim();
        if let Some(header) = trimmed.strip_prefix('[') {
            header
                .split(']')
                .next()
                .unwrap_or_default()
                .trim()
                .clone_into(&mut section);
        }
        if section != "generator" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "revision" {
            continue;
        }
        let pinned = value
            .trim_start()
            .strip_prefix('"')
            .and_then(|rest| rest.split_once('"').map(|(sha, _)| sha))
            .filter(|sha| is_full_revision(sha));
        let Some(pinned) = pinned else {
            return Err(GeneratorError::usage(format!(
                "[generator] revision is not a full 40-character commit SHA: {value:?}"
            )));
        };
        if stamped {
            return Err(GeneratorError::usage(
                "[generator] declares revision twice; refusing an ambiguous stamp".to_owned(),
            ));
        }
        old = Some(pinned.to_owned());
        *line = line.replacen(pinned, new_rev, 1);
        stamped = true;
    }
    match old {
        Some(old) => Ok((lines.join("\n"), old)),
        None => Err(GeneratorError::usage(format!(
            "no [generator] revision to stamp in {GENERATION_CONFIG}"
        ))),
    }
}

/// The default promotion subject follows the repository's `bump D19 pin`
/// history; a same-pin regeneration names itself so the message never claims
/// a pin advance it did not make.
fn promotion_message(options: &PromoteOptions, old_pin: &str) -> String {
    if let Some(message) = &options.message {
        return message.clone();
    }
    let short = &options.rev[..8];
    if old_pin == options.rev {
        format!("chore(ci): regenerate tree with velnor-workflow {short}")
    } else {
        format!("chore(ci): bump D19 pin to {short}")
    }
}

/// The promoted paths: tracked changes must all be inside `owned` (anything
/// else means the tree moved under the promotion), and only owned paths are
/// ever staged, so pre-existing untracked files are never swept into the
/// commit.
fn promotion_changes(
    repo: &Path,
    owned: &BTreeSet<PathBuf>,
) -> Result<Vec<PathBuf>, GeneratorError> {
    let mut changed = Vec::new();
    for entry in git_status(repo)?.split('\0') {
        if entry.len() < 4 {
            continue;
        }
        let (status, path) = entry.split_at(2);
        let path = path.strip_prefix(' ').unwrap_or(path);
        if status == "??" {
            if owned.contains(Path::new(path)) {
                changed.push(PathBuf::from(path));
            }
            continue;
        }
        let relative = PathBuf::from(path);
        if !owned.contains(&relative) {
            return Err(GeneratorError::usage(format!(
                "the tree changed outside the promoted surface during promotion ({}); refusing to commit",
                relative.display()
            )));
        }
        changed.push(relative);
    }
    changed.sort();
    changed.dedup();
    Ok(changed)
}

/// Promotion needs a tracked-clean tree: staged or modified files would
/// either be swept into the atomic commit or hide under it. Untracked files
/// are tolerated but never staged (see [`promotion_changes`]).
fn require_promotion_clean(repo: &Path) -> Result<(), GeneratorError> {
    let mut dirty = Vec::new();
    for entry in git_status(repo)?.split('\0') {
        if entry.len() < 4 || entry.starts_with("??") {
            continue;
        }
        dirty.push(entry.to_owned());
    }
    if dirty.is_empty() {
        Ok(())
    } else {
        Err(GeneratorError::usage(format!(
            "promotion needs a clean tree; commit or stash first: {}",
            dirty.join(", ")
        )))
    }
}

/// Generated paths are repository-root-relative and the commit must observe
/// the whole tree, so promotion runs at the root, never in a subdirectory.
fn require_repository_root(repo: &Path) -> Result<(), GeneratorError> {
    let top_level = git(repo, &["rev-parse", "--show-toplevel"])?;
    let top_level = canonicalize(Path::new(&top_level), "repository top level")?;
    if top_level == *repo {
        Ok(())
    } else {
        Err(GeneratorError::usage(format!(
            "promote runs at the repository root (root is {}, given {})",
            top_level.display(),
            repo.display()
        )))
    }
}

fn stage(repo: &Path, changed: &[PathBuf]) -> Result<(), GeneratorError> {
    let mut arguments = vec!["add".to_owned(), "-A".to_owned(), "--".to_owned()];
    for path in changed {
        arguments.push(path.display().to_string());
    }
    let refs: Vec<&str> = arguments.iter().map(String::as_str).collect();
    git(repo, &refs)?;
    if git(repo, &["diff", "--cached", "--quiet"]).is_ok() {
        return Err(GeneratorError::usage(
            "nothing staged for the promotion commit; the render left no tracked change".to_owned(),
        ));
    }
    Ok(())
}

/// NUL-separated porcelain with every untracked file listed individually:
/// without `--untracked-files=all` git collapses new directories (`?? .github/`),
/// which would hide promoted files from the change set and the stage list.
fn git_status(repo: &Path) -> Result<String, GeneratorError> {
    git(
        repo,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )
}

fn canonicalize(path: &Path, what: &str) -> Result<PathBuf, GeneratorError> {
    path.canonicalize()
        .map_err(|error| GeneratorError::io(&format!("canonicalize {what}"), path, &error))
}

fn git(repo: &Path, args: &[&str]) -> Result<String, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|error| GeneratorError::usage(format!("run git {}: {error}", args.join(" "))))?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Recorded pre-promotion bytes for every path the promotion may write or
/// delete, so a failure restores the tree instead of leaving it half-rendered.
struct Snapshot {
    entries: Vec<(PathBuf, Option<Vec<u8>>)>,
    seen: BTreeSet<PathBuf>,
}

impl Snapshot {
    fn capture(
        repo: &Path,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, GeneratorError> {
        let mut snapshot = Self {
            entries: Vec::new(),
            seen: BTreeSet::new(),
        };
        snapshot.extend(repo, paths)?;
        Ok(snapshot)
    }

    fn extend(
        &mut self,
        repo: &Path,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<(), GeneratorError> {
        for relative in paths {
            if !self.seen.insert(relative.clone()) {
                continue;
            }
            let path = repo.join(&relative);
            let preimage = match std::fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(GeneratorError::io("read preimage", &path, &error)),
            };
            self.entries.push((relative, preimage));
        }
        Ok(())
    }

    /// Add stale outputs from the schema-2 deletion preflight. That preflight
    /// proved each recorded digest against the exact captured file bytes.
    fn extend_reviewed_stale(
        &mut self,
        files: impl IntoIterator<Item = super::super::PlannedFile>,
    ) -> Result<(), GeneratorError> {
        for file in files {
            if !self.seen.insert(file.path.clone()) {
                continue;
            }
            let Some(bytes) = file.preimage.bytes() else {
                return Err(GeneratorError::usage(format!(
                    "stale generated file has no preimage: {}",
                    file.path.display()
                )));
            };
            self.entries.push((file.path, Some(bytes.to_vec())));
        }
        Ok(())
    }

    /// Best-effort restore of every recorded preimage: overwritten files get
    /// their bytes back, created files are removed, and emptied directories
    /// are pruned. Every entry is attempted even when one fails.
    fn restore(&mut self, repo: &Path) -> Result<(), GeneratorError> {
        let mut failures = Vec::new();
        for (relative, preimage) in &self.entries {
            let path = repo.join(relative);
            let result = match preimage {
                Some(bytes) if relative.as_path() == Path::new(GENERATION_CONFIG) => {
                    atomic_write_generation_config(repo, &path, bytes)
                        .map_err(|error| format!("restore {}: {error}", path.display()))
                }
                Some(bytes) => std::fs::write(&path, bytes)
                    .map_err(|error| format!("restore {}: {error}", path.display())),
                None => {
                    if path.exists() {
                        std::fs::remove_file(&path)
                            .map_err(|error| format!("remove {}: {error}", path.display()))
                    } else {
                        Ok(())
                    }
                }
            };
            if let Err(failure) = result {
                failures.push(failure);
            }
            if preimage.is_none() {
                prune_empty_ancestors(repo, &path);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(GeneratorError::usage(format!(
                "restore the pre-promotion tree: {}",
                failures.join("; ")
            )))
        }
    }
}

/// Remove directories the promotion created, stopping at the first
/// non-empty one (so only promotion-created empties disappear).
fn prune_empty_ancestors(repo: &Path, path: &Path) {
    let mut current = path.parent();
    while let Some(directory) = current {
        if directory == repo {
            break;
        }
        if std::fs::remove_dir(directory).is_err() {
            break;
        }
        current = directory.parent();
    }
}

/// One-line-per-fact CLI report for a promotion.
pub(crate) fn render_report(report: &PromoteReport) -> String {
    let mut out = format!(
        "promote: {} -> {} (closure {})\nrendered surface: {} promoted path(s)\n",
        &report.old_pin[..8],
        &report.new_pin[..8],
        &report.closure[..16],
        report.changed.len(),
    );
    for path in &report.changed {
        use std::fmt::Write as _;
        let _ = writeln!(out, "  {}", path.display());
    }
    match (&report.commit, report.noop) {
        (Some(commit), _) => {
            use std::fmt::Write as _;
            let _ = writeln!(out, "commit: {commit}");
        }
        (None, true) => out.push_str("no commit: the tree already matches the pin render\n"),
        (None, false) => out.push_str("no commit: dry run\n"),
    }
    out
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "fixture setup failures should panic with their source error"
    )]
    #![expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);

    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-{name}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn copy_tree(source: &Path, destination: &Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn git_output(repo: &Path, arguments: &[&str]) -> String {
        must(git(repo, arguments), &format!("git {}", arguments.join(" ")))
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf()
    }

    fn seed_repo_with_stale_output(root: &Path) -> (PathBuf, PathBuf, String, String) {
        const STALE_OUTPUT: &str = ".github/workflows/ci-unit-rust.yml";
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures-s2/promote-trust");
        let repo = root.join("consumer");
        copy_tree(&source, &repo);

        let config_path = repo.join(GENERATION_CONFIG);
        let config_before = fs::read_to_string(&config_path).unwrap();
        let config = config_before.replace(
            "[generator]\n",
            &format!("[generator]\nrevision = \"{OLD}\"\n"),
        );
        fs::write(&config_path, &config).unwrap();

        let rendered = must(render_tree(&repo, None, "main"), "render base consumer tree");
        let stale_path = PathBuf::from(STALE_OUTPUT);
        assert!(
            !rendered.files.contains_key(&stale_path),
            "the fixture path must be stale under the current renderer"
        );
        must(
            write_generated_with_options(
                &repo,
                &rendered.files,
                &rendered.inputs,
                false,
                false,
                false,
            ),
            "write base generated tree",
        );

        let stale_bytes = format!(
            "{}name: superseded generic unit\n",
            super::super::super::GENERATED_HEADER
        );
        let stale_absolute = repo.join(&stale_path);
        fs::create_dir_all(stale_absolute.parent().unwrap()).unwrap();
        fs::write(&stale_absolute, &stale_bytes).unwrap();
        let mut previous_files = rendered.files.clone();
        previous_files.insert(stale_path, stale_bytes.clone());
        let ownership_before = ownership_state_content(&previous_files, &rendered.inputs);
        fs::write(repo.join(OWNERSHIP_STATE), &ownership_before).unwrap();

        git_output(&repo, &["init", "--quiet", "-b", "main"]);
        git_output(&repo, &["config", "user.email", "promote@test"]);
        git_output(&repo, &["config", "user.name", "promote"]);
        git_output(&repo, &["config", "commit.gpgsign", "false"]);
        git_output(&repo, &["add", "-A"]);
        git_output(&repo, &["commit", "--quiet", "--message", "consumer base"]);
        (repo, stale_absolute, stale_bytes, ownership_before)
    }

    fn promote_options(repo: &Path) -> PromoteOptions {
        PromoteOptions {
            rev: super::super::super::SOURCE_REVISION.to_owned(),
            repo: repo.to_path_buf(),
            generator_repo: Some(workspace_root()),
            default_branch: Some("main".to_owned()),
            providers: None,
            message: None,
            dry_run: false,
        }
    }

    #[test]
    fn stamp_replaces_the_generator_revision_and_nothing_else() {
        let content = format!(
            "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n# D19 pin: bump in a single pin commit.\nrevision = \"{OLD}\"  # trailing comment\n\n[workflow]\nproviders = [\"github-hosted\", \"velnor\"]\n"
        );
        let (stamped, old) = must(stamp_pin(&content, NEW), "stamp");
        assert_eq!(old, OLD);
        assert_eq!(
            stamped,
            content.replace(OLD, NEW),
            "only the revision value changes"
        );
    }

    #[test]
    fn stamp_ignores_revision_keys_outside_the_generator_section() {
        let content = format!(
            "schema = 2\n\n[workflow]\nrevision = \"{OLD}\"\n\n[generator]\nrepository = \"example/consumer\"\nrevision = \"{OLD}\"\n"
        );
        let (stamped, _) = must(stamp_pin(&content, NEW), "stamp");
        assert_eq!(
            stamped.matches(NEW).count(),
            1,
            "only the generator revision is stamped: {stamped}"
        );
        assert!(
            stamped.contains(&format!("[workflow]\nrevision = \"{OLD}\"")),
            "other sections keep their bytes: {stamped}"
        );
    }

    #[test]
    fn promote_renders_schema2_trust_units() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures-s2/promote-trust");
        let rendered = must(
            render_tree(&root, None, "main"),
            "promotion renders a schema-2 tree carrying `trust`",
        );
        assert!(
            !rendered.files.is_empty(),
            "the trust-bearing tree renders files"
        );
    }

    #[test]
    fn promotion_commits_hash_verified_stale_output_deletion() {
        let root = temporary_root("stale-output");
        let (repo, stale_path, _, _) = seed_repo_with_stale_output(&root);

        let report = must(
            run_promote(&promote_options(&repo)),
            "promote and delete the reviewed stale output",
        );
        assert!(
            report
                .changed
                .contains(&PathBuf::from(".github/workflows/ci-unit-rust.yml")),
            "the stale output is in the promoted change set: {:?}",
            report.changed
        );
        assert!(!stale_path.exists(), "the stale output is removed");
        let status = git_output(&repo, &["show", "--name-status", "--format=", "HEAD"]);
        assert!(
            status
                .lines()
                .any(|line| line == "D\t.github/workflows/ci-unit-rust.yml"),
            "the promotion commit records stale deletion: {status}"
        );
        assert!(git_output(&repo, &["status", "--porcelain"]).is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_first_pin_write_restores_partially_written_config() {
        let root = temporary_root("pin-write-rollback");
        let (repo, stale_path, stale_before, ownership_before) =
            seed_repo_with_stale_output(&root);
        let config_path = repo.join(GENERATION_CONFIG);
        let config_before = fs::read(&config_path).unwrap();

        let error = must_fail(
            run_promote_with_config_writer(&promote_options(&repo), |_, path, content| {
                fs::write(path, &content[..content.len() / 2]).map_err(|error| {
                    GeneratorError::io("inject partial generation config write", path, &error)
                })?;
                Err(GeneratorError::usage(
                    "injected first config write failure".to_owned(),
                ))
            }),
            "a partial first config write rolls promotion back",
        );
        assert!(error.contains("injected first config write failure"), "{error}");
        assert_eq!(
            fs::read(&config_path).unwrap().as_slice(),
            config_before.as_slice()
        );
        assert_eq!(
            fs::read(&stale_path).unwrap().as_slice(),
            stale_before.as_bytes()
        );
        assert_eq!(
            fs::read(repo.join(OWNERSHIP_STATE)).unwrap().as_slice(),
            ownership_before.as_bytes()
        );
        assert!(git_output(&repo, &["status", "--porcelain"]).is_empty());
        assert_eq!(git_output(&repo, &["rev-list", "--count", "HEAD"]), "1");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn generation_config_path_rejects_symlinked_ancestor_and_file() {
        use std::os::unix::fs::symlink;

        let root = temporary_root("config-symlink");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let outside_config = outside.join("velnor-workflow.toml");
        fs::write(&outside_config, "external config\n").unwrap();

        symlink(&outside, root.join(".github-gen")).unwrap();
        let ancestor_error = must_fail(
            checked_generation_config_path(&root),
            "a symlinked config ancestor is rejected",
        );
        assert!(ancestor_error.contains("symlinked generation config path"));

        fs::remove_file(root.join(".github-gen")).unwrap();
        fs::create_dir(root.join(".github-gen")).unwrap();
        symlink(
            &outside_config,
            root.join(GENERATION_CONFIG),
        )
        .unwrap();
        let file_error = must_fail(
            checked_generation_config_path(&root),
            "a symlinked config file is rejected",
        );
        assert!(file_error.contains("symlinked generation config path"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn failed_commit_restores_hash_verified_stale_output_bytes() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = temporary_root("stale-output-rollback");
        let (repo, stale_path, stale_before, ownership_before) =
            seed_repo_with_stale_output(&root);
        let config_path = repo.join(GENERATION_CONFIG);
        let config_before = fs::read(&config_path).unwrap();
        let hook_directory = repo.join(".git/promotion-hooks");
        fs::create_dir_all(&hook_directory).unwrap();
        let hook = hook_directory.join("pre-commit");
        fs::write(
            &hook,
            concat!(
                "#!/bin/sh\n",
                "[ ! -e .github/workflows/ci-unit-rust.yml ] || exit 72\n",
                "printf observed > .git/promotion-hook-observed\n",
                "exit 73\n",
            ),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let hooks_path = hook_directory.canonicalize().unwrap();
        git_output(
            &repo,
            &[
                "config",
                "core.hooksPath",
                hooks_path.to_str().unwrap(),
            ],
        );

        let error = must_fail(
            run_promote(&promote_options(&repo)),
            "a failing pre-commit hook rolls promotion back",
        );
        assert!(error.contains("git commit"), "failure is post-delete: {error}");
        assert_eq!(
            fs::read(repo.join(".git/promotion-hook-observed"))
                .unwrap()
                .as_slice(),
            &b"observed"[..]
        );
        assert_eq!(
            fs::read(&stale_path).unwrap().as_slice(),
            stale_before.as_bytes()
        );
        assert_eq!(
            fs::read(&config_path).unwrap().as_slice(),
            config_before.as_slice()
        );
        assert_eq!(
            fs::read(repo.join(OWNERSHIP_STATE)).unwrap().as_slice(),
            ownership_before.as_bytes()
        );
        assert!(git_output(&repo, &["status", "--porcelain"]).is_empty());
        assert_eq!(git_output(&repo, &["rev-list", "--count", "HEAD"]), "1");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn promote_rejects_schema1_consumers() {
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-schema1-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let config_dir = root.join(".github-gen");
        must(
            std::fs::create_dir_all(&config_dir),
            "create schema-1 rejection fixture",
        );
        must(
            std::fs::write(
                config_dir.join("velnor-workflow.toml"),
                "schema = 1\n\n[generator]\nrepository = \"example/consumer\"\n",
            ),
            "write schema-1 rejection fixture",
        );
        let error = must_fail(
            render_tree(&root, None, "main"),
            "promotion must fail closed on schema 1",
        );
        assert!(
            error.contains("schema 2 only"),
            "the schema-2 parser rejects the legacy config: {error}"
        );
        must(
            std::fs::remove_dir_all(root),
            "remove schema-1 rejection fixture",
        );
    }

    #[test]
    fn promote_rejects_runner_alias_and_unknown_provider_ids() {
        let runner_alias = must_fail(
            super::super::promote_command(&[OsString::from("--runners"), OsString::from("both")]),
            "the retired runner option must fail",
        );
        assert!(
            runner_alias.contains("unsupported CI argument: --runners"),
            "promotion accepts only the provider vocabulary: {runner_alias}"
        );

        let unknown_provider = must_fail(
            super::super::promote_command(&[
                OsString::from("--providers"),
                OsString::from("github"),
            ]),
            "legacy provider aliases must fail",
        );
        assert!(
            unknown_provider.contains("unknown provider `github`"),
            "provider parsing stays strict: {unknown_provider}"
        );
    }

    #[test]
    fn stamp_fails_closed_on_missing_malformed_or_ambiguous_pins() {
        let missing = "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n";
        assert!(
            must_fail(stamp_pin(missing, NEW), "a missing pin fails")
                .contains("no [generator] revision"),
            "a missing pin names the defect"
        );
        let malformed = "schema = 2\n\n[generator]\nrevision = \"short\"\n".to_owned();
        assert!(
            must_fail(stamp_pin(&malformed, NEW), "a malformed pin fails")
                .contains("not a full 40-character commit SHA"),
            "a malformed pin names the defect"
        );
        let ambiguous =
            format!("schema = 2\n\n[generator]\nrevision = \"{OLD}\"\nrevision = \"{OLD}\"\n");
        assert!(
            must_fail(stamp_pin(&ambiguous, NEW), "an ambiguous pin fails").contains("twice"),
            "a doubled pin refuses the ambiguous stamp"
        );
    }
}

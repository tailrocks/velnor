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
//! wrote, and creates one signed-off commit. Any failure after the first
//! write restores the recorded preimages, so the tree is either promoted or
//! untouched — never half-rendered.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::s2::dispatch::dir_is_schema2;
use super::s2::policy::GENERATION_CONFIG;
use super::s2::provider::{ProviderId, ProviderSet};
use super::s2::{
    ownership_state_content, render_tree, write_generated_with_options, OWNERSHIP_STATE,
};
use super::{
    create_generator_symlink, is_full_revision, resolve_default_branch, GeneratorError, RunnerMode,
    SOURCE_CLOSURE, SOURCE_FEATURES, SOURCE_PROFILE,
};

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
    /// Default branch for branch gates. Defaults to the repository's own,
    /// exactly like a plain generator run.
    pub(crate) default_branch: Option<String>,
    /// Runner lanes, defaulting to both like a plain generator run (a
    /// repo-owned `[workflow] runners` still overrides).
    pub(crate) runners: RunnerMode,
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
    let pin_path = repo.join(GENERATION_CONFIG);
    // The pin stamp writes through to the config bytes, but the snapshot
    // records a link as a link: restoring through a symlinked config
    // would relink without reverting the stamped target. Refuse instead
    // of promoting a tree rollback cannot restore.
    if std::fs::symlink_metadata(&pin_path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(GeneratorError::usage(format!(
            "refusing to promote through symlinked generation config {}; restore it to a regular file so rollback can restore the pre-promotion bytes",
            pin_path.display()
        )));
    }
    let pin_content = std::fs::read_to_string(&pin_path)
        .map_err(|error| GeneratorError::io("read generation config", &pin_path, &error))?;
    let (stamped, old_pin) = stamp_pin(&pin_content, &options.rev)?;
    let default_branch = match &options.default_branch {
        Some(branch) => branch.clone(),
        None => resolve_default_branch(&repo)?,
    };
    let mut snapshot = Snapshot::capture(&repo, [PathBuf::from(GENERATION_CONFIG)])?;
    std::fs::write(&pin_path, &stamped)
        .map_err(|error| GeneratorError::io("write generation config", &pin_path, &error))?;
    let outcome = promote_rendered_tree(
        options,
        &repo,
        &default_branch,
        &mut snapshot,
        &old_pin,
        &closure,
    );
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
    let rendered = PromotedRender::render(repo, options.runners, default_branch)?;
    snapshot.extend(
        repo,
        rendered
            .files()
            .keys()
            .chain(rendered.symlinks().keys())
            .cloned()
            .chain([PathBuf::from(OWNERSHIP_STATE)]),
    )?;
    // Promotion overwrites generator-owned files by design: every genuine pin
    // advance changes existing rendered bytes, so the conflicts guard would
    // refuse every real promotion. Force is the intended semantic here — the
    // promotion already requires a tracked-clean tree, snapshots every
    // preimage for restore, and proves determinism plus write integrity after
    // the write. Force bypasses only the conflicts guard: ownership proof
    // still rejects manually modified files, and adopt stays false so unowned
    // workflows are never deleted.
    rendered.write(repo)?;
    verify_promoted_tree(
        repo,
        rendered.files(),
        rendered.symlinks(),
        options.runners,
        default_branch,
    )?;
    let expected_state = rendered.expected_ownership_state();
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
        .files()
        .keys()
        .chain(rendered.symlinks().keys())
        .cloned()
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
    if !super::closure::is_full_closure(SOURCE_CLOSURE) {
        return Err(GeneratorError::usage(
            "promotion requires a binary that proves its own source: this velnor-workflow stamps no closure (built without git); rebuild it from a git checkout"
                .to_owned(),
        ));
    }
    let pin_closure = super::closure::closure_of_tree(
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

/// Prove the write is faithful: a fresh re-render of the stamped tree must be
/// byte-identical to the render just written (determinism), and every disk
/// byte must match it (write integrity).
/// Map across the pipeline boundary the same way the R2 bridge does: both
/// error types carry a single message string.
#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err hands over ownership; borrowing would push a closure onto every call site"
)]
fn s2_error(error: super::s2::GeneratorError) -> GeneratorError {
    GeneratorError::usage(error.to_string())
}

/// One promotion render: the target tree's own pipeline, mirroring the R2
/// bridge dispatch. Schema-2 trees render through the provider pipeline (the
/// schema-1 parser predates schema-2 config fields like `trust` and rejects
/// trees the main pipeline accepts); schema-1 consumer trees keep the
/// original renderer (the schema-2 parser rejects legacy fields like
/// `velnor_labels`). Either pipeline's strict schema gate still fails a
/// misrouted tree closed.
enum PromotedRender {
    V1(Box<super::RenderedTree>),
    V2(Box<super::s2::RenderedTree>),
}

impl PromotedRender {
    fn render(
        repo: &Path,
        runners: RunnerMode,
        default_branch: &str,
    ) -> Result<Self, GeneratorError> {
        if dir_is_schema2(repo) {
            render_tree(repo, runners_to_providers(runners), default_branch)
                .map(Box::new)
                .map(Self::V2)
                .map_err(s2_error)
        } else {
            super::render_tree(repo, runners, default_branch)
                .map(Box::new)
                .map(Self::V1)
        }
    }

    fn files(&self) -> &BTreeMap<PathBuf, String> {
        match self {
            Self::V1(rendered) => &rendered.files,
            Self::V2(rendered) => &rendered.files,
        }
    }

    fn symlinks(&self) -> &BTreeMap<PathBuf, PathBuf> {
        match self {
            Self::V1(rendered) => &rendered.symlinks,
            Self::V2(rendered) => &rendered.symlinks,
        }
    }

    /// Write the render with promotion (force) semantics.
    fn write(&self, repo: &Path) -> Result<(), GeneratorError> {
        match self {
            Self::V1(rendered) => {
                super::write_generated_with_options(
                    repo,
                    &rendered.files,
                    &rendered.symlinks,
                    &rendered.inputs,
                    false,
                    false,
                    true,
                    false,
                )?;
                Ok(())
            }
            Self::V2(rendered) => {
                write_generated_with_options(
                    repo,
                    &rendered.files,
                    &rendered.symlinks,
                    &rendered.inputs,
                    false,
                    false,
                    true,
                    false,
                )
                .map_err(s2_error)?;
                Ok(())
            }
        }
    }

    /// The ownership metadata the write must have produced.
    fn expected_ownership_state(&self) -> String {
        match self {
            Self::V1(rendered) => super::ownership_state_content(
                &rendered.files,
                &rendered.symlinks,
                &rendered.inputs,
            ),
            Self::V2(rendered) => {
                ownership_state_content(&rendered.files, &rendered.symlinks, &rendered.inputs)
            }
        }
    }
}

/// Map the legacy `--runners` vocabulary onto provider sets. `both` stays
/// unset so the repository config (or the full three-provider universe)
/// decides, exactly like a plain generator run without `--providers`.
/// `github` predates the hosted/self-hosted split and keeps both flavors.
fn runners_to_providers(runners: RunnerMode) -> Option<ProviderSet> {
    match runners {
        RunnerMode::Both => None,
        RunnerMode::Github => Some(ProviderSet::from([
            ProviderId::GithubHosted,
            ProviderId::GithubSelfHosted,
        ])),
        RunnerMode::Velnor => Some(ProviderSet::from([ProviderId::Velnor])),
    }
}

fn verify_promoted_tree(
    repo: &Path,
    rendered: &BTreeMap<PathBuf, String>,
    symlinks: &BTreeMap<PathBuf, PathBuf>,
    runners: RunnerMode,
    default_branch: &str,
) -> Result<(), GeneratorError> {
    let again = PromotedRender::render(repo, runners, default_branch)?;
    let again_files = again.files();
    if again_files != rendered {
        let divergent: Vec<String> = rendered
            .keys()
            .chain(again_files.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|path| rendered.get(*path) != again_files.get(*path))
            .map(|path| path.display().to_string())
            .collect();
        return Err(GeneratorError::usage(format!(
            "the stamped tree does not render deterministically; refusing to promote: {}",
            divergent.join(", ")
        )));
    }
    if again.symlinks() != symlinks {
        return Err(GeneratorError::usage(
            "the stamped tree does not render symlinks deterministically; refusing to promote"
                .to_owned(),
        ));
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
    for (relative, wanted) in symlinks {
        let path = repo.join(relative);
        let target = std::fs::read_link(&path)
            .map_err(|error| GeneratorError::io("read promoted symlink", &path, &error))?;
        if &target != wanted {
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

/// Recorded pre-promotion content for every path the promotion may write, so
/// a failure restores the tree instead of leaving it half-rendered. Links
/// record their target: reading through a link would restore a symlink as a
/// regular file holding its target's bytes.
enum SnapshotPreimage {
    Bytes(Vec<u8>),
    Symlink(PathBuf),
}

struct Snapshot {
    entries: Vec<(PathBuf, Option<SnapshotPreimage>)>,
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
            let preimage = match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = std::fs::read_link(&path)
                        .map_err(|error| GeneratorError::io("read preimage", &path, &error))?;
                    Some(SnapshotPreimage::Symlink(target))
                }
                Ok(_) => match std::fs::read(&path) {
                    Ok(bytes) => Some(SnapshotPreimage::Bytes(bytes)),
                    Err(error) => {
                        return Err(GeneratorError::io("read preimage", &path, &error));
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(GeneratorError::io("read preimage", &path, &error)),
            };
            self.entries.push((relative, preimage));
        }
        Ok(())
    }

    /// Best-effort restore of every recorded preimage: overwritten files get
    /// their bytes back, overwritten links are relinked, created paths are
    /// removed, and emptied directories are pruned. Every entry is attempted
    /// even when one fails.
    fn restore(&mut self, repo: &Path) -> Result<(), GeneratorError> {
        let mut failures = Vec::new();
        for (relative, preimage) in &self.entries {
            let path = repo.join(relative);
            let result = match preimage {
                Some(SnapshotPreimage::Bytes(bytes)) => std::fs::write(&path, bytes)
                    .map_err(|error| format!("restore {}: {error}", path.display())),
                Some(SnapshotPreimage::Symlink(target)) => restore_symlink(&path, target),
                None => {
                    if std::fs::symlink_metadata(&path).is_ok() {
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

/// Relink a snapshot link preimage: remove whatever the promotion wrote at
/// the path, then recreate the recorded link.
fn restore_symlink(path: &Path, target: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_ok() {
        std::fs::remove_file(path)
            .map_err(|error| format!("remove {}: {error}", path.display()))?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("restore parent of {}: {error}", path.display()))?;
    }
    create_generator_symlink(target, path)
        .map_err(|error| format!("restore {}: {error}", path.display()))
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
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use super::*;

    const OLD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NEW: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

    #[test]
    fn stamp_replaces_the_generator_revision_and_nothing_else() {
        let content = format!(
            "schema = 1\n\n[generator]\nrepository = \"example/consumer\"\n# D19 pin: bump in a single pin commit.\nrevision = \"{OLD}\"  # trailing comment\n\n[workflow]\nrunners = \"both\"\n"
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
            "schema = 1\n\n[workflow]\nrevision = \"{OLD}\"\n\n[generator]\nrepository = \"example/consumer\"\nrevision = \"{OLD}\"\n"
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
    fn runners_map_onto_provider_sets() {
        assert_eq!(runners_to_providers(RunnerMode::Both), None);
        assert_eq!(
            runners_to_providers(RunnerMode::Github),
            Some(ProviderSet::from([
                ProviderId::GithubHosted,
                ProviderId::GithubSelfHosted,
            ]))
        );
        assert_eq!(
            runners_to_providers(RunnerMode::Velnor),
            Some(ProviderSet::from([ProviderId::Velnor]))
        );
    }

    #[test]
    fn promote_renders_schema2_trust_units() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures-s2/promote-trust");
        assert!(
            must_fail(
                crate::config::discover(&root),
                "the schema-1 parser must reject the trust-bearing tree"
            )
            .contains("trust"),
            "the fixture proves the schema-2-only path"
        );
        let rendered = must(
            PromotedRender::render(&root, RunnerMode::Both, "main"),
            "promotion renders a schema-2 tree carrying `trust`",
        );
        assert!(
            matches!(rendered, PromotedRender::V2(_)),
            "schema-2 trees render through the provider pipeline"
        );
        assert!(
            !rendered.files().is_empty(),
            "the trust-bearing tree renders files"
        );
    }

    #[test]
    fn promote_renders_schema1_consumers_through_the_legacy_pipeline() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic-workspace");
        assert!(
            !dir_is_schema2(&root),
            "the consumer fixture declares the legacy schema"
        );
        let rendered = must(
            PromotedRender::render(&root, RunnerMode::Both, "main"),
            "promotion renders a schema-1 consumer tree",
        );
        assert!(
            matches!(rendered, PromotedRender::V1(_)),
            "schema-1 trees keep the original renderer"
        );
        assert!(
            !rendered.files().is_empty(),
            "the consumer tree renders files"
        );
    }

    #[test]
    fn stamp_fails_closed_on_missing_malformed_or_ambiguous_pins() {
        let missing = "schema = 1\n\n[generator]\nrepository = \"example/consumer\"\n";
        assert!(
            must_fail(stamp_pin(missing, NEW), "a missing pin fails")
                .contains("no [generator] revision"),
            "a missing pin names the defect"
        );
        let malformed = "schema = 1\n\n[generator]\nrevision = \"short\"\n".to_owned();
        assert!(
            must_fail(stamp_pin(&malformed, NEW), "a malformed pin fails")
                .contains("not a full 40-character commit SHA"),
            "a malformed pin names the defect"
        );
        let ambiguous =
            format!("schema = 1\n\n[generator]\nrevision = \"{OLD}\"\nrevision = \"{OLD}\"\n");
        assert!(
            must_fail(stamp_pin(&ambiguous, NEW), "an ambiguous pin fails").contains("twice"),
            "a doubled pin refuses the ambiguous stamp"
        );
    }
}

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
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::s2::dispatch::dir_is_schema2;
use super::s2::policy::GENERATION_CONFIG;
use super::s2::provider::{ProviderId, ProviderSet};
use super::s2::{ownership_state_content, render_tree, OWNERSHIP_STATE};
use super::{
    create_generator_symlink, is_full_revision, resolve_default_branch, GenerationLock,
    GeneratorError, RunnerMode, SOURCE_CLOSURE, SOURCE_FEATURES, SOURCE_PROFILE,
};

fn read_regular_generation_config(
    repo: &Path,
    pin_path: &Path,
) -> Result<(String, std::fs::Permissions), GeneratorError> {
    super::ensure_no_symlinked_path_ancestors(repo, Path::new(GENERATION_CONFIG))?;
    let metadata = std::fs::symlink_metadata(pin_path)
        .map_err(|error| GeneratorError::io("inspect generation config", pin_path, &error))?;
    if !metadata.file_type().is_file() {
        return Err(GeneratorError::usage(format!(
            "refusing non-regular generation config: {}",
            pin_path.display()
        )));
    }
    let permissions = metadata.permissions();
    let content = std::fs::read_to_string(pin_path)
        .map_err(|error| GeneratorError::io("read generation config", pin_path, &error))?;
    Ok((content, permissions))
}

/// Replace the pin atomically while carrying Rust's portable permission
/// state: Unix mode bits or the Windows read-only flag.
fn replace_promotion_pin(
    repo: &Path,
    relative: &Path,
    path: &Path,
    expected: &str,
    content: &str,
    permissions: &std::fs::Permissions,
) -> Result<(), GeneratorError> {
    super::ensure_no_symlinked_path_ancestors(repo, relative)?;
    let (current, current_permissions) = read_regular_generation_config(repo, path)?;
    if current != expected || !same_permission_state(&current_permissions, permissions) {
        return Err(GeneratorError::usage(format!(
            "generation config changed after promotion preflight: {}; review again",
            path.display()
        )));
    }
    if content == expected {
        return Ok(());
    }

    // Atomic rename can replace a pin even when the caller cannot open the
    // existing file for writing. Preserve the prior in-place write
    // authorization contract before creating a sibling stage. `write(true)`
    // does not truncate unless `truncate(true)` is also requested.
    super::ensure_no_symlinked_path_ancestors(repo, relative)?;
    let writable_pin = std::fs::OpenOptions::new()
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|error| GeneratorError::io("open generation config for writing", path, &error))?;
    let opened_metadata = writable_pin
        .metadata()
        .map_err(|error| GeneratorError::io("inspect writable generation config", path, &error))?;
    if !opened_metadata.file_type().is_file()
        || !same_permission_state(&opened_metadata.permissions(), permissions)
    {
        return Err(GeneratorError::usage(format!(
            "generation config changed after promotion preflight: {}; review again",
            path.display()
        )));
    }
    drop(writable_pin);

    let parent = path
        .parent()
        .ok_or_else(|| GeneratorError::usage("generation config has no parent directory"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| GeneratorError::usage("generation config has no filename"))?;
    let (staged, mut file) = (0..16_u8)
        .find_map(|attempt| {
            let staged = parent.join(format!(
                ".{}.promote-{}-{}-{attempt}",
                file_name.to_string_lossy(),
                std::process::id(),
                crate::unique_suffix()
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)
            {
                Ok(file) => Some(Ok((staged, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(GeneratorError::io(
                    "create staged generation config",
                    &staged,
                    &error,
                ))),
            }
        })
        .transpose()?
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "could not reserve a staged generation config beside {}",
                path.display()
            ))
        })?;
    let staged_relative = staged.strip_prefix(repo).map_err(|error| {
        GeneratorError::usage(format!(
            "staged generation config is outside repository root: {error}"
        ))
    })?;
    let result = (|| {
        super::ensure_no_symlinked_path_ancestors(repo, staged_relative)?;
        file.write_all(content.as_bytes()).map_err(|error| {
            GeneratorError::io("write staged generation config", &staged, &error)
        })?;
        file.set_permissions(permissions.clone()).map_err(|error| {
            GeneratorError::io("set staged generation config permissions", &staged, &error)
        })?;
        file.sync_all().map_err(|error| {
            GeneratorError::io("sync staged generation config", &staged, &error)
        })?;
        drop(file);

        super::ensure_no_symlinked_path_ancestors(repo, relative)?;
        super::ensure_no_symlinked_path_ancestors(repo, staged_relative)?;
        let (current, current_permissions) = read_regular_generation_config(repo, path)?;
        if current != expected || !same_permission_state(&current_permissions, permissions) {
            return Err(GeneratorError::usage(format!(
                "generation config changed after promotion preflight: {}; review again",
                path.display()
            )));
        }
        std::fs::rename(&staged, path)
            .map_err(|error| GeneratorError::io("replace generation config", path, &error))
    })();
    if result.is_err() && super::ensure_no_symlinked_path_ancestors(repo, staged_relative).is_ok() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

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
    let generation_lock = GenerationLock::acquire(&repo, [])?;
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
    let (pin_content, pin_permissions) = read_regular_generation_config(&repo, &pin_path)?;
    let (stamped, old_pin) = stamp_pin(&pin_content, &options.rev)?;
    let default_branch = match &options.default_branch {
        Some(branch) => branch.clone(),
        None => resolve_default_branch(&repo)?,
    };
    // Capture the previous renderer's output set before stamping the pin.
    // Sidecar rows omitted by this render are untrusted and cannot enlarge the
    // promotion snapshot or git-add ownership set.
    let previous_render = PromotedRender::render(&repo, options.runners, &default_branch)?;
    let previous_outputs = previous_render.output_paths();
    let mut snapshot = Snapshot::capture(
        &repo,
        promotion_snapshot_paths(&previous_outputs, stamped != pin_content),
    )?;
    let outcome = (|| {
        let (current_pin, _) = read_regular_generation_config(&repo, &pin_path)?;
        if current_pin != pin_content {
            return Err(GeneratorError::usage(format!(
                "generation config changed after promotion preflight: {}; review again",
                pin_path.display()
            )));
        }
        replace_promotion_pin(
            &repo,
            Path::new(GENERATION_CONFIG),
            &pin_path,
            &pin_content,
            &stamped,
            &pin_permissions,
        )?;
        promote_rendered_tree(
            options,
            &repo,
            &default_branch,
            &mut snapshot,
            &old_pin,
            &closure,
            &previous_outputs,
            &generation_lock,
        )
    })();
    match outcome {
        Ok(report) => {
            if let Err(error) = snapshot.discard_recovery_links(&repo) {
                eprintln!("warning: promotion succeeded, but cleanup failed: {error}");
            }
            Ok(report)
        }
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

fn promotion_snapshot_paths(
    previous_outputs: &BTreeSet<PathBuf>,
    preserve_pin: bool,
) -> impl Iterator<Item = PathBuf> + '_ {
    previous_outputs
        .iter()
        .cloned()
        .chain([PathBuf::from(OWNERSHIP_STATE)])
        .chain(preserve_pin.then(|| PathBuf::from(GENERATION_CONFIG)))
}

/// Render, verify, stage, and commit after the pin file is stamped. Every
/// error path restores `snapshot` in the caller.
#[expect(
    clippy::too_many_arguments,
    reason = "the promotion transaction passes its explicit preflight evidence and lock"
)]
fn promote_rendered_tree(
    options: &PromoteOptions,
    repo: &Path,
    default_branch: &str,
    snapshot: &mut Snapshot,
    old_pin: &str,
    closure: &str,
    previous_outputs: &BTreeSet<PathBuf>,
    generation_lock: &GenerationLock,
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
    rendered.write(repo, generation_lock)?;
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
        .chain(previous_outputs.iter().cloned())
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

    fn output_paths(&self) -> BTreeSet<PathBuf> {
        self.files()
            .keys()
            .chain(self.symlinks().keys())
            .cloned()
            .collect()
    }

    /// Write the render with promotion (force) semantics.
    fn write(&self, repo: &Path, generation_lock: &GenerationLock) -> Result<(), GeneratorError> {
        match self {
            Self::V1(rendered) => {
                super::write_generated_with_static_sources_with_options_locked(
                    repo,
                    &rendered.files,
                    &rendered.symlinks,
                    &rendered.inputs,
                    &rendered.static_sources,
                    true,
                    false,
                    generation_lock,
                )?;
                Ok(())
            }
            Self::V2(rendered) => {
                super::s2::write_generated_with_static_sources_with_options_locked(
                    repo,
                    &rendered.files,
                    &rendered.symlinks,
                    &rendered.inputs,
                    &rendered.static_sources,
                    true,
                    false,
                    generation_lock,
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
    Bytes {
        bytes: Vec<u8>,
        permissions: std::fs::Permissions,
    },
    HardLink {
        backup: PathBuf,
        identity: PinFileIdentity,
        bytes: Vec<u8>,
        permissions: std::fs::Permissions,
    },
    Symlink(PathBuf),
}

/// Keep the original pin inode open until the promotion resolves. Metadata
/// equality alone cannot establish identity after the recovery link is
/// replaced or removed.
struct PinFileIdentity {
    #[cfg(any(unix, windows))]
    anchor: std::fs::File,
    #[cfg(windows)]
    file_id: win_file_id::FileId,
}

struct Snapshot {
    entries: Vec<(PathBuf, Option<SnapshotPreimage>)>,
    seen: BTreeSet<PathBuf>,
    preexisting_directories: BTreeSet<PathBuf>,
}

impl Snapshot {
    fn capture(
        repo: &Path,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, GeneratorError> {
        let mut snapshot = Self {
            entries: Vec::new(),
            seen: BTreeSet::new(),
            preexisting_directories: BTreeSet::new(),
        };
        match snapshot.extend(repo, paths) {
            Ok(()) => Ok(snapshot),
            Err(error) => match snapshot.discard_recovery_links(repo) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(GeneratorError::usage(format!(
                    "{error}; cleaning promotion recovery links failed: {cleanup}"
                ))),
            },
        }
    }

    fn extend(
        &mut self,
        repo: &Path,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<(), GeneratorError> {
        for relative in paths {
            crate::ensure_no_symlinked_path_ancestors(repo, &relative)?;
            if self.seen.contains(&relative) {
                continue;
            }
            let preexisting_directories = existing_parent_directories(repo, &relative)?;
            let path = repo.join(&relative);
            let preimage = match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = std::fs::read_link(&path)
                        .map_err(|error| GeneratorError::io("read preimage", &path, &error))?;
                    Some(SnapshotPreimage::Symlink(target))
                }
                Ok(metadata) if metadata.file_type().is_file() => {
                    let permissions = metadata.permissions();
                    if relative == Path::new(GENERATION_CONFIG) {
                        let bytes = std::fs::read(&path).map_err(|error| {
                            GeneratorError::io("read pin preimage", &path, &error)
                        })?;
                        let (backup, identity) = preserve_pin_preimage(repo, &relative, &path)?;
                        Some(SnapshotPreimage::HardLink {
                            backup,
                            identity,
                            bytes,
                            permissions,
                        })
                    } else {
                        match std::fs::read(&path) {
                            Ok(bytes) => Some(SnapshotPreimage::Bytes { bytes, permissions }),
                            Err(error) => {
                                return Err(GeneratorError::io("read preimage", &path, &error));
                            }
                        }
                    }
                }
                Ok(metadata) => {
                    return Err(GeneratorError::usage(format!(
                        "refusing non-regular promotion preimage: {} ({:?})",
                        path.display(),
                        metadata.file_type()
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(GeneratorError::io("read preimage", &path, &error)),
            };
            self.seen.insert(relative.clone());
            self.preexisting_directories.extend(preexisting_directories);
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
        let mut paths_to_prune = Vec::new();
        for (relative, preimage) in &self.entries {
            if let Err(error) = crate::ensure_no_symlinked_path_ancestors(repo, relative) {
                failures.push(format!("restore {}: {error}", relative.display()));
                continue;
            }
            let path = repo.join(relative);
            let result = match preimage {
                Some(SnapshotPreimage::Bytes { bytes, permissions }) => {
                    crate::restore_snapshot_file_atomically(repo, relative, bytes, permissions)
                        .map_err(|error| format!("restore {}: {error}", path.display()))
                }
                Some(SnapshotPreimage::HardLink {
                    backup,
                    identity,
                    bytes,
                    permissions,
                }) => restore_pin_preimage(repo, relative, backup, identity, bytes, permissions),
                Some(SnapshotPreimage::Symlink(target)) => {
                    restore_symlink(repo, relative, &path, target)
                }
                None => (|| -> Result<(), String> {
                    match std::fs::symlink_metadata(&path) {
                        Ok(_) => {
                            crate::ensure_no_symlinked_path_ancestors(repo, relative)
                                .map_err(|error| error.to_string())?;
                            std::fs::remove_file(&path)
                                .map_err(|error| format!("remove {}: {error}", path.display()))?;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Err(error) => {
                            return Err(format!("inspect {}: {error}", path.display()));
                        }
                    }
                    Ok(())
                })(),
            };
            let should_prune = preimage.is_none() && result.is_ok();
            if let Err(failure) = result {
                failures.push(failure);
            }
            if should_prune {
                paths_to_prune.push(relative.clone());
            }
        }
        failures.extend(prune_empty_ancestors(
            repo,
            &paths_to_prune,
            &self.preexisting_directories,
        ));
        if failures.is_empty() {
            Ok(())
        } else {
            Err(GeneratorError::usage(format!(
                "restore the pre-promotion tree: {}",
                failures.join("; ")
            )))
        }
    }

    /// Remove the pin recovery link only after promotion succeeds. Rollback
    /// consumes it with rename so the original inode is restored.
    fn discard_recovery_links(&self, repo: &Path) -> Result<(), String> {
        let mut failures = Vec::new();
        for (_, preimage) in &self.entries {
            let Some(SnapshotPreimage::HardLink {
                backup,
                identity,
                bytes,
                permissions,
            }) = preimage
            else {
                continue;
            };
            if let Err(error) = crate::ensure_no_symlinked_path_ancestors(repo, backup) {
                failures.push(format!("refusing cleanup of {}: {error}", backup.display()));
                continue;
            }
            let path = repo.join(backup);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    let matches_preimage = std::fs::read(&path)
                        .is_ok_and(|current| current.as_slice() == bytes.as_slice())
                        && same_permission_state(&metadata.permissions(), permissions)
                        && path_matches_file_identity(&path, identity).unwrap_or(false);
                    if !matches_preimage {
                        failures.push(format!(
                            "recovery link {} no longer matches the captured pin; left in place",
                            path.display()
                        ));
                    } else if let Err(error) = std::fs::remove_file(&path) {
                        failures.push(format!("remove recovery link {}: {error}", path.display()));
                    }
                }
                Ok(_) => failures.push(format!(
                    "refusing to remove non-regular recovery link {}",
                    path.display()
                )),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => {
                    failures.push(format!("inspect recovery link {}: {error}", path.display()));
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

/// Keep the pin's exact inode available until the transaction commits. The
/// hidden link is a sibling, so rollback can atomically rename it over the
/// stamped pin without recreating the file or changing its metadata.
#[cfg(any(unix, windows))]
fn preserve_pin_preimage(
    repo: &Path,
    relative: &Path,
    path: &Path,
) -> Result<(PathBuf, PinFileIdentity), GeneratorError> {
    crate::ensure_no_symlinked_path_ancestors(repo, relative)?;
    let parent = path
        .parent()
        .ok_or_else(|| GeneratorError::usage("generation config has no parent directory"))?;
    let anchor = std::fs::File::open(path)
        .map_err(|error| GeneratorError::io("identify pin preimage", path, &error))?;
    #[cfg(windows)]
    let file_id = win_file_id::get_high_res_file_id(path)
        .map_err(|error| GeneratorError::io("identify pin preimage", path, &error))?;
    let identity = PinFileIdentity {
        anchor,
        #[cfg(windows)]
        file_id,
    };
    for attempt in 0..16_u8 {
        let backup = parent.join(format!(
            ".velnor-pin-preimage-{}-{}-{attempt}",
            std::process::id(),
            crate::unique_suffix()
        ));
        let backup_relative = backup.strip_prefix(repo).map_err(|error| {
            GeneratorError::usage(format!(
                "pin recovery link escaped repository root: {error}"
            ))
        })?;
        crate::ensure_no_symlinked_path_ancestors(repo, backup_relative)?;
        match std::fs::hard_link(path, &backup) {
            Ok(()) => {
                let metadata = match std::fs::symlink_metadata(&backup) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        let cleanup = remove_uncommitted_pin_recovery_link(
                            repo,
                            backup_relative,
                            &backup,
                            &identity,
                        );
                        let diagnostic = match cleanup {
                            Ok(()) => format!(
                                "inspect pin recovery link {}: {error}; temporary link removed",
                                backup.display()
                            ),
                            Err(cleanup) => format!(
                                "inspect pin recovery link {}: {error}; cleanup failed: {cleanup}; recovery path may remain at {}",
                                backup.display(),
                                backup.display()
                            ),
                        };
                        return Err(GeneratorError::usage(diagnostic));
                    }
                };
                if !metadata.file_type().is_file() {
                    let cleanup = remove_uncommitted_pin_recovery_link(
                        repo,
                        backup_relative,
                        &backup,
                        &identity,
                    );
                    let recovery = match cleanup {
                        Ok(()) => "unexpected recovery entry removed".to_owned(),
                        Err(error) => format!(
                            "unexpected recovery entry preserved for review at {}: {error}",
                            backup.display()
                        ),
                    };
                    return Err(GeneratorError::usage(format!(
                        "refusing non-regular pin recovery link: {}; {recovery}",
                        backup.display(),
                    )));
                }
                return Ok((backup_relative.to_path_buf(), identity));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => {
                return Err(GeneratorError::io("preserve pin preimage", &backup, &error));
            }
        }
    }
    Err(GeneratorError::usage(format!(
        "could not reserve a pin recovery link beside {}",
        path.display()
    )))
}

fn remove_uncommitted_pin_recovery_link(
    repo: &Path,
    relative: &Path,
    path: &Path,
    identity: &PinFileIdentity,
) -> Result<(), String> {
    crate::ensure_no_symlinked_path_ancestors(repo, relative).map_err(|error| error.to_string())?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if !path_matches_file_identity(path, identity)
                .map_err(|error| format!("identify {}: {error}", path.display()))?
            {
                return Err(format!(
                    "path does not match the captured pin identity: {}",
                    path.display()
                ));
            }
            crate::ensure_no_symlinked_path_ancestors(repo, relative)
                .map_err(|error| error.to_string())?;
            std::fs::remove_file(path).map_err(|error| {
                format!("remove temporary recovery link {}: {error}", path.display())
            })
        }
        Ok(_) => Err(format!(
            "refusing to remove unexpected non-regular path {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

#[cfg(not(any(unix, windows)))]
fn preserve_pin_preimage(
    _repo: &Path,
    _relative: &Path,
    _path: &Path,
) -> Result<(PathBuf, PinFileIdentity), GeneratorError> {
    Err(GeneratorError::usage(
        "refusing to change generation config: file identity is unsupported on this platform",
    ))
}

fn restore_pin_preimage(
    repo: &Path,
    relative: &Path,
    backup: &Path,
    identity: &PinFileIdentity,
    bytes: &[u8],
    permissions: &std::fs::Permissions,
) -> Result<(), String> {
    crate::ensure_no_symlinked_path_ancestors(repo, relative).map_err(|error| error.to_string())?;
    crate::ensure_no_symlinked_path_ancestors(repo, backup).map_err(|error| error.to_string())?;
    let backup_path = repo.join(backup);
    let metadata = match std::fs::symlink_metadata(&backup_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let path = repo.join(relative);
            let current = std::fs::symlink_metadata(&path)
                .map_err(|inspect| format!("inspect pin {}: {inspect}", path.display()))?;
            if current.file_type().is_file()
                && std::fs::read(&path).is_ok_and(|current_bytes| current_bytes == bytes)
                && same_permission_state(&current.permissions(), permissions)
                && path_matches_file_identity(&path, identity).unwrap_or(false)
            {
                return Ok(());
            }
            return Err(format!(
                "pin recovery link {} is missing and {} does not match its captured preimage",
                backup_path.display(),
                path.display()
            ));
        }
        Err(error) => {
            return Err(format!(
                "inspect pin recovery link {}: {error}",
                backup_path.display()
            ));
        }
    };
    if !metadata.file_type().is_file() {
        return Err(format!(
            "refusing non-regular pin recovery link {}",
            backup_path.display()
        ));
    }
    let backup_matches = std::fs::read(&backup_path)
        .is_ok_and(|backup_bytes| backup_bytes.as_slice() == bytes)
        && same_permission_state(&metadata.permissions(), permissions)
        && path_matches_file_identity(&backup_path, identity).unwrap_or(false);
    if !backup_matches {
        return Err(format!(
            "pin recovery link {} no longer matches the captured preimage; preserved",
            backup_path.display()
        ));
    }
    let path = repo.join(relative);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata)
            if metadata.file_type().is_file()
                && std::fs::read(&path).is_ok_and(|current_bytes| current_bytes == bytes)
                && same_permission_state(&metadata.permissions(), permissions)
                && path_matches_file_identity(&path, identity).unwrap_or(false) =>
        {
            crate::ensure_no_symlinked_path_ancestors(repo, relative)
                .map_err(|error| error.to_string())?;
            crate::ensure_no_symlinked_path_ancestors(repo, backup)
                .map_err(|error| error.to_string())?;
            std::fs::remove_file(&backup_path).map_err(|error| {
                format!(
                    "remove redundant pin recovery link {}: {error}",
                    backup_path.display()
                )
            })?;
            return Ok(());
        }
        Ok(metadata) if metadata.file_type().is_dir() => {
            return Err(format!(
                "restore pin {}: a directory now occupies the path; recovery link remains at {}",
                path.display(),
                backup_path.display()
            ));
        }
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(format!("inspect pin {}: {error}", path.display())),
    }
    crate::ensure_no_symlinked_path_ancestors(repo, relative).map_err(|error| error.to_string())?;
    crate::ensure_no_symlinked_path_ancestors(repo, backup).map_err(|error| error.to_string())?;
    std::fs::rename(&backup_path, &path).map_err(|error| {
        format!(
            "restore pin {} from recovery link {}: {error}",
            path.display(),
            backup_path.display()
        )
    })?;
    Ok(())
}

fn path_matches_file_identity(path: &Path, expected: &PinFileIdentity) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let expected = expected.anchor.metadata()?;
        let actual = std::fs::metadata(path)?;
        Ok(expected.dev() == actual.dev() && expected.ino() == actual.ino())
    }
    #[cfg(windows)]
    {
        let _anchor = expected.anchor.metadata()?;
        let actual = win_file_id::get_high_res_file_id(path)?;
        Ok(actual == expected.file_id)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, expected);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "pin file identity is unsupported on this platform",
        ))
    }
}

fn same_permission_state(left: &std::fs::Permissions, right: &std::fs::Permissions) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        left.mode() == right.mode()
    }
    #[cfg(not(unix))]
    {
        left.readonly() == right.readonly()
    }
}

/// Relink a snapshot link preimage: remove whatever the promotion wrote at
/// the path, then recreate the recorded link.
fn restore_symlink(repo: &Path, relative: &Path, path: &Path, target: &Path) -> Result<(), String> {
    crate::ensure_no_symlinked_path_ancestors(repo, relative).map_err(|error| error.to_string())?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            crate::ensure_no_symlinked_path_ancestors(repo, relative)
                .map_err(|error| error.to_string())?;
            std::fs::remove_file(path)
                .map_err(|error| format!("remove {}: {error}", path.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(format!("inspect {}: {error}", path.display())),
    }
    if let Some(parent) = path.parent() {
        crate::ensure_no_symlinked_path_ancestors(repo, relative)
            .map_err(|error| error.to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("restore parent of {}: {error}", path.display()))?;
    }
    crate::ensure_no_symlinked_path_ancestors(repo, relative).map_err(|error| error.to_string())?;
    create_generator_symlink(target, path)
        .map_err(|error| format!("restore {}: {error}", path.display()))
}

/// Remove directories the promotion created, stopping at the first
/// non-empty one (so only promotion-created empties disappear).
fn existing_parent_directories(
    repo: &Path,
    relative: &Path,
) -> Result<BTreeSet<PathBuf>, GeneratorError> {
    let mut existing = BTreeSet::new();
    for ancestor in relative.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        let path = repo.join(ancestor);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                existing.insert(ancestor.to_path_buf());
            }
            Ok(metadata) => {
                return Err(GeneratorError::usage(format!(
                    "refusing non-directory promotion parent: {} ({:?})",
                    path.display(),
                    metadata.file_type()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => {
                return Err(GeneratorError::io(
                    "inspect promotion parent",
                    &path,
                    &error,
                ))
            }
        }
    }
    Ok(existing)
}

fn prune_empty_ancestors(
    repo: &Path,
    removed_paths: &[PathBuf],
    preexisting_directories: &BTreeSet<PathBuf>,
) -> Vec<String> {
    let mut candidates = BTreeSet::new();
    for removed_path in removed_paths {
        let mut current = removed_path.parent();
        while let Some(relative_directory) = current {
            if relative_directory.as_os_str().is_empty()
                || preexisting_directories.contains(relative_directory)
            {
                break;
            }
            candidates.insert(relative_directory.to_path_buf());
            current = relative_directory.parent();
        }
    }

    // Remove the union deepest-first. Shared ancestors are attempted once,
    // after every captured child has had a chance to disappear.
    let mut ordered: Vec<_> = candidates.into_iter().collect();
    ordered.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    let mut failures = Vec::new();
    for relative_directory in ordered {
        if let Err(error) = crate::ensure_no_symlinked_path_ancestors(repo, &relative_directory) {
            failures.push(format!(
                "refusing to prune {}: {error}",
                relative_directory.display()
            ));
            continue;
        }
        let directory = repo.join(&relative_directory);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                match std::fs::remove_dir(&directory) {
                    Ok(()) => (),
                    Err(error) => failures.push(format!(
                        "remove promotion-created directory {}: {error}",
                        directory.display()
                    )),
                }
            }
            Ok(metadata) => failures.push(format!(
                "refusing unexpected non-directory ancestor {} ({:?})",
                directory.display(),
                metadata.file_type()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => failures.push(format!("inspect {}: {error}", directory.display())),
        }
    }
    failures
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
    fn promotion_change_set_accepts_deletion_of_previous_owned_output() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-dropped-output-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(&root),
            "create promotion repository",
        );
        let run_git = |arguments: &[&str]| {
            let output = must(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(arguments)
                    .output(),
                "run test git command",
            );
            assert!(
                output.status.success(),
                "git {} failed: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&["init", "--quiet"]);
        run_git(&["config", "user.email", "promotion-test@example.invalid"]);
        run_git(&["config", "user.name", "Promotion Test"]);
        let previous = PathBuf::from("state/cache.env");
        must(
            std::fs::create_dir_all(root.join("state")),
            "create previous output parent",
        );
        must(
            std::fs::write(root.join(&previous), "previous output\n"),
            "write previous output",
        );
        run_git(&["add", "--", "state/cache.env"]);
        run_git(&["commit", "--quiet", "-m", "fixture"]);
        must(
            std::fs::remove_file(root.join(&previous)),
            "remove dropped previous output",
        );

        let changed = must(
            promotion_changes(&root, &BTreeSet::from([previous.clone()])),
            "include dropped previous ownership claim in promotion change set",
        );

        assert_eq!(changed, vec![previous]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn generation_config_read_refuses_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-parent-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".git")),
            "create Git metadata directory",
        );
        must(
            std::fs::write(root.join(".git/velnor-workflow.toml"), "protected pin\n"),
            "write Git metadata pin",
        );
        must(
            symlink(".git", root.join(".github-gen")),
            "symlink generation config parent to Git metadata",
        );

        let error = must_fail(
            read_regular_generation_config(&root, &root.join(GENERATION_CONFIG)),
            "pin read must reject a symlinked parent",
        );
        assert!(error.contains("symlinked ancestor"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn generation_config_read_rejects_fifo_before_opening() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-fifo-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let pin = root.join(GENERATION_CONFIG);
        let status = must(
            std::process::Command::new("mkfifo").arg(&pin).status(),
            "mkfifo must be available for the Unix safety test",
        );
        assert!(status.success(), "mkfifo failed with {status}");

        let error = must_fail(
            read_regular_generation_config(&root, &pin),
            "pin read must reject a fifo without opening it",
        );
        assert!(error.contains("non-regular generation config"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_pin_replacement_requires_existing_pin_write_access() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let uid_output = must(
            std::process::Command::new("id").arg("-u").output(),
            "read effective uid for permission test",
        );
        assert!(
            uid_output.status.success(),
            "id -u failed: {}",
            String::from_utf8_lossy(&uid_output.stderr)
        );
        let effective_uid = match String::from_utf8_lossy(&uid_output.stdout)
            .trim()
            .parse::<u32>()
        {
            Ok(uid) => uid,
            Err(error) => panic!("parse effective uid from id -u: {error}"),
        };
        if effective_uid == 0 {
            eprintln!(
                "skipping write-permission regression: effective uid 0 can bypass Unix mode-bit denial"
            );
            return;
        }

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-write-access-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let pin = root.join(GENERATION_CONFIG);
        must(
            std::fs::write(&pin, "old pin bytes\n"),
            "write generation config",
        );
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o444)),
            "remove generation config write permission",
        );
        let before = must(
            std::fs::symlink_metadata(&pin),
            "inspect protected generation config",
        );
        let permissions = before.permissions();
        match std::fs::OpenOptions::new()
            .write(true)
            .truncate(false)
            .open(&pin)
        {
            Ok(file) => {
                drop(file);
                let _ = std::fs::remove_dir_all(&root);
                eprintln!(
                    "skipping write-permission regression: effective credentials can write mode-0444 files"
                );
                return;
            }
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "mode-0444 fixture failed for an unexpected reason: {error}"
            ),
        }

        let error = must_fail(
            replace_promotion_pin(
                &root,
                Path::new(GENERATION_CONFIG),
                &pin,
                "old pin bytes\n",
                "new pin bytes\n",
                &permissions,
            ),
            "pin replacement must preserve in-place write authorization",
        );
        assert!(
            error.contains("open generation config for writing"),
            "{error}"
        );
        assert_eq!(
            must(std::fs::read(&pin), "read unchanged generation config"),
            b"old pin bytes\n"
        );
        let after = must(
            std::fs::symlink_metadata(&pin),
            "reinspect protected generation config",
        );
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
        assert_eq!(after.permissions().mode() & 0o777, 0o444);
        assert_eq!(
            must(
                std::fs::read_dir(root.join(".github-gen")),
                "inspect config directory after refused replacement"
            )
            .count(),
            1,
            "failed write preflight must not create a staged sibling"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_pin_replacement_does_not_mutate_hard_link_alias() {
        use std::os::unix::fs::PermissionsExt as _;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-hardlink-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let pin = root.join(GENERATION_CONFIG);
        let outside = root.with_extension("pin-alias");
        must(
            std::fs::write(&pin, "old pin bytes\n"),
            "write generation config",
        );
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)),
            "set generation config mode",
        );
        must(
            std::fs::hard_link(&pin, &outside),
            "hard-link pin outside repo",
        );
        let permissions = must(std::fs::symlink_metadata(&pin), "inspect pin").permissions();

        must(
            replace_promotion_pin(
                &root,
                Path::new(GENERATION_CONFIG),
                &pin,
                "old pin bytes\n",
                "new pin bytes\n",
                &permissions,
            ),
            "atomically replace pin",
        );

        assert_eq!(
            must(std::fs::read(&pin), "read replaced pin"),
            b"new pin bytes\n"
        );
        assert_eq!(
            must(std::fs::read(&outside), "read hard-link alias"),
            b"old pin bytes\n"
        );
        assert_eq!(
            must(std::fs::metadata(&pin), "inspect replaced pin mode")
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(outside);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_pin_replacement_refuses_permissions_changed_after_preflight() {
        use std::os::unix::fs::PermissionsExt as _;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-permission-drift-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let pin = root.join(GENERATION_CONFIG);
        must(
            std::fs::write(&pin, "old pin bytes\n"),
            "write generation config",
        );
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)),
            "set preflight generation config mode",
        );
        let preflight_permissions = must(
            std::fs::symlink_metadata(&pin),
            "inspect preflight generation config",
        )
        .permissions();
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o600)),
            "change generation config mode after preflight",
        );

        let error = must_fail(
            replace_promotion_pin(
                &root,
                Path::new(GENERATION_CONFIG),
                &pin,
                "old pin bytes\n",
                "new pin bytes\n",
                &preflight_permissions,
            ),
            "pin replacement must reject changed permissions",
        );
        assert!(
            error.contains("changed after promotion preflight"),
            "{error}"
        );
        assert_eq!(
            must(std::fs::read(&pin), "read unchanged generation config"),
            b"old pin bytes\n"
        );
        assert_eq!(
            must(std::fs::metadata(&pin), "inspect current generation config")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn uncommitted_recovery_cleanup_removes_only_the_captured_pin_inode() {
        use std::os::unix::fs::PermissionsExt as _;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-uncommitted-cleanup-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let pin = root.join(".github-gen/pin");
        must(
            std::fs::write(&pin, b"captured pin bytes\n"),
            "write captured pin",
        );
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)),
            "set captured pin mode",
        );
        let identity = PinFileIdentity {
            anchor: must(std::fs::File::open(&pin), "open captured pin identity"),
        };
        let relative = PathBuf::from(".github-gen/.velnor-pin-preimage-test");
        let recovery = root.join(&relative);
        must(
            std::fs::hard_link(&pin, &recovery),
            "link captured pin for recovery",
        );

        must(
            remove_uncommitted_pin_recovery_link(&root, &relative, &recovery, &identity),
            "remove verified captured recovery link",
        );
        assert!(std::fs::symlink_metadata(&recovery).is_err());

        must(
            std::fs::write(&recovery, b"captured pin bytes\n"),
            "write same-byte replacement recovery entry",
        );
        must(
            std::fs::set_permissions(&recovery, std::fs::Permissions::from_mode(0o640)),
            "set replacement recovery mode",
        );
        let error =
            match remove_uncommitted_pin_recovery_link(&root, &relative, &recovery, &identity) {
                Ok(()) => panic!("cleanup must preserve an entry with a different inode"),
                Err(error) => error,
            };
        assert!(error.contains("captured pin identity"), "{error}");
        assert_eq!(
            must(
                std::fs::read(&recovery),
                "read preserved recovery replacement"
            ),
            b"captured pin bytes\n"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_promotion_pin_keeps_its_file_identity_and_mtime() {
        use std::os::unix::fs::MetadataExt as _;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-noop-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let pin = root.join(GENERATION_CONFIG);
        must(
            std::fs::write(&pin, "same pin bytes\n"),
            "write generation config",
        );
        let permissions = must(std::fs::symlink_metadata(&pin), "inspect pin").permissions();
        let before = must(std::fs::metadata(&pin), "read pin metadata");

        must(
            replace_promotion_pin(
                &root,
                Path::new(GENERATION_CONFIG),
                &pin,
                "same pin bytes\n",
                "same pin bytes\n",
                &permissions,
            ),
            "keep unchanged pin",
        );

        let after = must(std::fs::metadata(&pin), "read unchanged pin metadata");
        assert_eq!(before.dev(), after.dev());
        assert_eq!(before.ino(), after.ino());
        assert_eq!(
            (before.mtime(), before.mtime_nsec()),
            (after.mtime(), after.mtime_nsec())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[expect(
        clippy::too_many_lines,
        reason = "one rollback fixture compares changed and no-op pin identity"
    )]
    fn assert_pin_snapshot_rollback_preserves_identity(changed: bool) {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-rollback-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let relative = PathBuf::from(GENERATION_CONFIG);
        let pin = root.join(&relative);
        must(
            std::fs::write(&pin, b"original pin bytes\n"),
            "write old pin",
        );
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)),
            "set old pin mode",
        );
        let outside = root.with_extension("pin-rollback-alias");
        if changed {
            must(
                std::fs::hard_link(&pin, &outside),
                "hard-link original pin outside repository",
            );
        }
        let before = must(std::fs::metadata(&pin), "inspect old pin");
        let mut snapshot = must(
            Snapshot::capture(&root, promotion_snapshot_paths(&BTreeSet::new(), changed)),
            "capture promotion rollback paths",
        );
        let backup = snapshot
            .entries
            .iter()
            .find_map(|(_, preimage)| match preimage {
                Some(SnapshotPreimage::HardLink { backup, .. }) => Some(backup.clone()),
                _ => None,
            });
        assert_eq!(
            backup.is_some(),
            changed,
            "promotion snapshot pin backup must match whether pin content changes"
        );
        if let Some(backup) = &backup {
            assert!(backup.starts_with(".github-gen"));
        }

        let new_bytes = if changed {
            "stamped pin bytes\n"
        } else {
            "original pin bytes\n"
        };
        must(
            replace_promotion_pin(
                &root,
                &relative,
                &pin,
                "original pin bytes\n",
                new_bytes,
                &before.permissions(),
            ),
            "apply pin stamp",
        );
        if let Some(backup) = &backup {
            let backup_metadata = must(
                std::fs::metadata(root.join(backup)),
                "inspect retained rollback hard link",
            );
            assert_eq!(before.dev(), backup_metadata.dev());
            assert_eq!(before.ino(), backup_metadata.ino());
        }

        must(snapshot.restore(&root), "rollback failed promotion");

        let after = must(std::fs::metadata(&pin), "inspect restored pin");
        assert_eq!(
            must(std::fs::read(&pin), "read restored pin"),
            b"original pin bytes\n"
        );
        assert_eq!(before.dev(), after.dev());
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.mode(), after.mode());
        assert_eq!(
            (before.mtime(), before.mtime_nsec()),
            (after.mtime(), after.mtime_nsec())
        );
        if changed {
            assert_eq!(
                must(std::fs::read(&outside), "read external pin alias"),
                b"original pin bytes\n"
            );
            let Some(backup) = backup else {
                panic!("changed pin snapshot must retain a recovery hard link");
            };
            assert!(
                std::fs::symlink_metadata(root.join(backup)).is_err(),
                "rollback recovery hard link should be consumed by rename"
            );
        } else {
            assert_eq!(before.ctime(), after.ctime());
            assert_eq!(before.nlink(), after.nlink());
            let entries = must(
                std::fs::read_dir(root.join(".github-gen")),
                "read pin directory",
            );
            assert!(
                entries.filter_map(Result::ok).all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".velnor-pin-preimage-")),
                "no-op pin rollback must not create a recovery link"
            );
        }
        let _ = std::fs::remove_dir_all(root);
        if changed {
            let _ = std::fs::remove_file(outside);
        }
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rollback_restores_changed_pin_inode_and_metadata() {
        assert_pin_snapshot_rollback_preserves_identity(true);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rollback_of_noop_pin_preserves_inode_and_metadata() {
        assert_pin_snapshot_rollback_preserves_identity(false);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rollback_cleans_pin_backup_when_stamp_never_replaced_pin() {
        use std::os::unix::fs::MetadataExt as _;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-rollback-unwritten-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let relative = PathBuf::from(GENERATION_CONFIG);
        let pin = root.join(&relative);
        must(std::fs::write(&pin, b"original pin bytes\n"), "write pin");
        let before = must(std::fs::metadata(&pin), "inspect pin before rollback");
        let mut snapshot = must(
            Snapshot::capture(&root, [relative]),
            "capture changed-pin preimage",
        );
        must(snapshot.restore(&root), "rollback before pin replacement");

        let after = must(std::fs::metadata(&pin), "inspect pin after rollback");
        assert_eq!(before.dev(), after.dev());
        assert_eq!(before.ino(), after.ino());
        let entries = must(
            std::fs::read_dir(root.join(".github-gen")),
            "read pin directory after rollback",
        );
        assert!(
            entries.filter_map(Result::ok).all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".velnor-pin-preimage-")),
            "rollback must remove the redundant pin recovery link"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rollback_accepts_missing_backup_when_pin_is_original_inode() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-rollback-missing-original-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let relative = PathBuf::from(GENERATION_CONFIG);
        let pin = root.join(&relative);
        must(std::fs::write(&pin, b"original pin bytes\n"), "write pin");
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)),
            "set pin mode",
        );
        let before = must(std::fs::metadata(&pin), "inspect original pin");
        let mut snapshot = must(Snapshot::capture(&root, [relative]), "capture pin preimage");
        let backup = snapshot
            .entries
            .iter()
            .find_map(|(_, preimage)| match preimage {
                Some(SnapshotPreimage::HardLink { backup, .. }) => Some(backup.clone()),
                _ => None,
            });
        let backup = match backup {
            Some(backup) => backup,
            None => panic!("pin recovery link was not captured"),
        };
        must(
            std::fs::remove_file(root.join(&backup)),
            "remove recovery link while original pin remains",
        );

        must(snapshot.restore(&root), "accept already-original pin");

        let after = must(std::fs::metadata(&pin), "inspect unchanged pin");
        assert_eq!(before.dev(), after.dev());
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.mode(), after.mode());
        assert_eq!(
            must(std::fs::read(&pin), "read unchanged pin"),
            b"original pin bytes\n"
        );
        assert!(!root.join(backup).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rollback_refuses_a_replaced_pin_recovery_link() {
        use std::os::unix::fs::PermissionsExt as _;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-recovery-replaced-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let relative = PathBuf::from(GENERATION_CONFIG);
        let pin = root.join(&relative);
        must(
            std::fs::write(&pin, b"original pin bytes\n"),
            "write old pin",
        );
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)),
            "set old pin mode",
        );
        let permissions = must(std::fs::metadata(&pin), "inspect old pin").permissions();
        let mut snapshot = must(
            Snapshot::capture(&root, [relative.clone()]),
            "capture pin preimage",
        );
        let backup = snapshot
            .entries
            .iter()
            .find_map(|(_, preimage)| match preimage {
                Some(SnapshotPreimage::HardLink { backup, .. }) => Some(backup.clone()),
                _ => None,
            });
        let backup = match backup {
            Some(backup) => backup,
            None => panic!("pin recovery link was not captured"),
        };
        must(
            replace_promotion_pin(
                &root,
                &relative,
                &pin,
                "original pin bytes\n",
                "stamped pin bytes\n",
                &permissions,
            ),
            "stamp pin before simulated failure",
        );
        let backup_path = root.join(&backup);
        must(
            std::fs::remove_file(&backup_path),
            "replace captured recovery link",
        );
        must(
            std::fs::write(&backup_path, b"original pin bytes\n"),
            "write replacement recovery file",
        );
        must(
            std::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o640)),
            "set replacement recovery mode",
        );

        let error = match snapshot.restore(&root) {
            Ok(()) => panic!("rollback must refuse a changed recovery link"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("no longer matches the captured preimage"),
            "{error}"
        );
        assert_eq!(
            must(std::fs::read(&pin), "read still-stamped pin"),
            b"stamped pin bytes\n"
        );
        assert_eq!(
            must(std::fs::read(&backup_path), "read preserved replacement"),
            b"original pin bytes\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_rollback_rejects_matching_pin_bytes_at_a_new_inode_when_backup_is_missing() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-recovery-missing-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let relative = PathBuf::from(GENERATION_CONFIG);
        let pin = root.join(&relative);
        must(
            std::fs::write(&pin, b"original pin bytes\n"),
            "write old pin",
        );
        must(
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)),
            "set old pin mode",
        );
        let permissions = must(std::fs::metadata(&pin), "inspect pin").permissions();
        let mut snapshot = must(
            Snapshot::capture(&root, [relative.clone()]),
            "capture pin preimage",
        );
        let backup = snapshot
            .entries
            .iter()
            .find_map(|(_, preimage)| match preimage {
                Some(SnapshotPreimage::HardLink { backup, .. }) => Some(backup.clone()),
                _ => None,
            });
        let backup = match backup {
            Some(backup) => backup,
            None => panic!("pin recovery link was not captured"),
        };
        must(
            replace_promotion_pin(
                &root,
                &relative,
                &pin,
                "original pin bytes\n",
                "stamped pin bytes\n",
                &permissions,
            ),
            "stamp pin before simulated failure",
        );
        must(
            std::fs::remove_file(root.join(&backup)),
            "remove missing recovery link",
        );
        must(
            replace_promotion_pin(
                &root,
                &relative,
                &pin,
                "stamped pin bytes\n",
                "original pin bytes\n",
                &permissions,
            ),
            "restore matching bytes at a replacement inode",
        );
        let replacement = must(std::fs::metadata(&pin), "inspect replacement pin");

        let error = match snapshot.restore(&root) {
            Ok(()) => panic!("rollback must reject a replacement pin inode"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("does not match its captured preimage"));
        let after = must(
            std::fs::metadata(&pin),
            "inspect replacement pin after rollback",
        );
        assert_eq!(replacement.dev(), after.dev());
        assert_eq!(replacement.ino(), after.ino());
        assert_eq!(
            must(std::fs::read(&pin), "read matching replacement pin"),
            b"original pin bytes\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn promotion_rollback_rejects_matching_pin_bytes_at_a_replacement_recovery_file() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-pin-recovery-replaced-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        let relative = PathBuf::from(GENERATION_CONFIG);
        let pin = root.join(&relative);
        must(
            std::fs::write(&pin, b"original pin bytes\n"),
            "write old pin",
        );
        let permissions = must(std::fs::metadata(&pin), "inspect pin").permissions();
        let mut snapshot = must(
            Snapshot::capture(&root, [relative.clone()]),
            "capture pin preimage",
        );
        let backup = snapshot
            .entries
            .iter()
            .find_map(|(_, preimage)| match preimage {
                Some(SnapshotPreimage::HardLink { backup, .. }) => Some(backup.clone()),
                _ => None,
            });
        let backup = match backup {
            Some(backup) => backup,
            None => panic!("pin recovery link was not captured"),
        };
        must(
            replace_promotion_pin(
                &root,
                &relative,
                &pin,
                "original pin bytes\n",
                "stamped pin bytes\n",
                &permissions,
            ),
            "stamp pin before simulated failure",
        );
        let backup_path = root.join(&backup);
        must(
            std::fs::remove_file(&backup_path),
            "replace captured recovery link",
        );
        must(
            std::fs::write(&backup_path, b"original pin bytes\n"),
            "write replacement recovery file",
        );
        must(
            std::fs::set_permissions(&backup_path, permissions),
            "set replacement recovery permissions",
        );

        let error = match snapshot.restore(&root) {
            Ok(()) => panic!("rollback must refuse a replacement recovery file"),
            Err(error) => error,
        };
        assert!(
            error.contains("no longer matches the captured preimage"),
            "{error}"
        );
        assert_eq!(
            must(std::fs::read(&pin), "read still-stamped pin"),
            b"stamped pin bytes\n"
        );
        assert_eq!(
            must(std::fs::read(&backup_path), "read preserved replacement"),
            b"original pin bytes\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn promotion_snapshot_restores_recorded_stale_output_and_mode() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-stale-{}-{nonce}",
            std::process::id()
        ));
        let output = PathBuf::from("state/cache.env");
        let output_path = root.join(&output);
        let output_parent = output_path.parent().unwrap_or(&root);
        must(
            std::fs::create_dir_all(output_parent),
            "create old output parent",
        );
        must(
            std::fs::write(&output_path, b"old generated source\n"),
            "write old output",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            must(
                std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(0o751)),
                "set old output mode",
            );
        }
        let files = BTreeMap::from([(output.clone(), String::from("old generated source\n"))]);
        let state_path = root.join(OWNERSHIP_STATE);
        let state_parent = state_path.parent().unwrap_or(&root);
        must(
            std::fs::create_dir_all(state_parent),
            "create ownership state parent",
        );
        must(
            std::fs::write(
                &state_path,
                ownership_state_content(
                    &files,
                    &BTreeMap::new(),
                    &crate::s2::GenerationInputs::parts(1, 2),
                ),
            ),
            "write ownership state",
        );

        let previous_outputs = BTreeSet::from([output.clone()]);
        assert!(previous_outputs.contains(&output));
        let state_before = must(std::fs::read(&state_path), "read prior ownership state");
        let mut snapshot = must(
            Snapshot::capture(&root, previous_outputs),
            "capture promotion preimages",
        );
        must(std::fs::remove_file(&output_path), "remove old output");
        must(
            std::fs::remove_file(&state_path),
            "remove old ownership state",
        );
        must(
            std::fs::remove_dir_all(output_parent),
            "remove old output parents",
        );
        must(
            snapshot.restore(&root),
            "restore stale output after failed promotion",
        );

        assert_eq!(
            must(std::fs::read(&output_path), "read restored output"),
            b"old generated source\n"
        );
        assert_eq!(
            must(std::fs::read(&state_path), "read restored ownership state"),
            state_before
        );
        assert!(must(
            std::fs::symlink_metadata(&output_path),
            "inspect restored output"
        )
        .file_type()
        .is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                must(std::fs::metadata(&output_path), "inspect restored mode")
                    .permissions()
                    .mode()
                    & 0o777,
                0o751
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn promotion_snapshot_preserves_preexisting_empty_parent() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-empty-parent-{}-{nonce}",
            std::process::id()
        ));
        must(
            std::fs::create_dir_all(root.join("state")),
            "create preexisting empty parent",
        );
        let relative = PathBuf::from("state/nested/cache.env");
        let mut snapshot = must(
            Snapshot::capture(&root, [relative.clone()]),
            "capture missing output under existing directory",
        );
        must(
            std::fs::create_dir_all(root.join("state/nested")),
            "create output subdirectory",
        );
        must(
            std::fs::write(root.join(&relative), "new generated output\n"),
            "write promoted output",
        );

        must(snapshot.restore(&root), "restore missing output");

        assert!(root.join("state").is_dir(), "preexisting parent was pruned");
        assert!(
            std::fs::symlink_metadata(root.join("state/nested")).is_err(),
            "directory created below preexisting parent survived rollback"
        );
        assert!(
            std::fs::symlink_metadata(root.join(&relative)).is_err(),
            "created output survived rollback"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn promotion_snapshot_prunes_parent_chain_created_after_capture() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-new-parents-{}-{nonce}",
            std::process::id()
        ));
        let relative = PathBuf::from("state/nested/cache.env");
        let mut snapshot = must(
            Snapshot::capture(&root, [relative.clone()]),
            "capture missing output and parents",
        );
        must(
            std::fs::create_dir_all(root.join("state/nested")),
            "create new output parents",
        );
        must(
            std::fs::write(root.join(&relative), "new generated output\n"),
            "write promoted output",
        );

        must(
            snapshot.restore(&root),
            "restore missing output and parents",
        );

        assert!(
            std::fs::symlink_metadata(root.join("state")).is_err(),
            "rollback retained directories created by promotion"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn promotion_snapshot_prunes_shared_created_ancestors_without_false_failure() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-shared-new-parents-{}-{nonce}",
            std::process::id()
        ));
        let first = PathBuf::from("state/a/cache.env");
        let second = PathBuf::from("state/b/cache.env");
        let mut snapshot = must(
            Snapshot::capture(&root, [first.clone(), second.clone()]),
            "capture missing outputs under shared parents",
        );
        must(
            std::fs::create_dir_all(root.join("state/a")),
            "create first output parents",
        );
        must(
            std::fs::create_dir_all(root.join("state/b")),
            "create second output parents",
        );
        must(
            std::fs::write(root.join(&first), "first generated output\n"),
            "write first generated output",
        );
        must(
            std::fs::write(root.join(&second), "second generated output\n"),
            "write second generated output",
        );

        must(
            snapshot.restore(&root),
            "restore missing outputs under shared parents",
        );

        assert!(
            std::fs::symlink_metadata(root.join("state")).is_err(),
            "rollback retained shared directory after removing both generated paths"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn promotion_snapshot_reports_unexpected_sibling_in_created_parent() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-unexpected-sibling-{}-{nonce}",
            std::process::id()
        ));
        let relative = PathBuf::from("state/nested/cache.env");
        let mut snapshot = must(
            Snapshot::capture(&root, [relative]),
            "capture missing output and parents",
        );
        let sibling = root.join("state/nested/unexpected.txt");
        must(
            std::fs::create_dir_all(sibling.parent().unwrap_or(&root)),
            "create output parent with unexpected sibling",
        );
        must(
            std::fs::write(&sibling, b"unexpected sibling\n"),
            "write unexpected sibling",
        );

        let error = match snapshot.restore(&root) {
            Ok(()) => panic!("rollback must report an unpruned created directory"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("remove promotion-created directory"));
        assert!(root.join("state/nested").is_dir());
        assert_eq!(
            must(std::fs::read(&sibling), "read preserved unexpected sibling"),
            b"unexpected sibling\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_snapshot_refuses_rollback_through_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-symlink-rollback-{}-{nonce}",
            std::process::id()
        ));
        let output = PathBuf::from("state/cache.env");
        must(
            std::fs::create_dir_all(root.join("state")),
            "create output parent",
        );
        must(
            std::fs::create_dir_all(root.join(".git")),
            "create Git metadata directory",
        );
        must(
            std::fs::write(root.join(&output), b"captured generated output\n"),
            "write captured output",
        );
        must(
            std::fs::write(root.join(".git/cache.env"), b"protected metadata\n"),
            "write Git metadata sentinel",
        );
        let mut snapshot = must(
            Snapshot::capture(&root, [output.clone()]),
            "capture output preimage",
        );
        must(
            std::fs::remove_file(root.join(&output)),
            "remove captured output",
        );
        must(
            std::fs::remove_dir(root.join("state")),
            "remove output parent",
        );
        must(
            symlink(".git", root.join("state")),
            "replace output parent with symlink",
        );

        let error = must_fail(
            snapshot.restore(&root),
            "rollback must reject a symlinked output ancestor",
        );
        assert!(error.contains("symlinked ancestor"), "{error}");
        assert_eq!(
            must(
                std::fs::read(root.join(".git/cache.env")),
                "read Git metadata sentinel"
            ),
            b"protected metadata\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn promotion_snapshot_rejects_special_preimage_without_reading() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "velnor-promote-special-{pid}-{nonce}",
            pid = std::process::id()
        ));
        let relative = PathBuf::from(".github/workflows/forged-output.yml");
        let path = root.join(&relative);
        must(
            std::fs::create_dir_all(path.parent().unwrap_or(&root)),
            "create special preimage parent",
        );
        let status = must(
            std::process::Command::new("mkfifo").arg(&path).status(),
            "mkfifo must be available for the Unix safety test",
        );
        assert!(status.success(), "mkfifo failed with {status}");
        let error = must_fail(
            Snapshot::capture(&root, [relative]),
            "special promotion preimage must fail closed",
        );
        assert!(error.contains("non-regular promotion preimage"), "{error}");
        let _ = std::fs::remove_dir_all(root);
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

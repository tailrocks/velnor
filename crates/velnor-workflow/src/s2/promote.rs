//! D19 pin promotion through the captured schema-2 repository root.
//!
//! Promotion proves that this binary's source closure matches the revision it
//! stamps, renders the target with the schema-2 provider pipeline, verifies
//! deterministic output, then commits only the pin and generator-owned files.
//! Any failure before the commit restores the recorded file preimages.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::provider::{self, ProviderSet};
use super::safe_fs::{self, SafeRoot};
use super::{FilePreimage, GeneratorError};

const BUILD_FEATURES: &str = env!("VELNOR_WORKFLOW_FEATURES");
const BUILD_PROFILE: &str = env!("VELNOR_WORKFLOW_PROFILE");

/// Parsed promote intent before dispatch captures the requested roots.
pub(crate) struct ParsedPromote {
    pub(crate) request: PromoteRequest,
    pub(crate) repository: PathBuf,
    pub(crate) generator_repository: Option<PathBuf>,
}

/// Promotion options after dispatch has captured both roots.
pub(crate) struct PromoteRequest {
    revision: String,
    providers: Option<ProviderSet>,
    default_branch: Option<String>,
    message: Option<String>,
    dry_run: bool,
}

/// What one promotion did, for the CLI report and tests.
pub(crate) struct PromoteReport {
    old_pin: String,
    new_pin: String,
    closure: String,
    changed: Vec<PathBuf>,
    commit: Option<String>,
    noop: bool,
}

struct PromoteFailure {
    error: GeneratorError,
    commit_landed: bool,
}

impl From<GeneratorError> for PromoteFailure {
    fn from(error: GeneratorError) -> Self {
        Self {
            error,
            commit_landed: false,
        }
    }
}

/// Parse `promote` options once, before dispatch opens the selected roots.
pub(crate) fn parse_request(arguments: &[OsString]) -> Result<ParsedPromote, GeneratorError> {
    let mut rest = Vec::with_capacity(arguments.len());
    let mut dry_run = false;
    for argument in arguments {
        if argument.as_os_str() == OsStr::new("--dry-run") {
            if dry_run {
                return Err(GeneratorError::usage(
                    "duplicate option: --dry-run".to_owned(),
                ));
            }
            dry_run = true;
        } else {
            rest.push(argument.clone());
        }
    }

    let mut options = BTreeMap::new();
    let mut provider_values = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        let raw = rest[index]
            .to_str()
            .ok_or_else(|| GeneratorError::usage("promote argument must be valid UTF-8"))?;
        let (name, inline) = raw
            .strip_prefix("--")
            .and_then(|value| {
                value
                    .split_once('=')
                    .map_or(Some((value, None)), |(name, value)| {
                        Some((name, Some(value)))
                    })
            })
            .ok_or_else(|| GeneratorError::usage(format!("unsupported promote argument: {raw}")))?;
        if !matches!(
            name,
            "rev" | "repo" | "generator-repo" | "default-branch" | "providers" | "message"
        ) {
            return Err(GeneratorError::usage(format!(
                "unsupported promote argument: --{name}"
            )));
        }
        let value = match inline {
            Some(value) if !value.is_empty() => value.to_owned(),
            Some(_) => {
                return Err(GeneratorError::usage(format!("--{name} needs a value")));
            }
            None => {
                index += 1;
                rest.get(index)
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| GeneratorError::usage(format!("--{name} needs a value")))?
                    .to_owned()
            }
        };
        if name == "providers" {
            provider_values.extend(value.split(',').map(str::to_owned));
        } else if options.insert(name.to_owned(), value).is_some() {
            return Err(GeneratorError::usage(format!("duplicate option: --{name}")));
        }
        index += 1;
    }

    let revision = options
        .remove("rev")
        .ok_or_else(|| GeneratorError::usage("promote requires --rev SHA or HEAD".to_owned()))?;
    let providers = if provider_values.is_empty() {
        None
    } else {
        let providers = provider::parse_provider_set(&provider_values, "--providers")?;
        provider::require_non_empty(&providers, "--providers")?;
        Some(providers)
    };
    Ok(ParsedPromote {
        request: PromoteRequest {
            revision,
            providers,
            default_branch: options.remove("default-branch"),
            message: options.remove("message"),
            dry_run,
        },
        repository: options
            .remove("repo")
            .map_or_else(|| PathBuf::from("."), PathBuf::from),
        generator_repository: options.remove("generator-repo").map(PathBuf::from),
    })
}

/// Promote the schema-2 tree held by `repository`, using only its captured
/// root for reads, writes, and Git commands.
pub(crate) fn run_promote_with_safe_roots(
    request: &PromoteRequest,
    repository: SafeRoot,
    generator_repository: Option<SafeRoot>,
) -> Result<PromoteReport, GeneratorError> {
    repository.validate_root_binding()?;
    super::config::require_with_safe_root(&repository)?;
    require_repository_root(&repository)?;
    let generator_repository = match generator_repository {
        Some(root) => root,
        None => repository.duplicate()?,
    };
    generator_repository.validate_root_binding()?;

    let revision = if request.revision == "HEAD" {
        git(&generator_repository, &["rev-parse", "HEAD"])?
    } else {
        request.revision.clone()
    };
    if !super::is_full_revision(&revision) {
        return Err(GeneratorError::usage(format!(
            "promote --rev must be a full 40-character commit SHA or HEAD, got {:?}",
            request.revision
        )));
    }

    // The binding runs before the first target-tree mutation: only a binary
    // whose stamped build identity matches the pin may stamp that pin.
    let closure = verify_render_stamp_binding(&generator_repository, &revision)?;
    require_promotion_clean(&repository)?;

    let pin_path = PathBuf::from(super::policy::GENERATION_CONFIG);
    let pin_preimage = repository.capture_file_preimage(&pin_path)?;
    let pin_content = file_preimage_text(&pin_preimage, &pin_path)?;
    let (stamped, old_pin) = stamp_pin(&pin_content, &revision)?;
    let default_branch = match &request.default_branch {
        Some(branch) => branch.clone(),
        None => super::resolve_default_branch_with_safe_root(&repository)?,
    };
    let mut snapshot = PromotionSnapshot::new(&pin_path, pin_preimage, stamped.as_bytes().to_vec());

    if stamped != pin_content
        && let Err(error) = safe_fs::write_reviewed_file_observed(
            &repository,
            &pin_path,
            &stamped,
            snapshot
                .preimage(&pin_path)
                .ok_or_else(|| GeneratorError::usage("promotion snapshot lost the pin preimage"))?,
            |_| Ok(()),
            |_| Ok(()),
        )
    {
        return Err(rollback_failure(&repository, &mut snapshot, error));
    }

    let outcome = promote_rendered_tree(
        request,
        &repository,
        &default_branch,
        &mut snapshot,
        &old_pin,
        &revision,
        &closure,
    );
    match outcome {
        Ok(report) if request.dry_run => {
            snapshot.restore(&repository)?;
            Ok(report)
        }
        Ok(report) => Ok(report),
        Err(failure) if failure.commit_landed => Err(failure.error),
        Err(failure) => Err(rollback_failure(&repository, &mut snapshot, failure.error)),
    }
}

fn promote_rendered_tree(
    request: &PromoteRequest,
    repository: &SafeRoot,
    default_branch: &str,
    snapshot: &mut PromotionSnapshot,
    old_pin: &str,
    revision: &str,
    closure: &str,
) -> Result<PromoteReport, PromoteFailure> {
    let rendered = PromotedRender::render(repository, request, default_branch)?;
    let plan = super::plan_generated_write_with_root(
        repository.command_directory(),
        &rendered.files,
        &rendered.inputs,
        false,
        Some(&rendered.source_identity),
        Some(repository),
    )?;
    let expected_state = super::ownership_state_content(&rendered.files, &rendered.inputs);
    snapshot.extend_from_plan(&plan, &rendered.files, &expected_state);
    verify_deterministic_render(repository, request, default_branch, &rendered)?;

    let owned: BTreeSet<PathBuf> = rendered
        .files
        .keys()
        .cloned()
        .chain(plan.files.iter().map(|file| file.path.clone()))
        .chain([
            PathBuf::from(super::policy::GENERATION_CONFIG),
            PathBuf::from(super::OWNERSHIP_STATE),
        ])
        .collect();
    let mut changed = planned_changes(&plan, &owned, snapshot);
    verify_only_promoted_changes(repository, &owned, &changed)?;
    changed.sort();
    changed.dedup();
    if !request.dry_run {
        super::apply_generated_write_plan_with_root(
            repository.command_directory(),
            &rendered.files,
            &rendered.inputs,
            false,
            false,
            true,
            &plan,
            Some(repository),
        )?;
        verify_written_tree(repository, &rendered, &expected_state)?;
        verify_only_promoted_changes(repository, &owned, &changed)?;
    }

    if changed.is_empty() {
        return Ok(PromoteReport {
            old_pin: old_pin.to_owned(),
            new_pin: revision.to_owned(),
            closure: closure.to_owned(),
            changed: Vec::new(),
            commit: None,
            noop: true,
        });
    }
    if request.dry_run {
        return Ok(PromoteReport {
            old_pin: old_pin.to_owned(),
            new_pin: revision.to_owned(),
            closure: closure.to_owned(),
            changed,
            commit: None,
            noop: false,
        });
    }

    stage(repository, &changed)?;
    let old_head = git(repository, &["rev-parse", "HEAD"])?;
    let message = promotion_message(request, old_pin, revision);
    if let Err(error) = git(repository, &["commit", "-s", "-m", &message]) {
        let current_head = git(repository, &["rev-parse", "HEAD"]);
        if let Ok(current_head) = current_head
            && current_head != old_head
        {
            return Err(PromoteFailure {
                error: GeneratorError::usage(format!(
                    "git commit created {current_head} but returned an error: {error}; the promoted commit remains in place"
                )),
                commit_landed: true,
            });
        }
        return Err(error.into());
    }
    let commit = git(repository, &["rev-parse", "HEAD"]).map_err(|error| PromoteFailure {
        error: GeneratorError::usage(format!(
            "the promotion commit landed but its ID could not be read: {error}"
        )),
        commit_landed: true,
    })?;
    Ok(PromoteReport {
        old_pin: old_pin.to_owned(),
        new_pin: revision.to_owned(),
        closure: closure.to_owned(),
        changed,
        commit: Some(commit),
        noop: false,
    })
}

/// A schema-2 render always uses the provider pipeline and the already
/// captured target root. The source identity travels into the writer plan.
struct PromotedRender {
    files: BTreeMap<PathBuf, String>,
    inputs: super::GenerationInputs,
    source_identity: super::safe_fs::SafeRootIdentity,
}

impl PromotedRender {
    fn render(
        repository: &SafeRoot,
        request: &PromoteRequest,
        default_branch: &str,
    ) -> Result<Self, GeneratorError> {
        repository.validate_root_binding()?;
        let tree = super::render_tree_with_safe_root(
            Arc::new(repository.duplicate()?),
            request.providers.clone(),
            default_branch,
        )?;
        repository.validate_root_binding()?;
        let identity = repository.identity()?;
        if tree.source_identity != identity {
            return Err(GeneratorError::usage(
                "promotion render used a repository other than the captured root".to_owned(),
            ));
        }
        Ok(Self {
            files: tree.files,
            inputs: tree.inputs,
            source_identity: tree.source_identity,
        })
    }
}

fn verify_render_stamp_binding(
    generator_repository: &SafeRoot,
    revision: &str,
) -> Result<String, GeneratorError> {
    if !super::closure::is_full_closure(super::SOURCE_CLOSURE) {
        return Err(GeneratorError::usage(
            "promotion requires a binary that proves its own source: this velnor-workflow stamps no closure (built without git); rebuild it from a git checkout"
                .to_owned(),
        ));
    }
    generator_repository.validate_root_binding()?;
    let pin_closure = super::closure::closure_of_tree_with_safe_root(
        generator_repository,
        revision,
        BUILD_FEATURES,
        BUILD_PROFILE,
    )
    .map_err(|error| {
        GeneratorError::usage(format!(
            "{error}; promote needs the stamped pin's generator history (pass --generator-repo at a checkout containing it)"
        ))
    })?;
    generator_repository.validate_root_binding()?;
    if pin_closure != super::SOURCE_CLOSURE {
        return Err(GeneratorError::usage(format!(
            "refusing to stamp {revision}: this binary renders with source closure {} but the pin names {pin_closure}; promotion renders with exactly the generator it stamps",
            super::SOURCE_CLOSURE
        )));
    }
    Ok(pin_closure)
}

fn verify_deterministic_render(
    repository: &SafeRoot,
    request: &PromoteRequest,
    default_branch: &str,
    rendered: &PromotedRender,
) -> Result<(), GeneratorError> {
    let again = PromotedRender::render(repository, request, default_branch)?;
    if again.files != rendered.files {
        let divergent: Vec<String> = rendered
            .files
            .keys()
            .chain(again.files.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|path| rendered.files.get(*path) != again.files.get(*path))
            .map(|path| path.display().to_string())
            .collect();
        return Err(GeneratorError::usage(format!(
            "the stamped tree does not render deterministically; refusing to promote: {}",
            divergent.join(", ")
        )));
    }
    Ok(())
}

fn verify_written_tree(
    repository: &SafeRoot,
    rendered: &PromotedRender,
    expected_state: &str,
) -> Result<(), GeneratorError> {
    repository.validate_root_binding()?;
    for (relative, wanted) in &rendered.files {
        let disk = repository.read_file(relative)?;
        if disk != wanted.as_bytes() {
            return Err(GeneratorError::usage(format!(
                "the written tree differs from the render at {}; refusing to promote",
                relative.display()
            )));
        }
    }
    let state_path = Path::new(super::OWNERSHIP_STATE);
    if repository.read_file(state_path)? != expected_state.as_bytes() {
        return Err(GeneratorError::usage(
            "the regenerated ownership metadata does not match the render; refusing to promote"
                .to_owned(),
        ));
    }
    repository.validate_root_binding()?;
    Ok(())
}

/// Replace only `[generator].revision`, preserving all other bytes.
pub(crate) fn stamp_pin(
    content: &str,
    new_revision: &str,
) -> Result<(String, String), GeneratorError> {
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
            .filter(|sha| super::is_full_revision(sha));
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
        *line = line.replacen(pinned, new_revision, 1);
        stamped = true;
    }
    match old {
        Some(old) => Ok((lines.join("\n"), old)),
        None => Err(GeneratorError::usage(format!(
            "no [generator] revision to stamp in {}",
            super::policy::GENERATION_CONFIG
        ))),
    }
}

fn file_preimage_text(preimage: &FilePreimage, relative: &Path) -> Result<String, GeneratorError> {
    let bytes = preimage.bytes().ok_or_else(|| {
        GeneratorError::usage(format!(
            "required promotion input is missing: {}",
            relative.display()
        ))
    })?;
    String::from_utf8(bytes.to_vec()).map_err(|error| {
        GeneratorError::usage(format!(
            "promotion input is not UTF-8: {}: {error}",
            relative.display()
        ))
    })
}

struct PromotionSnapshot {
    entries: BTreeMap<PathBuf, SnapshotEntry>,
}

struct SnapshotEntry {
    before: FilePreimage,
    after: Option<Vec<u8>>,
}

impl PromotionSnapshot {
    fn new(path: &Path, before: FilePreimage, after: Vec<u8>) -> Self {
        Self {
            entries: BTreeMap::from([(
                path.to_path_buf(),
                SnapshotEntry {
                    before,
                    after: Some(after),
                },
            )]),
        }
    }

    fn preimage(&self, path: &Path) -> Option<&FilePreimage> {
        self.entries.get(path).map(|entry| &entry.before)
    }

    fn extend_from_plan(
        &mut self,
        plan: &super::GeneratedWritePlan,
        files: &BTreeMap<PathBuf, String>,
        expected_state: &str,
    ) {
        for planned in &plan.files {
            let after = if planned.path == Path::new(super::OWNERSHIP_STATE) {
                Some(expected_state.as_bytes().to_vec())
            } else {
                files
                    .get(&planned.path)
                    .map(|content| content.as_bytes().to_vec())
            };
            self.entries
                .entry(planned.path.clone())
                .or_insert(SnapshotEntry {
                    before: planned.preimage.clone(),
                    after,
                });
        }
    }

    fn restore(&self, repository: &SafeRoot) -> Result<(), GeneratorError> {
        let mut failures = Vec::new();
        for (relative, entry) in &self.entries {
            let current = match repository.capture_file_preimage(relative) {
                Ok(current) => current,
                Err(error) => {
                    failures.push(format!("inspect {}: {error}", relative.display()));
                    continue;
                }
            };
            if current.bytes() == entry.before.bytes() {
                continue;
            }
            if current.bytes() != entry.after.as_deref() {
                failures.push(format!(
                    "{} changed during promotion; refusing to overwrite the new bytes",
                    relative.display()
                ));
                continue;
            }
            let restored = match entry.before.bytes() {
                None => safe_fs::delete_reviewed_file(repository, relative, &current),
                Some(bytes) => match String::from_utf8(bytes.to_vec()) {
                    Ok(content) => safe_fs::write_reviewed_file_observed(
                        repository,
                        relative,
                        &content,
                        &current,
                        |_| Ok(()),
                        |_| Ok(()),
                    ),
                    Err(error) => Err(GeneratorError::usage(format!(
                        "cannot restore non-UTF-8 promoted file {}: {error}",
                        relative.display()
                    ))),
                },
            };
            if let Err(error) = restored {
                failures.push(format!("restore {}: {error}", relative.display()));
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

fn rollback_failure(
    repository: &SafeRoot,
    snapshot: &mut PromotionSnapshot,
    original: GeneratorError,
) -> GeneratorError {
    let restore = snapshot.restore(repository);
    let reset = reset_promoted_paths(repository, snapshot);
    match (restore, reset) {
        (Ok(()), Ok(_)) => original,
        (restore, reset) => {
            let mut details = Vec::new();
            if let Err(error) = restore {
                details.push(error.to_string());
            }
            if let Err(error) = reset {
                details.push(format!("unstage promoted paths: {error}"));
            }
            GeneratorError::usage(format!("{original}; {}", details.join("; ")))
        }
    }
}

fn reset_promoted_paths(
    repository: &SafeRoot,
    snapshot: &PromotionSnapshot,
) -> Result<String, GeneratorError> {
    let mut arguments = vec![
        "reset".to_owned(),
        "--quiet".to_owned(),
        "HEAD".to_owned(),
        "--".to_owned(),
    ];
    for path in snapshot.entries.keys() {
        let path = path.to_str().ok_or_else(|| {
            GeneratorError::usage("promoted paths must be valid UTF-8".to_owned())
        })?;
        arguments.push(format!(":(literal){path}"));
    }
    let references = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    git(repository, &references)
}

fn planned_changes(
    plan: &super::GeneratedWritePlan,
    owned: &BTreeSet<PathBuf>,
    snapshot: &PromotionSnapshot,
) -> Vec<PathBuf> {
    let mut changed = plan
        .differences()
        .into_iter()
        .filter(|path| owned.contains(path))
        .collect::<Vec<_>>();
    if snapshot
        .entries
        .get(Path::new(super::policy::GENERATION_CONFIG))
        .is_some_and(|entry| entry.before.bytes() != entry.after.as_deref())
    {
        changed.push(PathBuf::from(super::policy::GENERATION_CONFIG));
    }
    changed.sort();
    changed.dedup();
    changed
}

fn verify_only_promoted_changes(
    repository: &SafeRoot,
    owned: &BTreeSet<PathBuf>,
    expected: &[PathBuf],
) -> Result<(), GeneratorError> {
    let output = git_output(
        repository,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    let changes = parse_porcelain_z(&output.stdout)?;
    let expected = expected.iter().cloned().collect::<BTreeSet<_>>();
    for change in changes {
        for path in std::iter::once(change.path).chain(change.second_path) {
            if change.status == *b"??" && !owned.contains(&path) {
                continue;
            }
            if !owned.contains(&path) {
                return Err(GeneratorError::usage(format!(
                    "the tree changed outside the promoted surface during promotion ({}); refusing to commit",
                    path.display()
                )));
            }
            if !expected.contains(&path) {
                return Err(GeneratorError::usage(format!(
                    "the tree changed inside the promoted surface outside the planned write set ({}); refusing to commit",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

struct PorcelainChange {
    status: [u8; 2],
    path: PathBuf,
    second_path: Option<PathBuf>,
}

fn parse_porcelain_z(bytes: &[u8]) -> Result<Vec<PorcelainChange>, GeneratorError> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let end = bytes[index..]
            .iter()
            .position(|byte| *byte == 0)
            .map(|offset| index + offset)
            .ok_or_else(|| GeneratorError::usage("git status output is not NUL terminated"))?;
        let record = &bytes[index..end];
        if record.len() < 4 || record[2] != b' ' {
            return Err(GeneratorError::usage(
                "git status returned malformed porcelain output".to_owned(),
            ));
        }
        let status = [record[0], record[1]];
        let path = PathBuf::from(OsString::from_vec(record[3..].to_vec()));
        index = end + 1;
        let second_path = if status.contains(&b'R') || status.contains(&b'C') {
            let end = bytes[index..]
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| index + offset)
                .ok_or_else(|| GeneratorError::usage("git status omitted a rename source path"))?;
            let source = PathBuf::from(OsString::from_vec(bytes[index..end].to_vec()));
            index = end + 1;
            Some(source)
        } else {
            None
        };
        result.push(PorcelainChange {
            status,
            path,
            second_path,
        });
    }
    Ok(result)
}

fn require_promotion_clean(repository: &SafeRoot) -> Result<(), GeneratorError> {
    let output = git_output(
        repository,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    let dirty = parse_porcelain_z(&output.stdout)?
        .into_iter()
        .filter(|change| change.status != *b"??")
        .flat_map(|change| {
            std::iter::once(change.path)
                .chain(change.second_path)
                .map(|path| path.display().to_string())
        })
        .collect::<Vec<_>>();
    if dirty.is_empty() {
        Ok(())
    } else {
        Err(GeneratorError::usage(format!(
            "promotion needs a clean tree; commit or stash first: {}",
            dirty.join(", ")
        )))
    }
}

fn require_repository_root(repository: &SafeRoot) -> Result<(), GeneratorError> {
    let top_level = git(repository, &["rev-parse", "--show-toplevel"])?;
    if Path::new(&top_level) == repository.command_directory() {
        Ok(())
    } else {
        Err(GeneratorError::usage(format!(
            "promote runs at the repository root (root is {}, given {})",
            top_level,
            repository.command_directory().display()
        )))
    }
}

fn stage(repository: &SafeRoot, changed: &[PathBuf]) -> Result<(), GeneratorError> {
    if changed.is_empty() {
        return Err(GeneratorError::usage(
            "nothing staged for the promotion commit; the render left no tracked change".to_owned(),
        ));
    }
    let mut arguments = vec![
        "add".to_owned(),
        "--force".to_owned(),
        "-A".to_owned(),
        "--".to_owned(),
    ];
    for path in changed {
        let path = path.to_str().ok_or_else(|| {
            GeneratorError::usage("promoted paths must be valid UTF-8".to_owned())
        })?;
        arguments.push(format!(":(literal){path}"));
    }
    let references = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    git(repository, &references)?;
    let staged = git_output(
        repository,
        &["diff", "--cached", "--name-only", "-z", "HEAD"],
    )?;
    let staged_paths = staged
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
        .collect::<BTreeSet<_>>();
    let expected = changed.iter().cloned().collect::<BTreeSet<_>>();
    if staged_paths != expected {
        return Err(GeneratorError::usage(format!(
            "the index differs from the promoted path set; refusing to commit (expected {}; staged {})",
            display_paths(&expected),
            display_paths(&staged_paths)
        )));
    }
    Ok(())
}

fn display_paths(paths: &BTreeSet<PathBuf>) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn promotion_message(request: &PromoteRequest, old_pin: &str, revision: &str) -> String {
    if let Some(message) = &request.message {
        return message.clone();
    }
    let short = &revision[..8];
    if old_pin == revision {
        format!("chore(ci): regenerate tree with velnor-workflow {short}")
    } else {
        format!("chore(ci): bump D19 pin to {short}")
    }
}

fn git(repository: &SafeRoot, arguments: &[&str]) -> Result<String, GeneratorError> {
    let output = git_output(repository, arguments)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn git_output(
    repository: &SafeRoot,
    arguments: &[&str],
) -> Result<std::process::Output, GeneratorError> {
    repository.validate_root_binding()?;
    let output =
        safe_fs::pinned_command::output(repository, "git", arguments).map_err(|error| {
            GeneratorError::usage(format!("run git {}: {error}", arguments.join(" ")))
        })?;
    repository.validate_root_binding()?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output)
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
    use super::*;
    use std::path::PathBuf;

    #[expect(
        clippy::panic,
        reason = "promotion test setup failures should name their cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn stamp_changes_only_generator_revision_bytes() {
        let old = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let new = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let content = format!(
            "schema = 2\n\n[generator]\nrepository = \"example/consumer\"\n# pin\nrevision = \"{old}\"  # comment\n\n[workflow]\nrevision = \"{old}\"\n"
        );
        let (stamped, previous) = must(stamp_pin(&content, new), "stamp a well-formed pin");
        assert_eq!(previous, old);
        assert_eq!(stamped, content.replacen(old, new, 1));
    }

    #[test]
    fn promote_options_use_only_the_schema2_provider_vocabulary() {
        let parsed = must(
            parse_request(&args(&[
                "--rev",
                "HEAD",
                "--providers",
                "github-hosted,velnor",
                "--providers=github-self-hosted",
            ])),
            "parse the provider override",
        );
        assert_eq!(parsed.request.revision, "HEAD");
        assert_eq!(
            parsed.request.providers,
            Some(ProviderSet::from([
                super::super::provider::ProviderId::GithubHosted,
                super::super::provider::ProviderId::GithubSelfHosted,
                super::super::provider::ProviderId::Velnor,
            ]))
        );
        let error = match parse_request(&args(&["--rev", "HEAD", "--runners", "both"])) {
            Ok(_) => panic!("schema-1 runner aliases are not part of promote"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unsupported promote argument"));
    }

    #[test]
    fn promote_renders_a_trust_bearing_schema2_unit() {
        let root_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures-s2/promote-trust");
        let root = must(SafeRoot::open(&root_path), "open trust fixture");
        let tree = must(
            super::super::render_tree_with_safe_root(
                Arc::new(must(root.duplicate(), "duplicate trust fixture root")),
                None,
                "main",
            ),
            "render schema-2 trust fixture",
        );
        assert!(
            tree.config.units.iter().any(|unit| {
                unit.id == "docs" && unit.trust == super::super::provider::TrustReq::TrustedOnly
            }),
            "the rendered config retains the trusted-only unit"
        );
        assert!(
            !tree.files.is_empty(),
            "the trust-bearing schema-2 tree renders workflow files"
        );
    }
}

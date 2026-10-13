//! Slice C: one auditable input/dependency model for affected selection,
//! artifact identity, successful-result reuse, and the planner-anchored aggregate.
//!
//! Selection, artifact identity, and result validity derive from one model so
//! an auditor can replay every decision: which inputs a unit's verification
//! reads, which dependency edges pull other units in, which digest names the
//! produced artifact, and which evidence lets a later run reuse that artifact
//! instead of re-executing. Nothing here names a repository: units arrive as
//! plain `(id, watch, depends_on)` views the generator and the runtime project
//! from their own config types, and every decision carries its reasons.
//!
//! # Contract
//!
//! * [`select_affected`] maps a `git diff --name-status` change list onto the
//!   watched units. Renames match both sides, deletes still select their
//!   owner, an unmatched path falls back to the full set, and dependency
//!   edges are followed across unit kinds.
//! * [`canonical_fingerprint`] digests everything that can change a unit's
//!   verdict: source blobs, config, the effective recipe (generator revision
//!   plus check implementation plus lane commands), reviewed action pins and
//!   unit tool pins, and the transitive dependency fingerprints.
//! * [`validate_reuse`] reuses a prior successful result only when the
//!   producing evidence proves the same fingerprint, the same recipe, the
//!   complete expected check set, a passing producing aggregate, compatible
//!   trust, and — for live-state checks — explicit freshness. Artifact
//!   presence alone never reuses: [`Evidence`] has no name field to match.
//! * [`aggregate`] scores reported results against the planner's
//!   [`ExpectedWork`]. An explicit planned no-work passes; missing results,
//!   unexpected skips, failed prerequisites, cancelled required work, and
//!   incomplete matrices fail. Every item gets a [`WorkExplanation`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::GeneratorError;

/// Slice-C model version. Bump when the fingerprint canonical form, the reuse
/// rules, or the aggregate verdicts change; digests minted under different
/// versions never compare equal because the version is part of every hashed
/// form and every [`current_check_impl`] identity.
pub(crate) const REUSE_VERSION: u8 = 1;

/// The stable aggregate check every repository ruleset gates on. The aggregate
/// renderers and the ruleset default read this constant instead of spelling
/// the literal, so the name cannot drift between surfaces.
pub(crate) const REQUIRED_CHECK: &str = "ci-required";

/// Path prefixes that always select the full set: workflow and CI contract
/// inputs whose change can redirect any unit.
pub(crate) const FULL_SELECTION_PREFIXES: &[&str] = &[".github/"];

/// The stable check name for one expected work item: a pure function of the
/// unit id, the lane, and the matrix entry, independent of run ids and
/// selections. Evidence check sets and aggregate explanations spell work in
/// exactly this form so sets compare across runs.
#[must_use]
pub(crate) fn check_name_for_work(unit_id: &str, lane: &str, matrix_entry: Option<&str>) -> String {
    match matrix_entry {
        Some(entry) if !entry.is_empty() => format!("ci/{unit_id}/{lane}[{entry}]"),
        _ => format!("ci/{unit_id}/{lane}"),
    }
}

/// The expected check set for one expected unit: one [`check_name_for_work`]
/// per lane and matrix entry. An empty matrix is one unmatrixed entry.
#[must_use]
pub(crate) fn check_names_for_unit(unit_id: &str, unit: &ExpectedUnit) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for lane in &unit.lanes {
        if unit.matrix.is_empty() {
            names.insert(check_name_for_work(unit_id, lane, None));
        } else {
            for entry in &unit.matrix {
                names.insert(check_name_for_work(unit_id, lane, Some(entry)));
            }
        }
    }
    names
}

/// One changed path from `git diff --name-status -M`: `path` is the current
/// path (the rename target), `previous` is the rename source, and `status`
/// is the entry kind. Renames carry both sides so selection matches the old
/// owner's globs (the file left) and the new owner's globs (the file arrived);
/// deletes keep their path so the owning unit is selected even though the
/// file is gone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChangedPath {
    pub(crate) path: String,
    pub(crate) previous: Option<String>,
    pub(crate) status: ChangeKind,
}

/// The entry kinds [`select_affected`] understands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl ChangeKind {
    /// The stable verb explanations render for a matched path.
    fn verb(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
        }
    }
}

/// One matchable path with the kind selection reports for it: a rename
/// contributes its target as renamed and its source as deleted, since the
/// file arrived at one and left the other.
struct EffectiveMatch<'a> {
    path: &'a str,
    kind: ChangeKind,
}

/// The matches selection runs: the current path of every change plus the
/// rename source of every rename. A rename is one entry that invalidates two
/// owners; expanding it here keeps the matcher (and its audit trail) to a
/// single path list.
fn effective_matches(changes: &[ChangedPath]) -> Vec<EffectiveMatch<'_>> {
    let mut matches = Vec::new();
    for change in changes {
        matches.push(EffectiveMatch {
            path: &change.path,
            kind: change.status,
        });
        if let Some(previous) = &change.previous {
            matches.push(EffectiveMatch {
                path: previous,
                kind: ChangeKind::Deleted,
            });
        }
    }
    matches
}

/// Parse one `git diff --name-status` line (without `-z`). Copies report
/// their new path as added; type changes report as modified. Anything else —
/// unmerged entries, unknown statuses, malformed lines — parses to [`None`]
/// so the caller falls back to the full set instead of guessing.
#[must_use]
pub(crate) fn parse_name_status_line(line: &str) -> Option<ChangedPath> {
    let mut fields = line.split('\t');
    let status = fields.next()?;
    if let Some(target) = status.strip_prefix('R') {
        if target.is_empty() || !target.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let from = fields.next()?;
        let to = fields.next()?;
        if fields.next().is_some() || from.is_empty() || to.is_empty() {
            return None;
        }
        return Some(ChangedPath {
            path: to.to_owned(),
            previous: Some(from.to_owned()),
            status: ChangeKind::Renamed,
        });
    }
    if let Some(target) = status.strip_prefix('C') {
        if target.is_empty() || !target.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let _from = fields.next()?;
        let to = fields.next()?;
        if fields.next().is_some() || to.is_empty() {
            return None;
        }
        return Some(ChangedPath {
            path: to.to_owned(),
            previous: None,
            status: ChangeKind::Added,
        });
    }
    let path = fields.next()?;
    if fields.next().is_some() || path.is_empty() {
        return None;
    }
    let status = match status {
        "A" => ChangeKind::Added,
        "M" | "T" => ChangeKind::Modified,
        "D" => ChangeKind::Deleted,
        _ => return None,
    };
    Some(ChangedPath {
        path: path.to_owned(),
        previous: None,
        status,
    })
}

/// The `(id, watch, depends_on)` view selection needs. The generator projects
/// it from [`crate::Unit`], the runtime from its own unit table; selection
/// itself never sees either config type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WatchedUnit {
    pub(crate) id: String,
    pub(crate) watch: Vec<String>,
    pub(crate) depends_on: Vec<String>,
}

/// The affected-selection verdict: `required` must run (changed units, their
/// dependents, and every prerequisite), `full_units` is the changed-plus-
/// dependents subset that runs full scope, and `explanations` names the
/// reason per selected unit. `fallback_full` marks a full set chosen by
/// fallback rather than by matching: an empty diff selects nothing with the
/// flag clear, and the planner records that as explicit no-work.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct AffectedSelection {
    pub(crate) required: BTreeSet<String>,
    pub(crate) full_units: BTreeSet<String>,
    pub(crate) fallback_full: bool,
    pub(crate) explanations: BTreeMap<String, String>,
}

/// Select the units a change list affects.
///
/// A unit is directly selected when any effective match path hits its watch
/// globs. Dependents of selected units join the full set (a prerequisite
/// change can break them), and prerequisites of selected units join the
/// required set (their outputs must exist before the run). Edges are followed
/// across unit kinds: a cross-language dependency invalidates exactly like a
/// same-language one. An unmatched path, a global-prefix path, or a duplicate
/// unit id falls back to the full set — or fails, for the duplicate id — so an
/// input the model cannot prove narrow never silently skips verification.
///
/// # Errors
///
/// Returns a usage error for a duplicate unit id or an invalid watch pattern.
pub(crate) fn select_affected(
    units: &[WatchedUnit],
    changes: &[ChangedPath],
    global_prefixes: &[&str],
) -> Result<AffectedSelection, GeneratorError> {
    let mut seen = BTreeSet::new();
    for unit in units {
        if !seen.insert(unit.id.as_str()) {
            return Err(GeneratorError::usage(format!(
                "duplicate unit id in affected selection: {}",
                unit.id
            )));
        }
    }
    let candidates = effective_matches(changes);
    if candidates.is_empty() {
        return Ok(AffectedSelection {
            required: BTreeSet::new(),
            full_units: BTreeSet::new(),
            fallback_full: false,
            explanations: BTreeMap::new(),
        });
    }
    if let Some(path) = candidates
        .iter()
        .map(|candidate| candidate.path)
        .find(|path| {
            global_prefixes
                .iter()
                .any(|prefix| path.starts_with(prefix))
        })
    {
        return Ok(fallback_selection(
            units,
            &format!("global path `{path}` selects every unit"),
        ));
    }
    let compiled = build_watch_matchers(units)?;
    let hits = match match_changes(&compiled, &candidates) {
        Ok(hits) => hits,
        Err(path) => {
            return Ok(fallback_selection(
                units,
                &format!("unmatched path `{path}` selects every unit"),
            ));
        }
    };
    let direct: BTreeSet<String> = hits.keys().map(ToString::to_string).collect();
    let closure = expand_selection_closure(units, &direct);
    let mut explanations = BTreeMap::new();
    for (id, items) in &hits {
        let mut items = items.clone();
        items.sort();
        explanations.insert(
            (*id).to_owned(),
            format!("matched changed path {}", items.join(", ")),
        );
    }
    for (id, parents) in &closure.affected_by {
        explanations.insert(
            id.clone(),
            format!(
                "dependent of {}",
                parents.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        );
    }
    for (id, children) in &closure.required_by {
        explanations.insert(
            id.clone(),
            format!(
                "prerequisite of {}",
                children.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        );
    }
    Ok(AffectedSelection {
        required: closure.required,
        full_units: closure.full_units,
        fallback_full: false,
        explanations,
    })
}

/// One unit's compiled watch globs.
type CompiledWatch<'a> = (&'a WatchedUnit, GlobSet);

/// Compile every unit's watch globs.
///
/// # Errors
///
/// Returns a usage error for an invalid watch pattern.
fn build_watch_matchers(units: &[WatchedUnit]) -> Result<Vec<CompiledWatch<'_>>, GeneratorError> {
    let mut compiled = Vec::with_capacity(units.len());
    for unit in units {
        let mut builder = GlobSetBuilder::new();
        for pattern in &unit.watch {
            let glob = Glob::new(pattern).map_err(|error| {
                GeneratorError::usage(format!(
                    "invalid watch pattern `{pattern}` for unit {}: {error}",
                    unit.id
                ))
            })?;
            builder.add(glob);
        }
        let set = builder
            .build()
            .map_err(|error| GeneratorError::usage(format!("build watch matcher: {error}")))?;
        compiled.push((unit, set));
    }
    Ok(compiled)
}

/// Match every candidate path against the compiled watches: the unit ids each
/// path selects, with the rendered match per unit. The first unmatched path
/// fails with itself so the caller falls back to the full set.
fn match_changes<'a>(
    compiled: &[CompiledWatch<'a>],
    candidates: &[EffectiveMatch<'_>],
) -> Result<BTreeMap<&'a str, Vec<String>>, String> {
    let mut hits: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for candidate in candidates {
        let mut hit = false;
        for (unit, set) in compiled {
            if set.is_match(candidate.path) {
                hits.entry(unit.id.as_str()).or_default().push(format!(
                    "`{}` ({})",
                    candidate.path,
                    candidate.kind.verb()
                ));
                hit = true;
            }
        }
        if !hit {
            return Err(candidate.path.to_owned());
        }
    }
    Ok(hits)
}

/// The dependency closure of the directly changed units: `required` must run
/// (changed units, their dependents, and every prerequisite), `full_units` is
/// the changed-plus-dependents subset that runs full scope, and the two maps
/// record why each indirect unit joined.
struct SelectionClosure {
    required: BTreeSet<String>,
    full_units: BTreeSet<String>,
    affected_by: BTreeMap<String, BTreeSet<String>>,
    required_by: BTreeMap<String, BTreeSet<String>>,
}

/// Expand the directly changed units across `depends_on` edges. Dependents of
/// a changed unit join the affected set — the changed inputs flow downstream,
/// so downstream verdicts need re-proving — and prerequisites join the
/// required set afterwards without pulling their own dependents.
fn expand_selection_closure(units: &[WatchedUnit], changed: &BTreeSet<String>) -> SelectionClosure {
    let mut affected = changed.clone();
    let mut affected_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut pending: Vec<String> = changed.iter().cloned().collect();
    while let Some(id) = pending.pop() {
        for unit in units {
            if unit.depends_on.iter().any(|dependency| dependency == &id)
                && affected.insert(unit.id.clone())
            {
                affected_by
                    .entry(unit.id.clone())
                    .or_default()
                    .insert(id.clone());
                pending.push(unit.id.clone());
            }
        }
    }
    let full_units = affected.clone();
    let mut required = affected;
    let mut required_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut pending: Vec<String> = required.iter().cloned().collect();
    while let Some(id) = pending.pop() {
        let Some(unit) = units.iter().find(|unit| unit.id == id) else {
            continue;
        };
        for dependency in &unit.depends_on {
            if required.insert(dependency.clone()) {
                required_by
                    .entry(dependency.clone())
                    .or_default()
                    .insert(id.clone());
                pending.push(dependency.clone());
            }
        }
    }
    SelectionClosure {
        required,
        full_units,
        affected_by,
        required_by,
    }
}

/// The full set with one shared fallback reason per unit: the planner could
/// not prove a narrow selection, so every unit runs. Callers use it when the
/// change list itself is unusable (no base, git unavailable, unparseable
/// entries) so every fallback carries its reason.
#[must_use]
pub(crate) fn fallback_selection(units: &[WatchedUnit], reason: &str) -> AffectedSelection {
    AffectedSelection {
        required: units.iter().map(|unit| unit.id.clone()).collect(),
        full_units: units.iter().map(|unit| unit.id.clone()).collect(),
        fallback_full: true,
        explanations: units
            .iter()
            .map(|unit| (unit.id.clone(), reason.to_owned()))
            .collect(),
    }
}

/// Parse one `git ls-tree -r` line (`<mode> SP <type> SP <sha> TAB <path>`)
/// into its `(path, sha)`. Anything else — truncated output, a corrupt
/// transport — parses to [`None`] so the caller fails closed instead of
/// fingerprinting a partial tree.
#[must_use]
pub(crate) fn parse_ls_tree_line(line: &str) -> Option<(String, String)> {
    let (meta, path) = line.split_once('\t')?;
    if path.is_empty() {
        return None;
    }
    let mut meta = meta.split(' ');
    let mode = meta.next()?;
    let kind = meta.next()?;
    let sha = meta.next()?;
    if meta.next().is_some() {
        return None;
    }
    if mode.is_empty() || kind.is_empty() || sha.is_empty() {
        return None;
    }
    Some((path.to_owned(), sha.to_owned()))
}

/// The source blobs of one unit's fingerprint: every `git ls-tree -r` entry
/// whose path matches the unit's watch globs, plus every pinned path (cache
/// key files the watch may not spell). A rename is a path-set change across
/// two fingerprints; a delete removes its entry.
///
/// # Errors
///
/// Returns a usage error for an invalid watch pattern or a malformed
/// `ls-tree` line.
pub(crate) fn source_digests(
    ls_tree: &str,
    watch: &[String],
    pinned_paths: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, GeneratorError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in watch {
        let glob = Glob::new(pattern).map_err(|error| {
            GeneratorError::usage(format!("invalid watch pattern `{pattern}`: {error}"))
        })?;
        builder.add(glob);
    }
    let matcher = builder
        .build()
        .map_err(|error| GeneratorError::usage(format!("build watch matcher: {error}")))?;
    let mut source = BTreeMap::new();
    for line in ls_tree.lines().filter(|line| !line.is_empty()) {
        let Some((path, sha)) = parse_ls_tree_line(line) else {
            return Err(GeneratorError::usage(
                "malformed git ls-tree line; refusing a partial source digest",
            ));
        };
        if matcher.is_match(&path) || pinned_paths.contains(&path) {
            source.insert(path, sha);
        }
    }
    Ok(source)
}

/// Everything that can change one unit's verification verdict, as sorted
/// auditable inputs. `source` maps repository-relative paths to content
/// digests (a rename is a path-set change, a delete removes its entry);
/// `config_digest` covers the generation config rows that reach the unit;
/// `recipe_digest` is the [`recipe_digest`] output for the unit's effective
/// lane commands; `pins` merges [`reviewed_action_pins`] with the unit's tool
/// pins through [`unit_pin_set`]; `transitive` maps each dependency id in the
/// `depends_on` closure — across unit kinds — to its own
/// [`canonical_fingerprint`]; `live_state` marks checks that observe live
/// external state and therefore need explicit evidence freshness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FingerprintInput {
    pub(crate) unit_id: String,
    pub(crate) kind: String,
    pub(crate) root: String,
    pub(crate) source: BTreeMap<String, String>,
    pub(crate) config_digest: String,
    pub(crate) recipe_digest: String,
    pub(crate) pins: BTreeMap<String, String>,
    pub(crate) transitive: BTreeMap<String, String>,
    pub(crate) live_state: bool,
}

/// The identity of the check implementation behind every recipe digest: the
/// generator revision that rendered the verification plus this module's
/// contract version. Any generator change that can alter verification bytes
/// bumps the revision, which invalidates every prior fingerprint.
#[must_use]
pub(crate) fn current_check_impl() -> String {
    format!(
        "generator:{}:reuse:{REUSE_VERSION}",
        crate::GENERATOR_REVISION
    )
}

/// The effective recipe digest: the lane commands the unit actually runs
/// (keyed by `lane/scope`), the generator revision, and the check
/// implementation identity. Two runs share a recipe only when all three
/// agree; a command edit, a lane override, or a generator bump all mint a new
/// recipe.
#[must_use]
pub(crate) fn recipe_digest(
    commands: &BTreeMap<String, Vec<String>>,
    generator_revision: &str,
    check_impl: &str,
) -> String {
    let mut bytes = Vec::new();
    push_line(&mut bytes, &format!("reuse-version:{REUSE_VERSION}"));
    push_line(&mut bytes, &format!("generator:{generator_revision}"));
    push_line(&mut bytes, &format!("check:{check_impl}"));
    for (lane_scope, steps) in commands {
        for (index, step) in steps.iter().enumerate() {
            push_line(&mut bytes, &format!("commands:{lane_scope}:{index}={step}"));
        }
    }
    hex_sha256(&bytes)
}

/// The canonical fingerprint: lowercase hex `SHA-256` over the sorted
/// [`FingerprintInput`] with a versioned header. Map iteration is already
/// sorted, so identical inputs always mint identical bytes.
#[must_use]
pub(crate) fn canonical_fingerprint(input: &FingerprintInput) -> String {
    let mut bytes = Vec::new();
    push_line(&mut bytes, &format!("reuse-version:{REUSE_VERSION}"));
    push_line(&mut bytes, &format!("unit:{}", input.unit_id));
    push_line(&mut bytes, &format!("kind:{}", input.kind));
    push_line(&mut bytes, &format!("root:{}", input.root));
    push_line(&mut bytes, &format!("live:{}", input.live_state));
    push_line(&mut bytes, &format!("config:{}", input.config_digest));
    push_line(&mut bytes, &format!("recipe:{}", input.recipe_digest));
    for (path, digest) in &input.source {
        push_line(&mut bytes, &format!("source:{path}={digest}"));
    }
    for (name, pinned) in &input.pins {
        push_line(&mut bytes, &format!("pin:{name}={pinned}"));
    }
    for (id, digest) in &input.transitive {
        push_line(&mut bytes, &format!("dep:{id}={digest}"));
    }
    hex_sha256(&bytes)
}

/// Whether `value` is a full 64-hex fingerprint. Reuse compares full digests;
/// prefixes are locators only.
#[must_use]
pub(crate) fn is_full_fingerprint(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The artifact locator for a fingerprinted unit: the unit id plus the
/// digest prefix. A locator finds a candidate; acceptance always compares the
/// full digest the evidence records, never this name.
#[must_use]
pub(crate) fn artifact_locator(unit_id: &str, fingerprint: &str) -> String {
    let prefix: String = fingerprint.chars().take(16).collect();
    format!("{unit_id}-{prefix}")
}

fn push_line(bytes: &mut Vec<u8>, line: &str) {
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
}

/// Lowercase hex `SHA-256` of `bytes`.
pub(crate) fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// The config rows that reach one unit, as plain data the generator and the
/// runtime both project from their own unit tables: identity, watched paths,
/// the effective lane commands (keyed by `lane/scope`, the same map
/// [`recipe_digest`] hashes), dependencies, the tool version, and the cache
/// contract's key files and paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UnitConfigView {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) root: String,
    pub(crate) watch: Vec<String>,
    pub(crate) commands: BTreeMap<String, Vec<String>>,
    pub(crate) depends_on: Vec<String>,
    pub(crate) tool_version: Option<String>,
    pub(crate) cache_key_files: Vec<String>,
    pub(crate) cache_paths: Vec<String>,
}

/// The config digest: lowercase hex `SHA-256` over the sorted config rows.
/// Every row that can redirect the unit's verification invalidates it; lists
/// sort before hashing so config order never mints a new digest.
#[must_use]
pub(crate) fn config_digest(view: &UnitConfigView) -> String {
    let mut bytes = Vec::new();
    push_line(&mut bytes, &format!("reuse-version:{REUSE_VERSION}"));
    push_line(&mut bytes, &format!("id:{}", view.id));
    push_line(&mut bytes, &format!("kind:{}", view.kind));
    push_line(&mut bytes, &format!("root:{}", view.root));
    let mut watch = view.watch.clone();
    watch.sort();
    for pattern in &watch {
        push_line(&mut bytes, &format!("watch:{pattern}"));
    }
    for (lane_scope, steps) in &view.commands {
        for (index, step) in steps.iter().enumerate() {
            push_line(&mut bytes, &format!("commands:{lane_scope}:{index}={step}"));
        }
    }
    let mut depends_on = view.depends_on.clone();
    depends_on.sort();
    for dependency in &depends_on {
        push_line(&mut bytes, &format!("depends:{dependency}"));
    }
    push_line(
        &mut bytes,
        &format!("tool:{}", view.tool_version.as_deref().unwrap_or("-")),
    );
    let mut key_files = view.cache_key_files.clone();
    key_files.sort();
    for file in &key_files {
        push_line(&mut bytes, &format!("key:{file}"));
    }
    let mut paths = view.cache_paths.clone();
    paths.sort();
    for path in &paths {
        push_line(&mut bytes, &format!("path:{path}"));
    }
    hex_sha256(&bytes)
}

/// One fingerprinted unit as the `fingerprint` command reports it: the unit
/// id, the full fingerprint, the locator prefix, the recipe, and whether the
/// unit observes live external state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct FingerprintReport {
    pub(crate) unit: String,
    pub(crate) fingerprint: String,
    pub(crate) locator: String,
    pub(crate) recipe: String,
    pub(crate) live: bool,
}

/// The reviewed action pin table as fingerprint inputs, straight from the
/// generator's single pin source. Any pin bump invalidates every fingerprint
/// that embeds this table.
#[must_use]
pub(crate) fn reviewed_action_pins() -> BTreeMap<String, String> {
    let pins = crate::primitives::Pins::resolved();
    BTreeMap::from([
        ("action:checkout".to_owned(), pins.checkout.to_owned()),
        (
            "action:cache-restore".to_owned(),
            pins.cache_restore.to_owned(),
        ),
        ("action:cache-save".to_owned(), pins.cache_save.to_owned()),
        (
            "action:opentofu-setup".to_owned(),
            pins.opentofu_setup.to_owned(),
        ),
        (
            "action:upload-artifact".to_owned(),
            pins.upload_artifact.to_owned(),
        ),
        (
            "action:download-artifact".to_owned(),
            pins.download_artifact.to_owned(),
        ),
        ("action:bun".to_owned(), pins.bun.to_owned()),
        ("action:node".to_owned(), pins.node.to_owned()),
        ("action:rust-tool".to_owned(), pins.rust_tool.to_owned()),
        ("action:mise".to_owned(), pins.mise.to_owned()),
        ("action:gradle".to_owned(), pins.gradle.to_owned()),
        ("action:sccache".to_owned(), pins.sccache.to_owned()),
        (
            "action:mr-boxington".to_owned(),
            pins.mr_boxington.to_owned(),
        ),
        (
            "action:github-runtime".to_owned(),
            pins.github_runtime.to_owned(),
        ),
        (
            "action:docker-buildx".to_owned(),
            pins.docker_buildx.to_owned(),
        ),
    ])
}

/// One unit's complete pin set: the reviewed action pins plus the unit's tool
/// pins (toolchain channel, `mise` lock entries, `tool_version`), namespaced
/// so the two families can never collide.
#[must_use]
pub(crate) fn unit_pin_set(tool_pins: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut pins = reviewed_action_pins();
    for (name, pinned) in tool_pins {
        pins.insert(format!("tool:{name}"), pinned.clone());
    }
    pins
}

/// The trust class of the event that produced a result. Trusted evidence
/// (a protected-branch run) reuses anywhere; untrusted evidence (a fork pull
/// request run) never satisfies a trusted requirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrustClass {
    Trusted,
    Untrusted,
}

impl TrustClass {
    /// Whether evidence of this class satisfies `required`. Trust only flows
    /// down: a protected-branch result may back any later verdict, but an
    /// untrusted result may never back a protected-branch verdict.
    #[must_use]
    pub(crate) fn satisfies(self, required: Self) -> bool {
        match (self, required) {
            (Self::Trusted, _) | (Self::Untrusted, Self::Untrusted) => true,
            (Self::Untrusted, Self::Trusted) => false,
        }
    }

    /// Parse the `trusted` or `untrusted` trust class a decision file names.
    ///
    /// # Errors
    ///
    /// Returns the offending value for anything else.
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "trusted" => Ok(Self::Trusted),
            "untrusted" => Ok(Self::Untrusted),
            other => Err(format!(
                "unsupported trust class `{other}`: use trusted or untrusted"
            )),
        }
    }
}

/// The producing evidence a prior successful result must carry to be reusable.
/// There is deliberately no artifact-name field: [`validate_reuse`] compares
/// full digests, and a bare locator or a presence bit cannot construct this
/// struct. `fresh_until` is epoch seconds bounding reuse for live-state
/// checks; content-addressed checks leave it empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Evidence {
    pub(crate) run_id: String,
    pub(crate) fingerprint: String,
    pub(crate) recipe: String,
    pub(crate) checks: BTreeSet<String>,
    pub(crate) aggregate_passed: bool,
    pub(crate) trust: TrustClass,
    pub(crate) fresh_until: Option<u64>,
}

/// What the current run needs a prior result to prove before reusing it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReuseRequest {
    pub(crate) fingerprint: String,
    pub(crate) recipe: String,
    pub(crate) expected_checks: BTreeSet<String>,
    pub(crate) required_trust: TrustClass,
    pub(crate) live_state: bool,
    pub(crate) now: u64,
}

/// The reuse verdict. Both arms carry audit reasons: what matched, or the
/// first rule that refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReuseDecision {
    Reuse {
        run_id: String,
        reasons: Vec<String>,
    },
    Execute {
        reasons: Vec<String>,
    },
}

impl ReuseDecision {
    /// The audit reasons behind the verdict.
    #[must_use]
    pub(crate) fn reasons(&self) -> &[String] {
        match self {
            Self::Reuse { reasons, .. } | Self::Execute { reasons } => reasons,
        }
    }
}

/// Decide whether a prior successful result may back the current verdict.
///
/// Every rule must hold: the evidence fingerprint equals the current full
/// fingerprint, the recipe matches, the producing run passed its own
/// aggregate, its executed check set covers the current expected set, its
/// trust satisfies the requirement, and — for live-state checks — its
/// explicit freshness still covers now. A missing evidence payload reuses
/// nothing: green never comes from an artifact name or a presence bit. The
/// first failing rule decides, so the reasons always name the refusal.
#[must_use]
pub(crate) fn validate_reuse(evidence: Option<&Evidence>, request: &ReuseRequest) -> ReuseDecision {
    let Some(evidence) = evidence else {
        return ReuseDecision::Execute {
            reasons: vec!["no producing evidence; artifact presence alone never reuses".to_owned()],
        };
    };
    if let Some(refused) = reject_truncated_inputs(evidence, request) {
        return refused;
    }
    if evidence.fingerprint != request.fingerprint {
        return ReuseDecision::Execute {
            reasons: vec![format!(
                "evidence fingerprint {} does not match current {}",
                evidence.fingerprint, request.fingerprint
            )],
        };
    }
    if evidence.recipe != request.recipe {
        return ReuseDecision::Execute {
            reasons: vec![format!(
                "evidence recipe {} does not match current {}",
                evidence.recipe, request.recipe
            )],
        };
    }
    if !evidence.aggregate_passed {
        return ReuseDecision::Execute {
            reasons: vec![format!(
                "producing run {} did not pass its own aggregate",
                evidence.run_id
            )],
        };
    }
    let missing: Vec<&str> = request
        .expected_checks
        .difference(&evidence.checks)
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return ReuseDecision::Execute {
            reasons: vec![format!(
                "producing run {} missed expected checks {}",
                evidence.run_id,
                missing.join(", ")
            )],
        };
    }
    if !evidence.trust.satisfies(request.required_trust) {
        return ReuseDecision::Execute {
            reasons: vec![format!(
                "producing run {} trust {:?} does not satisfy {:?}",
                evidence.run_id, evidence.trust, request.required_trust
            )],
        };
    }
    if let Some(refused) = reject_stale_evidence(evidence, request) {
        return refused;
    }
    accept_reuse(evidence, request)
}

/// Refuse comparison inputs that are not full digests: a truncated
/// fingerprint or recipe prefix must never compare equal to anything.
fn reject_truncated_inputs(evidence: &Evidence, request: &ReuseRequest) -> Option<ReuseDecision> {
    for (side, digest) in [
        ("evidence fingerprint", evidence.fingerprint.as_str()),
        ("current fingerprint", request.fingerprint.as_str()),
        ("evidence recipe", evidence.recipe.as_str()),
        ("current recipe", request.recipe.as_str()),
    ] {
        if !is_full_fingerprint(digest) {
            return Some(ReuseDecision::Execute {
                reasons: vec![format!(
                    "{side} `{digest}` is not a full digest; truncated inputs never reuse"
                )],
            });
        }
    }
    None
}

/// Refuse live-state reuse without explicit freshness that still covers now.
/// Content-addressed checks skip this rule entirely.
fn reject_stale_evidence(evidence: &Evidence, request: &ReuseRequest) -> Option<ReuseDecision> {
    if !request.live_state {
        return None;
    }
    match evidence.fresh_until {
        None => Some(ReuseDecision::Execute {
            reasons: vec![format!(
                "producing run {} carries no freshness bound for a live-state check",
                evidence.run_id
            )],
        }),
        Some(until) if request.now > until => Some(ReuseDecision::Execute {
            reasons: vec![format!(
                "producing run {} freshness expired at {until} (now {})",
                evidence.run_id, request.now
            )],
        }),
        Some(_) => None,
    }
}

/// Accept the reuse, recording every validated rule in the reasons.
fn accept_reuse(evidence: &Evidence, request: &ReuseRequest) -> ReuseDecision {
    let mut reasons = vec![
        format!("fingerprint {} matches", request.fingerprint),
        format!("recipe {} matches", request.recipe),
        format!("producing run {} passed its aggregate", evidence.run_id),
        format!(
            "producing checks cover {} expected checks",
            request.expected_checks.len()
        ),
        format!(
            "producing trust {:?} satisfies {:?}",
            evidence.trust, request.required_trust
        ),
    ];
    if request.live_state {
        reasons.push(format!("producing freshness covers now {}", request.now));
    }
    ReuseDecision::Reuse {
        run_id: evidence.run_id.clone(),
        reasons,
    }
}

/// One unit's planned work: the lanes that must run it, the matrix entries
/// each lane must report (empty is one unmatrixed entry), whether the
/// aggregate requires it, and — when the planner deliberately skips it — the
/// recorded reason. A skip without a recorded reason is unexpected and fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExpectedUnit {
    pub(crate) lanes: BTreeSet<String>,
    pub(crate) matrix: BTreeSet<String>,
    pub(crate) required: bool,
    pub(crate) planned_skip: Option<String>,
}

/// The planner's expected work: every unit the run must account for, plus the
/// explicit no-work marker. An empty `units` map passes only with
/// `planned_no_work` set — the planner's signed statement that the diff
/// selected nothing — and a no-work marker beside listed units fails closed
/// as a contradictory plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExpectedWork {
    pub(crate) units: BTreeMap<String, ExpectedUnit>,
    pub(crate) planned_no_work: bool,
}

impl ExpectedWork {
    /// Whether this is the explicit planned no-work: nothing selected, and
    /// the planner says so.
    #[must_use]
    pub(crate) fn is_explicit_no_work(&self) -> bool {
        self.units.is_empty() && self.planned_no_work
    }
}

/// One reported work item: which unit, lane, and matrix entry concluded, how,
/// and — for a reused success — which producing run backs it. A missing
/// report is the absence of this struct, which [`aggregate`] rejects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReportedResult {
    pub(crate) unit_id: String,
    pub(crate) lane: String,
    pub(crate) matrix_entry: Option<String>,
    pub(crate) outcome: ReportedOutcome,
    pub(crate) reused_from: Option<String>,
}

/// How one reported work item concluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReportedOutcome {
    Success,
    Failure,
    Skipped { reason: String },
    Cancelled,
}

/// What happened to one expected work item, for explanations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Disposition {
    Executed,
    Reused,
    SkippedPlanned,
    Failed,
    Missing,
    Cancelled,
    UnexpectedSkip,
    BlockedByPrerequisite,
    Extra,
}

impl Disposition {
    /// The stable one-line verb explanations render.
    #[must_use]
    pub(crate) fn verb(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::Reused => "reused",
            Self::SkippedPlanned => "skipped (planned)",
            Self::Failed => "failed",
            Self::Missing => "missing",
            Self::Cancelled => "cancelled",
            Self::UnexpectedSkip => "unexpected skip",
            Self::BlockedByPrerequisite => "blocked",
            Self::Extra => "extra",
        }
    }
}

/// One explained work item: the expected unit, lane, and matrix entry, what
/// happened to it, and the detail. Unit-level notes (blocked prerequisites)
/// leave `lane` empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkExplanation {
    pub(crate) unit_id: String,
    pub(crate) lane: String,
    pub(crate) matrix_entry: Option<String>,
    pub(crate) disposition: Disposition,
    pub(crate) detail: String,
}

/// The aggregate verdict: whether every expected item holds a success (or a
/// planned skip), the failure lines, one explanation per item in stable plan
/// order, and the complete expected check set — the same names producing
/// evidence must cover before [`validate_reuse`] reuses this run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AggregateVerdict {
    pub(crate) passed: bool,
    pub(crate) failures: Vec<String>,
    pub(crate) explanations: Vec<WorkExplanation>,
    pub(crate) checks: BTreeSet<String>,
}

/// Score reported results against the planner's expected work.
///
/// Every expected `(unit, lane, matrix entry)` must conclude success —
/// executed or backed by producing evidence — or carry the planner's recorded
/// skip reason. The aggregate rejects missing results, unexpected skips,
/// cancelled work, incomplete matrices (matrix entries with no report), a
/// prerequisite closure that did not itself pass, duplicate reports, and a
/// contradictory plan (the no-work marker beside listed units, or a
/// prerequisite outside the plan). Reports outside the plan are noted as
/// extra and ignored: unplanned work never greens planned work, and never
/// reds it either. `prerequisites` maps each unit to its direct dependencies.
#[must_use]
pub(crate) fn aggregate(
    expected: &ExpectedWork,
    results: &[ReportedResult],
    prerequisites: &BTreeMap<String, Vec<String>>,
) -> AggregateVerdict {
    let mut failures = Vec::new();
    let mut explanations = Vec::new();
    if expected.planned_no_work && !expected.units.is_empty() {
        failures.push(
            "planner marked no-work but listed expected units; the plan is contradictory"
                .to_owned(),
        );
    }
    let (mut index, mut duplicates) = index_results(results);
    failures.append(&mut duplicates);
    if expected.units.is_empty() {
        if !expected.is_explicit_no_work() {
            failures.push(
                "planner emitted no expected work without the explicit no-work marker".to_owned(),
            );
        }
        for result in results {
            explanations.push(WorkExplanation {
                unit_id: result.unit_id.clone(),
                lane: result.lane.clone(),
                matrix_entry: result.matrix_entry.clone(),
                disposition: Disposition::Extra,
                detail: "reported work outside the planned no-work set; ignored".to_owned(),
            });
        }
        return AggregateVerdict {
            passed: failures.is_empty(),
            failures,
            explanations,
            checks: BTreeSet::new(),
        };
    }
    let mut failed_units: BTreeSet<String> = BTreeSet::new();
    for (unit_id, unit) in &expected.units {
        if unit.lanes.is_empty() {
            failures.push(format!("expected unit `{unit_id}` names no lanes"));
            failed_units.insert(unit_id.clone());
            continue;
        }
        for lane in &unit.lanes {
            for entry in effective_entries(&unit.matrix) {
                let key = (unit_id.clone(), lane.clone(), entry.map(str::to_owned));
                let report = index.remove(&key);
                let score = score_expected_item(unit_id, unit, lane, entry, report);
                if let Some(failure) = score.failure {
                    failures.push(failure);
                }
                explanations.push(score.explanation);
                if score.failed {
                    failed_units.insert(unit_id.clone());
                }
            }
        }
    }
    for ((unit_id, lane, entry), _) in index {
        explanations.push(WorkExplanation {
            unit_id,
            lane,
            matrix_entry: entry,
            disposition: Disposition::Extra,
            detail: "reported work outside the plan; ignored".to_owned(),
        });
    }
    check_prerequisites(
        expected,
        prerequisites,
        &failed_units,
        &mut failures,
        &mut explanations,
    );
    let mut checks = BTreeSet::new();
    for (unit_id, unit) in &expected.units {
        checks.extend(check_names_for_unit(unit_id, unit));
    }
    AggregateVerdict {
        passed: failures.is_empty(),
        failures,
        explanations,
        checks,
    }
}

/// Reported results keyed by `(unit, lane, matrix entry)`.
type ResultIndex<'a> = BTreeMap<(String, String, Option<String>), &'a ReportedResult>;

/// Index reported results, keeping the first report per item and failing
/// every duplicate: two verdicts for one item prove neither.
fn index_results(results: &[ReportedResult]) -> (ResultIndex<'_>, Vec<String>) {
    let mut index: ResultIndex<'_> = BTreeMap::new();
    let mut failures = Vec::new();
    for result in results {
        let key = (
            result.unit_id.clone(),
            result.lane.clone(),
            result.matrix_entry.clone(),
        );
        if index.insert(key.clone(), result).is_some() {
            failures.push(format!(
                "duplicate result for {}",
                describe_item(&key.0, &key.1, key.2.as_deref())
            ));
        }
    }
    (index, failures)
}

/// The score of one expected work item: its failure line when it does not
/// hold, its explanation always, and whether its unit counts as failed.
struct ItemScore {
    failure: Option<String>,
    explanation: WorkExplanation,
    failed: bool,
}

/// Score one expected `(unit, lane, matrix entry)` against its report. A
/// success holds — executed or reused — and a skip holds only with the
/// planner's recorded reason; anything else fails the item and its unit.
fn score_expected_item(
    unit_id: &str,
    unit: &ExpectedUnit,
    lane: &str,
    entry: Option<&str>,
    report: Option<&ReportedResult>,
) -> ItemScore {
    let target = describe_item(unit_id, lane, entry);
    let explain = |disposition: Disposition, detail: String| WorkExplanation {
        unit_id: unit_id.to_owned(),
        lane: lane.to_owned(),
        matrix_entry: entry.map(str::to_owned),
        disposition,
        detail,
    };
    let Some(report) = report else {
        let detail = match entry {
            Some(entry) => format!("incomplete matrix: entry `{entry}` has no result"),
            None => "no result reported".to_owned(),
        };
        return ItemScore {
            failure: Some(format!("missing result for {target}")),
            explanation: explain(Disposition::Missing, detail),
            failed: true,
        };
    };
    match &report.outcome {
        ReportedOutcome::Success => match &report.reused_from {
            Some(run) => ItemScore {
                failure: None,
                explanation: explain(
                    Disposition::Reused,
                    format!("reused successful result from {run}"),
                ),
                failed: false,
            },
            None => ItemScore {
                failure: None,
                explanation: explain(Disposition::Executed, "ran to success".to_owned()),
                failed: false,
            },
        },
        ReportedOutcome::Failure => ItemScore {
            failure: Some(format!("failed work {target}")),
            explanation: explain(Disposition::Failed, "reported failure".to_owned()),
            failed: true,
        },
        ReportedOutcome::Skipped { reason } => {
            if let Some(planned) = &unit.planned_skip {
                ItemScore {
                    failure: None,
                    explanation: explain(
                        Disposition::SkippedPlanned,
                        format!("planned: {planned}; reported: {reason}"),
                    ),
                    failed: false,
                }
            } else {
                ItemScore {
                    failure: Some(format!("unexpected skip of {target}: {reason}")),
                    explanation: explain(
                        Disposition::UnexpectedSkip,
                        format!("skipped without a planned reason: {reason}"),
                    ),
                    failed: true,
                }
            }
        }
        ReportedOutcome::Cancelled => {
            let failure = if unit.required {
                format!("cancelled required work {target}")
            } else {
                format!("cancelled work {target} has no verdict")
            };
            ItemScore {
                failure: Some(failure),
                explanation: explain(
                    Disposition::Cancelled,
                    "run cancelled before a verdict".to_owned(),
                ),
                failed: true,
            }
        }
    }
}

/// The matrix entries one expected unit must report: the declared entries, or
/// one unmatrixed entry when the unit declares no matrix.
fn effective_entries(matrix: &BTreeSet<String>) -> Vec<Option<&str>> {
    if matrix.is_empty() {
        return vec![None];
    }
    matrix.iter().map(|entry| Some(entry.as_str())).collect()
}

/// The stable `unit lane[entry]` rendering failures and explanations share.
fn describe_item(unit_id: &str, lane: &str, entry: Option<&str>) -> String {
    match entry {
        Some(entry) => format!("{unit_id} {lane}[{entry}]"),
        None => format!("{unit_id} {lane}"),
    }
}

/// Reject expected units whose transitive prerequisites did not themselves
/// pass: a success behind a failed prerequisite proves nothing, so the
/// dependent fails closed even when it reported success. A prerequisite
/// outside the plan, or a dependency cycle, fails the whole aggregate.
fn check_prerequisites(
    expected: &ExpectedWork,
    prerequisites: &BTreeMap<String, Vec<String>>,
    failed_units: &BTreeSet<String>,
    failures: &mut Vec<String>,
    explanations: &mut Vec<WorkExplanation>,
) {
    for unit_id in expected.units.keys() {
        let transitive = match transitive_prerequisites(unit_id, prerequisites) {
            Ok(transitive) => transitive,
            Err(cycle) => {
                failures.push(cycle);
                continue;
            }
        };
        for dependency in &transitive {
            if !expected.units.contains_key(dependency) {
                failures.push(format!(
                    "prerequisite `{dependency}` of `{unit_id}` is outside the planned work"
                ));
            } else if failed_units.contains(dependency) {
                failures.push(format!(
                    "prerequisite `{dependency}` of `{unit_id}` did not pass"
                ));
                explanations.push(WorkExplanation {
                    unit_id: unit_id.clone(),
                    lane: String::new(),
                    matrix_entry: None,
                    disposition: Disposition::BlockedByPrerequisite,
                    detail: format!("prerequisite `{dependency}` did not pass"),
                });
            }
        }
    }
}

/// The transitive prerequisite closure of `unit`, failing closed on a cycle.
fn transitive_prerequisites(
    unit: &str,
    prerequisites: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeSet<String>, String> {
    let mut transitive = BTreeSet::new();
    let mut visiting = BTreeSet::from([unit.to_owned()]);
    let mut stack: Vec<String> = prerequisites.get(unit).cloned().unwrap_or_default();
    while let Some(id) = stack.pop() {
        if !transitive.insert(id.clone()) {
            continue;
        }
        if !visiting.insert(id.clone()) {
            return Err(format!("prerequisite graph contains a cycle at `{id}`"));
        }
        if let Some(deps) = prerequisites.get(&id) {
            stack.extend(deps.iter().cloned());
        }
    }
    Ok(transitive)
}

/// Render one explanation as a stable audit line.
#[must_use]
pub(crate) fn render_explanation(explanation: &WorkExplanation) -> String {
    let target = if explanation.lane.is_empty() {
        explanation.unit_id.clone()
    } else {
        describe_item(
            &explanation.unit_id,
            &explanation.lane,
            explanation.matrix_entry.as_deref(),
        )
    };
    format!(
        "{} {target} — {}",
        explanation.disposition.verb(),
        explanation.detail
    )
}

/// Render the full aggregate report: the verdict, the complete expected
/// check set, one line per explanation in stable plan order, then the
/// failure lines.
#[must_use]
pub(crate) fn render_report(verdict: &AggregateVerdict) -> String {
    let mut report = format!(
        "aggregate: {}\n",
        if verdict.passed { "PASS" } else { "FAIL" }
    );
    if !verdict.checks.is_empty() {
        report.push_str("checks:\n");
        for check in &verdict.checks {
            let _ = writeln!(report, "- {check}");
        }
    }
    for explanation in &verdict.explanations {
        report.push_str(&render_explanation(explanation));
        report.push('\n');
    }
    if !verdict.failures.is_empty() {
        report.push_str("failures:\n");
        for failure in &verdict.failures {
            let _ = writeln!(report, "- {failure}");
        }
    }
    report
}

/// The planner's expected-work file: the units the run must account for, the
/// explicit no-work marker, and the prerequisite map the aggregate checks.
/// `required` defaults to true: a unit is required unless the planner says
/// otherwise.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedWorkFile {
    #[serde(default)]
    pub(crate) planned_no_work: bool,
    #[serde(default)]
    pub(crate) units: Vec<ExpectedUnitFile>,
    #[serde(default)]
    pub(crate) prerequisites: BTreeMap<String, Vec<String>>,
}

/// One expected unit in the planner's file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedUnitFile {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) lanes: Vec<String>,
    #[serde(default)]
    pub(crate) matrix: Vec<String>,
    #[serde(default = "default_required")]
    pub(crate) required: bool,
    #[serde(default)]
    pub(crate) planned_skip: Option<String>,
}

fn default_required() -> bool {
    true
}

/// The reported-results file: one entry per concluded work item. `outcome`
/// is `success`, `failure`, `skipped` (which needs `reason`), or
/// `cancelled`; `reused_from` names the producing run behind a reused
/// success.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResultsFile {
    #[serde(default)]
    pub(crate) results: Vec<ReportedResultFile>,
}

/// One reported result in the results file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportedResultFile {
    pub(crate) unit: String,
    pub(crate) lane: String,
    #[serde(default)]
    pub(crate) matrix: Option<String>,
    pub(crate) outcome: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
    #[serde(default)]
    pub(crate) reused_from: Option<String>,
}

/// Parse an expected-work file and a results file, then [`aggregate`] them.
/// Malformed files are usage errors naming the offending value; contradictory
/// verdicts stay inside the returned [`AggregateVerdict`].
pub(crate) fn aggregate_files(
    expected_json: &str,
    results_json: &str,
) -> Result<AggregateVerdict, String> {
    let expected_file: ExpectedWorkFile = serde_json::from_str(expected_json)
        .map_err(|error| format!("the expected-work file is not valid JSON: {error}"))?;
    let results_file: ResultsFile = serde_json::from_str(results_json)
        .map_err(|error| format!("the results file is not valid JSON: {error}"))?;
    let mut units = BTreeMap::new();
    for unit in expected_file.units {
        if unit.id.is_empty() {
            return Err("the expected-work file names a unit with an empty id".to_owned());
        }
        if unit.lanes.is_empty() {
            return Err(format!("expected unit `{}` names no lanes", unit.id));
        }
        if units
            .insert(
                unit.id.clone(),
                ExpectedUnit {
                    lanes: unit.lanes.into_iter().collect(),
                    matrix: unit.matrix.into_iter().collect(),
                    required: unit.required,
                    planned_skip: unit.planned_skip,
                },
            )
            .is_some()
        {
            return Err(format!("duplicate expected unit `{}`", unit.id));
        }
    }
    let mut results = Vec::with_capacity(results_file.results.len());
    for result in results_file.results {
        results.push(ReportedResult {
            unit_id: result.unit,
            lane: result.lane,
            matrix_entry: result.matrix,
            outcome: parse_outcome(&result.outcome, result.reason.as_deref())?,
            reused_from: result.reused_from,
        });
    }
    Ok(aggregate(
        &ExpectedWork {
            units,
            planned_no_work: expected_file.planned_no_work,
        },
        &results,
        &expected_file.prerequisites,
    ))
}

/// Parse one file outcome. `skipped` needs its reported reason; anything else
/// fails closed on the unknown value.
fn parse_outcome(outcome: &str, reason: Option<&str>) -> Result<ReportedOutcome, String> {
    match outcome {
        "success" => Ok(ReportedOutcome::Success),
        "failure" => Ok(ReportedOutcome::Failure),
        "skipped" => reason.map_or_else(
            || Err("a skipped outcome needs its reported reason".to_owned()),
            |reason| {
                Ok(ReportedOutcome::Skipped {
                    reason: reason.to_owned(),
                })
            },
        ),
        "cancelled" => Ok(ReportedOutcome::Cancelled),
        other => Err(format!(
            "unsupported outcome `{other}`: use success, failure, skipped, or cancelled"
        )),
    }
}

/// The producing-evidence file: the evidence a prior run recorded beside its
/// artifact. `trust` is `trusted` or `untrusted`; `fresh_until` is epoch
/// seconds bounding reuse for live-state checks.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceFile {
    pub(crate) run_id: String,
    pub(crate) fingerprint: String,
    pub(crate) recipe: String,
    pub(crate) checks: Vec<String>,
    pub(crate) aggregate_passed: bool,
    pub(crate) trust: String,
    #[serde(default)]
    pub(crate) fresh_until: Option<u64>,
}

/// The reuse-request file: what the current run needs proven. `now` pins the
/// freshness clock; the `--now` flag overrides it, and a live run uses the
/// system clock when neither is set.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReuseRequestFile {
    pub(crate) fingerprint: String,
    pub(crate) recipe: String,
    pub(crate) expected_checks: Vec<String>,
    pub(crate) required_trust: String,
    #[serde(default)]
    pub(crate) live_state: bool,
    #[serde(default)]
    pub(crate) now: Option<u64>,
}

/// Parse an evidence file and a request file, then [`validate_reuse`] them.
/// `now` overrides the request file's pinned clock. Malformed files are
/// errors naming the offending value; a refused reuse is a normal
/// [`ReuseDecision::Execute`], never an error.
pub(crate) fn reuse_decision_files(
    evidence_json: &str,
    request_json: &str,
    now: Option<u64>,
) -> Result<ReuseDecision, String> {
    let evidence_file: EvidenceFile = serde_json::from_str(evidence_json)
        .map_err(|error| format!("the evidence file is not valid JSON: {error}"))?;
    let request_file: ReuseRequestFile = serde_json::from_str(request_json)
        .map_err(|error| format!("the request file is not valid JSON: {error}"))?;
    let evidence = Evidence {
        run_id: evidence_file.run_id,
        fingerprint: evidence_file.fingerprint,
        recipe: evidence_file.recipe,
        checks: evidence_file.checks.into_iter().collect(),
        aggregate_passed: evidence_file.aggregate_passed,
        trust: TrustClass::parse(&evidence_file.trust)?,
        fresh_until: evidence_file.fresh_until,
    };
    let request = ReuseRequest {
        fingerprint: request_file.fingerprint,
        recipe: request_file.recipe,
        expected_checks: request_file.expected_checks.into_iter().collect(),
        required_trust: TrustClass::parse(&request_file.required_trust)?,
        live_state: request_file.live_state,
        now: now.or(request_file.now).ok_or_else(|| {
            "the reuse request names no clock; pass --now or set `now`".to_owned()
        })?,
    };
    Ok(validate_reuse(Some(&evidence), &request))
}

/// Render one reuse decision: the `reuse <run>` or `execute` verdict first
/// for machines, then one audit reason per line.
#[must_use]
pub(crate) fn render_decision(decision: &ReuseDecision) -> String {
    let mut report = match decision {
        ReuseDecision::Reuse { run_id, .. } => format!("reuse {run_id}\n"),
        ReuseDecision::Execute { .. } => "execute\n".to_owned(),
    };
    for reason in decision.reasons() {
        let _ = writeln!(report, "- {reason}");
    }
    report
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    fn watched(id: &str, watch: &[&str], depends_on: &[&str]) -> WatchedUnit {
        WatchedUnit {
            id: id.to_owned(),
            watch: watch.iter().map(ToString::to_string).collect(),
            depends_on: depends_on.iter().map(ToString::to_string).collect(),
        }
    }

    fn change(path: &str, status: ChangeKind) -> ChangedPath {
        ChangedPath {
            path: path.to_owned(),
            previous: None,
            status,
        }
    }

    fn renamed(from: &str, to: &str) -> ChangedPath {
        ChangedPath {
            path: to.to_owned(),
            previous: Some(from.to_owned()),
            status: ChangeKind::Renamed,
        }
    }

    fn expected_unit(
        lanes: &[&str],
        matrix: &[&str],
        required: bool,
        planned_skip: Option<&str>,
    ) -> ExpectedUnit {
        ExpectedUnit {
            lanes: lanes.iter().map(ToString::to_string).collect(),
            matrix: matrix.iter().map(ToString::to_string).collect(),
            required,
            planned_skip: planned_skip.map(str::to_owned),
        }
    }

    fn success(
        unit: &str,
        lane: &str,
        entry: Option<&str>,
        reused_from: Option<&str>,
    ) -> ReportedResult {
        ReportedResult {
            unit_id: unit.to_owned(),
            lane: lane.to_owned(),
            matrix_entry: entry.map(str::to_owned),
            outcome: ReportedOutcome::Success,
            reused_from: reused_from.map(str::to_owned),
        }
    }

    fn expecting(units: BTreeMap<String, ExpectedUnit>) -> ExpectedWork {
        ExpectedWork {
            units,
            planned_no_work: false,
        }
    }

    fn no_work() -> ExpectedWork {
        ExpectedWork {
            units: BTreeMap::new(),
            planned_no_work: true,
        }
    }

    fn no_work_unmarked() -> ExpectedWork {
        ExpectedWork {
            units: BTreeMap::new(),
            planned_no_work: false,
        }
    }

    fn skipped(unit: &str, lane: &str, reason: &str) -> ReportedResult {
        ReportedResult {
            unit_id: unit.to_owned(),
            lane: lane.to_owned(),
            matrix_entry: None,
            outcome: ReportedOutcome::Skipped {
                reason: reason.to_owned(),
            },
            reused_from: None,
        }
    }

    fn fingerprint_input() -> FingerprintInput {
        FingerprintInput {
            unit_id: "rust-alpha".to_owned(),
            kind: "rust".to_owned(),
            root: "crates/alpha".to_owned(),
            source: BTreeMap::from([
                ("crates/alpha/src/lib.rs".to_owned(), "a".repeat(40)),
                ("crates/alpha/Cargo.toml".to_owned(), "b".repeat(40)),
            ]),
            config_digest: "c".repeat(64),
            recipe_digest: "d".repeat(64),
            pins: BTreeMap::from([("action:checkout".to_owned(), "ref".to_owned())]),
            transitive: BTreeMap::from([("rust-base".to_owned(), "e".repeat(64))]),
            live_state: false,
        }
    }

    fn matching_pair() -> (Evidence, ReuseRequest) {
        let fingerprint = "f".repeat(64);
        let recipe = "9".repeat(64);
        let checks = BTreeSet::from(["ci/rust-alpha/github".to_owned()]);
        (
            Evidence {
                run_id: "run-7".to_owned(),
                fingerprint: fingerprint.clone(),
                recipe: recipe.clone(),
                checks: checks.clone(),
                aggregate_passed: true,
                trust: TrustClass::Trusted,
                fresh_until: None,
            },
            ReuseRequest {
                fingerprint,
                recipe,
                expected_checks: checks,
                required_trust: TrustClass::Trusted,
                live_state: false,
                now: 1_700_000_000,
            },
        )
    }

    #[test]
    fn name_status_parsing_covers_rename_copy_and_rejects_unknown(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let renamed = parse_name_status_line("R100\tcrates/old.rs\tcrates/new.rs")
            .ok_or("rename must parse")?;
        assert_eq!(renamed.path, "crates/new.rs");
        assert_eq!(renamed.previous.as_deref(), Some("crates/old.rs"));
        assert_eq!(renamed.status, ChangeKind::Renamed);
        let copied = parse_name_status_line("C75\tsrc/a.rs\tsrc/b.rs").ok_or("copy must parse")?;
        assert_eq!(copied.path, "src/b.rs");
        assert_eq!(copied.previous, None);
        assert_eq!(copied.status, ChangeKind::Added);
        let typed = parse_name_status_line("T\tlink.rs").ok_or("type change must parse")?;
        assert_eq!(typed.status, ChangeKind::Modified);
        for line in [
            "U\tconflicted.rs",
            "X\tunknown.rs",
            "R\tonly-two.rs",
            "R100\t",
            "M\t",
            "bogus",
            "",
            "M\ta.rs\textra.rs",
        ] {
            assert!(
                parse_name_status_line(line).is_none(),
                "unusable status must fail closed: {line:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn rename_matches_both_sides_and_delete_selects_its_owner(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let units = vec![
            watched("rust-alpha", &["crates/alpha/**"], &[]),
            watched("rust-beta", &["crates/beta/**"], &[]),
        ];
        let selection = select_affected(
            &units,
            &[renamed("crates/alpha/moved.rs", "crates/beta/moved.rs")],
            FULL_SELECTION_PREFIXES,
        )?;
        assert!(!selection.fallback_full);
        assert_eq!(
            selection.required,
            BTreeSet::from(["rust-alpha".to_owned(), "rust-beta".to_owned()])
        );
        assert!(
            selection
                .explanations
                .get("rust-alpha")
                .is_some_and(|reason| reason.contains("crates/alpha/moved.rs")),
            "the old owner matches the rename source: {:?}",
            selection.explanations
        );
        let selection = select_affected(
            &units,
            &[change("crates/alpha/gone.rs", ChangeKind::Deleted)],
            FULL_SELECTION_PREFIXES,
        )?;
        assert_eq!(
            selection.required,
            BTreeSet::from(["rust-alpha".to_owned()])
        );
        Ok(())
    }

    #[test]
    fn unmatched_and_global_paths_fall_back_to_full() -> Result<(), Box<dyn std::error::Error>> {
        let units = vec![
            watched("rust-alpha", &["crates/alpha/**"], &[]),
            watched("node-beta", &["packages/beta/**"], &[]),
        ];
        let selection = select_affected(
            &units,
            &[change("unowned/notes.md", ChangeKind::Added)],
            FULL_SELECTION_PREFIXES,
        )?;
        assert!(selection.fallback_full);
        assert_eq!(selection.required.len(), 2);
        let selection = select_affected(
            &units,
            &[change(".github/workflows/ci.yml", ChangeKind::Modified)],
            FULL_SELECTION_PREFIXES,
        )?;
        assert!(selection.fallback_full);
        assert_eq!(
            selection.full_units, selection.required,
            "a fallback runs every unit at full scope"
        );
        Ok(())
    }

    #[test]
    fn empty_diff_selects_nothing_without_fallback() -> Result<(), Box<dyn std::error::Error>> {
        let units = vec![watched("rust-alpha", &["crates/alpha/**"], &[])];
        let selection = select_affected(&units, &[], FULL_SELECTION_PREFIXES)?;
        assert!(selection.required.is_empty());
        assert!(selection.full_units.is_empty());
        assert!(!selection.fallback_full);
        Ok(())
    }

    #[test]
    fn dependency_closure_pulls_dependents_and_prerequisites_across_kinds(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Edges carry no kind filter: the Node unit depends on the Rust unit
        // and both directions of the closure follow it.
        let units = vec![
            watched("rust-base", &["crates/base/**"], &[]),
            watched("rust-lib", &["crates/lib/**"], &["rust-base"]),
            watched("node-app", &["packages/app/**"], &["rust-lib"]),
            watched("node-sibling", &["packages/sibling/**"], &["rust-base"]),
        ];
        let selection = select_affected(
            &units,
            &[change("crates/lib/api.rs", ChangeKind::Modified)],
            FULL_SELECTION_PREFIXES,
        )?;
        assert_eq!(
            selection.full_units,
            BTreeSet::from(["rust-lib".to_owned(), "node-app".to_owned()]),
            "the changed unit and its downstream dependent run full scope"
        );
        assert_eq!(
            selection.required,
            BTreeSet::from([
                "rust-base".to_owned(),
                "rust-lib".to_owned(),
                "node-app".to_owned()
            ]),
            "the prerequisite joins the required set without pulling its own dependents"
        );
        assert!(
            !selection.required.contains("node-sibling"),
            "a sibling behind the added prerequisite stays out: {:?}",
            selection.required
        );
        Ok(())
    }

    #[test]
    fn selection_fails_closed_on_duplicate_ids_and_bad_globs() {
        let duplicated = vec![
            watched("rust-alpha", &["crates/alpha/**"], &[]),
            watched("rust-alpha", &["crates/other/**"], &[]),
        ];
        let error = select_affected(
            &duplicated,
            &[change("crates/alpha/a.rs", ChangeKind::Modified)],
            FULL_SELECTION_PREFIXES,
        )
        .map_or_else(
            |error| error.to_string(),
            |_| "unexpected success".to_owned(),
        );
        assert!(error.contains("duplicate unit id"), "got: {error}");
        let bad_glob = vec![watched("rust-alpha", &["["], &[])];
        let error = select_affected(
            &bad_glob,
            &[change("crates/alpha/a.rs", ChangeKind::Modified)],
            FULL_SELECTION_PREFIXES,
        )
        .map_or_else(
            |error| error.to_string(),
            |_| "unexpected success".to_owned(),
        );
        assert!(error.contains("invalid watch pattern"), "got: {error}");
    }

    #[test]
    fn fingerprint_is_stable_and_covers_every_input() {
        let base = canonical_fingerprint(&fingerprint_input());
        assert!(is_full_fingerprint(&base));
        assert_eq!(base, canonical_fingerprint(&fingerprint_input()));
        assert!(!is_full_fingerprint("abc"));
        assert!(!is_full_fingerprint(&"g".repeat(64)));
        let mut variants: Vec<(&str, FingerprintInput)> = Vec::new();
        let mut unit = fingerprint_input();
        unit.unit_id = "rust-other".to_owned();
        variants.push(("unit", unit));
        let mut kind = fingerprint_input();
        kind.kind = "node".to_owned();
        variants.push(("kind", kind));
        let mut root = fingerprint_input();
        root.root = "crates/other".to_owned();
        variants.push(("root", root));
        let mut source_changed = fingerprint_input();
        source_changed.insert_source("crates/alpha/src/lib.rs", &"z".repeat(40));
        variants.push(("source", source_changed));
        let mut source_added = fingerprint_input();
        source_added.insert_source("crates/alpha/src/new.rs", &"a".repeat(40));
        variants.push(("source-added", source_added));
        let mut source_removed = fingerprint_input();
        source_removed.remove_source("crates/alpha/Cargo.toml");
        variants.push(("source-removed", source_removed));
        let mut config = fingerprint_input();
        config.config_digest = "0".repeat(64);
        variants.push(("config", config));
        let mut recipe = fingerprint_input();
        recipe.recipe_digest = "1".repeat(64);
        variants.push(("recipe", recipe));
        let mut pins = fingerprint_input();
        pins.pins
            .insert("action:checkout".to_owned(), "moved".to_owned());
        variants.push(("pins", pins));
        let mut transitive = fingerprint_input();
        transitive
            .transitive
            .insert("rust-base".to_owned(), "2".repeat(64));
        variants.push(("transitive", transitive));
        let mut live = fingerprint_input();
        live.live_state = true;
        variants.push(("live", live));
        for (name, input) in &variants {
            assert_ne!(
                base,
                canonical_fingerprint(input),
                "the {name} input must invalidate the fingerprint"
            );
        }
    }

    #[test]
    fn recipe_digest_separates_commands_revision_and_impl() {
        let commands = BTreeMap::from([(
            "github/affected".to_owned(),
            vec!["cargo test --locked".to_owned()],
        )]);
        let base = recipe_digest(&commands, "rev-a", "impl-a");
        assert!(is_full_fingerprint(&base));
        let other_commands = BTreeMap::from([(
            "github/affected".to_owned(),
            vec!["cargo test --locked --all-features".to_owned()],
        )]);
        assert_ne!(base, recipe_digest(&other_commands, "rev-a", "impl-a"));
        assert_ne!(base, recipe_digest(&commands, "rev-b", "impl-a"));
        assert_ne!(base, recipe_digest(&commands, "rev-a", "impl-b"));
    }

    #[test]
    fn check_impl_names_the_generator_revision_and_locator_is_a_prefix() {
        let check = current_check_impl();
        assert!(
            check.contains(crate::GENERATOR_REVISION),
            "the check identity must stamp the generator revision: {check}"
        );
        let fingerprint = "ab".repeat(32);
        assert_eq!(
            artifact_locator("rust-alpha", &fingerprint),
            format!("rust-alpha-{}", "ab".repeat(8))
        );
    }

    #[test]
    fn pins_come_from_the_reviewed_table_with_namespaced_tools() {
        let pins = reviewed_action_pins();
        assert!(
            pins.len() >= 15,
            "the pin table must cover every emitted action: {}",
            pins.len()
        );
        assert!(
            pins.get("action:checkout")
                .is_some_and(|pinned| pinned.contains('@')),
            "pins are immutable references: {pins:?}"
        );
        let tool_pins = BTreeMap::from([("rust-channel".to_owned(), "1.90.0".to_owned())]);
        let merged = unit_pin_set(&tool_pins);
        assert_eq!(
            merged.get("tool:rust-channel").map(String::as_str),
            Some("1.90.0")
        );
        assert!(merged.contains_key("action:checkout"));
    }

    #[test]
    fn reuse_accepts_matching_evidence() {
        let (evidence, request) = matching_pair();
        let decision = validate_reuse(Some(&evidence), &request);
        assert!(
            matches!(decision, ReuseDecision::Reuse { .. }),
            "reasons: {:?}",
            decision.reasons()
        );
        assert!(!decision.reasons().is_empty());
    }

    #[test]
    fn reuse_rejects_without_evidence() {
        let (_, request) = matching_pair();
        let decision = validate_reuse(None, &request);
        assert!(!matches!(decision, ReuseDecision::Reuse { .. }));
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("artifact presence alone never reuses")),
            "reasons: {:?}",
            decision.reasons()
        );
    }

    #[test]
    fn reuse_rejects_fingerprint_recipe_aggregate_and_check_gaps() {
        let (evidence, request) = matching_pair();
        let mut fingerprint = evidence.clone();
        fingerprint.fingerprint = "0".repeat(64);
        assert!(!matches!(
            validate_reuse(Some(&fingerprint), &request),
            ReuseDecision::Reuse { .. }
        ));
        let mut recipe = evidence.clone();
        recipe.recipe = "0".repeat(64);
        let decision = validate_reuse(Some(&recipe), &request);
        assert!(!matches!(decision, ReuseDecision::Reuse { .. }));
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("recipe")),
            "reasons: {:?}",
            decision.reasons()
        );
        let mut aggregate = evidence.clone();
        aggregate.aggregate_passed = false;
        let decision = validate_reuse(Some(&aggregate), &request);
        assert!(!matches!(decision, ReuseDecision::Reuse { .. }));
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("aggregate")),
            "reasons: {:?}",
            decision.reasons()
        );
        let mut checks = evidence.clone();
        checks.checks = BTreeSet::new();
        let decision = validate_reuse(Some(&checks), &request);
        assert!(!matches!(decision, ReuseDecision::Reuse { .. }));
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("expected checks")),
            "reasons: {:?}",
            decision.reasons()
        );
        // A superset still reuses: the producing run proved everything the
        // current run expects, and more.
        let mut extra = evidence.clone();
        extra.checks.insert("ci/other/github".to_owned());
        assert!(matches!(
            validate_reuse(Some(&extra), &request),
            ReuseDecision::Reuse { .. }
        ));
    }

    #[test]
    fn reuse_trust_flows_down_only() {
        assert!(TrustClass::Trusted.satisfies(TrustClass::Trusted));
        assert!(TrustClass::Trusted.satisfies(TrustClass::Untrusted));
        assert!(TrustClass::Untrusted.satisfies(TrustClass::Untrusted));
        assert!(!TrustClass::Untrusted.satisfies(TrustClass::Trusted));
        let (mut evidence, mut request) = matching_pair();
        evidence.trust = TrustClass::Untrusted;
        assert!(!matches!(
            validate_reuse(Some(&evidence), &request),
            ReuseDecision::Reuse { .. }
        ));
        request.required_trust = TrustClass::Untrusted;
        assert!(matches!(
            validate_reuse(Some(&evidence), &request),
            ReuseDecision::Reuse { .. }
        ));
    }

    #[test]
    fn live_state_checks_need_explicit_freshness() {
        let (mut evidence, mut request) = matching_pair();
        request.live_state = true;
        let decision = validate_reuse(Some(&evidence), &request);
        assert!(!matches!(decision, ReuseDecision::Reuse { .. }));
        assert!(
            decision
                .reasons()
                .iter()
                .any(|reason| reason.contains("freshness")),
            "reasons: {:?}",
            decision.reasons()
        );
        evidence.fresh_until = Some(request.now - 1);
        assert!(!matches!(
            validate_reuse(Some(&evidence), &request),
            ReuseDecision::Reuse { .. }
        ));
        evidence.fresh_until = Some(request.now);
        assert!(matches!(
            validate_reuse(Some(&evidence), &request),
            ReuseDecision::Reuse { .. }
        ));
        // Content-addressed checks ignore freshness entirely.
        let (evidence, request) = matching_pair();
        assert!(matches!(
            validate_reuse(Some(&evidence), &request),
            ReuseDecision::Reuse { .. }
        ));
    }

    #[test]
    fn planned_no_work_passes_and_unmarked_emptiness_fails() {
        let verdict = aggregate(&no_work(), &[], &BTreeMap::new());
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(verdict.checks.is_empty());
        let verdict = aggregate(&no_work_unmarked(), &[], &BTreeMap::new());
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("no-work marker")),
            "failures: {:?}",
            verdict.failures
        );
        let contradictory = ExpectedWork {
            units: BTreeMap::from([(
                "rust-alpha".to_owned(),
                expected_unit(&["github"], &[], true, None),
            )]),
            planned_no_work: true,
        };
        let verdict = aggregate(&contradictory, &[], &BTreeMap::new());
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("contradictory")),
            "failures: {:?}",
            verdict.failures
        );
    }

    #[test]
    fn executed_and_reused_successes_pass_with_explanations() {
        let expected = expecting(BTreeMap::from([
            (
                "rust-alpha".to_owned(),
                expected_unit(&["github"], &[], true, None),
            ),
            (
                "node-beta".to_owned(),
                expected_unit(&["github"], &[], true, None),
            ),
        ]));
        let verdict = aggregate(
            &expected,
            &[
                success("rust-alpha", "github", None, None),
                success("node-beta", "github", None, Some("run-7")),
            ],
            &BTreeMap::new(),
        );
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert_eq!(
            verdict.checks,
            BTreeSet::from([
                "ci/node-beta/github".to_owned(),
                "ci/rust-alpha/github".to_owned()
            ])
        );
        let dispositions: Vec<Disposition> = verdict
            .explanations
            .iter()
            .map(|explanation| explanation.disposition)
            .collect();
        assert_eq!(
            dispositions,
            vec![Disposition::Reused, Disposition::Executed],
            "explanations run in stable plan order: {:?}",
            verdict.explanations
        );
        let report = render_report(&verdict);
        assert!(report.starts_with("aggregate: PASS\n"), "report:\n{report}");
        assert!(
            report.contains("reused node-beta github"),
            "report:\n{report}"
        );
    }

    #[test]
    fn missing_results_fail() {
        let expected = expecting(BTreeMap::from([(
            "rust-alpha".to_owned(),
            expected_unit(&["github", "velnor"], &[], true, None),
        )]));
        let verdict = aggregate(
            &expected,
            &[success("rust-alpha", "github", None, None)],
            &BTreeMap::new(),
        );
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("missing result for rust-alpha velnor")),
            "failures: {:?}",
            verdict.failures
        );
    }

    #[test]
    fn unexpected_skip_fails_and_planned_skip_passes() {
        let unexpected = expecting(BTreeMap::from([(
            "rust-alpha".to_owned(),
            expected_unit(&["github"], &[], true, None),
        )]));
        let verdict = aggregate(
            &unexpected,
            &[skipped("rust-alpha", "github", "lane gate closed")],
            &BTreeMap::new(),
        );
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("unexpected skip of rust-alpha github")),
            "failures: {:?}",
            verdict.failures
        );
        assert!(
            verdict
                .explanations
                .iter()
                .any(|explanation| explanation.disposition == Disposition::UnexpectedSkip),
            "explanations: {:?}",
            verdict.explanations
        );
        let planned = expecting(BTreeMap::from([(
            "rust-alpha".to_owned(),
            expected_unit(&["github"], &[], true, Some("lane cannot run kind")),
        )]));
        let verdict = aggregate(
            &planned,
            &[skipped("rust-alpha", "github", "lane gate closed")],
            &BTreeMap::new(),
        );
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(
            verdict
                .explanations
                .iter()
                .any(|explanation| explanation.disposition == Disposition::SkippedPlanned),
            "explanations: {:?}",
            verdict.explanations
        );
    }

    #[test]
    fn cancelled_required_work_fails() {
        let expected = expecting(BTreeMap::from([(
            "rust-alpha".to_owned(),
            expected_unit(&["github"], &[], true, None),
        )]));
        let verdict = aggregate(
            &expected,
            &[ReportedResult {
                unit_id: "rust-alpha".to_owned(),
                lane: "github".to_owned(),
                matrix_entry: None,
                outcome: ReportedOutcome::Cancelled,
                reused_from: None,
            }],
            &BTreeMap::new(),
        );
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("cancelled required work rust-alpha github")),
            "failures: {:?}",
            verdict.failures
        );
    }

    #[test]
    fn incomplete_matrix_fails() {
        let expected = expecting(BTreeMap::from([(
            "node-beta".to_owned(),
            expected_unit(&["github"], &["cpu", "gpu"], true, None),
        )]));
        let verdict = aggregate(
            &expected,
            &[success("node-beta", "github", Some("cpu"), None)],
            &BTreeMap::new(),
        );
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("missing result for node-beta github[gpu]")),
            "failures: {:?}",
            verdict.failures
        );
        assert!(
            verdict
                .explanations
                .iter()
                .any(|explanation| explanation.detail.contains("incomplete matrix")),
            "explanations: {:?}",
            verdict.explanations
        );
    }

    #[test]
    fn failed_prerequisite_blocks_a_successful_dependent() {
        let expected = expecting(BTreeMap::from([
            (
                "rust-base".to_owned(),
                expected_unit(&["github"], &[], true, None),
            ),
            (
                "node-app".to_owned(),
                expected_unit(&["github"], &[], true, None),
            ),
        ]));
        let prerequisites = BTreeMap::from([("node-app".to_owned(), vec!["rust-base".to_owned()])]);
        let verdict = aggregate(
            &expected,
            &[
                ReportedResult {
                    unit_id: "rust-base".to_owned(),
                    lane: "github".to_owned(),
                    matrix_entry: None,
                    outcome: ReportedOutcome::Failure,
                    reused_from: None,
                },
                success("node-app", "github", None, None),
            ],
            &prerequisites,
        );
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure == "prerequisite `rust-base` of `node-app` did not pass"),
            "failures: {:?}",
            verdict.failures
        );
        assert!(
            verdict
                .explanations
                .iter()
                .any(|explanation| explanation.disposition == Disposition::BlockedByPrerequisite),
            "explanations: {:?}",
            verdict.explanations
        );
        let outside = BTreeMap::from([("node-app".to_owned(), vec!["rust-ghost".to_owned()])]);
        let verdict = aggregate(
            &expected,
            &[
                success("rust-base", "github", None, None),
                success("node-app", "github", None, None),
            ],
            &outside,
        );
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("outside the planned work")),
            "failures: {:?}",
            verdict.failures
        );
    }

    #[test]
    fn stable_check_names_are_pure_functions_of_the_work() {
        assert_eq!(REQUIRED_CHECK, "ci-required");
        assert_eq!(
            check_name_for_work("rust-alpha", "github", None),
            "ci/rust-alpha/github"
        );
        assert_eq!(
            check_name_for_work("node-beta", "velnor", Some("gpu")),
            "ci/node-beta/velnor[gpu]"
        );
        let unit = expected_unit(&["github", "velnor"], &["cpu", "gpu"], true, None);
        assert_eq!(
            check_names_for_unit("node-beta", &unit),
            BTreeSet::from([
                "ci/node-beta/github[cpu]".to_owned(),
                "ci/node-beta/github[gpu]".to_owned(),
                "ci/node-beta/velnor[cpu]".to_owned(),
                "ci/node-beta/velnor[gpu]".to_owned(),
            ])
        );
    }

    #[test]
    fn aggregate_files_parses_and_scores() -> Result<(), String> {
        let expected = r#"{
            "units": [
                {"id": "rust-alpha", "lanes": ["github"]},
                {"id": "node-beta", "lanes": ["github"], "planned_skip": "lane cannot run kind"}
            ],
            "prerequisites": {"node-beta": ["rust-alpha"]}
        }"#;
        let results = r#"{
            "results": [
                {"unit": "rust-alpha", "lane": "github", "outcome": "success"},
                {"unit": "node-beta", "lane": "github", "outcome": "skipped", "reason": "lane gate closed"}
            ]
        }"#;
        let verdict = aggregate_files(expected, results)?;
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(aggregate_files("bogus", results).is_err());
        assert!(aggregate_files(expected, "bogus").is_err());
        let missing_reason = r#"{
            "results": [
                {"unit": "rust-alpha", "lane": "github", "outcome": "skipped"}
            ]
        }"#;
        assert!(aggregate_files(expected, missing_reason).is_err());
        let unknown_field = r#"{
            "units": [{"id": "rust-alpha", "lanes": ["github"], "bogus": true}]
        }"#;
        assert!(aggregate_files(unknown_field, results).is_err());
        Ok(())
    }

    impl FingerprintInput {
        fn insert_source(&mut self, path: &str, digest: &str) {
            self.source.insert(path.to_owned(), digest.to_owned());
        }

        fn remove_source(&mut self, path: &str) {
            self.source.remove(path);
        }
    }

    fn config_view() -> UnitConfigView {
        UnitConfigView {
            id: "rust-alpha".to_owned(),
            kind: "rust".to_owned(),
            root: "crates/alpha".to_owned(),
            watch: vec!["crates/alpha/**".to_owned()],
            commands: BTreeMap::from([(
                "github/affected".to_owned(),
                vec!["cargo test --locked".to_owned()],
            )]),
            depends_on: vec!["rust-base".to_owned()],
            tool_version: Some("1.90.0".to_owned()),
            cache_key_files: vec!["Cargo.lock".to_owned()],
            cache_paths: vec!["target".to_owned()],
        }
    }

    #[test]
    fn ls_tree_parsing_rejects_malformed_lines() -> Result<(), Box<dyn std::error::Error>> {
        let (path, sha) = parse_ls_tree_line(
            "100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tcrates/alpha/src/lib.rs",
        )
        .ok_or("a well-formed entry must parse")?;
        assert_eq!(path, "crates/alpha/src/lib.rs");
        assert_eq!(sha, "a".repeat(40));
        for line in [
            "100644 blob aaaa",
            "100644 blob\tmissing-tab-meta",
            "100644 blob aaaa\t",
            "\tpath-only.rs",
            "bogus",
            "",
            "100644 blob",
        ] {
            assert!(
                parse_ls_tree_line(line).is_none(),
                "a malformed entry must fail closed: {line:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn source_digests_match_watch_and_pinned_paths() -> Result<(), Box<dyn std::error::Error>> {
        let tree = [
            "100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tcrates/alpha/src/lib.rs",
            "100644 blob bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\tcrates/alpha/Cargo.toml",
            "100644 blob cccccccccccccccccccccccccccccccccccccccc\tCargo.lock",
            "100644 blob dddddddddddddddddddddddddddddddddddddddd\tcrates/other/src/lib.rs",
        ]
        .join("\n");
        let digests = source_digests(
            &tree,
            &["crates/alpha/**".to_owned()],
            &BTreeSet::from(["Cargo.lock".to_owned()]),
        )?;
        assert_eq!(
            digests,
            BTreeMap::from([
                ("crates/alpha/src/lib.rs".to_owned(), "a".repeat(40)),
                ("crates/alpha/Cargo.toml".to_owned(), "b".repeat(40)),
                ("Cargo.lock".to_owned(), "c".repeat(40)),
            ])
        );
        let error = source_digests(&tree, &["[".to_owned()], &BTreeSet::new()).map_or_else(
            |error| error.to_string(),
            |_| "unexpected success".to_owned(),
        );
        assert!(error.contains("invalid watch pattern"), "got: {error}");
        let error = source_digests("bogus line", &["**".to_owned()], &BTreeSet::new()).map_or_else(
            |error| error.to_string(),
            |_| "unexpected success".to_owned(),
        );
        assert!(error.contains("malformed git ls-tree"), "got: {error}");
        Ok(())
    }

    #[test]
    fn config_digest_covers_every_row_and_ignores_order() {
        let base = config_digest(&config_view());
        assert!(is_full_fingerprint(&base));
        let mut reordered = config_view();
        reordered.watch = vec!["b/**".to_owned(), "a/**".to_owned()];
        let mut reordered_other = config_view();
        reordered_other.watch = vec!["a/**".to_owned(), "b/**".to_owned()];
        assert_eq!(config_digest(&reordered), config_digest(&reordered_other));
        let mut variants: Vec<(&str, UnitConfigView)> = Vec::new();
        let mut id = config_view();
        id.id = "rust-other".to_owned();
        variants.push(("id", id));
        let mut kind = config_view();
        kind.kind = "node".to_owned();
        variants.push(("kind", kind));
        let mut root = config_view();
        root.root = ".".to_owned();
        variants.push(("root", root));
        let mut watch = config_view();
        watch.watch.push("extra/**".to_owned());
        variants.push(("watch", watch));
        let mut commands = config_view();
        commands.commands.insert(
            "velnor/full".to_owned(),
            vec!["cargo test --locked".to_owned()],
        );
        variants.push(("commands", commands));
        let mut depends = config_view();
        depends.depends_on.clear();
        variants.push(("depends", depends));
        let mut tool = config_view();
        tool.tool_version = None;
        variants.push(("tool", tool));
        let mut keys = config_view();
        keys.cache_key_files.clear();
        variants.push(("keys", keys));
        let mut paths = config_view();
        paths.cache_paths.clear();
        variants.push(("paths", paths));
        for (name, view) in &variants {
            assert_ne!(
                base,
                config_digest(view),
                "the {name} row must invalidate the config digest"
            );
        }
    }

    #[test]
    fn reuse_rejects_truncated_digests() {
        let (evidence, request) = matching_pair();
        for (name, mut evidence, mut request) in [
            ("evidence", evidence.clone(), request.clone()),
            ("request", evidence.clone(), request.clone()),
        ] {
            if name == "evidence" {
                evidence.fingerprint = "f".repeat(16);
            } else {
                request.recipe = "9".repeat(16);
            }
            let decision = validate_reuse(Some(&evidence), &request);
            assert!(!matches!(decision, ReuseDecision::Reuse { .. }));
            assert!(
                decision
                    .reasons()
                    .iter()
                    .any(|reason| reason.contains("not a full digest")),
                "reasons: {:?}",
                decision.reasons()
            );
        }
    }

    #[test]
    fn trust_parsing_accepts_only_the_two_classes() {
        assert_eq!(TrustClass::parse("trusted"), Ok(TrustClass::Trusted));
        assert_eq!(TrustClass::parse("untrusted"), Ok(TrustClass::Untrusted));
        assert!(TrustClass::parse("partially").is_err());
    }

    #[test]
    fn fallback_selection_marks_the_full_set_with_its_reason() {
        let units = vec![watched("rust-alpha", &["crates/alpha/**"], &[])];
        let selection = fallback_selection(&units, "git diff unavailable; fell back to full");
        assert!(selection.fallback_full);
        assert_eq!(
            selection.required,
            BTreeSet::from(["rust-alpha".to_owned()])
        );
        assert_eq!(
            selection.explanations.get("rust-alpha").map(String::as_str),
            Some("git diff unavailable; fell back to full")
        );
    }

    #[test]
    fn reuse_decision_files_score_and_render() -> Result<(), String> {
        let fingerprint = "f".repeat(64);
        let recipe = "9".repeat(64);
        let evidence = format!(
            r#"{{"run_id": "run-7", "fingerprint": "{fingerprint}", "recipe": "{recipe}", "checks": ["ci/rust-alpha/github"], "aggregate_passed": true, "trust": "trusted"}}"#
        );
        let request = format!(
            r#"{{"fingerprint": "{fingerprint}", "recipe": "{recipe}", "expected_checks": ["ci/rust-alpha/github"], "required_trust": "trusted", "now": 1700000000}}"#
        );
        let decision = reuse_decision_files(&evidence, &request, None)?;
        assert!(
            matches!(decision, ReuseDecision::Reuse { .. }),
            "reasons: {:?}",
            decision.reasons()
        );
        let report = render_decision(&decision);
        assert!(report.starts_with("reuse run-7\n"), "report:\n{report}");
        let stale = format!(
            r#"{{"fingerprint": "{fingerprint}", "recipe": "{recipe}", "expected_checks": ["ci/rust-alpha/github"], "required_trust": "trusted", "live_state": true, "now": 1700000000}}"#
        );
        let decision = reuse_decision_files(&evidence, &stale, None)?;
        assert!(!matches!(decision, ReuseDecision::Reuse { .. }));
        assert!(render_decision(&decision).starts_with("execute\n"));
        assert!(reuse_decision_files("bogus", &request, None).is_err());
        assert!(reuse_decision_files(&evidence, "bogus", None).is_err());
        let no_clock = format!(
            r#"{{"fingerprint": "{fingerprint}", "recipe": "{recipe}", "expected_checks": [], "required_trust": "trusted"}}"#
        );
        assert!(reuse_decision_files(&evidence, &no_clock, None).is_err());
        assert!(reuse_decision_files(&evidence, &no_clock, Some(1)).is_ok());
        Ok(())
    }
}

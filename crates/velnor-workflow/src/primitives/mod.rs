//! The declared CI surface: generic render primitives over scan + config.
//!
//! Every CI-side workflow family is emitted by a primitive in this registry. A
//! primitive is named by a `[[declare]]` row in the repository-owned generation
//! config, receives the scanned shape plus the resolved lane, cache, and pin
//! contracts, and contributes one of three things: workflow files, CI graph
//! nodes for the aggregate workflows to compose, or unit contract updates.
//!
//! Nothing in this module may name a repository. Repository knowledge enters
//! only through the scan (`RepositoryShape`) and the repo-owned config, which
//! is what makes one primitive able to render any repository's surface.

mod aggregate;
mod cache;
mod ir;
mod lanes;
mod pipeline;
mod plan;
mod regen;
pub(crate) mod release;
pub(crate) mod watch;

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::RepoGenerationConfig;
use crate::scan::RepositoryShape;
use crate::{
    nested_unit_workflow_file, CachePurpose, CacheSpec, GeneratorError, ProjectConfig, RunnerMode,
    Unit, UnitKind,
};

pub(crate) use ir::{
    checks_env, render_cargo_source_preparation, render_pinned_toolchain_steps,
    render_retained_output_cache_note, WorkflowIr, WorkflowKind,
};

/// Default `timeout-minutes` for a unit verification job.
pub(crate) const DEFAULT_UNIT_TIMEOUT_MINUTES: u32 = 45;

/// A primitive id the registry knows how to build.
pub(crate) const AFFECTED_PLAN: &str = "affected-plan";
pub(crate) const UNIT_AGGREGATION: &str = "unit-aggregation";
pub(crate) const LANE_MATRIX: &str = "lane-matrix";
pub(crate) const CACHE_CONTRACT: &str = "cache-contract";
pub(crate) const WATCH_GRAPH: &str = "watch-graph";
pub(crate) const REGEN_GATE: &str = "regen-gate";
pub(crate) const RUST_CRATE: &str = "rust-crate-pipeline";
pub(crate) const BUN_PACKAGE: &str = "bun-package-pipeline";
pub(crate) const NODE_PACKAGE: &str = "node-package-pipeline";
pub(crate) const GRADLE_PROJECT: &str = "gradle-project-pipeline";
pub(crate) const SWIFT_PACKAGE: &str = "swift-package-pipeline";
pub(crate) const OPENTOFU: &str = "opentofu-pipeline";
pub(crate) const DOCKER_IMAGE: &str = "docker-image-pipeline";
pub(crate) const HOMEBREW_TAP: &str = "homebrew-tap-pipeline";
pub(crate) const DOCS_LINT: &str = "docs-lint-pipeline";
/// The `release.yml` publisher: tag-triggered, verify-then-publish.
pub(crate) const RELEASE: &str = "release";
/// The `preview.yml` rolling artifact lane.
pub(crate) const PREVIEW: &str = "preview";
/// The `maintenance.yml` cache-hygiene workflow.
pub(crate) const MAINTENANCE: &str = "maintenance";
/// The release artifact provenance signer.
pub(crate) const RELEASE_SIGNER: &str = "release-signer";
/// A reviewed workflow body declared verbatim by the repository.
pub(crate) const STATIC_WORKFLOW: &str = "static-workflow";

/// The pinned action references every emitted job uses.
///
/// The table is resolved once per generation so a primitive never spells a
/// commit SHA, and so a later phase can admit reviewed pins from the config
/// without touching a renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Pins {
    pub(crate) checkout: &'static str,
    pub(crate) cache_restore: &'static str,
    pub(crate) cache_save: &'static str,
    pub(crate) opentofu_setup: &'static str,
    pub(crate) upload_artifact: &'static str,
    pub(crate) download_artifact: &'static str,
    pub(crate) bun: &'static str,
    pub(crate) node: &'static str,
    pub(crate) rust_tool: &'static str,
    pub(crate) mise: &'static str,
    pub(crate) gradle: &'static str,
    pub(crate) sccache: &'static str,
    pub(crate) mr_boxington: &'static str,
    pub(crate) github_runtime: &'static str,
    pub(crate) docker_buildx: &'static str,
}

impl Pins {
    /// The reviewed pin table the generator ships with.
    pub(crate) fn resolved() -> Self {
        Self {
            checkout: crate::ActionPin::Checkout.reference(),
            cache_restore: crate::ActionPin::CacheRestore.reference(),
            cache_save: crate::ActionPin::CacheSave.reference(),
            opentofu_setup: crate::ActionPin::OpenTofuSetup.reference(),
            upload_artifact: crate::ActionPin::UploadArtifact.reference(),
            download_artifact: crate::ActionPin::DownloadArtifact.reference(),
            bun: crate::ActionPin::Bun.reference(),
            node: crate::ActionPin::Node.reference(),
            rust_tool: crate::ActionPin::RustTool.reference(),
            mise: crate::ActionPin::Mise.reference(),
            gradle: crate::ActionPin::Gradle.reference(),
            sccache: crate::ActionPin::Sccache.reference(),
            mr_boxington: crate::ActionPin::MrBoxington.reference(),
            github_runtime: crate::ActionPin::GithubRuntime.reference(),
            docker_buildx: crate::ActionPin::DockerBuildx.reference(),
        }
    }
}

/// One lane job of a nested unit workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LaneJob {
    pub(crate) lane: RunnerMode,
    /// The hosted lane is the only lane allowed to save a cache entry: entries
    /// are written from trusted events only.
    pub(crate) cache_save: bool,
    /// The self-hosted lane runs trusted events only and says so with an
    /// explicit default-branch gate.
    pub(crate) trusted: bool,
}

/// Which cache transport a unit job uses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CacheBackend {
    /// The detected contract: the shared object cache when the repository's
    /// Rust commands run under it, otherwise the actions cache.
    Detected,
    /// Force the actions cache steps on, whatever the detected contract is.
    Actions,
    /// Force the shared object cache setup on.
    ObjectCache,
}

impl CacheBackend {
    /// Whether the actions cache restore and save steps are emitted for `unit`.
    ///
    /// The detected policy classifies by purpose: Cargo-source and tool caches
    /// ride the actions cache even when the unit compiles under Mr. Boxington
    /// — the object transport moves compiler state, not Cargo's registry
    /// archives, extracted sources, or Git dependencies. Raw output caches are
    /// the one suppression: the object transport already carries workspace
    /// state, so a declared output cache needs its justification on record.
    pub(crate) fn enables_actions_cache(self, ir: &WorkflowIr, unit: &Unit) -> bool {
        match self {
            Self::Detected => match unit.cache.as_ref().map(|cache| cache.purpose) {
                None => false,
                // A toolchain cache is generator-internal, never a unit's
                // declared contract, so it never arrives through this match.
                Some(
                    CachePurpose::CargoSources | CachePurpose::Toolchains | CachePurpose::Generic,
                ) => true,
                Some(CachePurpose::Outputs) => {
                    !ir.uses_mr_boxington(unit)
                        || unit
                            .cache
                            .as_ref()
                            .is_some_and(CacheSpec::justified_output_alongside_mr_boxington)
                }
            },
            Self::Actions => true,
            Self::ObjectCache => false,
        }
    }
}

/// Reject a raw output cache that would ride alongside the Mr. Boxington
/// object transport without a recorded justification: the two transports move
/// the same workspace state, so keeping both needs a reason on record.
///
/// # Errors
/// Returns a usage error for an unjustified or multi-line justification.
pub(crate) fn validate_cache_transports(ir: &WorkflowIr) -> Result<(), GeneratorError> {
    for unit in &ir.units {
        validate_cache_transports_for_unit(ir, unit)?;
    }
    Ok(())
}

/// Validate one unit's cache transport combination.
///
/// # Errors
/// Returns a usage error for an unjustified or multi-line justification.
pub(crate) fn validate_cache_transports_for_unit(
    ir: &WorkflowIr,
    unit: &Unit,
) -> Result<(), GeneratorError> {
    let Some(cache) = &unit.cache else {
        return Ok(());
    };
    if cache.purpose != CachePurpose::Outputs || !ir.uses_mr_boxington(unit) {
        return Ok(());
    }
    if !cache.justified_output_alongside_mr_boxington() {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares a raw output cache alongside the Mr. Boxington object transport; drop the output cache or set `mbx_output_cache_justification` to record why both transports are kept",
            unit.id
        )));
    }
    if cache
        .mbx_output_cache_justification
        .as_deref()
        .unwrap_or_default()
        .contains(['\n', '\r'])
    {
        return Err(GeneratorError::usage(format!(
            "unit `{}` uses a multi-line `mbx_output_cache_justification`; keep it to one line",
            unit.id
        )));
    }
    Ok(())
}

/// Everything a declared unit pipeline may tune for one unit's surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UnitContract {
    pub(crate) lanes: Vec<LaneJob>,
    pub(crate) timeout_minutes: u32,
    pub(crate) cache: CacheBackend,
    /// Cache entries are saved on trusted events only, which is a property of
    /// the workflow kind the aggregate owns, never of the unit.
    pub(crate) cache_save: bool,
}

/// What one primitive contributed to the declared surface.
#[derive(Default)]
pub(crate) struct Rendered {
    /// Workflow files, keyed by repository-relative path.
    pub(crate) files: BTreeMap<PathBuf, String>,
    /// CI graph nodes the aggregate workflows compose.
    pub(crate) nodes: Vec<GraphNode>,
    /// Unit contract updates, replacing the scanned unit of the same id.
    pub(crate) units: Vec<Unit>,
}

/// One node of the generated CI graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GraphNode {
    /// The affected-selection plan job an aggregate workflow starts from.
    Plan {
        /// The rendered `plan:` job block.
        job: String,
    },
    /// One verification unit's reusable-workflow caller.
    Unit {
        unit_id: String,
        job_id: String,
        /// The caller job's display name.
        name: String,
        /// The nested workflow file the caller invokes.
        file: String,
    },
}

impl GraphNode {
    /// The rendered `plan:` job block a contributed plan node carries.
    pub(crate) fn plan_job(&self) -> Option<&str> {
        match self {
            Self::Plan { job } => Some(job),
            Self::Unit { .. } => None,
        }
    }

    fn as_unit(&self) -> Option<(&str, &str, &str, &str)> {
        match self {
            Self::Plan { .. } => None,
            Self::Unit {
                unit_id,
                job_id,
                name,
                file,
            } => Some((unit_id, job_id, name, file)),
        }
    }
}

/// Read-only render input for one primitive invocation.
pub(crate) struct RenderCtx<'a> {
    /// The scanned repository root. A primitive may read repository files the
    /// walk observed, and must never execute project code.
    pub(crate) root: &'a std::path::Path,
    /// What the scan proved about the repository.
    pub(crate) shape: &'a RepositoryShape,
    /// The generated project contract the surface is rendered for.
    pub(crate) config: &'a ProjectConfig,
    /// The unit being rendered, for per-unit primitives.
    pub(crate) unit: Option<&'a Unit>,
    /// The units the current declaration applies to, for unit-contract
    /// primitives: the ids the row named, resolved against the scan.
    pub(crate) units: &'a [&'a Unit],
    /// The workflow file the declaration renders into, when the family renders
    /// one.
    pub(crate) file: Option<&'a str>,
    /// The id of the primitive being rendered, for error messages.
    pub(crate) family: &'static str,
    /// The pinned action references every emitted job uses. A primitive that
    /// spells an action reference itself takes it from here, never from a
    /// literal, so the reviewed pin table stays the single source.
    #[expect(dead_code, reason = "primitives render pins through the lane context")]
    pub(crate) pins: &'a Pins,
    /// The resolved lane matrix and the toolchain environment behind it.
    pub(crate) lanes: &'a lanes::ResolvedLanes,
    /// The resolved cache contract.
    pub(crate) cache: &'a cache::ResolvedCache,
    /// The CI graph nodes contributed by the primitives rendered so far.
    pub(crate) nodes: &'a [GraphNode],
}

/// One render primitive of the CI surface.
pub(crate) trait Primitive: Send + Sync {
    /// The name a `[[declare]]` row uses to invoke this primitive.
    fn id(&self) -> &'static str;

    /// The declared arguments this primitive accepts, with their meaning.
    /// Anything not listed here is rejected: a typo'd config must fail closed.
    fn schema(&self) -> &'static [&'static str];

    /// Render this primitive's contribution to the surface.
    ///
    /// # Errors
    /// Returns a usage error naming the offending declared value.
    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError>;
}

/// The declared arguments of one `[[declare]]` row.
pub(crate) struct Args<'a>(&'a BTreeMap<String, toml::Value>);

impl Args<'_> {
    /// The known argument names, for an error message.
    pub(crate) fn keys(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }

    pub(crate) fn strings(&self, key: &str) -> Result<Option<Vec<String>>, GeneratorError> {
        match self.0.get(key) {
            None => Ok(None),
            Some(toml::Value::Array(items)) => {
                let mut values = Vec::new();
                for item in items {
                    match item {
                        toml::Value::String(value) => values.push(value.clone()),
                        other => return Err(unexpected(key, other, "a string")),
                    }
                }
                Ok(Some(values))
            }
            Some(other) => Err(unexpected(key, other, "an array of strings")),
        }
    }

    pub(crate) fn string(&self, key: &str) -> Result<Option<String>, GeneratorError> {
        match self.0.get(key) {
            None => Ok(None),
            Some(toml::Value::String(value)) => Ok(Some(value.clone())),
            Some(other) => Err(unexpected(key, other, "a string")),
        }
    }

    pub(crate) fn integer(&self, key: &str) -> Result<Option<i64>, GeneratorError> {
        match self.0.get(key) {
            None => Ok(None),
            Some(toml::Value::Integer(value)) => Ok(Some(*value)),
            Some(other) => Err(unexpected(key, other, "an integer")),
        }
    }

    /// A table of string arrays, the form per-unit additions take.
    pub(crate) fn string_tables(
        &self,
        key: &str,
    ) -> Result<Option<BTreeMap<String, Vec<String>>>, GeneratorError> {
        match self.0.get(key) {
            None => Ok(None),
            Some(toml::Value::Table(table)) => {
                let mut mapped = BTreeMap::new();
                for (name, value) in table {
                    let toml::Value::Array(items) = value else {
                        return Err(unexpected(key, value, "an array of strings"));
                    };
                    let mut paths = Vec::new();
                    for item in items {
                        match item {
                            toml::Value::String(path) => paths.push(path.clone()),
                            other => return Err(unexpected(key, other, "a string")),
                        }
                    }
                    mapped.insert(name.clone(), paths);
                }
                Ok(Some(mapped))
            }
            Some(other) => Err(unexpected(key, other, "a table")),
        }
    }

    /// A table of string arrays indexed by unit id, validated against the scan.
    pub(crate) fn unit_strings(
        &self,
        ctx: &RenderCtx<'_>,
        key: &str,
    ) -> Result<Option<BTreeMap<String, Vec<String>>>, GeneratorError> {
        let tables = self.string_tables(key)?;
        if let Some(tables) = &tables {
            for unit in tables.keys() {
                if !ctx.shape.unit_ids().any(|candidate| candidate == unit) {
                    return Err(GeneratorError::usage(format!(
                        "`{key}` names unit `{unit}`, which the scan did not produce; available units: {}",
                        ctx.shape.unit_ids().collect::<Vec<_>>().join(", ")
                    )));
                }
            }
        }
        Ok(tables)
    }
}

fn unexpected(key: &str, found: &toml::Value, expected: &str) -> GeneratorError {
    GeneratorError::usage(format!(
        "`[[declare]]` argument `{key}` must be {expected}, found {found}"
    ))
}

/// The unit kinds each per-unit pipeline primitive renders, in registry order.
pub(crate) fn pipeline_id(kind: UnitKind) -> &'static str {
    match kind {
        UnitKind::Rust => RUST_CRATE,
        UnitKind::Bun => BUN_PACKAGE,
        UnitKind::Node => NODE_PACKAGE,
        UnitKind::Gradle => GRADLE_PROJECT,
        UnitKind::Swift => SWIFT_PACKAGE,
        UnitKind::OpenTofu => OPENTOFU,
        UnitKind::Docker => DOCKER_IMAGE,
        UnitKind::Homebrew => HOMEBREW_TAP,
        UnitKind::Docs => DOCS_LINT,
    }
}

/// The primitive registry, in family order.
pub(crate) fn registry() -> Vec<Box<dyn Primitive>> {
    vec![
        Box::new(plan::AffectedPlan),
        Box::new(aggregate::UnitAggregation),
        Box::new(lanes::LaneMatrix),
        Box::new(cache::CacheContract),
        Box::new(watch::WatchGraph),
        Box::new(regen::RegenGate),
        Box::new(pipeline::RustCrate),
        Box::new(pipeline::BunPackage),
        Box::new(pipeline::NodePackage),
        Box::new(pipeline::GradleProject),
        Box::new(pipeline::SwiftPackage),
        Box::new(pipeline::OpenTofu),
        Box::new(pipeline::DockerImage),
        Box::new(pipeline::HomebrewTap),
        Box::new(pipeline::DocsLint),
        Box::new(release::Release),
        Box::new(release::Preview),
        Box::new(release::Maintenance),
        Box::new(release::ReleaseSigner),
        Box::new(release::StaticWorkflow),
    ]
}

fn lookup(id: &str) -> Result<&'static dyn Primitive, GeneratorError> {
    static REGISTRY: std::sync::OnceLock<Vec<Box<dyn Primitive>>> = std::sync::OnceLock::new();
    let registry = REGISTRY.get_or_init(registry);
    registry
        .iter()
        .map(AsRef::as_ref)
        .find(|primitive| primitive.id() == id)
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "`[[declare]]` names primitive `{id}`, which this generator does not render; known primitives: {}",
                registry
                    .iter()
                    .map(|primitive| primitive.id())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// One declared primitive invocation, resolved from a config row.
#[derive(Clone, Debug)]
struct Declaration {
    primitive: String,
    units: Vec<String>,
    file: Option<String>,
    args: BTreeMap<String, toml::Value>,
}

impl Declaration {
    fn from_row(row: &crate::config::DeclareRow) -> Self {
        Self {
            primitive: row.primitive().to_owned(),
            units: row.units().to_vec(),
            file: row.file().map(str::to_owned),
            args: row.args().clone(),
        }
    }
}

/// The declared CI surface: the files, the workflow names they cover, and the
/// unit contracts the surface was rendered from.
pub(crate) struct Surface {
    /// Files the declared primitives emitted.
    pub(crate) files: BTreeMap<PathBuf, String>,
    /// Unit contracts after the unit-contract primitives ran.
    pub(crate) units: Vec<Unit>,
    /// Workflow file names a declared row adds to the owned surface: a
    /// release-side family the scan did not own becomes owned by declaring it.
    pub(crate) added_files: Vec<String>,
}

/// Render the declared CI surface for a scanned repository.
///
/// A repository without declared rows keeps the generator's default surface:
/// the same families, rendered by the same primitives, with the default
/// arguments. Declared rows replace the default row of the family they name, so
/// a repository adopts the config one section at a time.
///
/// # Errors
/// Returns a usage error for a declaration that names an unknown primitive, a
/// unit the scan did not produce, a file the surface does not own, or an
/// argument the primitive does not accept.
pub(crate) fn generate(
    root: &std::path::Path,
    shape: &RepositoryShape,
    config: &ProjectConfig,
    generation: Option<&RepoGenerationConfig>,
) -> Result<Surface, GeneratorError> {
    let declared = match generation {
        Some(generation) => generation
            .declare()
            .iter()
            .map(Declaration::from_row)
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let rows = rows_for(config, &declared)?;

    // Contracts first: every later primitive renders against them.
    let cache = cache::resolve(&rows)?;
    let pins = Pins::resolved();

    // Unit contracts, in declaration order, before any file is rendered: the
    // watch graph and the regeneration gate define what a unit is for this
    // surface, and the project config records the same result.
    let lanes = lanes::resolve(config, &rows)?;
    let mut units = config.units.clone();
    for row in rows.iter().filter(|row| row.unit_contract) {
        let primitive = lookup(&row.primitive)?;
        // An empty `units` list means every unit: a contract is repository
        // policy about the whole surface unless it narrows itself.
        let scoped: Vec<&Unit> = if row.units.is_empty() {
            units.iter().collect()
        } else {
            units
                .iter()
                .filter(|unit| row.units.iter().any(|id| id == &unit.id))
                .collect()
        };
        let rendered = primitive.render(
            &ctx(
                root,
                shape,
                config,
                None,
                &scoped,
                row.file.as_deref(),
                primitive.id(),
                &pins,
                &lanes,
                &cache,
                &[],
            ),
            &row.args(),
        )?;
        apply_units(&mut units, rendered.units, &row.primitive)?;
    }
    let mut resolved = config.clone();
    resolved.units.clone_from(&units);
    let lanes = lanes::resolve(&resolved, &rows)?;

    // Per-unit pipelines, then the plan, then the aggregates that compose both.
    let mut files = BTreeMap::new();
    let mut nodes = Vec::new();
    for row in rows.iter().filter(|row| !row.unit_contract) {
        let unit = row
            .units
            .first()
            .and_then(|id| units.iter().find(|unit| &unit.id == id));
        let primitive = lookup(&row.primitive)?;
        let rendered = primitive.render(
            &ctx(
                root,
                shape,
                &resolved,
                unit,
                &[],
                row.file.as_deref(),
                primitive.id(),
                &pins,
                &lanes,
                &cache,
                &nodes,
            ),
            &row.args(),
        )?;
        for (path, content) in rendered.files {
            if files.insert(path.clone(), content).is_some() {
                return Err(GeneratorError::usage(format!(
                    "two declared primitives both render {}",
                    path.display()
                )));
            }
        }
        nodes.extend(rendered.nodes);
    }

    // A declared file family can add a workflow the scan did not own: a
    // release lane becomes part of the surface by being declared, and the
    // caller extends the owned file list so it is emitted and recorded.
    let mut added_files: Vec<String> = Vec::new();
    for row in &rows {
        if row.unit_contract || !release::is_release_side(&row.primitive) {
            continue;
        }
        let Some(file) = &row.file else {
            continue;
        };
        if config.workflow_files.iter().any(|owned| owned == file) {
            continue;
        }
        reject_owned_file(config, file, &row.primitive)?;
        if !added_files.contains(file) {
            added_files.push(file.clone());
        }
    }
    added_files.sort();
    Ok(Surface {
        files,
        units,
        added_files,
    })
}

/// A file a non-surface renderer owns cannot be added by a declaration: the
/// caller would emit the declared bytes and then have the legacy render
/// overwrite them. Naming the renderer that owns the file turns the silent
/// clobber into a usage error.
fn reject_owned_file(
    config: &ProjectConfig,
    file: &str,
    primitive: &str,
) -> Result<(), GeneratorError> {
    // The nested per-unit workflows are rendered for every scanned unit unless
    // the surface was adopted as reviewed templates.
    if !config.adopted_workflow_surface
        && let Some(unit) = config
            .units
            .iter()
            .find(|unit| nested_unit_workflow_file(unit) == file)
    {
        return Err(GeneratorError::usage(format!(
            "`[[declare]]` primitive `{primitive}` declares `{file}`, which the `{}` family renders for unit `{}`; declare the unit's pipeline or adopt the workflow surface instead",
            pipeline_id(unit.kind),
            unit.id
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ctx<'a>(
    root: &'a std::path::Path,
    shape: &'a RepositoryShape,
    config: &'a ProjectConfig,
    unit: Option<&'a Unit>,
    units: &'a [&'a Unit],
    file: Option<&'a str>,
    family: &'static str,
    pins: &'a Pins,
    lanes: &'a lanes::ResolvedLanes,
    cache: &'a cache::ResolvedCache,
    nodes: &'a [GraphNode],
) -> RenderCtx<'a> {
    RenderCtx {
        root,
        shape,
        config,
        unit,
        units,
        file,
        family,
        pins,
        lanes,
        cache,
        nodes,
    }
}

fn apply_units(
    units: &mut [Unit],
    updates: Vec<Unit>,
    primitive: &str,
) -> Result<(), GeneratorError> {
    for update in updates {
        match units.iter_mut().find(|unit| unit.id == update.id) {
            Some(unit) => *unit = update,
            None => {
                return Err(GeneratorError::usage(format!(
                    "primitive `{primitive}` returned unit `{}`, which the scan did not produce",
                    update.id
                )))
            }
        }
    }
    Ok(())
}

/// The resolved declaration rows: the repository's rows over the default rows.
///
/// Defaults cover every family the generated surface owns, so a repository
/// without declared rows is rendered exactly as before. A declared row replaces
/// the default row of the family it names, which is what lets a repository
/// adopt the config one section at a time.
fn rows_for(
    config: &ProjectConfig,
    declared: &[Declaration],
) -> Result<Vec<ResolvedRow>, GeneratorError> {
    let mut rows: Vec<ResolvedRow> = Vec::new();
    // Contract rows configure the generation; they have no default row.
    let contracts = declared
        .iter()
        .filter(|row| is_unit_contract(&row.primitive))
        .cloned()
        .collect::<Vec<_>>();
    // Nested unit workflows. An adopted surface owns its workflow files as
    // reviewed templates and declares no per-unit family at all.
    if !config.adopted_workflow_surface {
        for unit in &config.units {
            let primitive = pipeline_id(unit.kind);
            if let Some(row) = declared
                .iter()
                .filter(|row| is_pipeline(&row.primitive))
                .find(|row| row.units.as_slice() == [unit.id.as_str()])
            {
                rows.push(ResolvedRow::declared(row));
                continue;
            }
            // A family the config owns is rendered from its rows only.
            if !declared
                .iter()
                .any(|row| is_pipeline(&row.primitive) && row.primitive == primitive)
            {
                rows.push(ResolvedRow::default_pipeline(primitive, unit));
            }
        }
    }
    // The plan job, then the aggregate workflows that compose it: the plan
    // contributes its node before any aggregate renders.
    let mut aggregates = Vec::new();
    for file in ["ci-pr.yml", "ci-main.yml", "nightly.yml"] {
        if !config.workflow_files.iter().any(|owned| owned == file) {
            continue;
        }
        match declared
            .iter()
            .find(|row| row.primitive == UNIT_AGGREGATION && row.file.as_deref() == Some(file))
        {
            Some(row) => aggregates.push(ResolvedRow::declared(row)),
            None => aggregates.push(ResolvedRow::default_aggregate(file)),
        }
    }
    if !aggregates.is_empty() {
        match declared.iter().find(|row| row.primitive == AFFECTED_PLAN) {
            Some(row) => rows.push(ResolvedRow::declared(row)),
            None => rows.push(ResolvedRow::default_plan()),
        }
    }
    rows.extend(aggregates);
    rows.extend(contracts.iter().map(ResolvedRow::declared));
    // Declared release-side families: the file rows they name, rendered by the
    // family's primitive.
    for row in declared
        .iter()
        .filter(|row| release::is_release_side(&row.primitive))
    {
        rows.push(ResolvedRow::declared(row));
    }
    // Release-side families: a default row per owned file, rendered by the
    // same primitives a declared row uses, unless the config declares the
    // family itself.
    if !config.adopted_workflow_surface {
        for (file, family) in release::RELEASE_SIDE_FILES {
            if !config.workflow_files.iter().any(|owned| owned == file) {
                continue;
            }
            if declared.iter().any(|row| row.file.as_deref() == Some(file)) {
                continue;
            }
            // A repository without a release contract omits the publisher.
            if *family == RELEASE && config.release.is_none() {
                continue;
            }
            rows.push(ResolvedRow {
                primitive: (*family).to_owned(),
                units: Vec::new(),
                file: Some((*file).to_owned()),
                unit_contract: false,
                args: BTreeMap::new(),
            });
        }
    }
    validate(config, declared, &rows)?;
    Ok(rows)
}

/// The per-unit pipeline primitive that renders `kind`.
/// The per-unit pipeline families: one file per unit, one graph node per unit.
const PIPELINES: &[&str] = &[
    RUST_CRATE,
    BUN_PACKAGE,
    NODE_PACKAGE,
    GRADLE_PROJECT,
    SWIFT_PACKAGE,
    OPENTOFU,
    DOCKER_IMAGE,
    HOMEBREW_TAP,
    DOCS_LINT,
];

fn is_pipeline(primitive: &str) -> bool {
    PIPELINES.contains(&primitive)
}

/// Contract rows mutate unit contracts before any file is rendered.
fn is_unit_contract(primitive: &str) -> bool {
    matches!(primitive, WATCH_GRAPH | REGEN_GATE)
}

#[derive(Clone, Debug)]
struct ResolvedRow {
    primitive: String,
    units: Vec<String>,
    file: Option<String>,
    unit_contract: bool,
    args: BTreeMap<String, toml::Value>,
}

impl ResolvedRow {
    fn declared(row: &Declaration) -> Self {
        Self {
            primitive: row.primitive.clone(),
            units: row.units.clone(),
            file: row.file.clone(),
            unit_contract: is_unit_contract(&row.primitive),
            args: row.args.clone(),
        }
    }

    fn default_pipeline(primitive: &str, unit: &Unit) -> Self {
        Self {
            primitive: primitive.to_owned(),
            units: vec![unit.id.clone()],
            file: Some(crate::nested_unit_workflow_file(unit)),
            unit_contract: false,
            args: BTreeMap::new(),
        }
    }

    fn default_aggregate(file: &str) -> Self {
        Self {
            primitive: UNIT_AGGREGATION.to_owned(),
            units: Vec::new(),
            file: Some(file.to_owned()),
            unit_contract: false,
            args: BTreeMap::new(),
        }
    }

    fn default_plan() -> Self {
        Self {
            primitive: AFFECTED_PLAN.to_owned(),
            units: Vec::new(),
            file: None,
            unit_contract: false,
            args: BTreeMap::new(),
        }
    }

    fn args(&self) -> Args<'_> {
        Args(&self.args)
    }

    /// The unit the row renders, when it names exactly one.
    fn nested_file<'a>(&self, config: &'a ProjectConfig) -> Option<&'a Unit> {
        config
            .units
            .iter()
            .find(|unit| self.units.as_slice() == [unit.id.as_str()])
    }
}

/// Referential validation of the merged declaration set against the surface it
/// is about to render.
#[expect(
    clippy::too_many_lines,
    reason = "each validation rule is one fail-closed clause over the declared rows"
)]
fn validate(
    config: &ProjectConfig,
    declared: &[Declaration],
    rows: &[ResolvedRow],
) -> Result<(), GeneratorError> {
    // A declared id must be one the registry knows, and only the arguments its
    // schema names: a typo'd config must fail closed, never no-op.
    for row in declared {
        let primitive = lookup(&row.primitive)?;
        let schema = primitive.schema();
        for key in Args(&row.args).keys() {
            if !schema.contains(&key) {
                return Err(GeneratorError::usage(format!(
                    "`[[declare]]` primitive `{}` does not accept the argument `{key}`; accepted arguments: {}",
                    row.primitive,
                    schema.join(", ")
                )));
            }
        }
    }
    for row in rows {
        if row.unit_contract {
            if let Some(file) = &row.file {
                return Err(GeneratorError::usage(format!(
                    "`[[declare]]` primitive `{}` renders no workflow file, so it takes no `file`; remove `{file}`",
                    row.primitive
                )));
            }
            for id in &row.units {
                if !config.units.iter().any(|unit| &unit.id == id) {
                    return Err(GeneratorError::usage(format!(
                        "`[[declare]]` primitive `{}` names unit `{id}`, which the scan did not produce; available units: {}",
                        row.primitive,
                        config.units.iter().map(|unit| unit.id.as_str()).collect::<Vec<_>>().join(", ")
                    )));
                }
            }
            continue;
        }
        // The plan contributes a graph node, not a file; the aggregates that
        // compose it are the ones that render.
        // A per-unit pipeline names no file unless it moves the unit off the
        // canonical name, which validation rejects; the plan names no file at
        // all. Only the aggregates choose their file, from the set the surface
        // owns.
        let file = if row.primitive == UNIT_AGGREGATION {
            row.file.as_deref().ok_or_else(|| {
                GeneratorError::usage(format!(
                    "`[[declare]]` primitive `{}` renders a workflow file and needs `file`",
                    row.primitive
                ))
            })?
        } else {
            row.file.as_deref().unwrap_or_default()
        };
        if row.primitive == UNIT_AGGREGATION {
            if !config.workflow_files.iter().any(|owned| owned == file) {
                return Err(GeneratorError::usage(format!(
                    "`[[declare]]` primitive `{UNIT_AGGREGATION}` declares `{file}`, which the generated surface does not own; owned files: {}",
                    config.workflow_files.join(", ")
                )));
            }
            if !row.units.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "`[[declare]]` primitive `{UNIT_AGGREGATION}` composes every declared unit and takes no `units`"
                )));
            }
        }
    }
    // Per-unit pipelines: one row per unit, the canonical file for that unit,
    // the right primitive for the unit's kind, and no silent gaps.
    let mut covered: Vec<(&str, &str)> = Vec::new();
    for row in rows.iter().filter(|row| is_pipeline(&row.primitive)) {
        let Some(unit) = row.nested_file(config) else {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `{}` must name exactly one unit",
                row.primitive
            )));
        };
        if pipeline_id(unit.kind) != row.primitive {
            return Err(GeneratorError::usage(format!(
                "unit `{}` is a {} unit, which `{}` does not render; use `{}`",
                unit.id,
                unit.kind.label(),
                row.primitive,
                pipeline_id(unit.kind)
            )));
        }
        if row.units.len() > 1 {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `{}` renders one workflow file per unit; declare one row per unit",
                row.primitive
            )));
        }
        if row
            .file
            .as_deref()
            .is_some_and(|file| file != nested_unit_workflow_file(unit))
        {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `{}` must declare unit `{}` as `{}`, not `{}`; the aggregate callers invoke the canonical file name",
                row.primitive,
                unit.id,
                nested_unit_workflow_file(unit),
                row.file.as_deref().unwrap_or_default()
            )));
        }
        covered.push((row.primitive.as_str(), unit.id.as_str()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (_, unit) in &covered {
        if !seen.insert(unit) {
            return Err(GeneratorError::usage(format!(
                "unit `{unit}` is declared twice; a unit renders exactly one workflow file"
            )));
        }
    }
    // A declared family must cover its kind: a unit that silently loses its CI
    // job is a gap, never a feature. `coverage = "explicit"` states the intent.
    let mut explicit = std::collections::BTreeSet::new();
    for row in rows.iter().filter(|row| is_pipeline(&row.primitive)) {
        if row.args().string("coverage")?.as_deref() == Some("explicit")
            && let Some(unit) = row.nested_file(config)
        {
            explicit.insert(unit.kind);
        }
    }
    for unit in &config.units {
        let primitive = pipeline_id(unit.kind);
        let family_declared = covered.iter().any(|(candidate, _)| *candidate == primitive);
        if family_declared && !covered.iter().any(|(_, id)| *id == unit.id) {
            if explicit.contains(&unit.kind) {
                continue;
            }
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` rows cover {} units but not `{}`; declare every {} unit or set `coverage = \"explicit\"` to own the family's scope",
                unit.kind.label(),
                unit.id,
                unit.kind.label()
            )));
        }
    }
    // The declared release-side families render whole-repository workflow
    // files: each names the canonical file it owns, and no units.
    for row in declared
        .iter()
        .filter(|row| release::is_release_side(&row.primitive))
    {
        if !row.units.is_empty() {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `{}` renders one whole-repository workflow and takes no `units`",
                row.primitive
            )));
        }
        let file = row.file.as_deref().ok_or_else(|| {
            GeneratorError::usage(format!(
                "`[[declare]]` primitive `{}` renders a workflow file and needs `file`",
                row.primitive
            ))
        })?;
        if let Some(canonical) = release::canonical_release_side_file(&row.primitive)
            && file != canonical
        {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `{}` must declare `{canonical}`, not `{file}`",
                row.primitive
            )));
        }
    }
    // Declaration order is canonical: the aggregate composes callers in the
    // order the rows were declared, so that order is pinned to the scan.
    let ordered = covered.iter().map(|(_, unit)| *unit).collect::<Vec<_>>();
    let canonical = config
        .units
        .iter()
        .map(|unit| unit.id.as_str())
        .filter(|id| ordered.contains(id))
        .collect::<Vec<_>>();
    if ordered != canonical {
        return Err(GeneratorError::usage(format!(
            "`[[declare]]` rows must be ordered like the scanned units ({}) so the aggregate composes a stable graph",
            canonical.join(", ")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use super::*;

    /// Every primitive id in the registry is unique and resolvable, and the
    /// registry is exactly the families the generated surface owns.
    #[test]
    fn registry_resolves_every_declared_family() {
        let mut ids = Vec::new();
        for pipeline in PIPELINES {
            assert!(lookup(pipeline).is_ok(), "`{pipeline}` is not registered");
            ids.push(pipeline.to_owned());
        }
        for contract in [
            LANE_MATRIX,
            CACHE_CONTRACT,
            AFFECTED_PLAN,
            UNIT_AGGREGATION,
            WATCH_GRAPH,
            REGEN_GATE,
            RELEASE,
            PREVIEW,
            MAINTENANCE,
            RELEASE_SIGNER,
            STATIC_WORKFLOW,
        ] {
            assert!(lookup(contract).is_ok(), "`{contract}` is not registered");
            ids.push(contract);
        }
        let unique = ids.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), ids.len(), "duplicate primitive id");
    }

    /// An unknown primitive is a usage error that names what is known.
    #[test]
    fn unknown_primitive_names_the_registry() {
        let error = match lookup("not-a-primitive") {
            Err(error) => error.to_string(),
            Ok(_) => panic!("an unknown primitive is a usage error"),
        };
        assert!(error.contains("not-a-primitive"), "{error}");
        assert!(error.contains("rust-crate-pipeline"), "{error}");
    }

    /// Contract rows and file rows partition the registry: a primitive is
    /// never both.
    #[test]
    fn contracts_and_file_rows_partition_the_registry() {
        let registry = registry();
        for primitive in registry.iter().map(AsRef::as_ref) {
            let contract = is_unit_contract(primitive.id());
            let file_row = !contract;
            assert!(
                contract ^ file_row,
                "`{}` is neither a contract nor a file row",
                primitive.id()
            );
            if is_pipeline(primitive.id()) {
                assert!(
                    !contract,
                    "`{}` is a pipeline, not a contract",
                    primitive.id()
                );
            }
        }
    }

    /// A contract primitive that returns a unit the scan did not produce is a
    /// usage error, not a silent widening of the surface.
    #[test]
    fn contract_updates_must_match_scanned_units() {
        let mut units = Vec::new();
        let error = match apply_units(
            &mut units,
            vec![crate::scan::unit(
                UnitKind::Rust,
                "crates/example",
                Vec::new(),
                Vec::new(),
                None,
            )],
            "watch-graph",
        ) {
            Err(error) => error.to_string(),
            Ok(()) => panic!("a contract update for an unscanned unit is a usage error"),
        };
        assert!(error.contains("watch-graph"), "{error}");
        assert!(error.contains("which the scan did not produce"), "{error}");
    }
}

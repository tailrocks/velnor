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
pub(crate) mod check_profiles;
pub(crate) mod docs_site;
mod ir;
mod package_release;
mod pipeline;
mod plan;
pub(crate) mod prepared_tools;
pub(crate) mod product_transport;
mod providers;
mod regen;
pub(crate) mod release;
pub(crate) mod renovate;
pub(crate) mod runtime_products;
pub(crate) mod snapshot;
pub(crate) mod watch;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::s2::config::RepoGenerationConfig;
use crate::s2::provider::ProviderId;
use crate::s2::scan::RepositoryShape;
use crate::s2::{
    nested_unit_workflow_file, validate_unit_phases, CachePurpose, CacheSpec, GeneratorError,
    ProjectConfig, Unit, UnitKind,
};

pub(crate) use ir::{
    checks_env, close_mise_tool_subset, config_snapshot_identity,
    default_branch_push_cache_save_expression, docker_build_token_env_for_members,
    member_backend_key, render_cargo_source_preparation,
    render_mutable_mount_seed_restore_for_unit, render_pinned_toolchain_steps,
    render_retained_output_cache_note, resolve_install_dep_names, trusted_cache_save_expression,
    unknown_backend_reason, validate_boltffi_tools_are_locked,
    validate_mise_install_deps_are_closed, validate_nextest_tools_are_locked,
    validate_xcodegen_tools_are_locked, ProviderAdmission, WorkflowIr, WorkflowKind,
    D19_PIN_FETCH_COMMANDS, GITHUB_WORKFLOW_BYTE_LIMIT, MISE_INSTALL_DEPS_MODEL_VERSION,
};
pub(crate) use package_release::release_admission_command;

#[cfg(test)]
pub(crate) use ir::jq_read_plan_matrix;

/// Default `timeout-minutes` for a unit verification job.
pub(crate) const DEFAULT_UNIT_TIMEOUT_MINUTES: u32 = 45;

/// A primitive id the registry knows how to build.
pub(crate) const AFFECTED_PLAN: &str = "affected-plan";
pub(crate) const UNIT_AGGREGATION: &str = "unit-aggregation";
pub(crate) const PROVIDER_MATRIX: &str = "provider-matrix";
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
/// The `docs.yml` documentation-site pipeline: build, link checks, spelling,
/// Pages deployment, and post-deployment verification from one `[docs]`
/// consumer contract.
pub(crate) const DOCS_SITE: &str = "docs-site";
/// The `release.yml` publisher: tag-triggered, verify-then-publish.
pub(crate) const RELEASE: &str = "release";
/// The `preview.yml` rolling artifact lane.
pub(crate) const PREVIEW: &str = "preview";
/// The typed six-payload rolling package release and consumer handoff.
pub(crate) const PACKAGE_RELEASE: &str = "package-release";
/// The `maintenance.yml` cache-hygiene workflow.
pub(crate) const MAINTENANCE: &str = "maintenance";
/// The release artifact provenance signer.
pub(crate) const RELEASE_SIGNER: &str = "release-signer";
/// The self-hosted Renovate writer workflow.
pub(crate) const RENOVATE: &str = "renovate";
/// The Renovate configuration validation workflow.
pub(crate) const RENOVATE_VALIDATE: &str = "renovate-validate";
/// A scheduled workflow file rendered from composable check profiles.
pub(crate) const SCHEDULED_CHECKS: &str = "scheduled-checks";
/// A reviewed workflow body declared verbatim by the repository.
pub(crate) const STATIC_WORKFLOW: &str = "static-workflow";
/// The owner-only Stage-0 runtime-product producer.
pub(crate) const RUNTIME_PRODUCTS: &str = "runtime-products";
/// A repository's prepared tools: per-unit consumer needs the generator binds
/// to governing lockfiles, recipe, and toolchain, and renders consumer steps
/// for. A unit-contract row: it records needs on the scoped units before any
/// file renders, and renders nothing itself.
pub(crate) const PREPARED_TOOL: &str = "prepared-tool";

/// The Dockerfile stage a mutable mount seed is injected through. The image
/// declares it as an empty `FROM scratch` stage so a build without the
/// `--build-context` override keeps its exact shape, and the host overrides it
/// with the restored seed directory only when one exists.
pub(crate) const MUTABLE_MOUNT_SEED_CONTEXT: &str = "velnor-cache-seed";
/// The Dockerfile target stage that copies the build's mutable cache mounts
/// back out, so a trusted full build can extract the state it produced.
pub(crate) const MUTABLE_MOUNT_EXPORT_TARGET: &str = "velnor-cache-export";
/// The workspace directory the Docker mutable mount seed lives in between the
/// restore and the build, and where the build's export lands afterwards.
pub(crate) const MUTABLE_MOUNT_HOST_DIR: &str = ".velnor-docker-cache";

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
            checkout: crate::s2::ActionPin::Checkout.reference(),
            cache_restore: crate::s2::ActionPin::CacheRestore.reference(),
            cache_save: crate::s2::ActionPin::CacheSave.reference(),
            opentofu_setup: crate::s2::ActionPin::OpenTofuSetup.reference(),
            upload_artifact: crate::s2::ActionPin::UploadArtifact.reference(),
            download_artifact: crate::s2::ActionPin::DownloadArtifact.reference(),
            bun: crate::s2::ActionPin::Bun.reference(),
            node: crate::s2::ActionPin::Node.reference(),
            rust_tool: crate::s2::ActionPin::RustTool.reference(),
            mise: crate::s2::ActionPin::Mise.reference(),
            gradle: crate::s2::ActionPin::Gradle.reference(),
            sccache: crate::s2::ActionPin::Sccache.reference(),
            mr_boxington: crate::s2::ActionPin::MrBoxington.reference(),
            github_runtime: crate::s2::ActionPin::GithubRuntime.reference(),
            docker_buildx: crate::s2::ActionPin::DockerBuildx.reference(),
        }
    }
}

/// One provider job of a nested unit workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProviderJob {
    pub(crate) provider: ProviderId,
    /// The hosted provider is the only one allowed to save a cache entry:
    /// entries are written from trusted events only.
    pub(crate) cache_save: bool,
    /// Local providers carry the generated trusted-event gate. Persistent
    /// cache writes remain restricted to trusted events.
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
    /// The detected policy classifies by purpose: Cargo-source and generic
    /// tool caches ride the actions cache even when the unit compiles under
    /// Mr. Boxington — the object transport moves compiler state, not Cargo's
    /// registry archives, extracted sources, or Git dependencies. Raw output
    /// caches are the one suppression: the object transport already carries
    /// workspace state, so a declared output cache needs its justification on
    /// record.
    pub(crate) fn enables_actions_cache(self, ir: &WorkflowIr, unit: &Unit) -> bool {
        match self {
            Self::Detected => {
                // A unit that declares a Docker mutable-mount seed has swapped
                // its retained-output transport for that seed lifecycle: the
                // declared paths are the seed directory and the declared keys
                // are the seed key inputs. Rendering the generic `ci-` cache
                // beside it would restore seed state under the wrong namespace
                // on lanes that never run the build commands.
                if unit
                    .cache
                    .as_ref()
                    .is_some_and(|cache| cache.mutable_mount_seed)
                {
                    return false;
                }
                match unit.cache.as_ref().map(|cache| cache.purpose) {
                    // The Rust-toolchain cache is generator-internal:
                    // `render_pinned_toolchain_steps` emits it directly and never
                    // hangs it off a unit's declared contract, so it cannot arrive
                    // through this match. Docker seeds render their own lifecycle.
                    // Refuse rather than silently enable either non-generic cache.
                    None | Some(CachePurpose::Toolchains | CachePurpose::DockerSeed) => false,
                    Some(
                        CachePurpose::CargoSources
                        | CachePurpose::Generic
                        | CachePurpose::SwiftPmSources
                        | CachePurpose::XcodeIntermediates,
                    ) => true,
                    Some(CachePurpose::Outputs) => {
                        !ir.uses_mr_boxington(unit)
                            || unit
                                .cache
                                .as_ref()
                                .is_some_and(CacheSpec::justified_output_alongside_mr_boxington)
                    }
                }
            }
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
        validate_mutable_mount_seed(unit)?;
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

/// Validate a unit whose cache contract is a Docker image's mutable mount
/// seed. A declared seed that the declared builds never inject is a silent lie
/// about persistence — exactly the cold-build state this contract exists to
/// remove — so the declared commands must prove both directions of the
/// lifecycle: the hosted full build consumes the seed and extracts the updated
/// state, hosted pull-request builds may consume the restored seed, and no
/// untrusted pull-request build extracts anything.
///
/// # Errors
/// Returns a usage error when the seed rides a non-Docker unit, when the
/// hosted full commands never inject the seed or never extract the export, or
/// when a self-hosted command references the seed context or any pull-request
/// command references the export target.
pub(crate) fn validate_mutable_mount_seed(unit: &Unit) -> Result<(), GeneratorError> {
    let Some(cache) = &unit.cache else {
        return Ok(());
    };
    if !cache.mutable_mount_seed {
        return Ok(());
    }
    if unit.kind != UnitKind::Docker {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares a mutable mount seed, but only a Docker image build owns cache \
             mounts a seed can be injected into",
            unit.id
        )));
    }
    if cache.key_files.is_empty() || cache.paths.is_empty() {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares a mutable mount seed without both `key_files` and `paths`; the \
             seed cannot be keyed or restored",
            unit.id
        )));
    }
    let injection = format!("--build-context {MUTABLE_MOUNT_SEED_CONTEXT}=");
    let extraction = format!("--target {MUTABLE_MOUNT_EXPORT_TARGET}");
    if !unit
        .full_commands
        .iter()
        .any(|command| command.contains(&injection))
    {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares a mutable mount seed, but no full command injects it with \
             `--build-context {MUTABLE_MOUNT_SEED_CONTEXT}=<dir>`; a seed the build never reads \
             is declared persistence that does not exist",
            unit.id
        )));
    }
    if !unit
        .full_commands
        .iter()
        .any(|command| command.contains(&extraction) && command.contains("--output type=local"))
    {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares a mutable mount seed, but no full command extracts the \
             updated state with `--target {MUTABLE_MOUNT_EXPORT_TARGET}` and `--output \
             type=local`; without extraction the seed can only ever go cold",
            unit.id
        )));
    }
    // Every provider runs the same commands, so the seed lifecycle is one
    // contract: extraction happens on trusted hosted full builds, and the
    // pull-request commands never touch the export target.
    if unit
        .pr_commands
        .iter()
        .any(|command| command.contains(&extraction))
    {
        return Err(GeneratorError::usage(format!(
            "unit `{}` runs pull-request commands that reference the mutable mount seed export \
             target; extraction is restricted to trusted hosted full builds",
            unit.id
        )));
    }
    Ok(())
}

/// Everything a declared unit pipeline may tune for one unit's surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UnitContract {
    pub(crate) providers: Vec<ProviderJob>,
    pub(crate) timeout_minutes: u32,
    pub(crate) cache: CacheBackend,
    /// Cache entries are saved on trusted events only, which is a property of
    /// the workflow kind the aggregate owns, never of the unit.
    pub(crate) cache_save: bool,
    /// Whether the unit's cache contract is the mutable mount seed of a Docker
    /// image build: the hosted lane restores it before the build, the build
    /// injects it into the image's cache mounts, and a trusted full build
    /// extracts the updated state for the next builder.
    pub(crate) mutable_mount_seed: bool,
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
    /// Per-unit pipeline contracts carried into the shared kind reusable.
    pub(crate) contracts: BTreeMap<String, UnitContract>,
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
    pub(crate) providers: &'a providers::ResolvedProviders,
    /// The resolved cache contract.
    pub(crate) cache: &'a cache::ResolvedCache,
    /// The CI graph nodes contributed by the primitives rendered so far.
    pub(crate) nodes: &'a [GraphNode],
    /// Per-unit pipeline contracts resolved by the primitives rendered so far.
    /// The aggregate passes them to the kind-reusable callers so caller
    /// `with:` values and callee step gates derive from one contract.
    pub(crate) contracts: &'a BTreeMap<String, UnitContract>,
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

    /// A table of declared reads indexed by unit id, validated against the
    /// scan: each unit names typed contract entries for relations the scan
    /// cannot discover (opaque task runners, helper scripts, dynamic
    /// includes). The `watch-graph` primitive unions the paths into the
    /// unit's watch set and renders a uniform `complete` claim as the
    /// unit's `reads_closed` runtime flag.
    pub(crate) fn unit_reads(
        &self,
        ctx: &RenderCtx<'_>,
        key: &str,
    ) -> Result<Option<BTreeMap<String, Vec<DeclaredRead>>>, GeneratorError> {
        let Some(value) = self.0.get(key) else {
            return Ok(None);
        };
        let tables = parse_declared_reads(key, value)?;
        for unit in tables.keys() {
            if !ctx.shape.unit_ids().any(|candidate| candidate == unit) {
                return Err(GeneratorError::usage(format!(
                    "`{key}` names unit `{unit}`, which the scan did not produce; available units: {}",
                    ctx.shape.unit_ids().collect::<Vec<_>>().join(", ")
                )));
            }
        }
        Ok(Some(tables))
    }
}

/// One declared read: paths a unit's owner asserts the unit reads, with the
/// non-empty reason that audits the claim and the contract type that says
/// what the claim covers. The declaration row itself is the audit trail;
/// the paths compile into the unit's watch set. `complete` asserts the
/// closed world: the entry's paths plus the scan watch are the unit's
/// entire read set, so unmatched paths provably exclude the unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeclaredRead {
    pub(crate) paths: Vec<String>,
    pub(crate) reason: String,
    pub(crate) kind: DeclaredReadKind,
    /// The opaque script a `script` contract binds, unioned into the watch
    /// set with the paths so changing the script reselects exactly its
    /// unit. `None` for every other contract type.
    pub(crate) script: Option<String>,
    /// The unprovable mechanism an `unresolved` contract records. `None`
    /// for every other contract type.
    pub(crate) limitation: Option<String>,
    /// Whether this entry claims the closed world. Uniform across a
    /// unit's entries (mixed claims fail generation); `true` on every
    /// entry closes the unit. Never set on an `unresolved` entry, whose
    /// recorded unknown contradicts completeness.
    pub(crate) complete: bool,
}

/// The contract type of one declared read: plain path reads, the opaque
/// script plus its declared input bound, or an explicit record of a
/// relationship the scan cannot bound. Every type unions its paths into the
/// unit's watch set; without `complete`, no type narrows selection beyond
/// the paths it covers, so a wrong claim over-selects instead of silently
/// skipping verification. A uniform `complete` claim closes the unit: the
/// trusted assertion that the bound is the whole read set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeclaredReadKind {
    Paths,
    Script,
    Unresolved,
}

impl DeclaredReadKind {
    /// The config spelling of the contract type.
    fn as_str(self) -> &'static str {
        match self {
            Self::Paths => "paths",
            Self::Script => "script",
            Self::Unresolved => "unresolved",
        }
    }

    /// Parse the `type` value of one entry: one of the three contract
    /// types, else a usage error listing them.
    fn parse(key: &str, unit: &str, value: &toml::Value) -> Result<Self, GeneratorError> {
        let toml::Value::String(name) = value else {
            return Err(unexpected(key, value, "a contract type string"));
        };
        match name.as_str() {
            "paths" => Ok(Self::Paths),
            "script" => Ok(Self::Script),
            "unresolved" => Ok(Self::Unresolved),
            _ => Err(GeneratorError::usage(format!(
                "`[[declare]]` argument `{key}` for unit `{unit}` takes type `\"paths\"`, `\"script\"`, or `\"unresolved\"`, found `{name}`"
            ))),
        }
    }

    /// The keys one entry of this type may spell, for the typo check.
    /// `complete` is allowed where a bound can be total (`paths`,
    /// `script`) and rejected on `unresolved`, whose recorded unknown
    /// contradicts completeness.
    fn allowed_keys(self) -> &'static [&'static str] {
        match self {
            Self::Paths => &["paths", "reason", "type", "complete"],
            Self::Script => &["paths", "reason", "type", "script", "complete"],
            Self::Unresolved => &["paths", "reason", "type", "limitation"],
        }
    }
}

/// Parse a `reads` table of unit ids to typed contract entries. Every entry
/// spells `paths` (an array of strings, each a repo-relative watch glob)
/// and `reason` (a non-empty string); `type` selects the contract
/// (`"paths"` when absent, else `"script"` or `"unresolved"`), and each
/// type adds its own required key (`script`, `limitation`). `complete`
/// (absent means false) asserts the closed world for `paths` and `script`
/// entries; completeness is a unit-wide claim, so a unit's entries must
/// agree on it — a `complete` claim beside an open or `unresolved` entry
/// fails as a mixed claim. A typo'd, mistyped, escaping, or undocumented
/// relation fails closed instead of silently widening (or never narrowing)
/// affected selection.
fn parse_declared_reads(
    key: &str,
    value: &toml::Value,
) -> Result<BTreeMap<String, Vec<DeclaredRead>>, GeneratorError> {
    let toml::Value::Table(table) = value else {
        return Err(unexpected(
            key,
            value,
            "a table of unit ids to typed contract entries",
        ));
    };
    let mut reads = BTreeMap::new();
    for (unit, entries) in table {
        let toml::Value::Array(items) = entries else {
            return Err(unexpected(
                key,
                entries,
                "an array of typed contract tables",
            ));
        };
        let mut declared = Vec::new();
        for item in items {
            let toml::Value::Table(entry) = item else {
                return Err(unexpected(key, item, "a typed contract table"));
            };
            let kind = match entry.get("type") {
                None => DeclaredReadKind::Paths,
                Some(value) => DeclaredReadKind::parse(key, unit, value)?,
            };
            for name in entry.keys() {
                if !kind.allowed_keys().contains(&name.as_str()) {
                    return Err(GeneratorError::usage(format!(
                        "`[[declare]]` argument `{key}` for unit `{unit}` takes only {} for a `\"{}\"` contract, found `{name}`",
                        kind.allowed_keys()
                            .iter()
                            .map(|key| format!("`{key}`"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        kind.as_str(),
                    )));
                }
            }
            let Some(paths) = entry.get("paths") else {
                return Err(GeneratorError::usage(format!(
                    "`[[declare]]` argument `{key}` for unit `{unit}` is missing `paths`"
                )));
            };
            let toml::Value::Array(paths) = paths else {
                return Err(unexpected(key, paths, "an array of path strings"));
            };
            let mut owned = Vec::new();
            for path in paths {
                let toml::Value::String(path) = path else {
                    return Err(unexpected(key, path, "a path string"));
                };
                if globset::Glob::new(path).is_err() {
                    return Err(GeneratorError::usage(format!(
                        "`[[declare]]` argument `{key}` for unit `{unit}` names an invalid watch glob `{path}`"
                    )));
                }
                if path.starts_with('/') || path.split('/').any(|segment| segment == "..") {
                    return Err(GeneratorError::usage(format!(
                        "`[[declare]]` argument `{key}` for unit `{unit}` names a path outside the repository `{path}`; contracts cover repo-relative globs only"
                    )));
                }
                owned.push(path.clone());
            }
            let reason = match entry.get("reason") {
                Some(toml::Value::String(reason)) if !reason.trim().is_empty() => reason.clone(),
                _ => {
                    return Err(GeneratorError::usage(format!(
                        "`[[declare]]` argument `{key}` for unit `{unit}` needs a non-empty `reason` auditing the read"
                    )));
                }
            };
            let script = match kind {
                DeclaredReadKind::Script => Some(parse_contract_script(key, unit, entry)?),
                _ => None,
            };
            let limitation = match kind {
                DeclaredReadKind::Unresolved => Some(parse_contract_limitation(key, unit, entry)?),
                _ => None,
            };
            let complete = match entry.get("complete") {
                None => false,
                Some(toml::Value::Boolean(complete)) => *complete,
                Some(other) => return Err(unexpected(key, other, "a boolean `complete` flag")),
            };
            declared.push(DeclaredRead {
                paths: owned,
                reason,
                kind,
                script,
                limitation,
                complete,
            });
        }
        check_complete_uniformity(key, unit, &declared)?;
        reads.insert(unit.clone(), declared);
    }
    Ok(reads)
}

/// Parse the `script` of a `script` contract: the repo-relative path of the
/// opaque script, treated as data (unioned into the watch set, never
/// executed or existence-checked — a build-generated script is legitimate).
/// Absolute paths and root escapes fail closed; glob metacharacters fail
/// closed too, since `script` names one script, not a pattern. The stored
/// path is trimmed; the glob check keeps the watch union infallible.
fn parse_contract_script(
    key: &str,
    unit: &str,
    entry: &toml::map::Map<String, toml::Value>,
) -> Result<String, GeneratorError> {
    let script = match entry.get("script") {
        Some(toml::Value::String(script)) if !script.trim().is_empty() => script.trim().to_owned(),
        _ => {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` argument `{key}` for unit `{unit}` needs a non-empty `script` naming the opaque script"
            )));
        }
    };
    if script.starts_with('/') || script.split('/').any(|segment| segment == "..") {
        return Err(GeneratorError::usage(format!(
            "`[[declare]]` argument `{key}` for unit `{unit}` names a script outside the repository `{script}`"
        )));
    }
    if script.contains(['*', '?', '[', ']']) {
        return Err(GeneratorError::usage(format!(
            "`[[declare]]` argument `{key}` for unit `{unit}` names a script glob `{script}`; `script` names one literal script path"
        )));
    }
    if globset::Glob::new(&script).is_err() {
        return Err(GeneratorError::usage(format!(
            "`[[declare]]` argument `{key}` for unit `{unit}` names an invalid watch glob `{script}`"
        )));
    }
    Ok(script)
}

/// Reject a unit's mixed `complete` claims: completeness is unit-wide, so
/// a `complete` entry beside an open or `unresolved` entry fails closed.
fn check_complete_uniformity(
    key: &str,
    unit: &str,
    declared: &[DeclaredRead],
) -> Result<(), GeneratorError> {
    let complete = declared.iter().filter(|entry| entry.complete).count();
    if complete > 0 && complete != declared.len() {
        return Err(GeneratorError::usage(format!(
            "`[[declare]]` argument `{key}` for unit `{unit}` mixes `complete` and open entries; completeness is a unit-wide claim"
        )));
    }
    Ok(())
}

/// Parse the `limitation` of an `unresolved` contract: the non-empty record
/// of the unprovable mechanism, so the limitation is explicit in the
/// authoritative config instead of silent in the classifier.
fn parse_contract_limitation(
    key: &str,
    unit: &str,
    entry: &toml::map::Map<String, toml::Value>,
) -> Result<String, GeneratorError> {
    match entry.get("limitation") {
        Some(toml::Value::String(limitation)) if !limitation.trim().is_empty() => {
            Ok(limitation.clone())
        }
        _ => Err(GeneratorError::usage(format!(
            "`[[declare]]` argument `{key}` for unit `{unit}` needs a non-empty `limitation` recording what cannot be proven"
        ))),
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
        Box::new(providers::ProviderMatrix),
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
        Box::new(package_release::PackageRelease),
        Box::new(release::Maintenance),
        Box::new(release::ReleaseSigner),
        Box::new(release::StaticWorkflow),
        Box::new(renovate::Renovate),
        Box::new(renovate::RenovateValidate),
        Box::new(check_profiles::ScheduledChecks),
        Box::new(docs_site::DocsSite),
        Box::new(runtime_products::RuntimeProducts),
        Box::new(prepared_tools::PreparedTool),
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
    fn from_row(row: &crate::s2::config::DeclareRow) -> Self {
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
    /// Per-unit pipeline contracts after the declared rows were resolved.
    pub(crate) contracts: BTreeMap<String, UnitContract>,
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
#[expect(
    clippy::too_many_lines,
    reason = "generation stages contract resolution, rendering, and ownership in one transaction"
)]
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
    let providers = providers::resolve(config, &rows)?;
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
                &providers,
                &cache,
                &[],
                &BTreeMap::new(),
            ),
            &row.args(),
        )?;
        apply_units(&mut units, rendered.units, &row.primitive)?;
    }
    let mut resolved = config.clone();
    resolved.units.clone_from(&units);
    // Unit contracts mutate commands after the scan validated them; refuse
    // a misaligned phase model before it reaches any `--phase` step.
    validate_unit_phases(&resolved)?;
    let providers = providers::resolve(&resolved, &rows)?;

    // Per-unit pipelines, then the plan, then the aggregates that compose both.
    let mut files = BTreeMap::new();
    let mut nodes = Vec::new();
    let mut contracts = BTreeMap::new();
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
                &providers,
                &cache,
                &nodes,
                &contracts,
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
        merge_pipeline_contracts(&mut contracts, rendered.contracts)?;
    }

    // A declared file family can add a workflow the scan did not own: a
    // release lane becomes part of the surface by being declared, and the
    // caller extends the owned file list so it is emitted and recorded.
    let mut added_files: Vec<String> = Vec::new();
    for row in &rows {
        if row.unit_contract
            || (!release::is_release_side(&row.primitive)
                && !renovate::is_renovate_side(&row.primitive)
                && !docs_site::is_docs_site_side(&row.primitive)
                && !check_profiles::is_scheduled_checks_side(&row.primitive)
                && !runtime_products::is_runtime_products_side(&row.primitive))
        {
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
        contracts,
        added_files,
    })
}

fn merge_pipeline_contracts(
    contracts: &mut BTreeMap<String, UnitContract>,
    updates: BTreeMap<String, UnitContract>,
) -> Result<(), GeneratorError> {
    for (unit_id, contract) in updates {
        if contracts.insert(unit_id.clone(), contract).is_some() {
            return Err(GeneratorError::usage(format!(
                "two declared pipeline rows configure unit `{unit_id}`"
            )));
        }
    }
    Ok(())
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
    providers: &'a providers::ResolvedProviders,
    cache: &'a cache::ResolvedCache,
    nodes: &'a [GraphNode],
    contracts: &'a BTreeMap<String, UnitContract>,
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
        providers,
        cache,
        nodes,
        contracts,
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

/// Default rows for one side-file family: every owned file the config does not
/// declare itself, when the family contract says the file applies.
fn push_default_side_rows(
    rows: &mut Vec<ResolvedRow>,
    config: &ProjectConfig,
    declared: &[Declaration],
    side_files: &[(&str, &str)],
    available: &dyn Fn(&str) -> bool,
) {
    for (file, family) in side_files {
        if !config.workflow_files.iter().any(|owned| owned == file) {
            continue;
        }
        if declared.iter().any(|row| row.file.as_deref() == Some(file)) {
            continue;
        }
        if !available(family) {
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
    for row in declared.iter().filter(|row| {
        release::is_release_side(&row.primitive)
            || renovate::is_renovate_side(&row.primitive)
            || docs_site::is_docs_site_side(&row.primitive)
            || check_profiles::is_scheduled_checks_side(&row.primitive)
            || runtime_products::is_runtime_products_side(&row.primitive)
    }) {
        rows.push(ResolvedRow::declared(row));
    }
    // Release-side families: a default row per owned file, rendered by the
    // same primitives a declared row uses, unless the config declares the
    // family itself.
    if !config.adopted_workflow_surface {
        push_default_side_rows(
            &mut rows,
            config,
            declared,
            release::RELEASE_SIDE_FILES,
            // A repository without a release contract omits the publisher.
            &|family| family != RELEASE || config.release.is_some(),
        );
        push_default_side_rows(
            &mut rows,
            config,
            declared,
            renovate::RENOVATE_SIDE_FILES,
            &|family| {
                config
                    .renovate
                    .as_ref()
                    .is_some_and(|spec| family != RENOVATE_VALIDATE || spec.validate)
            },
        );
        push_default_side_rows(
            &mut rows,
            config,
            declared,
            docs_site::DOCS_SITE_SIDE_FILES,
            // A repository without a docs contract omits the pipeline.
            &|_| config.docs.is_some(),
        );
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
    matches!(primitive, WATCH_GRAPH | REGEN_GATE | PREPARED_TOOL)
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
            file: Some(crate::s2::nested_unit_workflow_file(unit)),
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
    // The declared release-side and Renovate families render whole-repository
    // workflow files: each names the canonical file it owns, and no units.
    for row in declared.iter().filter(|row| {
        release::is_release_side(&row.primitive)
            || renovate::is_renovate_side(&row.primitive)
            || docs_site::is_docs_site_side(&row.primitive)
            || check_profiles::is_scheduled_checks_side(&row.primitive)
            || runtime_products::is_runtime_products_side(&row.primitive)
    }) {
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
        let canonical = release::canonical_release_side_file(&row.primitive)
            .or_else(|| renovate::canonical_renovate_side_file(&row.primitive))
            .or_else(|| docs_site::canonical_docs_site_side_file(&row.primitive))
            .or_else(|| runtime_products::canonical_runtime_products_side_file(&row.primitive));
        // Main-branch-driven release rows and package publishers own their
        // declared file; other tag-driven families stay pinned to a canonical
        // file.
        let main_branch_driven = row.primitive == RELEASE
            && Args(&row.args)
                .string("kind")
                .ok()
                .flatten()
                .is_some_and(|kind| release::is_main_branch_driven_release_kind(&kind));
        if let Some(canonical) = canonical
            && file != canonical
            && !main_branch_driven
        {
            return Err(GeneratorError::usage(format!(
                "`[[declare]]` primitive `{}` must declare `{canonical}`, not `{file}`",
                row.primitive
            )));
        }
    }
    // Release publishers render whole workflows, so two rows must share
    // neither a workflow name nor a version tag prefix: the same name would
    // collapse two publishers into one status context, and the same prefix
    // would mint one tag stream from two lanes. The default row counts: a
    // repository with a release contract already publishes `Release` from
    // `release.yml`.
    {
        let mut names: BTreeMap<String, String> = BTreeMap::new();
        let mut prefixes: BTreeMap<String, String> = BTreeMap::new();
        for row in rows.iter().filter(|row| row.primitive == RELEASE) {
            let Some(file) = row.file.as_deref() else {
                continue;
            };
            let name = release::release_workflow_name(file, &row.args())?;
            if names.insert(name.clone(), file.to_owned()).is_some() {
                return Err(GeneratorError::usage(format!(
                    "`[[declare]]` primitive `release` renders duplicate workflow name `{name}`; give each publisher its own `name`"
                )));
            }
            if let Some(prefix) = row.args().string("version_prefix")? {
                if prefix.is_empty() {
                    continue;
                }
                if let Some(owner) = prefixes.insert(prefix.clone(), file.to_owned()) {
                    return Err(GeneratorError::usage(format!(
                        "`[[declare]]` primitive `release` file `{file}` publishes tag prefix `{prefix}`, already owned by file `{owner}`"
                    )));
                }
            }
        }
    }
    // Every configured check profile renders exactly once: an uncovered
    // profile is a silent omission, and a doubly covered one would run the
    // same check on two cadences.
    {
        let mut covered = BTreeSet::new();
        for row in declared
            .iter()
            .filter(|row| check_profiles::is_scheduled_checks_side(&row.primitive))
        {
            // The label names the file, exactly like the render path: a
            // shared-selection error must point at the declare row's file,
            // not at the primitive every scheduled row shares.
            let label = row.file.as_deref().unwrap_or(row.primitive.as_str());
            for profile in
                check_profiles::select_profiles(&config.check_profiles, &Args(&row.args), label)?
            {
                if !covered.insert(profile.id.as_str()) {
                    return Err(GeneratorError::usage(format!(
                        "`[[declare]]` primitive `{}` renders check profile `{}` twice; declare each profile in exactly one scheduled-checks file",
                        row.primitive, profile.id
                    )));
                }
            }
        }
        for profile in &config.check_profiles {
            if !covered.contains(profile.id.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "[[check_profile]] `{}` is rendered by no `[[declare]] primitive = \"scheduled-checks\"` row; declare the file that renders it",
                    profile.id
                )));
            }
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
            PROVIDER_MATRIX,
            CACHE_CONTRACT,
            AFFECTED_PLAN,
            UNIT_AGGREGATION,
            WATCH_GRAPH,
            REGEN_GATE,
            RELEASE,
            PREVIEW,
            PACKAGE_RELEASE,
            MAINTENANCE,
            RELEASE_SIGNER,
            STATIC_WORKFLOW,
            RENOVATE,
            RENOVATE_VALIDATE,
            SCHEDULED_CHECKS,
            DOCS_SITE,
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
            vec![crate::s2::scan::unit(
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

    /// The `watch-graph` schema admits the generation-time `reads` table
    /// alongside the watch additions it unions with.
    #[test]
    fn watch_graph_schema_admits_declared_reads() -> Result<(), Box<dyn std::error::Error>> {
        let primitive = lookup(WATCH_GRAPH)?;
        let schema = primitive.schema();
        assert!(schema.contains(&"watch"));
        assert!(schema.contains(&"reads"));
        Ok(())
    }

    /// Declared reads parse per unit id into typed contract entries; an
    /// absent `type` defaults to the `paths` contract.
    #[test]
    fn declared_reads_parse_paths_and_reasons() -> Result<(), Box<dyn std::error::Error>> {
        let value: toml::Value = toml::from_str(
            r#""rust-alpha" = [
                { paths = ["scripts/check-boundary.sh"], reason = "task runner execs this script" },
                { paths = ["assets/**"], reason = "bundler consumes non-source inputs" },
            ]"#,
        )?;
        let reads = parse_declared_reads("reads", &value)?;
        assert_eq!(
            reads.get("rust-alpha"),
            Some(&vec![
                DeclaredRead {
                    paths: vec!["scripts/check-boundary.sh".to_owned()],
                    reason: "task runner execs this script".to_owned(),
                    kind: DeclaredReadKind::Paths,
                    script: None,
                    limitation: None,
                    complete: false,
                },
                DeclaredRead {
                    paths: vec!["assets/**".to_owned()],
                    reason: "bundler consumes non-source inputs".to_owned(),
                    kind: DeclaredReadKind::Paths,
                    script: None,
                    limitation: None,
                    complete: false,
                },
            ])
        );
        Ok(())
    }

    /// A missing, empty, or blank reason fails closed: every declared read
    /// must audit its claim.
    #[test]
    fn declared_reads_reject_an_empty_reason() -> Result<(), Box<dyn std::error::Error>> {
        for entry in [
            r#"{ paths = ["scripts/check.sh"] }"#,
            r#"{ paths = ["scripts/check.sh"], reason = "" }"#,
            r#"{ paths = ["scripts/check.sh"], reason = "  " }"#,
            r#"{ paths = ["scripts/check.sh"], reason = 7 }"#,
        ] {
            let value: toml::Value = toml::from_str(&format!(r#""a" = [ {entry} ]"#))?;
            let Err(error) = parse_declared_reads("reads", &value) else {
                panic!("an undocumented read must fail");
            };
            assert!(error.to_string().contains("reason"), "{error}");
        }
        Ok(())
    }

    /// Malformed entries fail closed: unknown keys, missing or mistyped
    /// paths, and invalid globs never silently widen selection.
    #[test]
    fn declared_reads_reject_malformed_entries() -> Result<(), Box<dyn std::error::Error>> {
        for (entry, needle) in [
            (
                r#"{ paths = ["a"], reason = "r", extra = true }"#,
                "takes only `paths`, `reason`, `type`, `complete`",
            ),
            (r#"{ reason = "r" }"#, "missing `paths`"),
            (
                r#"{ paths = "a", reason = "r" }"#,
                "an array of path strings",
            ),
            (r#"{ paths = [7], reason = "r" }"#, "a path string"),
            (
                r#"{ paths = ["["], reason = "r" }"#,
                "an invalid watch glob",
            ),
            (r#""just-a-string""#, "a typed contract table"),
        ] {
            let value: toml::Value = toml::from_str(&format!(r#""a" = [ {entry} ]"#))?;
            let Err(error) = parse_declared_reads("reads", &value) else {
                panic!("a malformed entry must fail: {entry}");
            };
            assert!(error.to_string().contains(needle), "entry {entry}: {error}");
        }
        let Err(error) = parse_declared_reads("reads", &toml::Value::Array(vec![])) else {
            panic!("a non-table reads value must fail");
        };
        assert!(error.to_string().contains("a table of unit ids"), "{error}");
        Ok(())
    }

    /// `script` and `unresolved` contracts parse with their kind and their
    /// type-specific key: the bound script, or the recorded limitation.
    #[test]
    fn declared_read_contracts_parse_script_and_unresolved_types(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let value: toml::Value = toml::from_str(
            r#""rust-alpha" = [
                { type = "script", script = "scripts/check-boundary.sh", paths = ["schemas/**"], reason = "task runner execs this script" },
                { type = "unresolved", paths = [], reason = "bundler reads are dynamic", limitation = "bundler plugin graph resolves at build time" },
            ]"#,
        )?;
        let reads = parse_declared_reads("reads", &value)?;
        assert_eq!(
            reads.get("rust-alpha"),
            Some(&vec![
                DeclaredRead {
                    paths: vec!["schemas/**".to_owned()],
                    reason: "task runner execs this script".to_owned(),
                    kind: DeclaredReadKind::Script,
                    script: Some("scripts/check-boundary.sh".to_owned()),
                    limitation: None,
                    complete: false,
                },
                DeclaredRead {
                    paths: vec![],
                    reason: "bundler reads are dynamic".to_owned(),
                    kind: DeclaredReadKind::Unresolved,
                    script: None,
                    limitation: Some("bundler plugin graph resolves at build time".to_owned()),
                    complete: false,
                },
            ])
        );
        Ok(())
    }

    /// Unknown types, misplaced type keys, and missing or escaping
    /// type-specific values fail closed with the valid contract spelled out.
    #[test]
    fn declared_read_contracts_reject_mistyped_entries() -> Result<(), Box<dyn std::error::Error>> {
        for (entry, needle) in [
            (
                r#"{ type = "glob", paths = ["a"], reason = "r" }"#,
                "takes type `\"paths\"`, `\"script\"`, or `\"unresolved\"`",
            ),
            (
                r#"{ type = 7, paths = ["a"], reason = "r" }"#,
                "a contract type string",
            ),
            (
                r#"{ paths = ["a"], reason = "r", script = "s.sh" }"#,
                "for a `\"paths\"` contract, found `script`",
            ),
            (
                r#"{ paths = ["a"], reason = "r", limitation = "l" }"#,
                "for a `\"paths\"` contract, found `limitation`",
            ),
            (
                r#"{ type = "script", paths = ["a"], reason = "r" }"#,
                "needs a non-empty `script`",
            ),
            (
                r#"{ type = "script", script = "/bin/x.sh", paths = ["a"], reason = "r" }"#,
                "a script outside the repository",
            ),
            (
                r#"{ type = "script", script = "../x.sh", paths = ["a"], reason = "r" }"#,
                "a script outside the repository",
            ),
            (
                r#"{ type = "script", script = "[", paths = ["a"], reason = "r" }"#,
                "names a script glob",
            ),
            (
                r#"{ type = "unresolved", paths = ["a"], reason = "r" }"#,
                "needs a non-empty `limitation`",
            ),
            (
                r#"{ type = "unresolved", paths = ["a"], reason = "r", limitation = "  " }"#,
                "needs a non-empty `limitation`",
            ),
        ] {
            let value: toml::Value = toml::from_str(&format!(r#""a" = [ {entry} ]"#))?;
            let Err(error) = parse_declared_reads("reads", &value) else {
                panic!("a mistyped entry must fail: {entry}");
            };
            assert!(error.to_string().contains(needle), "entry {entry}: {error}");
        }
        Ok(())
    }

    /// A script contract never requires its script to exist: the path is
    /// data unioned into the watch set, never executed during discovery —
    /// a build-generated script is legitimate.
    #[test]
    fn declared_script_contract_ignores_script_existence() -> Result<(), Box<dyn std::error::Error>>
    {
        let value: toml::Value = toml::from_str(
            r#""a" = [ { type = "script", script = "scripts/does-not-exist.sh", paths = [], reason = "generated at build time" } ]"#,
        )?;
        let reads = parse_declared_reads("reads", &value)?;
        assert_eq!(
            reads
                .get("a")
                .and_then(|entries| entries.first())
                .and_then(|entry| entry.script.as_deref()),
            Some("scripts/does-not-exist.sh")
        );
        Ok(())
    }

    /// Type-specific keys stay on their type: `script` on an `unresolved`
    /// entry, `limitation` on a `script` entry, and `complete` on an
    /// `unresolved` entry all fail with the misplaced key named.
    #[test]
    fn declared_read_contracts_reject_cross_type_keys() -> Result<(), Box<dyn std::error::Error>> {
        for (entry, needle) in [
            (
                r#"{ type = "unresolved", paths = ["a"], reason = "r", limitation = "l", script = "s.sh" }"#,
                "for a `\"unresolved\"` contract, found `script`",
            ),
            (
                r#"{ type = "script", script = "s.sh", paths = ["a"], reason = "r", limitation = "l" }"#,
                "for a `\"script\"` contract, found `limitation`",
            ),
            (
                r#"{ type = "unresolved", paths = ["a"], reason = "r", limitation = "l", complete = true }"#,
                "for a `\"unresolved\"` contract, found `complete`",
            ),
        ] {
            let value: toml::Value = toml::from_str(&format!(r#""a" = [ {entry} ]"#))?;
            let Err(error) = parse_declared_reads("reads", &value) else {
                panic!("a cross-type key must fail: {entry}");
            };
            assert!(error.to_string().contains(needle), "entry {entry}: {error}");
        }
        Ok(())
    }

    /// `complete` parses as a boolean on `paths` and `script` entries,
    /// absent means open, and a non-boolean fails closed.
    #[test]
    fn declared_read_complete_parses_as_a_boolean() -> Result<(), Box<dyn std::error::Error>> {
        let value: toml::Value = toml::from_str(
            r#""a" = [
                { paths = ["web/**"], reason = "bundler inputs", complete = true },
                { type = "script", script = "scripts/build.sh", paths = ["schemas/**"], reason = "task runner execs this script", complete = true },
            ]"#,
        )?;
        let reads = parse_declared_reads("reads", &value)?;
        let entries = reads.get("a").ok_or("the unit must carry entries")?;
        assert!(
            entries.iter().all(|entry| entry.complete),
            "uniform complete claims parse: {entries:?}"
        );
        let value: toml::Value =
            toml::from_str(r#""a" = [ { paths = ["web/**"], reason = "bundler inputs" } ]"#)?;
        let reads = parse_declared_reads("reads", &value)?;
        assert!(
            reads
                .get("a")
                .is_some_and(|entries| entries.iter().all(|entry| !entry.complete)),
            "absent complete means open"
        );
        let value: toml::Value =
            toml::from_str(r#""a" = [ { paths = ["a"], reason = "r", complete = "yes" } ]"#)?;
        let Err(error) = parse_declared_reads("reads", &value) else {
            panic!("a non-boolean complete must fail");
        };
        assert!(
            error.to_string().contains("a boolean `complete` flag"),
            "{error}"
        );
        Ok(())
    }

    /// Completeness is unit-wide: mixed `complete` and open entries fail —
    /// including a `complete` claim beside an `unresolved` entry, whose
    /// recorded unknown can never be complete.
    #[test]
    fn declared_read_complete_rejects_mixed_claims() -> Result<(), Box<dyn std::error::Error>> {
        for entries in [
            r#"{ paths = ["web/**"], reason = "bundler inputs", complete = true },
                { paths = ["assets/**"], reason = "more inputs" }"#,
            r#"{ paths = ["web/**"], reason = "bundler inputs", complete = true },
                { type = "unresolved", paths = [], reason = "dynamic", limitation = "plugin graph" }"#,
        ] {
            let value: toml::Value = toml::from_str(&format!(r#""a" = [ {entries} ]"#))?;
            let Err(error) = parse_declared_reads("reads", &value) else {
                panic!("mixed complete and open entries must fail: {entries}");
            };
            assert!(
                error
                    .to_string()
                    .contains("mixes `complete` and open entries"),
                "{error}"
            );
        }
        Ok(())
    }

    /// Declared paths stay inside the repository: absolute paths and root
    /// escapes fail closed, since diff paths are repo-relative and such a
    /// claim could never match.
    #[test]
    fn declared_read_paths_reject_escapes() -> Result<(), Box<dyn std::error::Error>> {
        for entry in [
            r#"{ paths = ["/etc/shadow"], reason = "r" }"#,
            r#"{ paths = ["../sibling/**"], reason = "r" }"#,
            r#"{ paths = ["a/../../escape"], reason = "r" }"#,
        ] {
            let value: toml::Value = toml::from_str(&format!(r#""a" = [ {entry} ]"#))?;
            let Err(error) = parse_declared_reads("reads", &value) else {
                panic!("an escaping path must fail: {entry}");
            };
            assert!(
                error.to_string().contains("outside the repository"),
                "entry {entry}: {error}"
            );
        }
        Ok(())
    }

    /// `script` names one literal script: globs fail closed, and
    /// surrounding whitespace is trimmed before the path is stored.
    #[test]
    fn declared_script_contract_rejects_globs_and_trims() -> Result<(), Box<dyn std::error::Error>>
    {
        for script in ["scripts/*.sh", "scripts/build?.sh", "scripts/[ab].sh"] {
            let value: toml::Value = toml::from_str(&format!(
                r#""a" = [ {{ type = "script", script = "{script}", paths = [], reason = "r" }} ]"#
            ))?;
            let Err(error) = parse_declared_reads("reads", &value) else {
                panic!("a script glob must fail: {script}");
            };
            assert!(
                error.to_string().contains("names a script glob"),
                "script {script}: {error}"
            );
        }
        let value: toml::Value = toml::from_str(
            r#""a" = [ { type = "script", script = "  scripts/build.sh  ", paths = [], reason = "r" } ]"#,
        )?;
        let reads = parse_declared_reads("reads", &value)?;
        assert_eq!(
            reads
                .get("a")
                .and_then(|entries| entries.first())
                .and_then(|entry| entry.script.as_deref()),
            Some("scripts/build.sh"),
            "the stored script is trimmed"
        );
        Ok(())
    }
}

//! Repository-owned generation config: the explicit input half of generation.
//!
//! Generation is a pure function of the repository shape, this config, and the
//! generator revision. The config lives in the target repository at
//! `.github-gen/velnor-workflow.toml`, is optional (a repository without one
//! keeps the generator's default behavior), and is fail-closed: a config that
//! fails to parse or validate stops generation instead of being ignored.
//!
//! The config is where a repository states everything the generator used to
//! know for it: its identity, its runner placement, its profile label, its
//! release contract, the units it adds or overrides, the adopted template
//! directory it renders from, and the repository-local files the generated
//! output owns. Nothing about a specific repository lives in the generator.

pub(crate) mod canonical;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::provider::{parse_provider_set, parse_selectors, ProviderSelector};
use crate::{content_digest_bytes, GeneratorError};

/// Location of the repository-owned generation config, relative to the
/// repository root.
pub(crate) const GENERATION_CONFIG_PATH: &str = ".github-gen/velnor-workflow.toml";

/// The only accepted `schema` value. Rejecting every other value keeps the
/// config contract explicit instead of guessing at future layouts. Schema 2
/// is the provider-set contract; schema 1 lane strings are not read.
const CONFIG_SCHEMA: i64 = 2;

/// Load and parse the generation config at `path`.
///
/// # Errors
/// Returns filesystem errors and parse errors with the affected path.
pub(crate) fn load(path: &Path) -> Result<RepoGenerationConfig, GeneratorError> {
    let bytes = fs::read(path)
        .map_err(|error| GeneratorError::io("read generation config", path, &error))?;
    parse(path, &bytes)
}

/// Discover the generation config at the repository root.
///
/// A missing config is a valid outcome: repositories without a config keep the
/// generator's default behavior.
///
/// # Errors
/// Returns filesystem errors and parse errors with the affected path.
pub(crate) fn discover(root: &Path) -> Result<Option<RepoGenerationConfig>, GeneratorError> {
    let path = root.join(GENERATION_CONFIG_PATH);
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Err(GeneratorError::usage(format!(
            "generation config is a directory: {}",
            path.display()
        ))),
        Ok(_) => load(&path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(GeneratorError::io(
            "inspect generation config",
            &path,
            &error,
        )),
    }
}

pub(crate) fn parse(path: &Path, bytes: &[u8]) -> Result<RepoGenerationConfig, GeneratorError> {
    let content = std::str::from_utf8(bytes).map_err(|_| {
        GeneratorError::usage(format!(
            "generation config must be UTF-8: {}",
            path.display()
        ))
    })?;
    let config = toml::from_str::<RepoGenerationConfig>(content).map_err(|error| {
        GeneratorError::usage(format!(
            "invalid generation config {}: {error}",
            path.display()
        ))
    })?;
    config.schema_error(path)?;
    Ok(config)
}

/// Repository-owned generation config.
///
/// Every override is optional: an absent value means "keep the generator's
/// current default", which is what makes a repository able to adopt the config
/// one section at a time.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepoGenerationConfig {
    /// Config contract version; must be exactly [`CONFIG_SCHEMA`].
    schema: Option<i64>,
    #[serde(default)]
    generator: GeneratorSection,
    #[serde(default)]
    workflow: WorkflowSection,
    #[serde(default)]
    scan: ScanSection,
    #[serde(default)]
    policy: PolicySection,
    #[serde(default)]
    release: ReleaseSection,
    #[serde(default)]
    renovate: RenovateSection,
    #[serde(default)]
    units: Vec<UnitSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    static_files: Vec<StaticFileSection>,
    #[serde(default)]
    declare: Vec<DeclareRow>,
    /// Generator-only dual-lane cache budgets. Never serialized into
    /// `.github/ci/project.toml`.
    #[serde(default)]
    cache: CacheRootSection,
}

/// GitHub Actions cache account retention (`[cache.github]`). Governs
/// `velnor-workflow cache-plan` only; never merged with Velnor host GC.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CacheGithubSection {
    pub(crate) budget_bytes: Option<u64>,
    pub(crate) producer_window_seconds: Option<u64>,
    pub(crate) mbx_generation_bound: Option<u32>,
}

/// Velnor host persistent-store budgets (`[cache.velnor]`). Emitted as a
/// fleet `velnor.env` snippet; never serialized into `.github/ci/project.toml`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CacheVelnorSection {
    pub(crate) budget_bytes: Option<u64>,
    pub(crate) producer_window_seconds: Option<u64>,
    pub(crate) mbx_generation_bound: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheRootSection {
    #[serde(default)]
    github: CacheGithubSection,
    #[serde(default)]
    velnor: CacheVelnorSection,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratorSection {
    /// `owner/repository` slug this config belongs to.
    repository: Option<String>,
    /// D19: the generator-repository commit whose `velnor-workflow` renders
    /// and audits this tree. Every generated pin — the policy runtime install
    /// `--rev`, the `setup-velnor-workflow` `rev:`, the runtime artifact
    /// names — is this one value, so the tree declares the generator that
    /// produced it and the policy validator regenerates the tree with exactly
    /// that generator. Absent keeps the running binary's own source commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowSection {
    /// The provider universe for this repo. Non-empty. Absent keeps the
    /// generator default (all three providers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    providers: Option<Vec<String>>,
    /// Providers that run on `pull_request`/`push`/`schedule`. A subset of
    /// `providers`; pure event-to-provider routing, never trust gating.
    /// Absent keeps the generator default (the full universe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    automatic_providers: Option<Vec<String>>,
    /// Default providers for the `workflow_dispatch` `providers:` multi-select
    /// input. A subset of `providers`. Absent keeps the generator default
    /// (the full universe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_dispatch_providers: Option<Vec<String>>,
    /// Per-provider `runs-on` routing, keyed by provider ID. The only place
    /// labels live; local providers need disjoint dedicated selectors.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    selectors: BTreeMap<String, ProviderSelector>,
    /// The repository profile recorded in the generated `project.toml`. A free
    /// label: it describes the surface, it never selects one.
    profile: Option<String>,
    /// The review flag recorded in the generated `project.toml`.
    verified: Option<bool>,
    /// The owned workflow file list, replacing the generator's default list.
    /// Declaring it is what makes the surface authoritative over whatever is
    /// checked in under `.github/workflows`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files: Option<Vec<String>>,
    /// Repository directory the adopted workflow surface renders from, instead
    /// of the legacy `.github/ci/workflow-templates` location. The declared
    /// directory must exist and own every workflow it names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    templates: Option<String>,
    /// Units whose release bumps are recorded in the generated `project.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version_bump_units: Option<Vec<String>>,
    /// Per-owner-block update channel grants for the rendered
    /// `package-update.yml` matrix; the `default` key covers owner blocks
    /// without their own row. Absent from the canonical form, so configs that
    /// do not use it keep their recorded digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_update_channels: Option<BTreeMap<String, Vec<String>>>,
    /// Overrides the resolved default branch used for branch gates.
    default_branch: Option<String>,
    /// How Rust unit jobs relate through GitHub Actions `needs:` on every
    /// provider. Absent keeps parallel starts; `dependency-closure` waits on
    /// direct `depends_on` Rust unit jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rust_needs: Option<String>,
    /// When set, generated local-provider PR aggregate workflows derive a
    /// pull-request-scoped concurrency group from this value so limited
    /// local capacity admits one verification run at a time across pull
    /// requests. Main aggregates append `github.run_id` so unrelated main
    /// executions remain concurrent. The separate read-only policy workflow
    /// derives a `-policy` suffix, so a queued policy check cannot hold the
    /// verification workflow at the GitHub workflow-run concurrency
    /// boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    concurrency_group: Option<String>,
    /// When true, aggregate stack-group callers on local providers chain
    /// through `needs:` instead of fanning out from `plan` in parallel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    serial_stack_groups: Option<bool>,
}

/// The Renovate contract a repository declares for self-hosted dependency
/// updates. Credentials and runner placement cannot be inferred from scan
/// evidence alone, so emission stays explicit and fail-closed.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenovateSection {
    enabled: Option<bool>,
    reason: Option<String>,
    schedule: Option<String>,
    token: Option<String>,
    config: Option<String>,
    validate: Option<bool>,
    cache: Option<bool>,
}

/// The release contract a repository declares for itself. `kind` names the
/// publisher the renderer implements; every other field is the contract that
/// publisher renders from, so an incomplete contract is a configuration error
/// instead of a partially rendered workflow.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseSection {
    enabled: Option<bool>,
    reason: Option<String>,
    kind: Option<String>,
    package: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    packages: Vec<String>,
    binary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    targets: Vec<String>,
    image: Option<String>,
    source_repository: Option<String>,
    consumer_repository: Option<String>,
    artifact_path: Option<String>,
    description: Option<String>,
    manifest_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    apt_arches: Vec<String>,
    signer_fingerprint: Option<String>,
    passphrase_secret: Option<String>,
    keyring_path: Option<String>,
    apt_origin: Option<String>,
    apt_identity_dir: Option<String>,
    apt_feed_url: Option<String>,
    retention: Option<i64>,
}

/// One verification unit the repository adds to, or overrides in, the scanned
/// shape. A row whose `id` the scan produced overrides only the fields it
/// names; any other id adds a unit, and then `kind` is required.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitSection {
    id: Option<String>,
    label: Option<String>,
    kind: Option<String>,
    root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    watch: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pr_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    full_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depends_on: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache: Option<UnitCacheSection>,
    /// Whether the unit's Cargo verification holds behind a root `Cargo.lock`
    /// pin. The scan derives this for scanned units; a `[[unit]]` row that
    /// adds a unit the scan did not produce states it explicitly so the
    /// rendered Cargo source preparation matches a scanned crate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pinned_lockfile: Option<bool>,
    tool_version: Option<String>,
    /// Workspace-wide `cargo check`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_check: Option<bool>,
    /// Named mise tasks that exist in `mise.toml`. Not a shell-command array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ci_tasks: Option<Vec<String>>,
    /// Additional mise tool ids the unit's jobs install when the scanner
    /// cannot observe a runtime-invoked tool. Each id renders verbatim into
    /// `install_args`, so it must equal a key the root `mise.lock` pins, bare
    /// or backend-qualified exactly as the lock spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mise_tools: Option<Vec<String>>,
    /// The trust tier this unit needs: `untrusted-ok` (default) or
    /// `trusted-only`. Typed; trust is evaluated against (event, provider),
    /// never expressed as a label on `runs-on`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trust: Option<String>,
    /// The execution platform this unit needs: `linux-x64` (default),
    /// `linux-arm64`, or `macos-arm64`. Typed; platforms are never labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform: Option<String>,
}

/// The cache contract of a `[[unit]]` row. Each field is independent, so an
/// override can replace one side of the contract without restating the other.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnitCacheSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paths: Option<Vec<String>>,
    /// Whether this cache is the mutable mount seed of a Docker image build:
    /// the generator renders the host restore, seed-context preparation, and
    /// trusted-only export collection around the lane's declared build
    /// commands instead of the generic path cache, so the compiler state the
    /// build's cache mounts hold survives onto a fresh builder. Declared, not
    /// guessed: the declared build commands must prove they inject and extract
    /// the seed, and the validator refuses the combination otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mutable_mount_seed: Option<bool>,
}

/// A repository-local file outside the workflow surface that the generated
/// output owns verbatim, such as a composite action the emitted workflows
/// call. `source` is read from the repository at generation time, so the
/// repository owns the bytes and the generator owns the write.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StaticFileSection {
    file: Option<String>,
    source: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanSection {
    /// Repository paths the scan must ignore.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicySection {
    /// Require a `Signed-off-by` trailer on every commit.
    dco_required: Option<bool>,
    /// Require the generated policy workflow to conclude on a pull request.
    ci_required: Option<bool>,
    /// Repository-ruleset status-check contexts that `ci-pr.yml` must expose as
    /// top-level job `name:` values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ruleset_required_status_checks: Vec<String>,
    /// Repository-ruleset status-check contexts reported by GitHub Apps rather
    /// than by a workflow (for example `DCO`). The policy validator requires
    /// the live ruleset to equal the union of both lists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ruleset_external_status_checks: Vec<String>,
    /// Admission rule for actions that are not pinned to a full commit SHA.
    action_pin_admission: Option<String>,
    /// Emit `config-variables: null` in the generated actionlint config.
    actionlint_config_variables_null: Option<bool>,
    /// Workflow basenames skipped by `velnor-workflow policy` until migrated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    exclude_workflows: Vec<String>,
}

impl RenovateSection {
    pub(crate) fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub(crate) fn schedule(&self) -> Option<&str> {
        self.schedule.as_deref()
    }

    pub(crate) fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub(crate) fn config(&self) -> Option<&str> {
        self.config.as_deref()
    }

    pub(crate) fn validate(&self) -> Option<bool> {
        self.validate
    }

    pub(crate) fn cache(&self) -> Option<bool> {
        self.cache
    }
}

impl ReleaseSection {
    pub(crate) fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub(crate) fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    pub(crate) fn package(&self) -> Option<&str> {
        self.package.as_deref()
    }

    pub(crate) fn packages(&self) -> &[String] {
        &self.packages
    }

    pub(crate) fn binary(&self) -> Option<&str> {
        self.binary.as_deref()
    }

    pub(crate) fn targets(&self) -> &[String] {
        &self.targets
    }

    pub(crate) fn image(&self) -> Option<&str> {
        self.image.as_deref()
    }

    pub(crate) fn source_repository(&self) -> Option<&str> {
        self.source_repository.as_deref()
    }

    pub(crate) fn consumer_repository(&self) -> Option<&str> {
        self.consumer_repository.as_deref()
    }

    pub(crate) fn artifact_path(&self) -> Option<&str> {
        self.artifact_path.as_deref()
    }

    pub(crate) fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub(crate) fn manifest_schema(&self) -> Option<&str> {
        self.manifest_schema.as_deref()
    }

    pub(crate) fn apt_arches(&self) -> &[String] {
        &self.apt_arches
    }

    pub(crate) fn signer_fingerprint(&self) -> Option<&str> {
        self.signer_fingerprint.as_deref()
    }

    pub(crate) fn passphrase_secret(&self) -> Option<&str> {
        self.passphrase_secret.as_deref()
    }

    pub(crate) fn keyring_path(&self) -> Option<&str> {
        self.keyring_path.as_deref()
    }

    pub(crate) fn apt_origin(&self) -> Option<&str> {
        self.apt_origin.as_deref()
    }

    pub(crate) fn apt_identity_dir(&self) -> Option<&str> {
        self.apt_identity_dir.as_deref()
    }

    pub(crate) fn apt_feed_url(&self) -> Option<&str> {
        self.apt_feed_url.as_deref()
    }

    pub(crate) fn retention(&self) -> Option<i64> {
        self.retention
    }
}

impl UnitSection {
    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub(crate) fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub(crate) fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    pub(crate) fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    pub(crate) fn watch(&self) -> Option<&[String]> {
        self.watch.as_deref()
    }

    pub(crate) fn pr_commands(&self) -> Option<&[String]> {
        self.pr_commands.as_deref()
    }

    pub(crate) fn full_commands(&self) -> Option<&[String]> {
        self.full_commands.as_deref()
    }

    pub(crate) fn depends_on(&self) -> Option<&[String]> {
        self.depends_on.as_deref()
    }

    pub(crate) fn cache(&self) -> Option<&UnitCacheSection> {
        self.cache.as_ref()
    }

    pub(crate) fn pinned_lockfile(&self) -> Option<bool> {
        self.pinned_lockfile
    }

    pub(crate) fn tool_version(&self) -> Option<&str> {
        self.tool_version.as_deref()
    }

    pub(crate) fn workspace_check(&self) -> bool {
        self.workspace_check == Some(true)
    }

    pub(crate) fn ci_tasks(&self) -> &[String] {
        self.ci_tasks.as_deref().unwrap_or(&[])
    }

    pub(crate) fn mise_tools(&self) -> Option<&[String]> {
        self.mise_tools.as_deref()
    }

    pub(crate) fn trust(&self) -> Option<&str> {
        self.trust.as_deref()
    }

    pub(crate) fn platform(&self) -> Option<&str> {
        self.platform.as_deref()
    }
}

impl UnitCacheSection {
    pub(crate) fn key_files(&self) -> Option<&[String]> {
        self.key_files.as_deref()
    }

    pub(crate) fn paths(&self) -> Option<&[String]> {
        self.paths.as_deref()
    }

    pub(crate) fn mutable_mount_seed(&self) -> bool {
        self.mutable_mount_seed.unwrap_or(false)
    }
}

impl StaticFileSection {
    pub(crate) fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    pub(crate) fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
}

/// One declared render primitive and the units it applies to.
///
/// `args` is free-form on purpose: this phase stores it verbatim and digests
/// it as given, so a future primitive can define its own arguments without a
/// config schema migration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeclareRow {
    /// The primitive the row names.
    pub(crate) primitive: Option<String>,
    /// The unit ids the row applies to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) units: Vec<String>,
    /// The bare workflow file name the primitive renders into.
    pub(crate) file: Option<String>,
    /// Opaque primitive arguments, stored and digested as given.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) args: BTreeMap<String, toml::Value>,
}

impl DeclareRow {
    /// The primitive the row names.
    pub(crate) fn primitive(&self) -> &str {
        self.primitive.as_deref().unwrap_or_default()
    }

    /// The unit ids the row applies to.
    pub(crate) fn units(&self) -> &[String] {
        &self.units
    }

    /// The workflow file the row renders into, when it names one.
    pub(crate) fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    /// The primitive's own arguments, as the config gave them.
    pub(crate) fn args(&self) -> &BTreeMap<String, toml::Value> {
        &self.args
    }
}

/// Default Velnor host cache budget: 50 GiB (`[cache.velnor].budget_bytes`).
pub(crate) const DEFAULT_VELNOR_HOST_CACHE_BYTES: u64 = 53_687_091_200;

/// Render the fleet host env snippet from `[cache.velnor]` overrides.
pub(crate) fn render_velnor_host_env(section: &CacheVelnorSection) -> String {
    let budget_caches = section
        .budget_bytes
        .unwrap_or(DEFAULT_VELNOR_HOST_CACHE_BYTES);
    let mbx_generation_bound = section.mbx_generation_bound.unwrap_or(6);
    format!(
        "# Generated by velnor-workflow. Merge into /etc/velnor/velnor.env on fleet hosts.\n\
         # Generator-only [cache.velnor]; never written to .github/ci/project.toml.\n\
         VELNOR_STORAGE_ROOT=/var\n\
         VELNOR_BUDGET_CACHES_BYTES={budget_caches}\n\
         VELNOR_BUDGET_CARGO_BYTES=21474836480\n\
         VELNOR_BUDGET_MISE_BYTES=21474836480\n\
         VELNOR_BUDGET_ARTIFACTS_BYTES=21474836480\n\
         VELNOR_BUDGET_TARGETS_BYTES=214748364800\n\
         MBX_GC_MAX_TOTAL_SIZE=50GiB\n\
         # Same-repo PR jobs write the pr scope only; trusted events write trusted (D18).\n\
         VELNOR_MBX_GENERATION_BOUND={mbx_generation_bound}\n"
    )
}

impl RepoGenerationConfig {
    /// The declared render primitives, in the order the config declares them.
    pub(crate) fn declare(&self) -> &[DeclareRow] {
        &self.declare
    }

    /// The declared per-owner-block update channel grants, when the config
    /// declares them.
    pub(crate) fn package_update_channels(&self) -> Option<BTreeMap<String, Vec<String>>> {
        self.workflow.package_update_channels.clone()
    }

    /// The repository directory the adopted workflow surface renders from.
    pub(crate) fn templates_dir(&self) -> Option<&str> {
        self.workflow.templates.as_deref()
    }

    /// The `owner/repository` slug the config declares, if any.
    pub(crate) fn repository(&self) -> Option<&str> {
        self.generator.repository.as_deref()
    }

    /// The D19 generator pin the config declares, if any.
    pub(crate) fn revision(&self) -> Option<&str> {
        self.generator.revision.as_deref()
    }

    /// The declared provider universe, if any.
    pub(crate) fn providers(&self) -> Option<&[String]> {
        self.workflow.providers.as_deref()
    }

    /// The declared automatic providers, if any.
    pub(crate) fn automatic_providers(&self) -> Option<&[String]> {
        self.workflow.automatic_providers.as_deref()
    }

    /// The declared default dispatch providers, if any.
    pub(crate) fn default_dispatch_providers(&self) -> Option<&[String]> {
        self.workflow.default_dispatch_providers.as_deref()
    }

    /// The declared per-provider selectors, keyed by provider id string.
    pub(crate) fn selectors(&self) -> &BTreeMap<String, ProviderSelector> {
        &self.workflow.selectors
    }

    /// The declared Rust unit `needs:` topology.
    pub(crate) fn rust_needs(&self) -> Option<&str> {
        self.workflow.rust_needs.as_deref()
    }

    /// The declared repository-scoped local-provider concurrency group.
    pub(crate) fn concurrency_group(&self) -> Option<&str> {
        self.workflow.concurrency_group.as_deref()
    }

    /// Whether aggregate stack groups serialize on local providers.
    pub(crate) fn serial_stack_groups(&self) -> Option<bool> {
        self.workflow.serial_stack_groups
    }
    /// The declared profile label.
    pub(crate) fn profile(&self) -> Option<&str> {
        self.workflow.profile.as_deref()
    }

    /// The declared review flag.
    pub(crate) fn verified(&self) -> Option<bool> {
        self.workflow.verified
    }

    /// The declared owned workflow file list.
    pub(crate) fn files(&self) -> Option<&[String]> {
        self.workflow.files.as_deref()
    }

    /// The declared version-bump unit ids.
    pub(crate) fn version_bump_units(&self) -> Option<&[String]> {
        self.workflow.version_bump_units.as_deref()
    }

    /// The declared default-branch override.
    pub(crate) fn default_branch(&self) -> Option<&str> {
        self.workflow.default_branch.as_deref()
    }

    /// Whether the generated actionlint config declares no configuration
    /// variables.
    pub(crate) fn actionlint_config_variables_null(&self) -> Option<bool> {
        self.policy.actionlint_config_variables_null
    }

    /// The declared release contract.
    pub(crate) fn release(&self) -> &ReleaseSection {
        &self.release
    }

    /// The declared Renovate contract.
    pub(crate) fn renovate(&self) -> &RenovateSection {
        &self.renovate
    }

    /// The declared unit rows, in the order the config declares them.
    pub(crate) fn units(&self) -> &[UnitSection] {
        &self.units
    }

    /// GitHub Actions cache retention overrides from `[cache.github]`.
    pub(crate) fn cache_github(&self) -> &CacheGithubSection {
        &self.cache.github
    }

    /// Velnor host cache budget overrides from `[cache.velnor]`.
    pub(crate) fn cache_velnor(&self) -> &CacheVelnorSection {
        &self.cache.velnor
    }

    /// The declared repository-local files the generated output owns.
    pub(crate) fn static_files(&self) -> &[StaticFileSection] {
        &self.static_files
    }

    /// The repository paths excluded from the scan.
    pub(crate) fn scan_exclude(&self) -> Result<&[String], GeneratorError> {
        validate_excludes(&self.scan.exclude)?;
        Ok(&self.scan.exclude)
    }

    /// Whether the generated CI aggregate should be required.
    pub(crate) fn ci_required(&self) -> Option<bool> {
        self.policy.ci_required
    }

    /// Status-check contexts the repository ruleset gates on that `ci-pr.yml`
    /// must expose as job display names.
    pub(crate) fn ruleset_required_status_checks(&self) -> &[String] {
        &self.policy.ruleset_required_status_checks
    }

    /// Status-check contexts the repository ruleset gates on that GitHub Apps
    /// report rather than workflows.
    pub(crate) fn ruleset_external_status_checks(&self) -> &[String] {
        &self.policy.ruleset_external_status_checks
    }

    /// Workflow basenames excluded from static policy validation.
    pub(crate) fn policy_exclude_workflows(&self) -> &[String] {
        &self.policy.exclude_workflows
    }

    /// Explicit policy excludes plus every owned static workflow file.
    pub(crate) fn effective_policy_exclude_workflows(&self) -> BTreeSet<String> {
        let mut excludes = self
            .policy_exclude_workflows()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for row in &self.static_files {
            let Some(file) = row.file.as_deref() else {
                continue;
            };
            if !file.starts_with(".github/workflows/") {
                continue;
            }
            let Some(name) = Path::new(file).file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            excludes.insert(name.to_owned());
        }
        excludes
    }

    fn schema_error(&self, path: &Path) -> Result<(), GeneratorError> {
        match self.schema {
            Some(CONFIG_SCHEMA) => Ok(()),
            Some(found) => Err(GeneratorError::usage(format!(
                "generation config {} has schema {found}; this generator reads schema {CONFIG_SCHEMA} only",
                path.display()
            ))),
            None => Err(GeneratorError::usage(format!(
                "generation config {} is missing `schema = {CONFIG_SCHEMA}`",
                path.display()
            ))),
        }
    }

    /// Referential validation against the scanned repository shape.
    ///
    /// Only rules that need the shape live here; structural rules are checked
    /// while parsing and while canonicalizing.
    ///
    /// # Errors
    /// Returns an error naming the offending value and, for unknown unit ids,
    /// every unit id the scan did produce.
    pub(crate) fn validate(
        &self,
        unit_ids: &[String],
        package_update_blocks: &[&str],
        mise_lock_keys: &BTreeSet<String>,
    ) -> Result<(), GeneratorError> {
        let repository = self.generator.repository.as_deref().ok_or_else(|| {
            GeneratorError::usage(
                "generation config is missing `[generator] repository = \"owner/repository\"`",
            )
        })?;
        validate_repository_slug(repository)?;
        validate_workflow(&self.workflow)?;
        for row in &self.declare {
            validate_declare_row(row, unit_ids)?;
        }
        validate_excludes(&self.scan.exclude)?;
        validate_package_update_channels(
            self.workflow.package_update_channels.as_ref(),
            package_update_blocks,
        )?;
        validate_workflow_files(self.workflow.files.as_deref())?;
        validate_units(&self.units, mise_lock_keys)?;
        validate_unit_references(
            &self.units,
            self.workflow.version_bump_units.as_deref(),
            unit_ids,
        )?;
        validate_static_files(&self.static_files)?;
        self.validate_release()?;
        self.validate_renovate()?;
        Ok(())
    }

    /// Canonical serialization used for the config input digest.
    ///
    /// Declaration order is significant and preserved; table keys are sorted.
    ///
    /// # Errors
    /// Returns an error for values without a stable canonical form.
    pub(crate) fn canonical_json(&self) -> Result<String, GeneratorError> {
        let value = serde_json::to_value(self).map_err(|error| {
            GeneratorError::usage(format!("canonicalize generation config: {error}"))
        })?;
        canonical::canonical_value(&value)
    }

    /// FNV-1a digest of the canonical config, or of the empty canonical form
    /// when the repository has no config.
    ///
    /// # Errors
    /// Returns an error for values without a stable canonical form.
    pub(crate) fn digest(config: Option<&Self>) -> Result<u64, GeneratorError> {
        let canonical = match config {
            Some(config) => config.canonical_json()?,
            None => canonical::EMPTY_CANONICAL_FORM.to_owned(),
        };
        Ok(content_digest_bytes(canonical.as_bytes()))
    }
}

fn validate_repository_slug(repository: &str) -> Result<(), GeneratorError> {
    let (owner, name) = repository.split_once('/').ok_or_else(|| {
        GeneratorError::usage(format!(
            "`[generator] repository` must be `owner/repository`: {repository}"
        ))
    })?;
    for (role, segment) in [("owner", owner), ("repository", name)] {
        // GitHub names are case-insensitive where it matters, so a `.git`
        // suffix is rejected in any case.
        let git_suffix = Path::new(segment)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("git"));
        let valid = !segment.is_empty()
            && segment.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_')
            })
            && !segment.starts_with('.')
            && !git_suffix;
        if !valid {
            return Err(GeneratorError::usage(format!(
                "`[generator] repository` {role} is not a valid GitHub name: {repository}"
            )));
        }
    }
    Ok(())
}

fn validate_declare_row(row: &DeclareRow, unit_ids: &[String]) -> Result<(), GeneratorError> {
    let primitive = row.primitive.as_deref().unwrap_or_default();
    if primitive.is_empty() {
        return Err(GeneratorError::usage(
            "[[declare]] is missing `primitive`; name the render primitive the row declares",
        ));
    }
    // A family that renders no file of its own — the contracts and the plan —
    // declares none.
    if let Some(file) = row.file.as_deref() {
        validate_workflow_file_name(file)?;
    }
    for unit in &row.units {
        if !unit_ids.iter().any(|candidate| candidate == unit) {
            return Err(GeneratorError::usage(format!(
                "[[declare]] primitive `{primitive}` names unit `{unit}`, which the scan did not produce; available units: {}",
                unit_ids.join(", ")
            )));
        }
    }
    for (key, value) in &row.args {
        validate_arg_value(key, value)?;
    }
    Ok(())
}

/// A declared file is a workflow file name only: never a path, never a
/// directory write, never another extension.
fn validate_workflow_file_name(file: &str) -> Result<(), GeneratorError> {
    let valid = file.len() > ".yml".len()
        && Path::new(file)
            .extension()
            .is_some_and(|extension| extension == "yml")
        && !file.contains(['/', '\\'])
        && !file.contains("..")
        && Path::new(file)
            .file_stem()
            .is_some_and(|stem| !stem.is_empty());
    if valid {
        return Ok(());
    }
    Err(GeneratorError::usage(format!(
        "[[declare]] file must be a bare `.yml` workflow file name: {file}"
    )))
}

fn validate_excludes(exclude: &[String]) -> Result<(), GeneratorError> {
    for pattern in exclude {
        if pattern.is_empty() {
            return Err(GeneratorError::usage(
                "[scan] exclude must not contain an empty pattern",
            ));
        }
        if globset::Glob::new(pattern).is_err() {
            return Err(GeneratorError::usage(format!(
                "[scan] exclude is not a valid glob: {pattern}"
            )));
        }
    }
    Ok(())
}

fn validate_workflow(workflow: &WorkflowSection) -> Result<(), GeneratorError> {
    use crate::provider::{require_non_empty, require_subset, validate_selector_disjointness};
    let universe = workflow
        .providers
        .as_deref()
        .map(|providers| parse_provider_set(providers, "[workflow] providers"))
        .transpose()?
        .unwrap_or_else(|| crate::provider::ProviderId::ALL.into_iter().collect());
    if workflow.providers.is_some() {
        require_non_empty(&universe, "[workflow] providers")?;
    }
    if let Some(automatic) = workflow.automatic_providers.as_deref() {
        let automatic = parse_provider_set(automatic, "[workflow] automatic_providers")?;
        require_subset(
            &automatic,
            &universe,
            "[workflow] automatic_providers",
            "[workflow] providers",
        )?;
    }
    if let Some(dispatch) = workflow.default_dispatch_providers.as_deref() {
        let dispatch = parse_provider_set(dispatch, "[workflow] default_dispatch_providers")?;
        require_subset(
            &dispatch,
            &universe,
            "[workflow] default_dispatch_providers",
            "[workflow] providers",
        )?;
    }
    let selectors = parse_selectors(&workflow.selectors)?;
    validate_selector_disjointness(&selectors)?;
    if workflow.templates.is_some() {
        return Err(GeneratorError::usage(
            "[workflow] templates is not supported; imported workflow bodies are not a generation input",
        ));
    }
    if let Some(value) = workflow.rust_needs.as_deref() {
        crate::parse_rust_needs(value)?;
    }
    if let Some(value) = workflow.concurrency_group.as_deref() {
        crate::validate_config_text(value, "[workflow] concurrency_group")?;
    }
    Ok(())
}

/// The update channels the rendered `package-update.yml` matrix can grant. The
/// consumer-side `package-updater.yml` implements a `stable` arm and a
/// `preview` arm and nothing else, so any other channel renders a scheduled job
/// that can never succeed. The legacy grant table in the crate root draws only
/// from this set.
const PACKAGE_UPDATE_CHANNELS: &[&str] = &["stable", "preview"];

/// The `package_update_channels` key replaces the legacy per-block grant table
/// wholesale, so a declared table has to stand on its own: every grant names a
/// channel the rendered updater implements, no grant is empty, and every owner
/// block of the rendered workflow is covered by a row or by `default`. Anything
/// else would silently narrow or drop a publish lane.
fn validate_package_update_channels(
    grants: Option<&BTreeMap<String, Vec<String>>>,
    blocks: &[&str],
) -> Result<(), GeneratorError> {
    let Some(grants) = grants else {
        return Ok(());
    };
    // A grant table with no rendered matrix to grant is a configuration that
    // narrowed nothing and said so: name the file or drop the table.
    if blocks.is_empty() {
        return Err(GeneratorError::usage(
            "[workflow] package_update_channels is declared, but the generated surface renders no `package-update.yml`; declare its template or drop the grant table",
        ));
    }
    for (block, channels) in grants {
        if channels.is_empty() {
            return Err(GeneratorError::usage(format!(
                "[workflow] package_update_channels grants block `{block}` an empty channel list; name the channels it may consult or remove the row"
            )));
        }
        for channel in channels {
            if !PACKAGE_UPDATE_CHANNELS.contains(&channel.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "[workflow] package_update_channels grants block `{block}` the channel `{channel}`, which the rendered updater does not implement; implemented channels: {}",
                    PACKAGE_UPDATE_CHANNELS.join(", ")
                )));
            }
        }
    }
    for block in grants.keys() {
        if block != "default" && !blocks.contains(&block.as_str()) {
            return Err(GeneratorError::usage(format!(
                "[workflow] package_update_channels grants `{block}`, which the rendered `package-update.yml` does not declare; owner blocks: {}, or grant `default`",
                blocks.join(", ")
            )));
        }
    }
    if !grants.contains_key("default") {
        for block in blocks {
            if !grants.contains_key(*block) {
                return Err(GeneratorError::usage(format!(
                    "[workflow] package_update_channels covers no row for owner block `{block}` and declares no `default`; owner blocks: {}",
                    blocks.join(", ")
                )));
            }
        }
    }
    Ok(())
}

/// The declared workflow file list replaces the generator's default list, so
/// every entry has to be a bare workflow file name: the list is a surface, not
/// a set of paths.
fn validate_workflow_files(files: Option<&[String]>) -> Result<(), GeneratorError> {
    let Some(files) = files else {
        return Ok(());
    };
    if files.is_empty() {
        return Err(GeneratorError::usage(
            "[workflow] files must not be empty; omit the list to keep the generator's default surface",
        ));
    }
    for file in files {
        validate_workflow_file_name(file)?;
    }
    Ok(())
}

/// A mise tool id renders verbatim into a job's `install_args`, so its shape
/// must be a plain tool key: no versions, flags, whitespace, traversal, or
/// shell metacharacters. This is the shape half of the contract only; whether
/// the id names a locked tool is checked separately against the lock keys (see
/// [`RepoGenerationConfig::validate`]).
///
/// The predicate mirrors `is_valid_install_arg_token` in
/// `crates/velnor-runner/src/mise.rs` exactly, so an id the generator accepts
/// can never fail the runner's shape check. The `backend:` prefix is optional:
/// locks mix bare keys (`cargo-binstall`) and qualified keys
/// (`aqua:nextest-rs/nextest/cargo-nextest`), and `mise --locked` requires the
/// install args to equal the lock keys byte for byte.
fn valid_mise_tool_id(value: &str) -> bool {
    if value.is_empty() || value.len() > 200 {
        return false;
    }
    // Flags, version pins, URLs, and template/shell syntax are never tool keys.
    if value.starts_with('-') || value.contains('@') || value.contains("://") {
        return false;
    }
    // Filesystem paths (absolute, relative, home, traversal, Windows).
    if value.starts_with('/')
        || value.starts_with('.')
        || value.starts_with('~')
        || value.contains('\\')
        || value.contains("..")
    {
        return false;
    }
    if value.starts_with(':') {
        return false;
    }
    // Whitelist the character set. Anything else (whitespace already split off,
    // plus `$`, backticks, quotes, `;`, `&`, `|`, `*`, `?`, parens, braces …)
    // is rejected as a shell metacharacter.
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'+')
    }) {
        return false;
    }
    // Must carry at least one alphanumeric character.
    value.bytes().any(|byte| byte.is_ascii_alphanumeric())
}

/// Location of the mise lockfile, relative to the repository root.
pub(crate) const MISE_LOCK_PATH: &str = "mise.lock";

/// Collect every tool key committed in a `mise.lock` (`[[tools.<key>]]`).
/// Mirrors `lock_tool_keys` in `crates/velnor-runner/src/mise.rs`: strict TOML
/// parsing (never hand-splitting), so quoted keys such as
/// `[[tools."cargo:sccache"]]` resolve to the same key both sides check.
///
/// # Errors
/// Returns a usage error when the lock text is not valid TOML.
pub(crate) fn parse_mise_lock_keys(lock_toml: &str) -> Result<BTreeSet<String>, GeneratorError> {
    let table: toml::Table = lock_toml.parse().map_err(|error| {
        GeneratorError::usage(format!("parse lock TOML for committed tool keys: {error}"))
    })?;
    Ok(table
        .get("tools")
        .and_then(toml::Value::as_table)
        .map(|tools| tools.keys().cloned().collect())
        .unwrap_or_default())
}

/// The actionlint version the root `mise.lock` pins under either spelling of
/// its key (`actionlint` or `aqua:rhysd/actionlint`), or `None` when the lock
/// is absent or does not pin it.
///
/// # Errors
/// Returns an I/O error when the lock cannot be read, and a usage error when
/// it is not valid UTF-8 TOML.
pub(crate) fn mise_lock_actionlint_version(root: &Path) -> Result<Option<String>, GeneratorError> {
    let path = root.join(MISE_LOCK_PATH);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(GeneratorError::io("read mise.lock", &path, &error)),
    };
    let text = String::from_utf8(bytes).map_err(|error| {
        GeneratorError::usage(format!("parse mise.lock {}: {error}", path.display()))
    })?;
    let table: toml::Table = text.parse().map_err(|error| {
        GeneratorError::usage(format!("{}: parse lock TOML: {error}", path.display()))
    })?;
    let Some(tools) = table.get("tools").and_then(toml::Value::as_table) else {
        return Ok(None);
    };
    let entry = ["actionlint", "aqua:rhysd/actionlint"]
        .iter()
        .find_map(|key| tools.get(*key));
    let Some(entry) = entry else {
        return Ok(None);
    };
    let version = entry
        .as_array()
        .and_then(|rows| rows.first())
        .or(Some(entry))
        .and_then(|row| row.get("version"))
        .and_then(toml::Value::as_str);
    Ok(version.map(str::to_owned))
}

/// Read the committed tool keys from the root `mise.lock`.
///
/// A missing lock is a valid outcome — the repository does not pin mise tools,
/// so identity checking has nothing to check against and validation falls back
/// to shape only. Only the root lock is consulted: a unit nested in a
/// subdirectory with its own `mise.lock` is validated against the root keys,
/// which may reject a tool its own lock pins (or admit one the root lock pins
/// but its own does not). Per-unit locks are a known gap; the runner's own
/// lock check stays the final gate.
///
/// # Errors
/// Returns an I/O error when the lock cannot be read, and a usage error when
/// it is not valid UTF-8 TOML.
pub(crate) fn mise_lock_keys_for_root(root: &Path) -> Result<BTreeSet<String>, GeneratorError> {
    let path = root.join(MISE_LOCK_PATH);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(GeneratorError::io("read mise.lock", &path, &error)),
    };
    let text = String::from_utf8(bytes).map_err(|error| {
        GeneratorError::usage(format!("parse mise.lock {}: {error}", path.display()))
    })?;
    parse_mise_lock_keys(&text)
        .map_err(|error| GeneratorError::usage(format!("{}: {error}", path.display())))
}

/// Unit rows either override a scanned unit by id or add one. Two rows for one
/// id would make the effective contract depend on which one the reader trusts,
/// so the second row is refused instead of merged.
fn validate_units(
    units: &[UnitSection],
    mise_lock_keys: &BTreeSet<String>,
) -> Result<(), GeneratorError> {
    for row in units {
        let id = row.id.as_deref().unwrap_or_default();
        if id.is_empty() {
            return Err(GeneratorError::usage(
                "[[unit]] is missing `id`; name the unit the row adds or overrides",
            ));
        }
        if let Some(kind) = row.kind.as_deref()
            && unit_kind_prefix(kind).is_none()
        {
            return Err(GeneratorError::usage(format!(
                    "[[unit]] {id} declares kind `{kind}`, which the generator does not implement; implemented kinds: {}",
                    UNIT_KIND_PREFIXES.join(", ")
                )));
        }
        if row.pr_commands.is_some() || row.full_commands.is_some() {
            return Err(GeneratorError::usage(format!(
                "[[unit]] {id} declares command arrays; generation config is not a workflow programming language. Detected work uses typed capabilities; remove pr_commands and full_commands"
            )));
        }
        if let Some(trust) = row.trust.as_deref() {
            crate::provider::TrustReq::parse(trust).map_err(|_| {
                GeneratorError::usage(format!(
                    "[[unit]] {id} declares trust `{trust}`; expected one of: untrusted-ok, trusted-only"
                ))
            })?;
        }
        if let Some(platform) = row.platform.as_deref() {
            crate::provider::Platform::parse(platform).map_err(|_| {
                GeneratorError::usage(format!(
                    "[[unit]] {id} declares platform `{platform}`; expected one of: linux-x64, linux-arm64, macos-arm64"
                ))
            })?;
        }
        if let Some(cache) = &row.cache
            && (cache.key_files.as_ref().is_none_or(std::vec::Vec::is_empty)
                || cache.paths.as_ref().is_none_or(std::vec::Vec::is_empty))
        {
            return Err(GeneratorError::usage(format!(
                    "[[unit]] {id} declares `[unit.cache]` without both `key_files` and `paths`; a partial cache contract cannot be keyed"
                )));
        }
        if let Some(tools) = row.mise_tools.as_deref() {
            if tools.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "[[unit]] {id} declares an empty mise_tools; omit it or name the tools the unit needs"
                )));
            }
            let mut seen = BTreeSet::new();
            for tool in tools {
                if !valid_mise_tool_id(tool) {
                    return Err(GeneratorError::usage(format!(
                        "[[unit]] {id} declares mise tool {tool}, which is not a plain tool id; use ids such as cargo-binstall or github:owner/repo without versions, flags, whitespace, traversal, or shell metacharacters"
                    )));
                }
                // Shape is checked first so a compromised lock can never
                // smuggle shell syntax into `install_args` through membership.
                // An empty key set means the scan root has no mise.lock, so
                // identity has nothing to check against and shape alone rules.
                if !mise_lock_keys.is_empty() && !mise_lock_keys.contains(tool) {
                    let known = mise_lock_keys
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(GeneratorError::usage(format!(
                        "[[unit]] {id} declares mise tool {tool}, which mise.lock does not pin; install_args must equal the lock keys, known keys: {known}"
                    )));
                }
                if !seen.insert(tool) {
                    return Err(GeneratorError::usage(format!(
                        "[[unit]] {id} declares mise tool {tool} more than once"
                    )));
                }
            }
        }
    }
    for (index, left) in units.iter().enumerate() {
        let duplicate = units[index + 1..].iter().any(|right| right.id == left.id);
        if duplicate {
            return Err(GeneratorError::usage(format!(
                "[[unit]] declares `{}` twice; one row per unit",
                left.id.as_deref().unwrap_or_default()
            )));
        }
    }
    Ok(())
}

/// Every unit a row or a bump list names has to exist once the declared rows
/// are applied: a bumped or depended-on unit that is not there would silently
/// release or order nothing.
fn validate_unit_references(
    units: &[UnitSection],
    version_bump_units: Option<&[String]>,
    scanned: &[String],
) -> Result<(), GeneratorError> {
    let mut known = scanned.to_vec();
    known.extend(units.iter().filter_map(|row| row.id.clone()));
    let report = |role: &str, id: &str| -> Result<(), GeneratorError> {
        if known.iter().any(|candidate| candidate == id) {
            Ok(())
        } else {
            Err(GeneratorError::usage(format!(
                "`{role}` names `{id}`, a unit the repository does not declare; known units: {}",
                known.join(", ")
            )))
        }
    };
    for row in units {
        for id in row.depends_on.iter().flatten() {
            report("depends_on", id)?;
        }
    }
    for id in version_bump_units.into_iter().flatten() {
        report("version_bump_units", id)?;
    }
    Ok(())
}

/// Static file rows write inside `.github/` only, from a repository file that
/// stays inside the repository: anything else would turn configuration into an
/// arbitrary filesystem write.
fn validate_static_files(rows: &[StaticFileSection]) -> Result<(), GeneratorError> {
    for row in rows {
        let file = row.file.as_deref().unwrap_or_default();
        let source = row.source.as_deref().unwrap_or_default();
        if !is_contained_github_path(file) {
            return Err(GeneratorError::usage(format!(
                "[[static_file]] file must be a repository-relative path inside `.github/`, found `{file}`"
            )));
        }
        if !is_contained_repository_path(source) {
            return Err(GeneratorError::usage(format!(
                "[[static_file]] source must be a repository-relative path, found `{source}`"
            )));
        }
        let duplicate = rows
            .iter()
            .filter(|other| other.file.as_deref() == Some(file))
            .count();
        if duplicate > 1 {
            return Err(GeneratorError::usage(format!(
                "[[static_file]] declares `{file}` twice; one row per owned file"
            )));
        }
    }
    Ok(())
}

fn is_contained_github_path(path: &str) -> bool {
    is_contained_repository_path(path) && Path::new(path).starts_with(".github/")
}

fn is_contained_repository_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.split('/').any(|segment| segment == "..")
}

/// The publishers the renderer implements, and the contract fields each one
/// renders from.
const RELEASE_KINDS: &[&str] = &[
    "crates",
    "rust-binary",
    "native",
    "pages",
    "homebrew",
    "apt",
];

pub(crate) fn validate_renovate_token_name(token: &str) -> Result<(), GeneratorError> {
    if token == "GITHUB_TOKEN" {
        return Err(GeneratorError::usage(
            "[renovate] token must name a dedicated PAT secret, not GITHUB_TOKEN",
        ));
    }
    let valid = !token.is_empty()
        && token.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
        && token
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase());
    if !valid {
        return Err(GeneratorError::usage(format!(
            "[renovate] token must be an uppercase secret name such as GH_RENOVATE_TOKEN, found `{token}`"
        )));
    }
    Ok(())
}

pub(crate) fn validate_renovate_cron(schedule: &str) -> Result<(), GeneratorError> {
    let fields = schedule.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err(GeneratorError::usage(format!(
            "[renovate] schedule must be a 5-field cron expression, found `{schedule}`"
        )));
    }
    Ok(())
}

pub(crate) fn validate_renovate_config_path(config: &str) -> Result<(), GeneratorError> {
    if config.is_empty()
        || config.starts_with('/')
        || config.contains('\\')
        || config.contains("..")
    {
        return Err(GeneratorError::usage(format!(
            "[renovate] config must be a repository-relative Renovate config path, found `{config}`"
        )));
    }
    Ok(())
}

/// A `kind` the renderer does not implement has no rendered `release.yml`: it
/// is accepted only from a repository that renders its own publisher verbatim
/// as a `static-workflow` row, and is a configuration error anywhere else.
impl RepoGenerationConfig {
    fn validate_renovate(&self) -> Result<(), GeneratorError> {
        let renovate = &self.renovate;
        if renovate.enabled != Some(true) {
            return Ok(());
        }
        let reason = renovate.reason.as_deref().unwrap_or_default();
        if reason.is_empty() {
            return Err(GeneratorError::usage(
                "[renovate] enabled = true requires `reason` documenting why Renovate runs on trusted Velnor runners",
            ));
        }
        if !self
            .declare
            .iter()
            .any(|row| row.primitive() == crate::primitives::RENOVATE)
        {
            return Err(GeneratorError::usage(
                "[renovate] enabled = true requires `[[declare]] primitive = \"renovate\" file = \"renovate.yml\"`",
            ));
        }
        if renovate.validate == Some(true)
            && !self
                .declare
                .iter()
                .any(|row| row.primitive() == crate::primitives::RENOVATE_VALIDATE)
        {
            return Err(GeneratorError::usage(
                "[renovate] validate = true requires `[[declare]] primitive = \"renovate-validate\" file = \"renovate-validate.yml\"`",
            ));
        }
        let universe = self
            .workflow
            .providers
            .as_deref()
            .map(|providers| parse_provider_set(providers, "[workflow] providers"))
            .transpose()?
            .unwrap_or_else(|| crate::provider::ProviderId::ALL.into_iter().collect());
        if !universe.contains(&crate::provider::ProviderId::Velnor) {
            return Err(GeneratorError::usage(
                "[renovate] enabled = true requires the velnor provider in [workflow] providers; the Renovate writer runs on Velnor",
            ));
        }
        if !self.workflow.selectors.contains_key("velnor") {
            return Err(GeneratorError::usage(
                "[renovate] enabled = true requires [workflow.selectors.velnor] for the writer job",
            ));
        }
        if let Some(token) = renovate.token.as_deref() {
            validate_renovate_token_name(token)?;
        }
        if let Some(schedule) = renovate.schedule.as_deref() {
            validate_renovate_cron(schedule)?;
        }
        if let Some(config) = renovate.config.as_deref() {
            validate_renovate_config_path(config)?;
        }
        Ok(())
    }

    fn validate_release(&self) -> Result<(), GeneratorError> {
        let release = &self.release;
        if release.enabled != Some(true) {
            return Ok(());
        }
        let kind = release.kind.as_deref().unwrap_or_default();
        let complete = match kind {
            "crates" => !release.packages.is_empty(),
            "rust-binary" => {
                release
                    .package
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                    && release
                        .binary
                        .as_deref()
                        .is_some_and(|value| !value.is_empty())
                    && !release.targets.is_empty()
            }
            "pages" => release
                .artifact_path
                .as_deref()
                .is_some_and(|value| !value.is_empty()),
            "native" => {
                release
                    .package
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                    && release
                        .binary
                        .as_deref()
                        .is_some_and(|value| !value.is_empty())
                    && !release.targets.is_empty()
            }
            "homebrew" => {
                release
                    .package
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                    && release
                        .source_repository
                        .as_deref()
                        .is_some_and(|value| !value.is_empty())
            }
            "apt" => {
                release
                    .package
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                    && release
                        .consumer_repository
                        .as_deref()
                        .is_some_and(|value| !value.is_empty())
            }
            _ => false,
        };
        if complete {
            return Ok(());
        }
        let declared = self.declare.iter().any(|row| {
            row.primitive() == crate::primitives::STATIC_WORKFLOW
                && row.file.as_deref() == Some(crate::RELEASE_WORKFLOW)
        });
        if declared && !kind.is_empty() {
            return Ok(());
        }
        Err(GeneratorError::usage(format!(
            "[release] enabled repositories must declare `kind`, one of {}, each with the contract fields that publisher renders from; a `kind` the renderer does not implement requires a `static-workflow` row that renders `{}` verbatim",
            RELEASE_KINDS.join(", "),
            crate::RELEASE_WORKFLOW
        )))
    }
}

/// The unit kinds the generator implements, as the `[[unit]]` `kind` strings.
const UNIT_KIND_PREFIXES: &[&str] = &[
    "rust", "gradle", "node", "bun", "swift", "opentofu", "docker", "homebrew", "docs",
];

fn unit_kind_prefix(kind: &str) -> Option<&'static str> {
    UNIT_KIND_PREFIXES
        .iter()
        .copied()
        .find(|candidate| *candidate == kind)
}

fn validate_arg_value(key: &str, value: &toml::Value) -> Result<(), GeneratorError> {
    match value {
        toml::Value::String(_) | toml::Value::Integer(_) | toml::Value::Boolean(_) => Ok(()),
        toml::Value::Array(items) => {
            for item in items {
                validate_arg_value(key, item)?;
            }
            Ok(())
        }
        toml::Value::Table(table) => {
            for item in table.values() {
                validate_arg_value(key, item)?;
            }
            Ok(())
        }
        toml::Value::Float(_) => Err(GeneratorError::usage(format!(
            "[[declare]] args `{key}` contains a float; floats have no canonical form, use an integer or string"
        ))),
        toml::Value::Datetime(_) => Err(GeneratorError::usage(format!(
            "[[declare]] args `{key}` contains a TOML datetime; use a string"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

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
    fn must_fail<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}"),
            Err(error) => error,
        }
    }

    /// A scanned throwaway repository: the only way to obtain a real shape.
    fn scanned_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-config-{name}-{}",
            crate::unique_suffix()
        ));
        must(fs::create_dir_all(&root), "create config test repository");
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
            ),
            "write fixture manifest",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.91.1\"\n",
            ),
            "write fixture toolchain pin",
        );
        root
    }

    fn shape_for(root: &Path) -> crate::scan::RepositoryShape {
        let providers: crate::provider::ProviderSet =
            crate::provider::ProviderId::ALL.into_iter().collect();
        must(
            crate::scan::scan_shape(root, &providers, "main", &[]),
            "scan config test repository",
        )
    }

    fn config_for(text: &str) -> RepoGenerationConfig {
        must(
            toml::from_str::<RepoGenerationConfig>(text),
            "parse config under test",
        )
    }

    /// A test-owned `package-update.yml` body: the grant rules are validated
    /// against the surface the repository renders, never against a
    /// generator-side copy of a template.
    const PACKAGE_UPDATE_TEMPLATE: &str = concat!(
        "# Generated by velnor-workflow. Regenerate; do not hand-edit.\n",
        "name: Package update\n",
        "\n",
        "jobs:\n",
        "  example_owner:\n",
        "    runs-on: ubuntu-24.04\n",
        "    steps:\n",
        "      - run: echo update\n",
        "  other_owner:\n",
        "    runs-on: ubuntu-24.04\n",
        "    steps:\n",
        "      - run: echo update\n",
    );

    /// The owner blocks that body really declares, in template order.
    fn package_update_blocks() -> Vec<&'static str> {
        crate::estate::apt_package_update_owner_blocks(PACKAGE_UPDATE_TEMPLATE)
    }

    fn full_config(unit: &str) -> String {
        format!(
            "schema = 2\n\
             \n\
             [generator]\n\
             repository = \"example/fixture\"\n\
             \n\
             [workflow]\n\
             providers = [\"github-hosted\", \"velnor\"]\n\
             automatic_providers = [\"github-hosted\", \"velnor\"]\n\
             default_branch = \"trunk\"\n\
             \n\
             [workflow.selectors.github-hosted]\n\
             runs_on = [\"ubuntu-24.04\"]\n\
             \n\
             [workflow.selectors.velnor]\n\
             runs_on = [\"self-hosted\", \"example-runner-label\"]\n\
             \n\
             [scan]\n\
             exclude = [\"config/fleet/**\", \"docs/**\"]\n\
             \n\
             [policy]\n\
             dco_required = true\n\
             ci_required = true\n\
             action_pin_admission = \"reviewed-allowlist\"\n\
             actionlint_config_variables_null = true\n\
             \n\
             [[declare]]\n\
             primitive = \"rust-crate\"\n\
             units = [\"{unit}\"]\n\
             file = \"rust-crate.yml\"\n\
             [declare.args]\n\
             targets = [\"x86_64-unknown-linux-gnu\"]\n\
             channel = \"stable\"\n\
             nested = {{ keep = true, depth = 2 }}\n\
             \n\
             [[declare]]\n\
             primitive = \"docs-site\"\n\
             file = \"docs.yml\"\n"
        )
    }

    #[test]
    fn full_config_validates_against_a_scanned_shape() {
        let root = scanned_root("valid");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let unit = unit_ids.first().cloned().unwrap_or_default();
        let config = config_for(&full_config(&unit));
        must(
            config.validate(&unit_ids, &package_update_blocks(), &BTreeSet::new()),
            "validate full config",
        );
        assert_eq!(config.schema, Some(2));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn canonical_form_preserves_declarations_and_sorts_table_keys() {
        let config = config_for(&full_config("ignored"));
        let canonical = must(config.canonical_json(), "canonicalize full config");
        let repeated = must(config.canonical_json(), "canonicalize again");
        assert_eq!(canonical, repeated, "canonical form must be stable");
        // Both declarations survive, each carrying its own row fields.
        assert!(
            canonical.contains("\"primitive\":\"rust-crate\""),
            "{canonical}"
        );
        assert!(
            canonical.contains("\"primitive\":\"docs-site\""),
            "{canonical}"
        );
        assert!(canonical.contains("\"units\":[\""), "{canonical}");
        // Table keys are sorted, so `channel` precedes `nested` and `targets`.
        let args_position = canonical.find("\"args\":").unwrap_or_default();
        let channel = canonical.find("\"channel\"").unwrap_or_default();
        let targets = canonical.find("\"targets\"").unwrap_or_default();
        assert!(args_position < channel && channel < targets);
    }

    #[test]
    fn workflow_providers_accepts_strict_ids() {
        for providers in [
            "[\"github-hosted\"]",
            "[\"velnor\"]",
            "[\"github-hosted\", \"github-self-hosted\", \"velnor\"]",
        ] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = {providers}\n"
            ));
            must(
                config.validate(&[], &[], &BTreeSet::new()),
                "validate accepted provider universe",
            );
        }
        let config = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = [\"velnor\"]\n",
        );
        assert_eq!(config.providers(), Some(&["velnor".to_owned()][..]));
    }

    #[test]
    fn workflow_dispatch_and_automatic_provider_defaults_are_optional() {
        let config = config_for("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n");
        assert_eq!(config.automatic_providers(), None);
        assert_eq!(config.default_dispatch_providers(), None);
        let declared = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nautomatic_providers = [\"velnor\"]\ndefault_dispatch_providers = [\"github-hosted\"]\n",
        );
        assert_eq!(
            declared.automatic_providers(),
            Some(&["velnor".to_owned()][..])
        );
        assert_eq!(
            declared.default_dispatch_providers(),
            Some(&["github-hosted".to_owned()][..])
        );
        must(
            declared.validate(&[], &[], &BTreeSet::new()),
            "validate declared provider defaults",
        );
    }

    #[test]
    fn workflow_dispatch_providers_must_stay_inside_the_universe() {
        let config = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = [\"github-hosted\"]\ndefault_dispatch_providers = [\"velnor\"]\n",
        );
        let error = must_fail(
            config.validate(&[], &[], &BTreeSet::new()),
            "dispatch default outside the universe",
        );
        assert!(
            error.to_string().contains(
                "[workflow] default_dispatch_providers names provider `velnor` outside [workflow] providers"
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn workflow_providers_is_optional_without_changing_canonical_shape() {
        let config = config_for("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n");
        assert_eq!(config.providers(), None);
        let canonical = must(
            config.canonical_json(),
            "canonicalize default workflow config",
        );
        assert!(!canonical.contains("\"providers\""), "{canonical}");
    }

    #[test]
    fn workflow_providers_rejects_unknown_ids_and_repeats() {
        for providers in ["GitHub", "both", "hosted", "github"] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = [\"{providers}\"]\n"
            ));
            let error = must_fail(
                config.validate(&[], &[], &BTreeSet::new()),
                "unknown provider id must fail validation",
            );
            assert!(
                error.to_string().contains("has unknown provider"),
                "unexpected error for {providers}: {error}"
            );
        }
        let repeated = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = [\"velnor\", \"velnor\"]\n",
        );
        let error = must_fail(
            repeated.validate(&[], &[], &BTreeSet::new()),
            "repeated provider id must fail validation",
        );
        assert!(
            error.to_string().contains("must not repeat an id"),
            "unexpected error: {error}"
        );
        let empty = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = []\n",
        );
        let error = must_fail(
            empty.validate(&[], &[], &BTreeSet::new()),
            "empty provider universe must fail validation",
        );
        assert!(
            error
                .to_string()
                .contains("[workflow] providers must name at least one provider"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn workflow_automatic_providers_must_stay_inside_the_universe() {
        let subset = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = [\"github-hosted\", \"velnor\"]\nautomatic_providers = [\"github-hosted\"]\n",
        );
        assert_eq!(
            subset.automatic_providers(),
            Some(&["github-hosted".to_owned()][..])
        );
        must(
            subset.validate(&[], &[], &BTreeSet::new()),
            "an automatic subset of the universe is valid",
        );

        let error = must_fail(
            config_for(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = [\"github-hosted\"]\nautomatic_providers = [\"velnor\"]\n",
            )
            .validate(&[], &[], &BTreeSet::new()),
            "automatic outside the universe must be rejected",
        );
        assert!(
            error.to_string().contains(
                "[workflow] automatic_providers names provider `velnor` outside [workflow] providers"
            ),
            "{error}"
        );
    }

    #[test]
    fn workflow_selectors_route_each_provider_and_reject_empty_labels() {
        let config = config_for("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n");
        assert!(config.selectors().is_empty());
        let canonical = must(
            config.canonical_json(),
            "canonicalize default workflow config",
        );
        assert!(!canonical.contains("\"selectors\""), "{canonical}");
        let declared = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow.selectors.velnor]\nruns_on = [\"self-hosted\", \"example-runner\"]\n",
        );
        assert_eq!(
            declared
                .selectors()
                .get("velnor")
                .map(|selector| selector.runs_on.as_slice()),
            Some(&["self-hosted".to_owned(), "example-runner".to_owned()][..])
        );
        must(
            declared.validate(&[], &[], &BTreeSet::new()),
            "validate declared selectors",
        );
        let empty = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow.selectors.velnor]\nruns_on = []\n",
        );
        let error = must_fail(
            empty.validate(&[], &[], &BTreeSet::new()),
            "empty runs_on must fail validation",
        );
        assert!(
            error
                .to_string()
                .contains("[workflow.selectors.velnor] runs_on must name at least one label"),
            "{error}"
        );
        let unknown = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow.selectors.both]\nruns_on = [\"self-hosted\"]\n",
        );
        let error = must_fail(
            unknown.validate(&[], &[], &BTreeSet::new()),
            "unknown selector provider must fail validation",
        );
        assert!(
            error
                .to_string()
                .contains("[workflow.selectors] has unknown provider `both`"),
            "{error}"
        );
    }

    #[test]
    fn workflow_selectors_reject_shared_local_labels() {
        let shared = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow.selectors.github-self-hosted]\nruns_on = [\"shared-label\"]\n\n[workflow.selectors.velnor]\nruns_on = [\"shared-label\"]\n",
        );
        let error = must_fail(
            shared.validate(&[], &[], &BTreeSet::new()),
            "shared local labels must fail validation",
        );
        assert!(
            error
                .to_string()
                .contains("label `shared-label` is claimed by github-self-hosted and velnor"),
            "{error}"
        );
    }

    #[test]
    fn unit_trust_and_platform_are_typed() {
        let config = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[[units]]\nid = \"example\"\nkind = \"docs\"\ntrust = \"trusted-only\"\nplatform = \"macos-arm64\"\n",
        );
        assert_eq!(
            config.units().first().and_then(|unit| unit.trust()),
            Some("trusted-only")
        );
        assert_eq!(
            config.units().first().and_then(|unit| unit.platform()),
            Some("macos-arm64")
        );
        must(
            config.validate(&[], &[], &BTreeSet::new()),
            "typed trust and platform validate",
        );
        let bad_trust = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[[units]]\nid = \"example\"\nkind = \"docs\"\ntrust = \"trusted\"\n",
        );
        let error = must_fail(
            bad_trust.validate(&[], &[], &BTreeSet::new()),
            "unknown trust tier must fail validation",
        );
        assert!(
            error
                .to_string()
                .contains("declares trust `trusted`; expected one of: untrusted-ok, trusted-only"),
            "{error}"
        );
        let bad_platform = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[[units]]\nid = \"example\"\nkind = \"docs\"\nplatform = \"macos-26\"\n",
        );
        let error = must_fail(
            bad_platform.validate(&[], &[], &BTreeSet::new()),
            "label platform must fail validation",
        );
        assert!(
            error.to_string().contains(
                "declares platform `macos-26`; expected one of: linux-x64, linux-arm64, macos-arm64"
            ),
            "{error}"
        );
    }

    #[test]
    fn workflow_runners_keeps_unknown_fields_denied() {
        let error = must_fail(
            toml::from_str::<RepoGenerationConfig>(
                "schema = 2\n\n[workflow]\nrunner = \"velnor\"\n",
            ),
            "unknown workflow fields must be rejected",
        );
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn declaration_order_is_part_of_the_digest() {
        let leading = "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                       [[declare]]\nprimitive = \"a\"\nfile = \"a.yml\"\n\n\
                       [[declare]]\nprimitive = \"b\"\nfile = \"b.yml\"\n";
        let trailing = "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                        [[declare]]\nprimitive = \"b\"\nfile = \"b.yml\"\n\n\
                        [[declare]]\nprimitive = \"a\"\nfile = \"a.yml\"\n";
        let first = must(config_for(leading).canonical_json(), "canonicalize leading");
        let second = must(
            config_for(trailing).canonical_json(),
            "canonicalize trailing",
        );
        assert_ne!(
            first, second,
            "declaration order must change the digest input"
        );
    }

    #[test]
    fn schema_must_be_exactly_two() {
        let root = scanned_root("schema");
        let path = root.join(GENERATION_CONFIG_PATH);
        must(
            fs::create_dir_all(path.parent().unwrap_or(&root)),
            "create config dir",
        );
        must(
            fs::write(
                &path,
                "schema = 3\n\n[generator]\nrepository = \"example/fixture\"\n",
            ),
            "write future schema",
        );
        let error = must_some_error(load(&path).err(), "future schema must fail");
        assert!(
            error.contains("schema 3"),
            "error must name the schema: {error}"
        );
        must(
            fs::write(
                &path,
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n",
            ),
            "write previous schema",
        );
        let error = must_some_error(load(&path).err(), "previous schema must fail");
        assert!(
            error.contains("has schema 1; this generator reads schema 2 only"),
            "error must name the schema: {error}"
        );
        must(
            fs::write(&path, "[generator]\nrepository = \"example/fixture\"\n"),
            "write config without schema",
        );
        let missing = must_some_error(load(&path).err(), "missing schema must fail");
        assert!(
            missing.contains("missing `schema = 2`"),
            "error must name the missing schema: {missing}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_declared_unit_names_the_available_units() {
        let root = scanned_root("unknown-unit");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let available = shape.unit_ids().collect::<Vec<_>>().join(", ");
        let config = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[declare]]\nprimitive = \"rust-crate\"\nunits = [\"not-a-unit\"]\nfile = \"rust.yml\"\n",
        );
        let error = must_some_error(
            config
                .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new())
                .err(),
            "unknown unit must fail",
        );
        assert!(error.contains("not-a-unit"), "error names the row: {error}");
        assert!(
            error.contains(&format!("available units: {available}")),
            "error lists the scanned units: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    fn mise_tools_config(unit: &str, tools: &str) -> String {
        format!(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[[units]]\nid = \"{unit}\"\nmise_tools = [{tools}]\n"
        )
    }

    /// Lock keys mixing the bare and backend-qualified spellings real locks
    /// carry, parsed through the same parser the scan uses.
    fn mixed_lock_keys() -> BTreeSet<String> {
        must(
            parse_mise_lock_keys(
                "[[tools.cargo-binstall]]\nversion = \"1.0.0\"\n\n[[tools.\"aqua:nextest-rs/nextest/cargo-nextest\"]]\nversion = \"0.9.0\"\n\n[[tools.\"github:open-telemetry/weaver\"]]\nversion = \"0.24.2\"\n",
            ),
            "parse mixed lock fixture",
        )
    }

    #[test]
    fn declared_mise_tools_match_lock_keys_by_exact_spelling() {
        let root = scanned_root("mise-tools");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let unit = unit_ids.first().cloned().unwrap_or_default();
        let keys = mixed_lock_keys();
        // Bare and qualified ids validate when the lock pins that spelling.
        must(
            config_for(&mise_tools_config(
                &unit,
                "\"cargo-binstall\", \"github:open-telemetry/weaver\", \"aqua:nextest-rs/nextest/cargo-nextest\"",
            ))
            .validate(&unit_ids, &package_update_blocks(), &keys),
            "lock-pinned bare and qualified tool ids validate",
        );
        // A qualified id the lock pins only bare is the incident this check
        // exists for: the generator used to mandate the qualified spelling the
        // runner's own lock check then rejected.
        for rejected in [
            "aqua:cargo-bins/cargo-binstall",
            "cargo:open-telemetry/weaver",
            "cargo:nonexistent-tool",
        ] {
            let error = must_some_error(
                config_for(&mise_tools_config(&unit, &format!("\"{rejected}\"")))
                    .validate(&unit_ids, &package_update_blocks(), &keys)
                    .err(),
                "unpinned mise tool must fail",
            );
            assert!(
                error.contains("mise.lock does not pin"),
                "error names the lock mismatch: {error}"
            );
            assert!(
                error.contains("known keys: aqua:nextest-rs/nextest/cargo-nextest, cargo-binstall, github:open-telemetry/weaver"),
                "error lists the known keys: {error}"
            );
        }
        // Without a lock there is no identity to check, so shape alone rules
        // and either spelling validates.
        must(
            config_for(&mise_tools_config(
                &unit,
                "\"cargo-binstall\", \"aqua:cargo-bins/cargo-binstall\"",
            ))
            .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new()),
            "any well-shaped tool ids validate without a lock",
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn misshapen_mise_tools_fail_regardless_of_lock() {
        let root = scanned_root("mise-tools-shape");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let unit = unit_ids.first().cloned().unwrap_or_default();
        for rejected in [
            "",
            "github:open-telemetry/weaver@0.24.2",
            "github:../weaver",
            "..",
            "github:open-telemetry/weaver;touch",
            "github:open-telemetry/weaver touch",
            "-install",
            "https://example.com/tool",
            "github:open-telemetry\\\\weaver",
            "~/weaver",
            "/usr/bin/weaver",
            ".weaver",
            ":weaver",
            "weaver$(touch)",
        ] {
            // Shape is checked before membership, so a lock can never launder
            // shell syntax into `install_args`: the id fails with or without
            // keys.
            for keys in [mixed_lock_keys(), BTreeSet::new()] {
                let error = must_some_error(
                    config_for(&mise_tools_config(&unit, &format!("\"{rejected}\"")))
                        .validate(&unit_ids, &package_update_blocks(), &keys)
                        .err(),
                    "misshapen mise tool must fail",
                );
                assert!(
                    error.contains("not a plain tool id"),
                    "error names the mise tool shape contract: {error}"
                );
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_or_duplicate_mise_tools_fail() {
        let root = scanned_root("mise-tools-empty-dup");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let unit = unit_ids.first().cloned().unwrap_or_default();
        let error = must_some_error(
            config_for(&mise_tools_config(&unit, ""))
                .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new())
                .err(),
            "empty mise_tools must fail",
        );
        assert!(error.contains("empty mise_tools"), "{error}");
        let error = must_some_error(
            config_for(&mise_tools_config(
                &unit,
                "\"github:open-telemetry/weaver\", \"github:open-telemetry/weaver\"",
            ))
            .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new())
            .err(),
            "duplicate mise_tools must fail",
        );
        assert!(error.contains("more than once"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lock_key_parser_collects_bare_and_quoted_qualified_keys() {
        let keys = must(
            parse_mise_lock_keys(
                "# @generated - this file is auto-generated by `mise lock`\n\n[[tools.cargo-binstall]]\nversion = \"1.0.0\"\n\n[tools.cargo-binstall.\"platforms.linux-x64\"]\nchecksum = \"sha256:abc\"\n\n[[tools.\"cargo:sccache\"]]\nversion = \"0.9.0\"\n\n[[tools.\"aqua:nextest-rs/nextest/cargo-nextest\"]]\nversion = \"0.9.0\"\n",
            ),
            "parse mixed lock",
        );
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec![
                "aqua:nextest-rs/nextest/cargo-nextest".to_owned(),
                "cargo-binstall".to_owned(),
                "cargo:sccache".to_owned(),
            ],
            "quoted qualified keys resolve to their literal spelling"
        );
        let empty = must(parse_mise_lock_keys(""), "parse empty lock");
        assert!(empty.is_empty(), "a lock without tools pins nothing");
        let missing = must(
            parse_mise_lock_keys("[settings]\nlockfile = true\n"),
            "parse lock without tools table",
        );
        assert!(missing.is_empty(), "a lock without tools pins nothing");
        let error = must_fail(
            parse_mise_lock_keys("[[tools.unclosed\n"),
            "invalid lock TOML must fail",
        );
        assert!(
            error.to_string().contains("committed tool keys"),
            "error names the lock contract: {error}"
        );
    }

    #[test]
    fn lock_key_reader_treats_a_missing_lock_as_unpinned() {
        let root = scanned_root("mise-lock-missing");
        let keys = must(mise_lock_keys_for_root(&root), "read missing lock");
        assert!(keys.is_empty(), "a missing lock pins no keys");
        must(
            fs::write(root.join("mise.lock"), "[[tools.cargo-binstall]]\n"),
            "write lock",
        );
        let keys = must(mise_lock_keys_for_root(&root), "read present lock");
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec!["cargo-binstall".to_owned()]
        );
        must(
            fs::write(root.join("mise.lock"), "[[tools.unclosed\n"),
            "write broken lock",
        );
        let error = must_fail(mise_lock_keys_for_root(&root), "broken lock must fail");
        assert!(
            error.to_string().contains("mise.lock"),
            "error names the lock file: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn declared_files_stay_bare_yml_names() {
        let root = scanned_root("declare-file");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        for file in ["../escape.yml", "nested/deep.yml", "workflow.yaml", ".yml"] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [[declare]]\nprimitive = \"rust-crate\"\nfile = \"{file}\"\n"
            ));
            let error = must_some_error(
                config
                    .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new())
                    .err(),
                "declared file must fail validation",
            );
            assert!(
                error.contains("bare `.yml` workflow file name"),
                "{file} must be rejected: {error}"
            );
        }
        let separator = config_for(concat!(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n",
            "[[declare]]\nprimitive = \"rust-crate\"\nfile = 'back\\slash.yml'\n",
        ));
        let error = must_some_error(
            separator
                .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new())
                .err(),
            "backslash file name must fail validation",
        );
        assert!(error.contains("bare `.yml` workflow file name"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn repository_must_be_an_owner_and_a_name() {
        let root = scanned_root("repository-slug");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        for repository in [
            "example",
            "example/",
            "/fixture",
            "example/.git",
            "example/fixture/extra",
        ] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"{repository}\"\n"
            ));
            let error = must_some_error(
                config
                    .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new())
                    .err(),
                "repository slug must fail validation",
            );
            assert!(
                error.contains("`[generator] repository`"),
                "{repository} must be rejected: {error}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_generator_section_is_a_config_error() {
        let root = scanned_root("missing-generator");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let config = config_for("schema = 2\n");
        let error = must_some_error(
            config
                .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new())
                .err(),
            "missing generator must fail",
        );
        assert!(error.contains("[generator] repository"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn args_stay_opaque_but_reject_unstable_values() {
        let root = scanned_root("args");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let opaque = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[declare]]\nprimitive = \"rust-crate\"\nfile = \"rust.yml\"\n\
             [declare.args]\nanything = { goes = [\"here\", 3, true] }\n",
        );
        must(
            opaque.validate(&unit_ids, &package_update_blocks(), &BTreeSet::new()),
            "opaque args validate",
        );
        let float = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[declare]]\nprimitive = \"rust-crate\"\nfile = \"rust.yml\"\n\
             [declare.args]\nratio = 1.5\n",
        );
        let error = must_some_error(
            float
                .validate(&unit_ids, &package_update_blocks(), &BTreeSet::new())
                .err(),
            "float args must fail",
        );
        assert!(error.contains("float"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cache_sections_parse_and_stay_generator_only() {
        let config = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [cache.github]\nbudget_bytes = 8589934592\nproducer_window_seconds = 7200\n\
             mbx_generation_bound = 2\n\n[cache.velnor]\nbudget_bytes = 53687091200\n\
             mbx_generation_bound = 6\n",
        );
        assert_eq!(config.cache_github().budget_bytes, Some(8_589_934_592));
        assert_eq!(config.cache_velnor().budget_bytes, Some(53_687_091_200));
        let env = super::render_velnor_host_env(config.cache_velnor());
        assert!(env.contains("VELNOR_STORAGE_ROOT=/var"));
        assert!(env.contains("VELNOR_BUDGET_CACHES_BYTES=53687091200"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error = match toml::from_str::<RepoGenerationConfig>(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [policy]\ndco_required_is_spelled_like_this = true\n",
        ) {
            Ok(_) => String::from("accepted"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("unknown field"),
            "typo'd fields must fail closed: {error}"
        );
    }

    /// A declared channel-grant table has to stand on its own: empty grants,
    /// channels the rendered updater never implements, unknown blocks, and
    /// uncovered owner blocks are all usage errors, never a silent default.
    #[test]
    fn package_update_channel_grants_fail_closed() {
        let blocks = package_update_blocks();
        assert!(blocks.len() > 1, "the rendered surface declares {blocks:?}");
        // The owner blocks are read off the render surface, so the test never
        // spells one: the names belong to the estate, not to a generic module.
        let block = blocks[0];
        let other = blocks[1];
        for (grants, expected) in [
            (format!("{block} = []"), "empty channel list"),
            (
                format!("{block} = [\"stable\", \"beta\"]"),
                "does not implement",
            ),
            (
                format!("default = [\"stable\"], {block}_typo = [\"stable\"]"),
                "does not declare",
            ),
            (format!("{other} = [\"stable\"]"), "declares no `default`"),
        ] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [workflow]\npackage_update_channels = {{{grants}}}\n"
            ));
            let error = must_some_error(
                config.validate(&[], &blocks, &BTreeSet::new()).err(),
                "channel grants must fail validation",
            );
            assert!(
                error.contains(expected),
                "`{grants}` must be rejected for `{expected}`: {error}"
            );
        }
        // The two shapes that are legal: every block named, or `default` alone.
        let every_block = blocks
            .iter()
            .map(|block| format!("{block} = [\"stable\"]"))
            .collect::<Vec<_>>()
            .join(", ");
        for grants in [every_block, String::from("default = [\"stable\"]")] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [workflow]\npackage_update_channels = {{{grants}}}\n"
            ));
            must(
                config.validate(&[], &blocks, &BTreeSet::new()),
                "channel grants validate",
            );
        }
    }

    #[test]
    fn absent_config_digests_the_empty_canonical_form() {
        let empty = must(
            config_for("schema = 2\n").canonical_json(),
            "canonicalize minimal",
        );
        assert_ne!(empty, canonical::EMPTY_CANONICAL_FORM);
        assert_ne!(
            must(RepoGenerationConfig::digest(None), "digest absent config"),
            must(
                RepoGenerationConfig::digest(Some(&config_for("schema = 2\n"))),
                "digest minimal config"
            ),
            "introducing a config must change the recorded input"
        );
    }

    #[test]
    fn discovery_reads_the_repository_root_and_ignores_absence() {
        let root = scanned_root("discovery");
        assert!(must(discover(&root), "discover absent config").is_none());
        let path = root.join(GENERATION_CONFIG_PATH);
        must(
            fs::create_dir_all(path.parent().unwrap_or(&root)),
            "create config directory",
        );
        must(
            fs::write(
                &path,
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n",
            ),
            "write discovered config",
        );
        let discovered = must(discover(&root), "discover present config");
        assert!(discovered.is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_some_error<E: std::fmt::Display>(value: Option<E>, context: &str) -> String {
        match value {
            Some(value) => value.to_string(),
            None => panic!("{context}"),
        }
    }

    #[test]
    fn renovate_enabled_requires_declare_row_and_trusted_runners() {
        let root = scanned_root("renovate-validate-config");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let error = must_fail(
            config_for(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [renovate]\nenabled = true\nreason = \"test\"\n",
            )
            .validate(&unit_ids, &[], &BTreeSet::new()),
            "enabled renovate without declare must fail",
        );
        assert!(error
            .to_string()
            .contains("[[declare]] primitive = \"renovate\""));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn renovate_unknown_field_fails_closed() {
        let error = must_some_error(
            toml::from_str::<RepoGenerationConfig>(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [renovate]\nenabled = true\nreason = \"test\"\nunknown = true\n",
            )
            .err(),
            "unknown renovate field must fail",
        );
        assert!(error.contains("unknown field"), "{error}");
    }
}

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
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::s2::provider::{parse_provider_set, parse_selectors, ProviderId, ProviderSelector};
use crate::s2::{content_digest_bytes, GeneratorError};

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
    #[serde(default, skip_serializing_if = "MaintenanceSection::is_empty")]
    maintenance: MaintenanceSection,
    #[serde(default)]
    docs: DocsSection,
    /// Composable scheduled-check profiles. Skipped while empty so the
    /// canonical form of a repository without profiles is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    check_profile: Vec<CheckProfileSection>,
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
    /// Additional cron schedules beside `schedule`, each a 5-field cron.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    schedules: Vec<String>,
    token: Option<String>,
    config: Option<String>,
    validate: Option<bool>,
    cache: Option<bool>,
    /// Repository targets (`owner/repository`) the writer renovates instead
    /// of autodiscovering. Empty keeps autodiscovery.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repositories: Vec<String>,
    /// Secret holding the JSON `hostRules` array Renovate authenticates
    /// private registries with. A name, never the credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host_rules_secret: Option<String>,
    /// The git author Renovate commits as (`Name <email>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    /// Append a `Signed-off-by` trailer for `author` to Renovate commits.
    /// Requires `author`; the DCO check the repository gates on stays the
    /// enforcement, this only makes the writer produce signed-off commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signoff: Option<bool>,
    /// Regex allowlist for Renovate post-upgrade commands
    /// (`allowedCommands`): execution allowances for self-hosted runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allowed_commands: Vec<String>,
}

/// Cache-hygiene maintenance overrides (`[maintenance]`). Generator-only:
/// the rendered `maintenance.yml` carries the schedule, the producer gate,
/// and the per-run delete bound, so the workflow needs no inputs of its own.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MaintenanceSection {
    /// Cron schedule of the retention sweep. Absent keeps `31 3 * * *`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schedule: Option<String>,
    /// Producer workflows whose in-progress runs defer retention. Absent
    /// keeps `ci-main.yml` and `nightly.yml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    producers: Option<Vec<String>>,
    /// Per-run bound on cache deletes in any one maintenance job. A run that
    /// reaches it fails with "rerun maintenance" instead of an unbounded
    /// sweep. Absent keeps 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_deletes: Option<u32>,
}

impl MaintenanceSection {
    /// Whether the repository declared any maintenance override. The whole
    /// table is skipped in the canonical form when empty, so repositories
    /// that do not use it keep their recorded config digest.
    fn is_empty(&self) -> bool {
        self.schedule.is_none() && self.producers.is_none() && self.max_deletes.is_none()
    }
}

/// The release contract a repository declares for itself. `kind` names the
/// publisher the renderer implements; every other field is the contract that
/// publisher renders from, so an incomplete contract is a configuration error
/// instead of a partially rendered workflow.
/// One composable scheduled-check profile the repository declares for itself.
///
/// A profile is a named scheduled job: its cadence, its platform, the named
/// tasks it runs, and the status it reports. The generator renders the
/// schedule, the lane, and the job shell; the named tasks own every product
/// assertion and threshold, so generic code never interprets a check result.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckProfileSection {
    id: Option<String>,
    name: Option<String>,
    schedule: Option<String>,
    runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tasks: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    needs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifacts: Option<Vec<String>>,
    status: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseJobSection {
    id: Option<String>,
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tasks: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    needs: Option<Vec<String>>,
    runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attest_subjects: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    permissions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
}

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
    /// The workspace package the image lane compiles into the container
    /// (`release-binaries/<arch>/<package>`, which the Dockerfile copies).
    /// Empty selects `package`: repositories whose image embeds a different
    /// binary than the shipped product declare it here.
    image_package: Option<String>,
    source_repository: Option<String>,
    consumer_repository: Option<String>,
    artifact_path: Option<String>,
    description: Option<String>,
    manifest_schema: Option<String>,
    dockerfile: Option<String>,
    context: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    platforms: Vec<String>,
    /// The trusted producer workflow a `workflow_run` event must come from.
    producer_workflow: Option<String>,
    /// The producer conclusion the publish gate requires (`success`).
    producer_conclusion: Option<String>,
    /// The dispatch modes the workflows offer. `publish` is never a dispatch
    /// option: publication stays tag-triggered (or admitted-producer).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    modes: Vec<String>,
    /// Extra members packaged into each release archive next to the binary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    archive_members: Vec<String>,
    /// The archive checksum sidecar algorithm (`sha256`).
    archive_checksum: Option<String>,
    /// The `retention-days` release artifacts upload with (1-90).
    archive_retention_days: Option<i64>,
    /// Credential setup/teardown pairs, one `[[release.credential]]` row per
    /// credential the lane mounts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    credential: Vec<CredentialSection>,
    /// The tag filter the release triggers match on. Empty selects `v*`:
    /// repositories whose release tags share a narrower shape (a prefix, a
    /// path) declare it here instead of triggering on every `v` tag.
    tag_pattern: Option<String>,
    /// The OCI registry host the docker publisher logs in to. Absent keeps
    /// the GHCR automatic-token login; a declared host requires both
    /// credential secret names, and the triple renders only for `kind =
    /// "docker"`.
    registry: Option<String>,
    /// The secret holding the registry username. A name, never the value.
    registry_username_secret: Option<String>,
    /// The secret holding the registry password. A name, never the value.
    registry_password_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    job: Vec<ReleaseJobSection>,
}

/// One credential the release lane mounts: the setup command that
/// materializes it and the teardown command that restores the host. Both
/// are required; a setup without a teardown is a configuration error.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialSection {
    name: Option<String>,
    setup: Option<String>,
    teardown: Option<String>,
}

/// The documentation-site contract a repository declares for itself. Every
/// value is consumer-owned: the site address, the built output directory, the
/// path filters that feed the reuse digest, and the commands that build and
/// check the site. The generator renders only the pipeline structure — the
/// event-split gates, the artifact-reuse lookups, the bounded deploy retry,
/// and the failure reporting — never an address, mapping, or content rule.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocsSection {
    enabled: Option<bool>,
    reason: Option<String>,
    site_url: Option<String>,
    site_dir: Option<String>,
    sitemap_path: Option<String>,
    schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    build_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_link_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    site_link_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spell_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verify_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    external_link_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    docs_paths: Option<Vec<String>>,
}

impl DocsSection {
    pub(crate) fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub(crate) fn site_url(&self) -> Option<&str> {
        self.site_url.as_deref()
    }

    pub(crate) fn site_dir(&self) -> Option<&str> {
        self.site_dir.as_deref()
    }

    pub(crate) fn sitemap_path(&self) -> Option<&str> {
        self.sitemap_path.as_deref()
    }

    pub(crate) fn schedule(&self) -> Option<&str> {
        self.schedule.as_deref()
    }

    pub(crate) fn build_commands(&self) -> &[String] {
        self.build_commands.as_deref().unwrap_or(&[])
    }

    pub(crate) fn source_link_commands(&self) -> &[String] {
        self.source_link_commands.as_deref().unwrap_or(&[])
    }

    pub(crate) fn site_link_commands(&self) -> &[String] {
        self.site_link_commands.as_deref().unwrap_or(&[])
    }

    pub(crate) fn spell_commands(&self) -> &[String] {
        self.spell_commands.as_deref().unwrap_or(&[])
    }

    pub(crate) fn verify_commands(&self) -> &[String] {
        self.verify_commands.as_deref().unwrap_or(&[])
    }

    pub(crate) fn external_link_commands(&self) -> &[String] {
        self.external_link_commands.as_deref().unwrap_or(&[])
    }

    pub(crate) fn docs_paths(&self) -> Option<&[String]> {
        self.docs_paths.as_deref()
    }
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
    /// Extra environment the unit's jobs export: build flags and product
    /// outputs. Declared as `[units.env]`; replaces nothing, the scan
    /// derives no env of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
    /// Whether this Rust unit verifies under the object transport. Absent
    /// keeps the default (on for Rust); `false` opts out. Meaningless — and
    /// refused as `true` — on any other kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mbx: Option<bool>,
    /// Named build products this unit produces for consumers, as
    /// `[[units.products]]` rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    products: Vec<ProductSection>,
    /// Prerequisite edges this consumer declares, as
    /// `[[units.prerequisites]]` rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    prerequisites: Vec<PrerequisiteSection>,
    /// Named local Docker build contexts rendered as `--build-context name=path`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    docker_contexts: Vec<DockerContextSection>,
}

/// One named build product a `[[units]]` row declares: the product's name,
/// the repository task that rebuilds it, the task outputs consumers
/// receive as environment, the repo-relative artifact paths the
/// rebuild materializes, and the repo-relative input paths and globs
/// whose bytes feed the rebuild.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProductSection {
    name: Option<String>,
    task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outputs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inputs: Option<Vec<String>>,
}

/// One prerequisite edge a `[[units]]` row declares: the producer unit, the
/// product it builds, an optional task override, and the task inputs the
/// consumer's prepare step exports.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrerequisiteSection {
    producer: Option<String>,
    product: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
}

/// One repository-local named Docker build context.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DockerContextSection {
    name: Option<String>,
    path: Option<String>,
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

    pub(crate) fn schedules(&self) -> &[String] {
        &self.schedules
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

    pub(crate) fn repositories(&self) -> &[String] {
        &self.repositories
    }

    pub(crate) fn host_rules_secret(&self) -> Option<&str> {
        self.host_rules_secret.as_deref()
    }

    pub(crate) fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }

    pub(crate) fn signoff(&self) -> Option<bool> {
        self.signoff
    }

    pub(crate) fn allowed_commands(&self) -> &[String] {
        &self.allowed_commands
    }
}

impl MaintenanceSection {
    pub(crate) fn schedule(&self) -> Option<&str> {
        self.schedule.as_deref()
    }

    pub(crate) fn producers(&self) -> Option<&[String]> {
        self.producers.as_deref()
    }

    pub(crate) fn max_deletes(&self) -> Option<u32> {
        self.max_deletes
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

    pub(crate) fn image_package(&self) -> Option<&str> {
        self.image_package.as_deref()
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

    pub(crate) fn dockerfile(&self) -> Option<&str> {
        self.dockerfile.as_deref()
    }

    pub(crate) fn context(&self) -> Option<&str> {
        self.context.as_deref()
    }

    pub(crate) fn platforms(&self) -> &[String] {
        &self.platforms
    }

    pub(crate) fn producer_workflow(&self) -> Option<&str> {
        self.producer_workflow.as_deref()
    }

    pub(crate) fn producer_conclusion(&self) -> Option<&str> {
        self.producer_conclusion.as_deref()
    }

    pub(crate) fn modes(&self) -> &[String] {
        &self.modes
    }

    pub(crate) fn archive_members(&self) -> &[String] {
        &self.archive_members
    }

    pub(crate) fn archive_checksum(&self) -> Option<&str> {
        self.archive_checksum.as_deref()
    }

    pub(crate) fn archive_retention_days(&self) -> Option<i64> {
        self.archive_retention_days
    }

    pub(crate) fn credentials(&self) -> &[CredentialSection] {
        &self.credential
    }

    pub(crate) fn tag_pattern(&self) -> Option<&str> {
        self.tag_pattern.as_deref()
    }

    pub(crate) fn registry(&self) -> Option<&str> {
        self.registry.as_deref()
    }

    pub(crate) fn registry_username_secret(&self) -> Option<&str> {
        self.registry_username_secret.as_deref()
    }

    pub(crate) fn registry_password_secret(&self) -> Option<&str> {
        self.registry_password_secret.as_deref()
    }

    pub(crate) fn jobs(&self) -> &[ReleaseJobSection] {
        &self.job
    }
}

impl ReleaseJobSection {
    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn tasks(&self) -> Option<&[String]> {
        self.tasks.as_deref()
    }

    pub(crate) fn needs(&self) -> Option<&[String]> {
        self.needs.as_deref()
    }

    pub(crate) fn runner(&self) -> Option<&str> {
        self.runner.as_deref()
    }

    pub(crate) fn modes(&self) -> Option<&[String]> {
        self.modes.as_deref()
    }

    pub(crate) fn timeout_minutes(&self) -> Option<i64> {
        self.timeout_minutes
    }

    pub(crate) fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }

    pub(crate) fn attest_subjects(&self) -> Option<&[String]> {
        self.attest_subjects.as_deref()
    }

    pub(crate) fn permissions(&self) -> &BTreeMap<String, String> {
        &self.permissions
    }

    pub(crate) fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }
}

impl CredentialSection {
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn setup(&self) -> Option<&str> {
        self.setup.as_deref()
    }

    pub(crate) fn teardown(&self) -> Option<&str> {
        self.teardown.as_deref()
    }
}

impl CheckProfileSection {
    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn schedule(&self) -> Option<&str> {
        self.schedule.as_deref()
    }

    pub(crate) fn runner(&self) -> Option<&str> {
        self.runner.as_deref()
    }

    pub(crate) fn tools(&self) -> Option<&[String]> {
        self.tools.as_deref()
    }

    pub(crate) fn tasks(&self) -> Option<&[String]> {
        self.tasks.as_deref()
    }

    pub(crate) fn needs(&self) -> Option<&[String]> {
        self.needs.as_deref()
    }

    pub(crate) fn timeout_minutes(&self) -> Option<i64> {
        self.timeout_minutes
    }

    pub(crate) fn artifacts(&self) -> Option<&[String]> {
        self.artifacts.as_deref()
    }

    pub(crate) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub(crate) fn env(&self) -> &BTreeMap<String, String> {
        &self.env
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

    pub(crate) fn mbx(&self) -> Option<bool> {
        self.mbx
    }

    pub(crate) fn products(&self) -> &[ProductSection] {
        &self.products
    }

    pub(crate) fn prerequisites(&self) -> &[PrerequisiteSection] {
        &self.prerequisites
    }

    pub(crate) fn docker_contexts(&self) -> &[DockerContextSection] {
        &self.docker_contexts
    }

    pub(crate) fn named_docker_contexts(
        &self,
        id: &str,
        root: &Path,
    ) -> Result<Vec<crate::s2::DockerContext>, GeneratorError> {
        let canonical_root = fs::canonicalize(root).map_err(|error| {
            GeneratorError::io("resolve repository root for Docker context", root, &error)
        })?;
        let mut contexts = Vec::with_capacity(self.docker_contexts.len());
        let mut names = BTreeSet::new();
        for context in &self.docker_contexts {
            let name = context.name.as_deref().unwrap_or_default();
            let path = context.path.as_deref().unwrap_or_default();
            validate_docker_context_name(id, name, &mut names)?;
            validate_docker_context_path(id, name, path, &canonical_root)?;
            contexts.push(crate::s2::DockerContext {
                name: name.to_owned(),
                path: path.to_owned(),
            });
        }
        Ok(contexts)
    }

    /// The validated env this row declares, when it declares any.
    ///
    /// # Errors
    /// Returns a usage error for an env name or value that cannot render.
    pub(crate) fn validated_env(
        &self,
        id: &str,
    ) -> Result<Option<BTreeMap<String, String>>, GeneratorError> {
        if let Some(env) = self.env.as_ref() {
            crate::s2::platform::validate_env(env, &format!("[[units]] {id}"))?;
            return Ok(Some(env.clone()));
        }
        Ok(None)
    }

    /// The named products this row declares.
    ///
    /// # Errors
    /// Returns a usage error for a product without a name, an invalid task,
    /// env, output, or input, or a name the row declares twice.
    pub(crate) fn named_products(
        &self,
        id: &str,
    ) -> Result<Vec<crate::s2::platform::NamedProduct>, GeneratorError> {
        let mut products = Vec::new();
        for product in &self.products {
            let name = product.name.as_deref().unwrap_or_default();
            if !crate::s2::platform::valid_product_name(name) {
                return Err(GeneratorError::usage(format!(
                    "[[units]] {id} declares a product without a valid name; name it with lowercase letters, digits, dots, underscores, and dashes"
                )));
            }
            if products
                .iter()
                .any(|declared: &crate::s2::platform::NamedProduct| declared.name == name)
            {
                return Err(GeneratorError::usage(format!(
                    "[[units]] {id} declares product `{name}` twice; one row per product"
                )));
            }
            if let Some(task) = product.task.as_deref()
                && !crate::s2::platform::valid_task_name(task)
            {
                return Err(GeneratorError::usage(format!(
                    "[[units]] {id} declares product `{name}` with task `{task}`, which is not a repository task name; use letters, digits, and `_.:/-` without whitespace"
                )));
            }
            if let Some(env) = product.env.as_ref() {
                crate::s2::platform::validate_env(
                    env,
                    &format!("[[units]] {id} product `{name}`"),
                )?;
            }
            if let Some(outputs) = product.outputs.as_ref() {
                for output in outputs {
                    if !crate::s2::platform::valid_product_output(output) {
                        return Err(GeneratorError::usage(format!(
                            "[[units]] {id} declares product `{name}` with output `{output}`, which is not a repo-relative path in normal form; use forward slashes without leading `/`, `.`, `..`, or empty segments"
                        )));
                    }
                }
            }
            if let Some(inputs) = product.inputs.as_ref() {
                for input in inputs {
                    if !crate::s2::platform::valid_product_input(input) {
                        return Err(GeneratorError::usage(format!(
                            "[[units]] {id} declares product `{name}` with input `{input}`, which is not a repo-relative path or glob in normal form; use forward slashes without leading `/`, `.`, `..`, or empty segments"
                        )));
                    }
                }
            }
            products.push(crate::s2::platform::NamedProduct {
                name: name.to_owned(),
                task: product.task.clone(),
                env: product.env.clone().unwrap_or_default(),
                outputs: product.outputs.clone().unwrap_or_default(),
                // Declared rows cannot list expected files: only the
                // scanner derives them, from the adapter's layout facts.
                output_files: Vec::new(),
                // Binding facts likewise arrive from the scanner; a
                // declared row carries no binding contract.
                bindings_dir: String::new(),
                bindings_file: String::new(),
                deployment_target: String::new(),
                inputs: product.inputs.clone().unwrap_or_default(),
                inputs_unknown: Vec::new(),
                // Declared rows cannot claim a digest: only the scanner
                // computes one, over bytes it actually read.
                inputs_digest: None,
            });
        }
        Ok(products)
    }

    /// The prerequisite edges this row declares.
    ///
    /// # Errors
    /// Returns a usage error for an edge without a producer or product, an
    /// invalid task or env, or an edge the row declares twice.
    pub(crate) fn declared_prerequisites(
        &self,
        id: &str,
    ) -> Result<Vec<crate::s2::platform::Prerequisite>, GeneratorError> {
        let mut prerequisites = Vec::new();
        for prerequisite in &self.prerequisites {
            let producer = prerequisite.producer.as_deref().unwrap_or_default();
            let product = prerequisite.product.as_deref().unwrap_or_default();
            if producer.is_empty() || product.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "[[units]] {id} declares a prerequisite without both `producer` and `product`; name the unit that builds it and the product it builds"
                )));
            }
            if !crate::s2::platform::valid_product_name(product) {
                return Err(GeneratorError::usage(format!(
                    "[[units]] {id} declares a prerequisite on product `{product}`, which is not a product name"
                )));
            }
            if prerequisites
                .iter()
                .any(|declared: &crate::s2::platform::Prerequisite| {
                    declared.producer == producer && declared.product == product
                })
            {
                return Err(GeneratorError::usage(format!(
                    "[[units]] {id} declares the prerequisite `{producer}:{product}` twice; one row per edge"
                )));
            }
            if let Some(task) = prerequisite.task.as_deref()
                && !crate::s2::platform::valid_task_name(task)
            {
                return Err(GeneratorError::usage(format!(
                    "[[units]] {id} declares prerequisite `{producer}:{product}` with task `{task}`, which is not a repository task name"
                )));
            }
            if let Some(env) = prerequisite.env.as_ref() {
                crate::s2::platform::validate_env(
                    env,
                    &format!("[[units]] {id} prerequisite `{producer}:{product}`"),
                )?;
            }
            prerequisites.push(crate::s2::platform::Prerequisite {
                producer: producer.to_owned(),
                product: product.to_owned(),
                task: prerequisite.task.clone(),
                env: prerequisite.env.clone().unwrap_or_default(),
            });
        }
        Ok(prerequisites)
    }
}

const RESERVED_DOCKER_CONTEXT_NAMES: &[&str] =
    &["default", "dockerfile", "scratch", "velnor-cache-seed"];

fn validate_docker_context_name(
    id: &str,
    name: &str,
    names: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        });
    if !valid {
        return Err(GeneratorError::usage(format!(
            "[[units.docker_contexts]] {id} declares invalid Docker context name `{name}`"
        )));
    }
    if RESERVED_DOCKER_CONTEXT_NAMES.contains(&name) {
        return Err(GeneratorError::usage(format!(
            "[[units.docker_contexts]] {id} declares reserved Docker context name `{name}`"
        )));
    }
    if !names.insert(name.to_owned()) {
        return Err(GeneratorError::usage(format!(
            "[[units.docker_contexts]] {id} declares Docker context `{name}` more than once"
        )));
    }
    Ok(())
}

fn validate_docker_context_path(
    id: &str,
    name: &str,
    path: &str,
    canonical_root: &Path,
) -> Result<(), GeneratorError> {
    let relative = Path::new(path);
    let lexical_safe = !path.is_empty()
        && !path.bytes().any(|byte| byte == 0 || byte == b'\\')
        && relative.is_relative()
        && relative.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if !lexical_safe {
        return Err(GeneratorError::usage(format!(
            "[[units.docker_contexts]] {id} context `{name}` path `{path}` must be repository-relative without traversal"
        )));
    }
    let candidate = canonical_root.join(relative);
    let canonical_candidate = fs::canonicalize(&candidate).map_err(|error| {
        GeneratorError::usage(format!(
            "[[units.docker_contexts]] {id} context `{name}` path `{path}` does not exist: {error}"
        ))
    })?;
    if !canonical_candidate.starts_with(canonical_root) {
        return Err(GeneratorError::usage(format!(
            "[[units.docker_contexts]] {id} context `{name}` path `{path}` escapes the repository"
        )));
    }
    if !canonical_candidate.is_dir() {
        return Err(GeneratorError::usage(format!(
            "[[units.docker_contexts]] {id} context `{name}` path `{path}` must name a directory"
        )));
    }
    Ok(())
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

    /// The declared documentation-site contract.
    pub(crate) fn docs(&self) -> &DocsSection {
        &self.docs
    }

    /// The declared scheduled-check profiles, in the order the config declares
    /// them.
    pub(crate) fn check_profiles(&self) -> &[CheckProfileSection] {
        &self.check_profile
    }

    /// The declared maintenance overrides.
    pub(crate) fn maintenance(&self) -> &MaintenanceSection {
        &self.maintenance
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

    /// Whether every commit must carry a `Signed-off-by` trailer.
    pub(crate) fn dco_required(&self) -> Option<bool> {
        self.policy.dco_required
    }

    /// Admission rule for actions that are not pinned to a full commit SHA.
    pub(crate) fn action_pin_admission(&self) -> Option<&str> {
        self.policy.action_pin_admission.as_deref()
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
        self.validate_docs()?;
        self.validate_check_profiles(mise_lock_keys)?;
        self.validate_maintenance()?;
        self.validate_policy()?;
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

/// A repeated list entry is a typo-class error: the render would carry it
/// once, so the declaration must name it once.
fn reject_duplicates(owner: &str, values: &[String]) -> Result<(), GeneratorError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(GeneratorError::usage(format!(
                "{owner} names `{value}` twice; list each entry once"
            )));
        }
    }
    Ok(())
}

fn validate_workflow(workflow: &WorkflowSection) -> Result<(), GeneratorError> {
    use crate::s2::provider::{require_non_empty, require_subset, validate_selector_disjointness};
    if let Some(branch) = workflow.default_branch.as_deref()
        && !crate::s2::runtime::valid_branch(branch)
    {
        return Err(GeneratorError::usage(format!(
            "[workflow] default_branch renders into trigger branches and `github.ref` guards; `{branch}` is outside the branch alphabet"
        )));
    }
    let universe = workflow
        .providers
        .as_deref()
        .map(|providers| parse_provider_set(providers, "[workflow] providers"))
        .transpose()?
        .unwrap_or_else(|| crate::s2::provider::ProviderId::ALL.into_iter().collect());
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
        crate::s2::parse_rust_needs(value)?;
    }
    if let Some(value) = workflow.concurrency_group.as_deref() {
        crate::s2::validate_config_text(value, "[workflow] concurrency_group")?;
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
            crate::s2::provider::TrustReq::parse(trust).map_err(|_| {
                GeneratorError::usage(format!(
                    "[[unit]] {id} declares trust `{trust}`; expected one of: untrusted-ok, trusted-only"
                ))
            })?;
        }
        if let Some(platform) = row.platform.as_deref() {
            crate::s2::provider::Platform::parse(platform).map_err(|_| {
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
        row.validated_env(id)?;
        row.named_products(id)?;
        row.declared_prerequisites(id)?;
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
        for prerequisite in &row.prerequisites {
            if let Some(producer) = prerequisite.producer.as_deref() {
                report("prerequisite producer", producer)?;
            }
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
    "docker",
    "tasks",
];

/// The modes a typed release job gates on. validate runs only on workflow
/// dispatch; publish runs only on tag pushes. Empty runs on both.
pub(crate) const RELEASE_JOB_MODES: &[&str] = &["validate", "publish"];

/// GitHub permission scopes accepted by a typed release job.
pub(crate) const RELEASE_JOB_PERMISSIONS: &[&str] = &[
    "actions",
    "attestations",
    "checks",
    "contents",
    "deployments",
    "discussions",
    "id-token",
    "issues",
    "models",
    "packages",
    "pages",
    "pull-requests",
    "repository-projects",
    "security-events",
    "statuses",
];

/// Permission levels accepted by a typed release job.
pub(crate) const RELEASE_JOB_PERMISSION_LEVELS: &[&str] = &["read", "write", "none"];

// Keep this in lockstep with the Velnor runner's
// `actions/attest-build-provenance` capability contract.
const VELNOR_ATTESTATION_SUBJECTS: &[&str] = &["dist/*.tar.gz", "dist/l2-subject.json"];

/// The OCI platforms the `docker` publisher builds. Native builders exist
/// for exactly these; anything else fails closed instead of silently
/// emulating an architecture under QEMU.
pub(crate) const DOCKER_PLATFORMS: &[&str] = &["linux/amd64", "linux/arm64"];

/// Whether every declared `docker` platform names a natively built Linux
/// architecture. An empty list selects both; an unknown value is a
/// configuration error, never a silently dropped platform.
pub(crate) fn valid_docker_platforms(platforms: &[String]) -> bool {
    platforms
        .iter()
        .all(|platform| DOCKER_PLATFORMS.contains(&platform.as_str()))
}

/// Whether `value` is a well-formed OCI image reference without a digest:
/// `[host[:port]/]path[/path…][:tag]` over the lowercase OCI alphabet. The
/// reference renders into YAML `env:` and shell-adjacent `with:` blocks, so
/// anything outside this alphabet — whitespace, quotes, shell metacharacters,
/// uppercase — fails closed here instead of reaching a render sink.
pub(crate) fn valid_docker_image(value: &str) -> bool {
    if value.is_empty() || value.len() > 255 {
        return false;
    }
    let (name, tag) = match value.rsplit_once(':') {
        // A colon after the last slash starts the tag; before it, a port.
        Some((head, tag)) if !tag.contains('/') => (head, Some(tag)),
        _ => (value, None),
    };
    if let Some(tag) = tag {
        let mut bytes = tag.bytes();
        let first = bytes.next();
        if !first.is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || tag.len() > 128
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return false;
        }
    }
    let mut components = name.split('/');
    let Some(first) = components.next() else {
        return false;
    };
    // The first component may be a registry host (`ghcr.io`, `host:5000`);
    // every later one is a repository path component.
    let host = first.split(':').next().unwrap_or_default();
    if !valid_docker_host(host) {
        return false;
    }
    if name.contains(':') && !first.contains(':') {
        return false;
    }
    if first.contains(':') {
        let port = first.rsplit(':').next().unwrap_or_default();
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
    }
    components.all(valid_docker_component)
}

fn valid_docker_host(host: &str) -> bool {
    !host.is_empty()
        && host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

fn valid_docker_component(component: &str) -> bool {
    !component.is_empty()
        && component.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && !component.starts_with(['.', '-', '_'])
        && !component.ends_with(['.', '-', '_'])
}

pub(crate) fn validate_renovate_token_name(token: &str) -> Result<(), GeneratorError> {
    validate_secret_name("[renovate] token", token, "GH_RENOVATE_TOKEN")
}

/// The `[renovate]` credential/target contract: repository targets,
/// host-rules secret, commit author/signoff, and execution allowances.
fn validate_renovate_contract(renovate: &RenovateSection) -> Result<(), GeneratorError> {
    for repository in &renovate.repositories {
        validate_renovate_repository_target(repository)?;
    }
    reject_duplicates("[renovate] repositories", &renovate.repositories)?;
    if let Some(secret) = renovate.host_rules_secret.as_deref() {
        validate_renovate_host_rules_secret(secret)?;
    }
    if let Some(author) = renovate.author.as_deref() {
        validate_renovate_git_author(author)?;
    }
    if renovate.signoff == Some(true) && renovate.author.is_none() {
        return Err(GeneratorError::usage(
            "[renovate] signoff = true requires `author`: the Signed-off-by trailer names the author Renovate commits as",
        ));
    }
    for command in &renovate.allowed_commands {
        validate_renovate_allowed_command(command)?;
    }
    reject_duplicates("[renovate] allowed_commands", &renovate.allowed_commands)?;
    Ok(())
}

pub(crate) fn validate_renovate_host_rules_secret(secret: &str) -> Result<(), GeneratorError> {
    validate_secret_name(
        "[renovate] host_rules_secret",
        secret,
        "RENOVATE_HOST_RULES_JSON",
    )
}

/// A credential reference: an uppercase secret name, never `GITHUB_TOKEN`
/// (a declared credential needs a dedicated secret the automatic token must
/// not silently stand in for) and never an empty or lowercase spelling a
/// `secrets.` lookup would miss.
pub(crate) fn validate_secret_name(
    owner: &str,
    token: &str,
    example: &str,
) -> Result<(), GeneratorError> {
    if token == "GITHUB_TOKEN" {
        return Err(GeneratorError::usage(format!(
            "{owner} must name a dedicated secret, not GITHUB_TOKEN"
        )));
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
            "{owner} must be an uppercase secret name such as {example}, found `{token}`"
        )));
    }
    Ok(())
}

pub(crate) fn validate_renovate_cron(schedule: &str) -> Result<(), GeneratorError> {
    validate_cron_schedule("[renovate] schedule", schedule)
}

pub(crate) fn validate_maintenance_cron(schedule: &str) -> Result<(), GeneratorError> {
    validate_cron_schedule("[maintenance] schedule", schedule)
}

fn validate_cron_schedule(owner: &str, schedule: &str) -> Result<(), GeneratorError> {
    let fields = schedule.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err(GeneratorError::usage(format!(
            "{owner} must be a 5-field cron expression, found `{schedule}`"
        )));
    }
    Ok(())
}

/// One `[renovate] repositories` target: an `owner/repository` slug with the
/// same GitHub-name shape the generator's own repository slug carries.
pub(crate) fn validate_renovate_repository_target(repository: &str) -> Result<(), GeneratorError> {
    let valid = repository.split_once('/').is_some_and(|(owner, name)| {
        !repository.contains("..")
            && [owner, name].iter().all(|segment| {
                !segment.is_empty()
                    && segment.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_')
                    })
                    && !segment.starts_with('.')
                    && !Path::new(segment)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("git"))
            })
    });
    if !valid {
        return Err(GeneratorError::usage(format!(
            "[renovate] repositories must be `owner/repository` slugs, found `{repository}`"
        )));
    }
    Ok(())
}

/// The `[renovate] author` Renovate commits as: a display name plus an
/// angle-bracket email, the shape `gitAuthor` and the DCO trailer share.
pub(crate) fn validate_renovate_git_author(author: &str) -> Result<(), GeneratorError> {
    let valid = author
        .rsplit_once('<')
        .and_then(|(name, email)| email.strip_suffix('>').map(|email| (name, email)))
        .is_some_and(|(name, email)| {
            !name.trim().is_empty()
                && !name.contains(['\n', '\r'])
                && !email.contains(['\n', '\r', ' ', '<', '>'])
                && email.split_once('@').is_some_and(|(user, host)| {
                    !user.is_empty() && !host.is_empty() && host.contains('.')
                })
        });
    if !valid {
        return Err(GeneratorError::usage(format!(
            "[renovate] author must be `Name <email>`, found `{author}`"
        )));
    }
    Ok(())
}

/// One `[renovate] allowed_commands` entry: a single-line regex the rendered
/// `RENOVATE_ALLOWED_COMMANDS` JSON array carries verbatim.
pub(crate) fn validate_renovate_allowed_command(command: &str) -> Result<(), GeneratorError> {
    if command.is_empty() || command.contains(['\n', '\r', '\0']) {
        return Err(GeneratorError::usage(format!(
            "[renovate] allowed_commands must be single-line command patterns, found `{command}`"
        )));
    }
    Ok(())
}

/// One `[maintenance] producers` entry: a bare `.yml` workflow basename the
/// retention gate watches, never a path.
pub(crate) fn validate_maintenance_producer(producer: &str) -> Result<(), GeneratorError> {
    let valid = producer.len() > ".yml".len()
        && Path::new(producer)
            .extension()
            .is_some_and(|extension| extension == "yml")
        && !producer.contains(['/', '\\'])
        && !producer.contains("..")
        && Path::new(producer)
            .file_stem()
            .is_some_and(|stem| !stem.is_empty());
    if !valid {
        return Err(GeneratorError::usage(format!(
            "[maintenance] producers must be bare `.yml` workflow file names, found `{producer}`"
        )));
    }
    Ok(())
}

/// The `[maintenance] max_deletes` per-run bound: at least one delete, and
/// small enough that one run cannot sweep the account in a single pass.
pub(crate) fn validate_maintenance_max_deletes(max_deletes: u32) -> Result<(), GeneratorError> {
    if !(1..=5000).contains(&max_deletes) {
        return Err(GeneratorError::usage(format!(
            "[maintenance] max_deletes must be between 1 and 5000, found `{max_deletes}`"
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

/// A `[docs]` schedule is a 5-field cron expression, like the Renovate writer.
pub(crate) fn validate_docs_cron(schedule: &str) -> Result<(), GeneratorError> {
    let fields = schedule.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err(GeneratorError::usage(format!(
            "[docs] schedule must be a 5-field cron expression, found `{schedule}`"
        )));
    }
    Ok(())
}

/// A `[docs]` site URL is the consumer-owned deployed address: an `https://`
/// URL without whitespace or control characters.
pub(crate) fn validate_docs_site_url(site_url: &str) -> Result<(), GeneratorError> {
    let valid = site_url.starts_with("https://")
        && site_url.len() > "https://".len()
        && !site_url
            .chars()
            .any(|character| character.is_whitespace() || character.is_control());
    if !valid {
        return Err(GeneratorError::usage(format!(
            "[docs] site_url must be an `https://` URL without whitespace, found `{site_url}`"
        )));
    }
    Ok(())
}

/// A `[docs]` command renders verbatim into one workflow `run:` block, so it
/// must be a single non-empty line without control characters.
pub(crate) fn validate_docs_command(command: &str, field: &str) -> Result<(), GeneratorError> {
    if command.is_empty()
        || command.contains(['\n', '\r'])
        || command
            .chars()
            .any(|character| character.is_control() && !character.is_whitespace())
        || command.trim().is_empty()
    {
        return Err(GeneratorError::usage(format!(
            "[docs] {field} must hold single-line shell commands, found an empty or multi-line entry"
        )));
    }
    Ok(())
}

/// A `[docs]` repository path (output directory, sitemap) stays inside the
/// repository: relative, no traversal, no separators that escape it.
pub(crate) fn validate_docs_path(path: &str, field: &str) -> Result<(), GeneratorError> {
    if !is_contained_repository_path(path) {
        return Err(GeneratorError::usage(format!(
            "[docs] {field} must be a repository-relative path without traversal, found `{path}`"
        )));
    }
    Ok(())
}

/// Every `[docs]` command table holds single-line shell commands. The table
/// shape is one loop so a new stage cannot forget its own validation.
fn validate_docs_command_table(docs: &DocsSection) -> Result<(), GeneratorError> {
    for (field, commands) in [
        ("build_commands", docs.build_commands()),
        ("source_link_commands", docs.source_link_commands()),
        ("site_link_commands", docs.site_link_commands()),
        ("spell_commands", docs.spell_commands()),
        ("verify_commands", docs.verify_commands()),
        ("external_link_commands", docs.external_link_commands()),
    ] {
        for command in commands {
            validate_docs_command(command, field)?;
        }
    }
    Ok(())
}

/// The `[docs]` stage rules: at least one local check, and the schedule and
/// the scheduled-external check come together or not at all.
fn validate_docs_stages(docs: &DocsSection) -> Result<(), GeneratorError> {
    let checks = docs.source_link_commands().len()
        + docs.site_link_commands().len()
        + docs.spell_commands().len();
    if checks == 0 {
        return Err(GeneratorError::usage(
            "[docs] enabled = true requires at least one local check: `source_link_commands`, `site_link_commands`, or `spell_commands`",
        ));
    }
    match (
        !docs.external_link_commands().is_empty(),
        docs.schedule(),
    ) {
        (true, Some(schedule)) => validate_docs_cron(schedule),
        (true, None) => Err(GeneratorError::usage(
            "[docs] external_link_commands requires `schedule`, the cron that runs the scheduled-external live-link check",
        )),
        (false, Some(_)) => Err(GeneratorError::usage(
            "[docs] schedule without external_link_commands runs nothing; declare the scheduled-external check or drop the schedule",
        )),
        (false, None) => Ok(()),
    }
}

/// Whether `id` is a valid scheduled-check profile id: a GitHub job id and an
/// artifact name at once, so the rendered job and its uploaded bundle share
/// one spelling.
fn valid_check_profile_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Whether `task` is a safe named-task reference: it renders verbatim into
/// `mise run <task>`, so the first byte cannot be a flag and the rest cannot
/// carry shell syntax. Shared with the versioned-tool release rows, whose
/// gate, assert, and build task lists carry the same `mise run` shape.
pub(crate) fn valid_check_profile_task(task: &str) -> bool {
    let mut bytes = task.bytes();
    bytes.next().is_some_and(|first| {
        first.is_ascii_alphanumeric() || matches!(first, b'_' | b'.' | b':' | b'/' | b'+')
    }) && bytes.all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'+')
    })
}

/// Whether `key` is a valid environment threshold name: a shell identifier the
/// rendered job exports for the named task to read.
fn valid_check_profile_env_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// The release events on which one named-task job can run. An omitted mode
/// means both dispatch validation and tag publication.
fn release_job_events(row: &ReleaseJobSection) -> (bool, bool) {
    let modes = row.modes.as_deref().unwrap_or_default();
    if modes.is_empty() {
        (true, true)
    } else {
        (
            modes.iter().any(|mode| mode == "validate"),
            modes.iter().any(|mode| mode == "publish"),
        )
    }
}

/// Reject cycles before a release job graph reaches GitHub Actions. GitHub
/// does not report a useful configuration error for a cycle; it leaves every
/// member waiting forever, so generation must fail with the actual cycle.
fn validate_release_job_cycles(rows: &[ReleaseJobSection]) -> Result<(), GeneratorError> {
    fn visit(
        node: &str,
        graph: &BTreeMap<String, Vec<String>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        stack: &mut Vec<String>,
    ) -> Result<(), GeneratorError> {
        if visited.contains(node) {
            return Ok(());
        }
        if visiting.contains(node) {
            let start = stack.iter().position(|entry| entry == node).unwrap_or(0);
            let mut cycle = stack[start..].to_vec();
            cycle.push(node.to_owned());
            return Err(GeneratorError::usage(format!(
                "[[release.job]] dependency cycle: {}",
                cycle.join(" -> ")
            )));
        }

        visiting.insert(node.to_owned());
        stack.push(node.to_owned());
        if let Some(needs) = graph.get(node) {
            for dependency in needs {
                // Unknown dependencies are reported by validate_release_jobs
                // before this graph check. Keeping this guard makes the
                // helper total when unit-tested directly.
                if graph.contains_key(dependency) {
                    visit(dependency, graph, visiting, visited, stack)?;
                }
            }
        }
        stack.pop();
        visiting.remove(node);
        visited.insert(node.to_owned());
        Ok(())
    }

    let graph = rows
        .iter()
        .map(|row| {
            (
                row.id.as_deref().unwrap_or_default().to_owned(),
                row.needs.clone().unwrap_or_default(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut stack = Vec::new();
    for node in graph.keys() {
        visit(node, &graph, &mut visiting, &mut visited, &mut stack)?;
    }
    Ok(())
}

fn validate_check_profile_cron(id: &str, schedule: &str) -> Result<(), GeneratorError> {
    if schedule.contains(['\n', '\r']) {
        return Err(GeneratorError::usage(format!(
            "[[check_profile]] {id} schedule must be one line, found a multi-line value"
        )));
    }
    let fields = schedule.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err(GeneratorError::usage(format!(
            "[[check_profile]] {id} schedule must be a 5-field cron expression, found `{schedule}`"
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
            .any(|row| row.primitive() == crate::s2::primitives::RENOVATE)
        {
            return Err(GeneratorError::usage(
                "[renovate] enabled = true requires `[[declare]] primitive = \"renovate\" file = \"renovate.yml\"`",
            ));
        }
        if renovate.validate == Some(true)
            && !self
                .declare
                .iter()
                .any(|row| row.primitive() == crate::s2::primitives::RENOVATE_VALIDATE)
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
            .unwrap_or_else(|| crate::s2::provider::ProviderId::ALL.into_iter().collect());
        if !universe.contains(&crate::s2::provider::ProviderId::Velnor) {
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
        for schedule in &renovate.schedules {
            validate_renovate_cron(schedule)?;
        }
        reject_duplicates("[renovate] schedules", &renovate.schedules)?;
        if let Some(primary) = renovate.schedule.as_deref()
            && renovate.schedules.iter().any(|extra| extra == primary)
        {
            return Err(GeneratorError::usage(format!(
                "[renovate] schedules repeats the primary schedule `{primary}`; list each cron once"
            )));
        }
        if let Some(config) = renovate.config.as_deref() {
            validate_renovate_config_path(config)?;
        }
        validate_renovate_contract(renovate)?;
        Ok(())
    }

    /// The `[maintenance]` overrides: a valid cron, producer basenames, and
    /// a delete bound that keeps one run's sweep finite.
    fn validate_maintenance(&self) -> Result<(), GeneratorError> {
        let maintenance = &self.maintenance;
        if let Some(schedule) = maintenance.schedule.as_deref() {
            validate_maintenance_cron(schedule)?;
        }
        if let Some(producers) = maintenance.producers.as_deref() {
            if producers.is_empty() {
                return Err(GeneratorError::usage(
                    "[maintenance] producers must name at least one producer workflow",
                ));
            }
            for producer in producers {
                validate_maintenance_producer(producer)?;
            }
            reject_duplicates("[maintenance] producers", producers)?;
        }
        if let Some(max_deletes) = maintenance.max_deletes {
            validate_maintenance_max_deletes(max_deletes)?;
        }
        Ok(())
    }

    /// The `[policy]` compliance contract: the only implemented action-pin
    /// admission is the reviewed allowlist — full-SHA pins and reviewed
    /// local paths, exactly what the validator's `action-pins` rule admits —
    /// and a required DCO sign-off needs the external DCO check that
    /// enforces it.
    fn validate_policy(&self) -> Result<(), GeneratorError> {
        if let Some(admission) = self.action_pin_admission()
            && admission != "reviewed-allowlist"
        {
            return Err(GeneratorError::usage(format!(
                "[policy] action_pin_admission must be `reviewed-allowlist`, found `{admission}`: the validator admits only full-SHA pins and reviewed local paths"
            )));
        }
        if self.dco_required() == Some(true)
            && !self
                .ruleset_external_status_checks()
                .iter()
                .any(|context| context == "DCO")
        {
            return Err(GeneratorError::usage(
                "[policy] dco_required = true requires `DCO` in ruleset_external_status_checks: sign-off is enforced by the external DCO check, and without it the requirement is unenforced",
            ));
        }
        Ok(())
    }

    fn validate_release(&self) -> Result<(), GeneratorError> {
        let release = &self.release;
        validate_release_jobs(&self.workflow, release)?;
        validate_release_bindings(release)?;
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
            "docker" => {
                release
                    .image
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
                    && valid_docker_platforms(&release.platforms)
                    && release
                        .dockerfile
                        .as_deref()
                        .is_none_or(is_contained_repository_path)
                    && release
                        .context
                        .as_deref()
                        .is_none_or(is_contained_repository_path)
            }
            "tasks" => !release.job.is_empty(),
            _ => false,
        };
        if complete {
            return Ok(());
        }
        let declared = self.declare.iter().any(|row| {
            row.primitive() == crate::s2::primitives::STATIC_WORKFLOW
                && row.file.as_deref() == Some(crate::s2::RELEASE_WORKFLOW)
        });
        if declared && !kind.is_empty() {
            return Ok(());
        }
        Err(GeneratorError::usage(format!(
            "[release] enabled repositories must declare `kind`, one of {}, each with the contract fields that publisher renders from; a `kind` the renderer does not implement requires a `static-workflow` row that renders `{}` verbatim",
            RELEASE_KINDS.join(", "),
            crate::s2::RELEASE_WORKFLOW
        )))
    }

    fn validate_docs(&self) -> Result<(), GeneratorError> {
        let docs = &self.docs;
        if docs.enabled != Some(true) {
            return Ok(());
        }
        if docs.reason.as_deref().unwrap_or_default().is_empty() {
            return Err(GeneratorError::usage(
                "[docs] enabled = true requires `reason` documenting why this repository publishes a documentation site",
            ));
        }
        if !self.declare.iter().any(|row| {
            row.primitive() == crate::s2::primitives::DOCS_SITE
                && row.file.as_deref() == Some(crate::s2::primitives::docs_site::DOCS_SITE_FILE)
        }) {
            return Err(GeneratorError::usage(
                "[docs] enabled = true requires `[[declare]] primitive = \"docs-site\" file = \"docs.yml\"`",
            ));
        }
        let site_url = docs.site_url.as_deref().unwrap_or_default();
        validate_docs_site_url(site_url)?;
        let site_dir = docs.site_dir.as_deref().unwrap_or_default();
        if site_dir.is_empty() {
            return Err(GeneratorError::usage(
                "[docs] enabled = true requires `site_dir`, the built site directory the pipeline uploads to Pages",
            ));
        }
        validate_docs_path(site_dir, "site_dir")?;
        if let Some(sitemap) = docs.sitemap_path.as_deref() {
            validate_docs_path(sitemap, "sitemap_path")?;
        }
        let build = docs.build_commands.as_deref().unwrap_or(&[]);
        if build.is_empty() {
            return Err(GeneratorError::usage(
                "[docs] enabled = true requires `build_commands`, the consumer-owned site build",
            ));
        }
        validate_docs_command_table(docs)?;
        validate_docs_stages(docs)?;
        if let Some(paths) = docs.docs_paths.as_deref() {
            for pattern in paths {
                if pattern.is_empty() {
                    return Err(GeneratorError::usage(
                        "[docs] docs_paths must not contain an empty pattern",
                    ));
                }
                if globset::Glob::new(pattern).is_err() {
                    return Err(GeneratorError::usage(format!(
                        "[docs] docs_paths is not a valid glob: {pattern}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn validate_check_profiles(
        &self,
        mise_lock_keys: &BTreeSet<String>,
    ) -> Result<(), GeneratorError> {
        let mut ids = BTreeSet::new();
        for row in &self.check_profile {
            let id = row.id.as_deref().unwrap_or_default();
            if id.is_empty() {
                return Err(GeneratorError::usage(
                    "[[check_profile]] is missing `id`; name the scheduled job the row declares",
                ));
            }
            if !valid_check_profile_id(id) {
                return Err(GeneratorError::usage(format!(
                    "[[check_profile]] {id} must be a job id: start with a letter or underscore, then letters, digits, `-`, or `_`"
                )));
            }
            if !ids.insert(id) {
                return Err(GeneratorError::usage(format!(
                    "[[check_profile]] {id} is declared twice; a profile renders exactly one scheduled job"
                )));
            }
        }
        for row in &self.check_profile {
            let id = row.id.as_deref().unwrap_or_default();
            validate_check_profile_row(self, row, id, &ids, mise_lock_keys)?;
        }
        Ok(())
    }
}

/// One profile row against the repository around it: cadence, platform, task
/// references, dependencies, timeout, artifacts, thresholds, and status.
fn validate_check_profile_row(
    config: &RepoGenerationConfig,
    row: &CheckProfileSection,
    id: &str,
    ids: &BTreeSet<&str>,
    mise_lock_keys: &BTreeSet<String>,
) -> Result<(), GeneratorError> {
    if let Some(name) = row.name.as_deref()
        && (name.is_empty() || name.contains(['\n', '\r']))
    {
        return Err(GeneratorError::usage(format!(
            "[[check_profile]] {id} name must be one non-empty line"
        )));
    }
    // A missing schedule is allowed here: cron-lessness is a file-level
    // property ("schedule-less only in files whose row sets `events`"),
    // enforced in `select_profiles` where file context exists.
    if let Some(schedule) = row.schedule.as_deref() {
        validate_check_profile_cron(id, schedule)?;
    }
    match row.runner.as_deref() {
        None | Some("github" | "macos" | "velnor") => {}
        Some(runner) => {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} runner must be one of: github, macos, velnor; found `{runner}`"
            )));
        }
    }
    if row.runner.as_deref().unwrap_or("github") == "velnor" {
        let universe = config
            .workflow
            .providers
            .as_deref()
            .map(|providers| parse_provider_set(providers, "[workflow] providers"))
            .transpose()?
            .unwrap_or_else(|| crate::s2::provider::ProviderId::ALL.into_iter().collect());
        if !universe.contains(&ProviderId::Velnor) {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} runs on velnor, but [workflow] providers has no velnor provider"
            )));
        }
        if !config.workflow.selectors.contains_key("velnor") {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} runs on velnor, but [workflow.selectors.velnor] names no selector for the job"
            )));
        }
    }
    if let Some(tools) = row.tools.as_deref() {
        validate_check_profile_tools(id, tools, mise_lock_keys)?;
    }
    match row.tasks.as_deref() {
        None => {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} is missing `tasks`; name the mise tasks the job runs"
            )));
        }
        Some(tasks) => {
            if tasks.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "[[check_profile]] {id} declares an empty tasks; name the mise tasks the job runs"
                )));
            }
            for task in tasks {
                if !valid_check_profile_task(task) {
                    return Err(GeneratorError::usage(format!(
                        "[[check_profile]] {id} names task `{task}`, which is not a plain task reference; use names such as check-smoke without whitespace or shell syntax"
                    )));
                }
            }
        }
    }
    validate_check_profile_result(row, id, ids)?;
    Ok(())
}

/// A profile's dependency, timeout, artifact, threshold, and status contract.
fn validate_check_profile_result(
    row: &CheckProfileSection,
    id: &str,
    ids: &BTreeSet<&str>,
) -> Result<(), GeneratorError> {
    if let Some(needs) = row.needs.as_deref() {
        for dependency in needs {
            if dependency == id {
                return Err(GeneratorError::usage(format!(
                    "[[check_profile]] {id} needs itself; a job cannot wait on its own completion"
                )));
            }
            if !ids.contains(dependency.as_str()) {
                let known = ids.iter().copied().collect::<Vec<_>>().join(", ");
                return Err(GeneratorError::usage(format!(
                    "[[check_profile]] {id} needs `{dependency}`, which no profile declares; declared profiles: {known}"
                )));
            }
        }
    }
    if let Some(timeout) = row.timeout_minutes
        && (timeout < 1 || u32::try_from(timeout).is_err())
    {
        return Err(GeneratorError::usage(format!(
            "[[check_profile]] {id} timeout_minutes must be a positive number of minutes, found {timeout}"
        )));
    }
    if let Some(artifacts) = row.artifacts.as_deref() {
        for artifact in artifacts {
            if artifact.is_empty() || artifact.contains(['\n', '\r']) {
                return Err(GeneratorError::usage(format!(
                    "[[check_profile]] {id} declares an artifact path that is empty or multi-line; name the paths the job uploads"
                )));
            }
        }
    }
    for (key, value) in &row.env {
        if !valid_check_profile_env_key(key) {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} env names `{key}`, which is not a threshold name; use shell identifiers such as MAX_SECONDS"
            )));
        }
        if value.contains(['\n', '\r']) {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} env `{key}` must be one line"
            )));
        }
    }
    match row.status.as_deref() {
        None | Some("required" | "advisory") => {}
        Some(status) => {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} status must be `required` or `advisory`, found `{status}`"
            )));
        }
    }
    Ok(())
}

/// The declared tool ids against the root lock: shape and duplicate checks run
/// first so a compromised lock can never smuggle shell syntax into
/// `install_args` through membership.
/// A non-empty tool declaration requires a non-empty `mise.lock` key set:
/// strict `mise --locked install` must have a pinned identity to enforce.
fn validate_check_profile_tools(
    id: &str,
    tools: &[String],
    mise_lock_keys: &BTreeSet<String>,
) -> Result<(), GeneratorError> {
    if tools.is_empty() {
        return Err(GeneratorError::usage(format!(
            "[[check_profile]] {id} declares an empty tools; omit it or name the tools the job installs"
        )));
    }
    let mut seen = BTreeSet::new();
    for tool in tools {
        if !valid_mise_tool_id(tool) {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} declares tool {tool}, which is not a plain tool id; use ids such as cargo-binstall without versions, flags, whitespace, traversal, or shell metacharacters"
            )));
        }
        if !seen.insert(tool) {
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} declares tool {tool} more than once"
            )));
        }
    }
    if mise_lock_keys.is_empty() {
        return Err(GeneratorError::usage(format!(
            "[[check_profile]] {id} declares tools but the repository has no pinned mise.lock tool keys; strict locked installation requires mise.lock"
        )));
    }
    for tool in tools {
        if !mise_lock_keys.contains(tool) {
            let known = mise_lock_keys
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return Err(GeneratorError::usage(format!(
                "[[check_profile]] {id} declares tool {tool}, which mise.lock does not pin; install_args must equal the lock keys, known keys: {known}"
            )));
        }
    }
    Ok(())
}

/// The dispatch modes a release contract may offer. `publish` is never a
/// dispatch option: publication stays tag-triggered (stable) or
/// admitted-producer-triggered (rolling).
pub(crate) const RELEASE_MODES: &[&str] = &["validate", "build", "rehearse"];

/// Whether `value` is a dispatch mode the release renderer offers.
pub(crate) fn is_release_mode(value: &str) -> bool {
    RELEASE_MODES.contains(&value)
}

/// Validate the shape of the release event bindings, modes, archive
/// contract, and credential pairs. Shape errors fail closed whether or not
/// the contract is enabled: a typo'd mode or an unpaired credential must
/// never render a silently weaker lane.
/// The `[release]` naming contract: `package`, `binary`, and `targets`
/// render into shell double-quotes (`assemble-manifest --subjects`), YAML
/// scalars, and glob patterns, so they must match the same alphabets the
/// runtime verbs enforce — generation-time rejection is the only layer that
/// runs before the shell parses. `image` renders into YAML `env:` and
/// shell-adjacent `with:` blocks, so it must be a well-formed OCI reference.
fn validate_release_naming(release: &ReleaseSection) -> Result<(), GeneratorError> {
    if let Some(package) = release.package.as_deref()
        && !crate::s2::runtime::valid_package(package)
    {
        return Err(GeneratorError::usage(format!(
            "[release] package must be ASCII alphanumeric with `-`/`_`, found `{package}`"
        )));
    }
    if let Some(binary) = release.binary.as_deref()
        && !crate::s2::runtime::valid_binary(binary)
    {
        return Err(GeneratorError::usage(format!(
            "[release] binary must be ASCII alphanumeric with `.`/`-`/`_`, found `{binary}`"
        )));
    }
    if let Some(package) = release.image_package.as_deref()
        && !crate::s2::runtime::valid_package(package)
    {
        return Err(GeneratorError::usage(format!(
            "[release] image_package must be ASCII alphanumeric with `-`/`_`, found `{package}`"
        )));
    }
    for target in &release.targets {
        if !crate::s2::runtime::valid_target(target) {
            return Err(GeneratorError::usage(format!(
                "[release] targets must be ASCII alphanumeric with `.`/`-`/`_`, found `{target}`"
            )));
        }
    }
    if let Some(image) = release.image.as_deref()
        && !valid_docker_image(image)
    {
        return Err(GeneratorError::usage(format!(
            "[release] image must be a lowercase OCI reference `[host[:port]/]path[:tag]`, found `{image}`"
        )));
    }
    if let Some(pattern) = release.tag_pattern.as_deref()
        && !crate::s2::runtime::valid_tag_pattern(pattern)
    {
        return Err(GeneratorError::usage(format!(
            "[release] tag_pattern must be one non-empty line with no whitespace, found `{pattern}`"
        )));
    }
    validate_release_registry(release)?;
    Ok(())
}

/// The registry-auth triple: a declared host always travels with both
/// credential secret names, and a declared credential always names its
/// host. A partial triple is a usage error naming the missing member, never
/// a login against the wrong registry with the wrong identity.
fn validate_release_registry(release: &ReleaseSection) -> Result<(), GeneratorError> {
    let members = [
        ("registry", release.registry.as_deref()),
        (
            "registry_username_secret",
            release.registry_username_secret.as_deref(),
        ),
        (
            "registry_password_secret",
            release.registry_password_secret.as_deref(),
        ),
    ];
    if members.iter().all(|(_, value)| value.is_none()) {
        return Ok(());
    }
    for (name, value) in &members {
        if value.is_none() {
            return Err(GeneratorError::usage(format!(
                "[release] {name} is missing: registry auth declares `registry`, `registry_username_secret`, and `registry_password_secret` together"
            )));
        }
    }
    let registry = release.registry.as_deref().unwrap_or_default();
    if !crate::s2::runtime::valid_registry_host(registry) {
        return Err(GeneratorError::usage(format!(
            "[release] registry must be a lowercase host with an optional `:port`, found `{registry}`"
        )));
    }
    validate_secret_name(
        "[release] registry_username_secret",
        release
            .registry_username_secret
            .as_deref()
            .unwrap_or_default(),
        "REGISTRY_USERNAME",
    )?;
    validate_secret_name(
        "[release] registry_password_secret",
        release
            .registry_password_secret
            .as_deref()
            .unwrap_or_default(),
        "REGISTRY_PASSWORD",
    )?;
    Ok(())
}

/// Tarball bindings (producer bindings, dispatch modes, archive contracts,
/// credential pairings) render only for the `rust-binary` and `native`
/// publishers. Declaring them on any other kind is a usage error, not a
/// silent omission: a typo'd `modes` on a `docker` contract must fail the
/// generation, never drop the publisher.
fn validate_release_binding_kind(release: &ReleaseSection) -> Result<(), GeneratorError> {
    let kind = release.kind.as_deref().unwrap_or_default();
    let registry_auth = release.registry.is_some()
        || release.registry_username_secret.is_some()
        || release.registry_password_secret.is_some();
    if registry_auth {
        if kind.is_empty() {
            return Err(GeneratorError::usage(
                "[release] registry auth needs `kind = \"docker\"`: no publisher is declared to log in with it",
            ));
        }
        if kind != "docker" {
            return Err(GeneratorError::usage(format!(
                "[release] registry auth renders only for kind `docker`, not `{kind}`"
            )));
        }
    }
    if kind.is_empty() || matches!(kind, "rust-binary" | "native") {
        return Ok(());
    }
    if kind == "tasks" {
        let unsupported = release.producer_workflow.is_some()
            || release.producer_conclusion.is_some()
            || release.archive_checksum.is_some()
            || release.archive_retention_days.is_some()
            || !release.archive_members.is_empty()
            || !release.credential.is_empty();
        if unsupported {
            return Err(GeneratorError::usage(
                "[release] producer bindings, archive contracts, and credential pairings render only for rust-binary or native, not tasks",
            ));
        }
        return Ok(());
    }
    let bound = release.producer_workflow.is_some()
        || !release.modes.is_empty()
        || release.archive_checksum.is_some()
        || release.archive_retention_days.is_some()
        || !release.archive_members.is_empty()
        || !release.credential.is_empty();
    if bound {
        return Err(GeneratorError::usage(format!(
            "[release] producer bindings, modes, archive contracts, and credential pairings render only for kind `rust-binary` or `native`, not `{kind}`"
        )));
    }
    Ok(())
}

fn validate_release_bindings(release: &ReleaseSection) -> Result<(), GeneratorError> {
    validate_release_naming(release)?;
    validate_release_binding_kind(release)?;
    if let Some(conclusion) = release.producer_conclusion.as_deref()
        && conclusion != "success"
    {
        return Err(GeneratorError::usage(format!(
            "[release] producer_conclusion must be `success`, found `{conclusion}`"
        )));
    }
    if release.producer_conclusion.is_some() && release.producer_workflow.is_none() {
        return Err(GeneratorError::usage(
            "[release] producer_conclusion needs producer_workflow: a required conclusion without a trusted producer binds nothing",
        ));
    }
    for mode in &release.modes {
        if !is_release_mode(mode) {
            return Err(GeneratorError::usage(format!(
                "[release] modes must be one of {}, found `{mode}`; publish is tag-triggered, never a dispatch option",
                RELEASE_MODES.join(", ")
            )));
        }
    }
    if let Some(checksum) = release.archive_checksum.as_deref()
        && checksum != "sha256"
    {
        return Err(GeneratorError::usage(format!(
            "[release] archive_checksum must be `sha256`, found `{checksum}`"
        )));
    }
    if let Some(retention) = release.archive_retention_days
        && !(1..=90).contains(&retention)
    {
        return Err(GeneratorError::usage(format!(
            "[release] archive_retention_days must be 1-90, found `{retention}`"
        )));
    }
    for member in &release.archive_members {
        if !is_archive_member(member) {
            return Err(GeneratorError::usage(format!(
                "[release] archive_members must be portable file names without directories, found `{member}`"
            )));
        }
    }
    for credential in &release.credential {
        let name = credential.name.as_deref().unwrap_or_default();
        if name.is_empty() {
            return Err(GeneratorError::usage(
                "[release] every [[release.credential]] row needs `name`",
            ));
        }
        if !crate::s2::runtime::valid_package(name) {
            return Err(GeneratorError::usage(format!(
                "[release] credential names render into shell function names; `{name}` is outside the portable alphabet"
            )));
        }
        if credential.setup.as_deref().is_none_or(str::is_empty) {
            return Err(GeneratorError::usage(format!(
                "[release] credential `{name}` needs `setup`"
            )));
        }
        if credential.teardown.as_deref().is_none_or(str::is_empty) {
            return Err(GeneratorError::usage(format!(
                "[release] credential `{name}` needs `teardown`: a setup without a teardown leaks host state"
            )));
        }
    }
    Ok(())
}

/// Whether `value` is a portable archive member name: a bare file name over
/// the portable asset alphabet, never a path.
/// Validate the typed named-task release graph. Tasks are repository-owned
/// commands, but job identity, dependencies, runner aliases, mode gates, and
/// execution metadata are generator-owned structure and fail closed here.
#[allow(clippy::too_many_lines)]
fn validate_release_jobs(
    workflow: &WorkflowSection,
    release: &ReleaseSection,
) -> Result<(), GeneratorError> {
    if release.job.is_empty() {
        return Ok(());
    }
    let kind = release.kind.as_deref().unwrap_or_default();
    if kind != "tasks" {
        return Err(GeneratorError::usage(format!(
            "[[release.job]] rows render only for kind tasks, not {kind}; other publishers own their job graph"
        )));
    }
    let mut ids = BTreeSet::new();
    for row in &release.job {
        let id = row.id.as_deref().unwrap_or_default();
        if id.is_empty() {
            return Err(GeneratorError::usage(
                "[[release.job]] is missing id; name the job the row renders",
            ));
        }
        if !valid_check_profile_id(id) {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} is not a job id; use letters, digits, - and _ starting with a letter or _"
            )));
        }
        if !ids.insert(id.to_owned()) {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} is declared twice; job ids must be unique"
            )));
        }
    }
    let providers = workflow
        .providers
        .as_deref()
        .map(|values| parse_provider_set(values, "[workflow] providers"))
        .transpose()?
        .unwrap_or_else(|| ProviderId::ALL.into_iter().collect());
    // The repository config may override scan defaults, but omitted selectors
    // still resolve to those defaults before rendering. Validate release
    // runner choices against the same effective map so a valid config can
    // never render `runs-on:` empty.
    let mut selectors = crate::s2::scan::default_selectors();
    selectors.extend(parse_selectors(&workflow.selectors)?);
    for row in &release.job {
        let id = row.id.as_deref().unwrap_or_default();
        if let Some(name) = row.name.as_deref()
            && (name.is_empty() || name.contains(['\n', '\r']))
        {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} name must be one non-empty line"
            )));
        }
        let Some(tasks) = row.tasks.as_deref() else {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} is missing tasks; name the mise tasks the job runs"
            )));
        };
        if tasks.is_empty() {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} declares empty tasks; name the mise tasks the job runs"
            )));
        }
        for task in tasks {
            if !valid_check_profile_task(task) {
                return Err(GeneratorError::usage(format!(
                    "[[release.job]] {id} names task {task}, which is not a plain task reference"
                )));
            }
        }
        if let Some(needs) = row.needs.as_deref() {
            for dependency in needs {
                if dependency == id {
                    return Err(GeneratorError::usage(format!(
                        "[[release.job]] {id} needs itself; a job cannot wait on its own completion"
                    )));
                }
                if !ids.contains(dependency) {
                    return Err(GeneratorError::usage(format!(
                        "[[release.job]] {id} needs {dependency}, which no job declares"
                    )));
                }
            }
        }
        let runner = row.runner.as_deref().unwrap_or("github");
        let (provider, needs_selector) = match runner {
            "github" => (ProviderId::GithubHosted, true),
            // macOS is a fixed GitHub-hosted label, so it needs the hosted
            // provider but not its Linux selector.
            "macos" => (ProviderId::GithubHosted, false),
            "velnor" => (ProviderId::Velnor, true),
            _ => {
                return Err(GeneratorError::usage(format!(
                    "[[release.job]] {id} runner must be one of github, macos, velnor; found {runner}"
                )));
            }
        };
        if !providers.contains(&provider) {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} runner `{runner}` selects {provider}, but [workflow] providers does not include that lane"
            )));
        }
        if needs_selector && !selectors.contains_key(&provider) {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} runner `{runner}` selects {provider}, but no selector supplies its runs-on labels"
            )));
        }
        if let Some(modes) = row.modes.as_deref() {
            for mode in modes {
                if !RELEASE_JOB_MODES.contains(&mode.as_str()) {
                    return Err(GeneratorError::usage(format!(
                        "[[release.job]] {id} modes must be one of {}, found {mode}",
                        RELEASE_JOB_MODES.join(", ")
                    )));
                }
            }
        }
        if let Some(timeout) = row.timeout_minutes
            && (timeout < 1 || u32::try_from(timeout).is_err())
        {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} timeout_minutes must be a positive number of minutes, found {timeout}"
            )));
        }
        if let Some(environment) = row.environment.as_deref()
            && (environment.is_empty() || environment.contains(['\n', '\r']))
        {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} environment must be one non-empty line"
            )));
        }
        if let Some(subjects) = row.attest_subjects.as_deref() {
            for subject in subjects {
                if subject.is_empty() || subject.contains(['\n', '\r']) {
                    return Err(GeneratorError::usage(format!(
                        "[[release.job]] {id} attest_subjects must be one non-empty line per subject"
                    )));
                }
            }
            if runner == "velnor"
                && (subjects.len() != 1
                    || !VELNOR_ATTESTATION_SUBJECTS.contains(&subjects[0].as_str()))
            {
                return Err(GeneratorError::usage(format!(
                    "[[release.job]] {id} runner `velnor` supports exactly one attest_subjects value from {}; found [{}]",
                    VELNOR_ATTESTATION_SUBJECTS.join(", "),
                    subjects.join(", "),
                )));
            }
        }
        for (scope, level) in &row.permissions {
            if !RELEASE_JOB_PERMISSIONS.contains(&scope.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "[[release.job]] {id} permissions names {scope}, which is not a job permission scope"
                )));
            }
            if !RELEASE_JOB_PERMISSION_LEVELS.contains(&level.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "[[release.job]] {id} permissions {scope} must be one of {}, found {level}",
                    RELEASE_JOB_PERMISSION_LEVELS.join(", ")
                )));
            }
        }
        if row
            .attest_subjects
            .as_deref()
            .is_some_and(|subjects| !subjects.is_empty())
        {
            for (scope, required) in [("id-token", "write"), ("attestations", "write")] {
                if let Some(level) = row.permissions.get(scope)
                    && level != required
                {
                    return Err(GeneratorError::usage(format!(
                        "[[release.job]] {id} attest_subjects requires permissions.{scope} = {required}, found {level}"
                    )));
                }
            }
        }
        if row
            .permissions
            .get("contents")
            .is_some_and(|level| level == "none")
        {
            return Err(GeneratorError::usage(format!(
                "[[release.job]] {id} permissions cannot set contents = none; checkout requires contents: read"
            )));
        }
        if let Some(needs) = row.needs.as_deref() {
            let (runs_validate, runs_publish) = release_job_events(row);
            for dependency in needs {
                let Some(dependency_row) = release
                    .job
                    .iter()
                    .find(|candidate| candidate.id.as_deref() == Some(dependency.as_str()))
                else {
                    // The earlier reference check returns this same config
                    // error before this mode check in normal validation.
                    continue;
                };
                let (dependency_validate, dependency_publish) = release_job_events(dependency_row);
                if (runs_validate && !dependency_validate) || (runs_publish && !dependency_publish)
                {
                    return Err(GeneratorError::usage(format!(
                        "[[release.job]] {id} needs {dependency}, but their release modes do not overlap for every event; a gated dependency would silently skip this job"
                    )));
                }
            }
        }
        for (key, value) in &row.env {
            if !valid_check_profile_env_key(key) {
                return Err(GeneratorError::usage(format!(
                    "[[release.job]] {id} env name {key} is not a shell identifier"
                )));
            }
            if value.contains(['\n', '\r']) {
                return Err(GeneratorError::usage(format!(
                    "[[release.job]] {id} env {key} must be one line"
                )));
            }
        }
    }
    validate_release_job_cycles(&release.job)?;
    Ok(())
}

fn is_archive_member(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
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
        let _ = fs::remove_dir_all(&root);
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

    #[test]
    fn docker_context_names_are_typed_unique_and_reserved_names_fail() {
        let root = scanned_root("docker-context-names");
        let duplicate = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[units]]\nid = \"docker\"\nkind = \"docker\"\n\n\
             [[units.docker_contexts]]\nname = \"checkout\"\npath = \".\"\n\n\
             [[units.docker_contexts]]\nname = \"checkout\"\npath = \".\"\n",
        );
        let error = must_fail(
            duplicate.units()[0].named_docker_contexts("docker", &root),
            "duplicate Docker context name must fail",
        );
        assert!(error.to_string().contains("more than once"), "{error}");

        let reserved = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[units]]\nid = \"docker\"\nkind = \"docker\"\n\n\
             [[units.docker_contexts]]\nname = \"velnor-cache-seed\"\npath = \".\"\n",
        );
        let error = must_fail(
            reserved.units()[0].named_docker_contexts("docker", &root),
            "reserved Docker context name must fail",
        );
        assert!(error.to_string().contains("reserved"), "{error}");
        let invalid = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[units]]\nid = \"docker\"\nkind = \"docker\"\n\n\
             [[units.docker_contexts]]\nname = \"Checkout\"\npath = \".\"\n",
        );
        let error = must_fail(
            invalid.units()[0].named_docker_contexts("docker", &root),
            "invalid Docker context name must fail",
        );
        assert!(error.to_string().contains("invalid"), "{error}");
        must(
            fs::remove_dir_all(root),
            "remove Docker context name fixture",
        );
    }

    #[test]
    fn docker_context_paths_reject_missing_directories() {
        let root = scanned_root("docker-context-missing");
        let missing = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[units]]\nid = \"docker\"\nkind = \"docker\"\n\n\
             [[units.docker_contexts]]\nname = \"checkout\"\npath = \"missing\"\n",
        );
        let error = must_fail(
            missing.units()[0].named_docker_contexts("docker", &root),
            "missing Docker context path must fail",
        );
        assert!(error.to_string().contains("does not exist"), "{error}");
        must(
            fs::remove_dir_all(root),
            "remove missing Docker context fixture",
        );
    }

    #[cfg(unix)]
    #[test]
    fn docker_context_path_rejects_escape_but_allows_in_repo_symlink() {
        use std::os::unix::fs::symlink;

        let root = scanned_root("docker-context-paths");
        let inside = root.join("inside");
        let outside = root.with_extension("outside");
        must(fs::create_dir_all(&inside), "create in-repository context");
        must(fs::create_dir_all(&outside), "create outside context");
        must(
            symlink(&inside, root.join("inside-link")),
            "create in-repository symlink",
        );
        must(
            symlink(&outside, root.join("escape-link")),
            "create escaping symlink",
        );

        let valid = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[units]]\nid = \"docker\"\nkind = \"docker\"\n\n\
             [[units.docker_contexts]]\nname = \"checkout\"\npath = \"inside-link\"\n",
        );
        must(
            valid.units()[0].named_docker_contexts("docker", &root),
            "in-repository symlink must remain usable",
        );

        let escaping = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[units]]\nid = \"docker\"\nkind = \"docker\"\n\n\
             [[units.docker_contexts]]\nname = \"checkout\"\npath = \"escape-link\"\n",
        );
        let error = must_fail(
            escaping.units()[0].named_docker_contexts("docker", &root),
            "escaping symlink must fail",
        );
        assert!(
            error.to_string().contains("escapes the repository"),
            "{error}"
        );

        let traversal = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[units]]\nid = \"docker\"\nkind = \"docker\"\n\n\
             [[units.docker_contexts]]\nname = \"checkout\"\npath = \"../outside\"\n",
        );
        let error = must_fail(
            traversal.units()[0].named_docker_contexts("docker", &root),
            "traversal path must fail",
        );
        assert!(error.to_string().contains("repository-relative"), "{error}");

        must(
            fs::remove_dir_all(root),
            "remove Docker context path fixture",
        );
        must(
            fs::remove_dir_all(outside),
            "remove outside context fixture",
        );
    }

    fn shape_for(root: &Path) -> crate::s2::scan::RepositoryShape {
        let providers: crate::s2::provider::ProviderSet =
            crate::s2::provider::ProviderId::ALL.into_iter().collect();
        must(
            crate::s2::scan::scan_shape(root, &providers, "main", &[]),
            "scan config test repository",
        )
    }

    fn config_for(text: &str) -> RepoGenerationConfig {
        must(
            toml::from_str::<RepoGenerationConfig>(text),
            "parse config under test",
        )
    }

    fn tasks_release_config(workflow: &str, jobs: &str) -> RepoGenerationConfig {
        config_for(&format!(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n{workflow}\n\n[release]\nenabled = true\nkind = \"tasks\"\n\n{jobs}\n"
        ))
    }

    fn tasks_release_validation_error(workflow: &str, jobs: &str) -> String {
        must_fail(
            tasks_release_config(workflow, jobs).validate(&[], &[], &BTreeSet::new()),
            "tasks release config must fail",
        )
        .to_string()
    }

    #[test]
    fn release_job_rejects_explicit_contents_none() {
        let error = tasks_release_validation_error(
            "",
            "[[release.job]]\nid = \"build\"\ntasks = [\"build\"]\n\n[release.job.permissions]\ncontents = \"none\"\n",
        );
        assert!(error.contains("contents = none"), "{error}");
        assert!(
            error.contains("checkout requires contents: read"),
            "{error}"
        );
    }

    #[test]
    fn release_job_rejects_attestation_permission_downgrade() {
        let error = tasks_release_validation_error(
            "",
            "[[release.job]]\nid = \"sign\"\ntasks = [\"sign\"]\nattest_subjects = [\"dist/*.tar.gz\"]\n\n[release.job.permissions]\nid-token = \"read\"\n",
        );
        assert!(
            error.contains("attest_subjects requires permissions.id-token = write"),
            "{error}"
        );
    }

    #[test]
    fn release_job_rejects_velnor_attestation_subject_outside_capability() {
        let error = tasks_release_validation_error(
            "[workflow]\nproviders = [\"velnor\"]\n",
            "[[release.job]]\nid = \"sign\"\ntasks = [\"sign\"]\nrunner = \"velnor\"\nattest_subjects = [\"dist/app.zip\"]\n",
        );
        assert!(error.contains("runner `velnor`"), "{error}");
        assert!(error.contains("dist/*.tar.gz"), "{error}");
        assert!(error.contains("dist/l2-subject.json"), "{error}");
    }

    #[test]
    fn release_job_rejects_runner_outside_provider_universe() {
        let error = tasks_release_validation_error(
            "[workflow]\nproviders = [\"velnor\"]\n",
            "[[release.job]]\nid = \"build\"\ntasks = [\"build\"]\nrunner = \"github\"\n",
        );
        assert!(
            error.contains("runner `github` selects github-hosted"),
            "{error}"
        );
        assert!(error.contains("does not include that lane"), "{error}");
    }

    #[test]
    fn release_job_rejects_dependency_cycles() {
        let error = tasks_release_validation_error(
            "",
            "[[release.job]]\nid = \"build\"\ntasks = [\"build\"]\nneeds = [\"publish\"]\n\n[[release.job]]\nid = \"publish\"\ntasks = [\"publish\"]\nneeds = [\"build\"]\n",
        );
        assert!(error.contains("dependency cycle"), "{error}");
        assert!(error.contains("build -> publish -> build"), "{error}");
    }

    #[test]
    fn release_job_rejects_mode_incompatible_dependency() {
        let error = tasks_release_validation_error(
            "",
            "[[release.job]]\nid = \"validate\"\ntasks = [\"validate\"]\nmodes = [\"validate\"]\n\n[[release.job]]\nid = \"publish\"\ntasks = [\"publish\"]\nmodes = [\"publish\"]\nneeds = [\"validate\"]\n",
        );
        assert!(error.contains("modes do not overlap"), "{error}");
        assert!(error.contains("silently skip"), "{error}");
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
        crate::s2::estate::apt_package_update_owner_blocks(PACKAGE_UPDATE_TEMPLATE)
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
             ruleset_external_status_checks = [\"DCO\"]\n\
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
    fn docker_release_accepts_an_image_with_defaulted_build_inputs() {
        let config = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[release]\nenabled = true\nkind = \"docker\"\nimage = \"ghcr.io/example/app\"\n",
        );
        must(
            config.validate(&[], &[], &BTreeSet::new()),
            "validate minimal docker release",
        );
        let release = config.release();
        assert_eq!(release.image(), Some("ghcr.io/example/app"));
        assert!(release.platforms().is_empty());
        let declared = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[release]\nenabled = true\nkind = \"docker\"\nimage = \"ghcr.io/example/app\"\ndockerfile = \"images/app/Dockerfile\"\ncontext = \"images/app\"\nplatforms = [\"linux/amd64\", \"linux/arm64\"]\n",
        );
        must(
            declared.validate(&[], &[], &BTreeSet::new()),
            "validate declared docker release",
        );
        assert_eq!(
            declared.release().dockerfile(),
            Some("images/app/Dockerfile")
        );
        assert_eq!(declared.release().context(), Some("images/app"));
        assert_eq!(
            declared.release().platforms(),
            &["linux/amd64".to_owned(), "linux/arm64".to_owned()]
        );
    }

    #[test]
    fn docker_release_rejects_unknown_platforms_and_escaping_paths() {
        for (name, release) in [
            (
                "platform",
                "kind = \"docker\"\nimage = \"ghcr.io/example/app\"\nplatforms = [\"linux/riscv64\"]\n",
            ),
            (
                "image",
                "kind = \"docker\"\nplatforms = [\"linux/amd64\"]\n",
            ),
            (
                "dockerfile",
                "kind = \"docker\"\nimage = \"ghcr.io/example/app\"\ndockerfile = \"../Dockerfile\"\n",
            ),
            (
                "context",
                "kind = \"docker\"\nimage = \"ghcr.io/example/app\"\ncontext = \"/tmp\"\n",
            ),
        ] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[release]\nenabled = true\n{release}"
            ));
            let error = must_fail(
                config.validate(&[], &[], &BTreeSet::new()),
                "an incomplete docker release must fail",
            );
            assert!(
                error.to_string().contains("[release] enabled repositories"),
                "unexpected error for {name}: {error}"
            );
        }
    }

    #[test]
    fn default_branch_outside_the_branch_alphabet_is_a_usage_error() {
        for branch in ["main' || '1' == '1", "main\n  injected: true", ""] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\ndefault_branch = {branch:?}\n"
            ));
            let error = must_fail(
                config.validate(&[], &[], &BTreeSet::new()),
                "injection-shaped default_branch must fail",
            );
            assert!(
                error.to_string().contains("[workflow] default_branch"),
                "unexpected error for {branch:?}: {error}"
            );
        }
        let config = config_for(
            "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\ndefault_branch = \"trunk\"\n",
        );
        must(
            config.validate(&[], &[], &BTreeSet::new()),
            "plain default_branch validates",
        );
    }

    #[test]
    fn valid_docker_image_accepts_references_and_rejects_injection() {
        for valid in [
            "ghcr.io/example/app",
            "ghcr.io/example/app:1.2.3",
            "registry.example:5000/team/app:latest",
            "example/app",
            "library",
        ] {
            assert!(valid_docker_image(valid), "{valid}");
        }
        for invalid in [
            "",
            "GHCR.IO/example/app",
            "ghcr.io/example/app with space",
            "ghcr.io/example/app\n  INJECTED: 1",
            "ghcr.io/example/app\"; touch /tmp/pwned; echo \"",
            "ghcr.io/example/app`id`",
            "ghcr.io/example/app$(id)",
            "ghcr.io/example/app@sha256:deadbeef",
            "ghcr.io//app",
            "/leading/slash",
            "trailing/slash/",
            "ghcr.io/.leading-dot",
            "host:notaport/app",
        ] {
            assert!(!valid_docker_image(invalid), "{invalid:?}");
        }
    }

    #[test]
    fn release_naming_outside_shell_safe_alphabets_is_a_usage_error() {
        for (name, release) in [
            (
                "target",
                "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\ntargets = ['x-unknown-linux-gnu\"; touch /tmp/pwned; echo \"-unknown-linux-gnu']\n",
            ),
            (
                "binary",
                "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\\\"; touch /tmp/pwned; echo \\\"\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n",
            ),
            (
                "package",
                "kind = \"rust-binary\"\npackage = \"example/pkg\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n",
            ),
            (
                "image_package",
                "kind = \"native\"\npackage = \"example\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\nimage = \"ghcr.io/example/app\"\nimage_package = \"example; touch /tmp/pwned\"\n",
            ),
            (
                "image",
                "kind = \"docker\"\nimage = \"ghcr.io/example/app\\n  INJECTED: 1\"\n",
            ),
        ] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[release]\nenabled = true\n{release}"
            ));
            let error = must_fail(
                config.validate(&[], &[], &BTreeSet::new()),
                "injection-shaped release naming must fail",
            );
            assert!(
                error.to_string().contains("[release]"),
                "unexpected error for {name}: {error}"
            );
        }
    }

    #[test]
    fn tarball_bindings_on_other_kinds_are_a_usage_error() {
        for (name, release) in [
            (
                "modes",
                "kind = \"docker\"\nimage = \"ghcr.io/example/app\"\nmodes = [\"validate\"]\n",
            ),
            (
                "producer",
                "kind = \"docker\"\nimage = \"ghcr.io/example/app\"\nproducer_workflow = \"build.yml\"\n",
            ),
            (
                "archive",
                "kind = \"pages\"\nartifact_path = \"dist\"\narchive_members = [\"app.tar.gz\"]\n",
            ),
            (
                "credential",
                "kind = \"crates\"\npackages = [\"example\"]\n[[release.credential]]\nname = \"signing\"\nsetup = \"setup-signing\"\nteardown = \"teardown-signing\"\n",
            ),
        ] {
            let config = config_for(&format!(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[release]\nenabled = true\n{release}"
            ));
            let error = must_fail(
                config.validate(&[], &[], &BTreeSet::new()),
                "tarball bindings on another kind must fail",
            );
            assert!(
                error.to_string().contains("render only for kind"),
                "unexpected error for {name}: {error}"
            );
        }
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

    #[test]
    fn renovate_lanes_key_is_rejected() {
        let error = must_some_error(
            toml::from_str::<RepoGenerationConfig>(
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [renovate]\nenabled = true\nreason = \"test\"\nlanes = \"velnor\"\n",
            )
            .err(),
            "the removed renovate lanes key must fail",
        );
        assert!(error.contains("unknown field"), "{error}");
    }

    fn check_profile_config(body: &str) -> String {
        format!("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n{body}")
    }

    #[test]
    fn check_profile_rows_validate_and_leave_canonical_shape_without_them() {
        let plain = config_for("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n");
        assert!(plain.check_profiles().is_empty());
        let canonical = must(
            plain.canonical_json(),
            "canonicalize config without profiles",
        );
        assert!(!canonical.contains("check_profile"), "{canonical}");

        let config = config_for(&check_profile_config(
            "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\n\n\
             [[check_profile]]\nid = \"load\"\nname = \"Load probe\"\nschedule = \"23 2 * * *\"\n\
             runner = \"velnor\"\ntasks = [\"check-load\"]\nneeds = [\"smoke\"]\n\
             timeout_minutes = 90\nartifacts = [\"load-results/\"]\nstatus = \"advisory\"\n\
             env = { MAX_SECONDS = \"300\" }\n\n\
             [workflow]\nproviders = [\"github-hosted\", \"velnor\"]\n\n\
             [workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n\
             [workflow.selectors.velnor]\nruns_on = [\"self-hosted\"]\n",
        ));
        assert_eq!(config.check_profiles().len(), 2);
        must(
            config.validate(&[], &[], &BTreeSet::new()),
            "two coherent profiles validate",
        );
    }

    #[test]
    fn check_profile_tools_require_pinned_lock_keys() {
        let body = "[[check_profile]]\nid = \"smoke\"\ntasks = [\"check-smoke\"]\ntools = [\"cargo-binstall\"]\n";
        let error = must_fail(
            config_for(&check_profile_config(body)).validate(&[], &[], &BTreeSet::new()),
            "check-profile tools without a lock must fail",
        );
        assert!(error.to_string().contains("requires mise.lock"), "{error}");

        must(
            config_for(&check_profile_config(body)).validate(&[], &[], &mixed_lock_keys()),
            "check-profile tools pinned by the lock must validate",
        );
    }

    #[test]
    fn check_profile_rejects_bad_identity_cadence_and_contract() {
        for (body, fragment) in [
            (
                "[[check_profile]]\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\n",
                "missing `id`",
            ),
            (
                "[[check_profile]]\nid = \"9smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\n",
                "must be a job id",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\n\n\
                 [[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\n",
                "declared twice",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"daily\"\ntasks = [\"check-smoke\"]\n",
                "5-field cron",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\nrunner = \"planetary\"\ntasks = [\"check-smoke\"]\n",
                "runner must be",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\n",
                "missing `tasks`",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke; rm -rf /\"]\n",
                "not a plain task reference",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\nneeds = [\"ghost\"]\n",
                "which no profile declares",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\nneeds = [\"smoke\"]\n",
                "needs itself",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\ntimeout_minutes = 0\n",
                "positive number of minutes",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\nstatus = \"optional\"\n",
                "must be `required` or `advisory`",
            ),
            (
                "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\nenv = { \"max seconds\" = \"300\" }\n",
                "not a threshold name",
            ),
        ] {
            let error = must_fail(
                config_for(&check_profile_config(body)).validate(&[], &[], &BTreeSet::new()),
                "an incoherent profile must fail validation",
            );
            assert!(
                error.to_string().contains(fragment),
                "expected `{fragment}`: {error}"
            );
        }
    }

    /// A schedule-less profile passes config validation: cron-lessness is
    /// decided per file in `select_profiles`, which refuses schedule-less
    /// profiles in cron files (`schedule_less_profile_in_cron_file_is_refused`).
    #[test]
    fn check_profile_without_schedule_passes_config_validation() {
        let config = config_for(&check_profile_config(
            "[[check_profile]]\nid = \"smoke\"\ntasks = [\"check-smoke\"]\n",
        ));
        must(
            config.validate(&[], &[], &BTreeSet::new()),
            "a schedule-less profile validates; the file decides the trigger",
        );
    }

    #[test]
    fn check_profile_velnor_runner_needs_a_provider_and_selector() {
        let github_only = config_for(&check_profile_config(
            "[[check_profile]]\nid = \"fleet\"\nschedule = \"23 2 * * *\"\nrunner = \"velnor\"\ntasks = [\"check-fleet\"]\n\n\
             [workflow]\nproviders = [\"github-hosted\"]\n\n\
             [workflow.selectors.velnor]\nruns_on = [\"self-hosted\"]\n",
        ));
        let error = must_fail(
            github_only.validate(&[], &[], &BTreeSet::new()),
            "a Velnor profile on a GitHub-only surface must fail",
        );
        assert!(
            error.to_string().contains("has no velnor provider"),
            "{error}"
        );

        let unrouted = config_for(&check_profile_config(
            "[[check_profile]]\nid = \"fleet\"\nschedule = \"23 2 * * *\"\nrunner = \"velnor\"\ntasks = [\"check-fleet\"]\n",
        ));
        let error = must_fail(
            unrouted.validate(&[], &[], &BTreeSet::new()),
            "a Velnor profile without a selector must fail",
        );
        assert!(
            error.to_string().contains("[workflow.selectors.velnor]"),
            "{error}"
        );
    }

    #[test]
    fn check_profile_tools_match_lock_keys_by_exact_spelling() {
        let keys = BTreeSet::from(["ripgrep".to_owned(), "cargo:example-tool".to_owned()]);
        let pinned = config_for(&check_profile_config(
            "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\ntools = [\"ripgrep\"]\n",
        ));
        must(
            pinned.validate(&[], &[], &keys),
            "a lock-pinned tool validates",
        );
        let unpinned = config_for(&check_profile_config(
            "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\ntools = [\"cargo:example-missing\"]\n",
        ));
        let error = must_fail(
            unpinned.validate(&[], &[], &keys),
            "an unpinned tool must fail",
        );
        assert!(
            error.to_string().contains("mise.lock does not pin"),
            "{error}"
        );
        let unshaped = config_for(&check_profile_config(
            "[[check_profile]]\nid = \"smoke\"\nschedule = \"23 2 * * *\"\ntasks = [\"check-smoke\"]\ntools = [\"ripgrep; evil\"]\n",
        ));
        let error = must_fail(
            unshaped.validate(&[], &[], &BTreeSet::new()),
            "a shell-shaped tool must fail without a lock",
        );
        assert!(error.to_string().contains("not a plain tool id"), "{error}");
    }

    const DOCS_DECLARE: &str = "[[declare]]\nprimitive = \"docs-site\"\nfile = \"docs.yml\"\n";

    fn docs_config_text(body: &str) -> String {
        format!(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n[docs]\nenabled = true\n{body}\n{DOCS_DECLARE}",
        )
    }

    fn validate_docs_text(body: &str) -> Result<(), crate::s2::GeneratorError> {
        let root = scanned_root("docs-validate-config");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let outcome =
            config_for(&docs_config_text(body)).validate(&unit_ids, &[], &BTreeSet::new());
        let _ = fs::remove_dir_all(root);
        outcome
    }

    #[test]
    fn docs_enabled_requires_declare_row() {
        let root = scanned_root("docs-declare-config");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let error = must_fail(
            config_for(
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [docs]\nenabled = true\nreason = \"test\"\nsite_url = \"https://docs.example.com\"\n\
                 site_dir = \"site\"\nbuild_commands = [\"mise run docs:build\"]\n\
                 spell_commands = [\"mise run docs:spell\"]\n",
            )
            .validate(&unit_ids, &[], &BTreeSet::new()),
            "enabled docs without declare must fail",
        );
        assert!(
            error
                .to_string()
                .contains("[[declare]] primitive = \"docs-site\""),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn docs_enabled_requires_reason_address_build_and_a_check() {
        let without_reason = must_fail(
            validate_docs_text(
                "site_url = \"https://docs.example.com\"\nsite_dir = \"site\"\n\
                 build_commands = [\"mise run docs:build\"]\nspell_commands = [\"mise run docs:spell\"]\n",
            ),
            "enabled docs without reason must fail",
        );
        assert!(
            without_reason.to_string().contains("reason"),
            "{without_reason}"
        );
        let without_check = must_fail(
            validate_docs_text(
                "reason = \"test\"\nsite_url = \"https://docs.example.com\"\nsite_dir = \"site\"\n\
                 build_commands = [\"mise run docs:build\"]\n",
            ),
            "enabled docs without a local check must fail",
        );
        assert!(
            without_check
                .to_string()
                .contains("at least one local check"),
            "{without_check}"
        );
        let bad_url = must_fail(
            validate_docs_text(
                "reason = \"test\"\nsite_url = \"http://docs.example.com\"\nsite_dir = \"site\"\n\
                 build_commands = [\"mise run docs:build\"]\nspell_commands = [\"mise run docs:spell\"]\n",
            ),
            "enabled docs with a non-https site_url must fail",
        );
        assert!(bad_url.to_string().contains("site_url"), "{bad_url}");
    }

    #[test]
    fn docs_schedule_and_external_check_come_together() {
        let external_without_schedule = must_fail(
            validate_docs_text(
                "reason = \"test\"\nsite_url = \"https://docs.example.com\"\nsite_dir = \"site\"\n\
                 build_commands = [\"mise run docs:build\"]\nspell_commands = [\"mise run docs:spell\"]\n\
                 external_link_commands = [\"mise run docs:check-live\"]\n",
            ),
            "external links without a schedule must fail",
        );
        assert!(
            external_without_schedule.to_string().contains("schedule"),
            "{external_without_schedule}"
        );
        let schedule_without_external = must_fail(
            validate_docs_text(
                "reason = \"test\"\nsite_url = \"https://docs.example.com\"\nsite_dir = \"site\"\n\
                 schedule = \"17 4 * * *\"\nbuild_commands = [\"mise run docs:build\"]\n\
                 spell_commands = [\"mise run docs:spell\"]\n",
            ),
            "a schedule without external links must fail",
        );
        assert!(
            schedule_without_external
                .to_string()
                .contains("without external_link_commands"),
            "{schedule_without_external}"
        );
    }

    #[test]
    fn docs_complete_contract_validates() {
        must(
            validate_docs_text(
                "reason = \"test\"\nsite_url = \"https://docs.example.com\"\nsite_dir = \"site\"\n\
                 schedule = \"17 4 * * *\"\nbuild_commands = [\"mise run docs:build\"]\n\
                 spell_commands = [\"mise run docs:spell\"]\n\
                 external_link_commands = [\"mise run docs:check-live\"]\n",
            ),
            "a complete docs contract validates",
        );
    }

    #[test]
    fn docs_unknown_field_fails_closed() {
        let error = must_some_error(
            toml::from_str::<RepoGenerationConfig>(
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [docs]\nenabled = true\nreason = \"test\"\nunknown = true\n",
            )
            .err(),
            "unknown docs field must fail",
        );
        assert!(error.contains("unknown field"), "{error}");
    }

    /// Release binding shapes fail closed whether or not the contract is
    /// enabled: a typo'd mode or an unpaired credential must never render
    /// a silently weaker lane.
    #[test]
    fn release_binding_shapes_fail_closed() {
        let root = scanned_root("release-bindings-validate");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        for (name, release, expected) in [
            (
                "publish-mode",
                "[release]\nmodes = [\"publish\"]\n",
                "never a dispatch option",
            ),
            (
                "bad-mode",
                "[release]\nmodes = [\"ship\"]\n",
                "must be one of",
            ),
            (
                "bad-conclusion",
                "[release]\nproducer_workflow = \"CI\"\nproducer_conclusion = \"completed\"\n",
                "must be `success`",
            ),
            (
                "lonely-conclusion",
                "[release]\nproducer_conclusion = \"success\"\n",
                "needs producer_workflow",
            ),
            (
                "bad-checksum",
                "[release]\narchive_checksum = \"sha512\"\n",
                "must be `sha256`",
            ),
            (
                "bad-retention",
                "[release]\narchive_retention_days = 0\n",
                "must be 1-90",
            ),
            (
                "bad-member",
                "[release]\narchive_members = [\"sub/dir\"]\n",
                "portable file names",
            ),
            (
                "nameless-credential",
                "[release]\n[[release.credential]]\nsetup = \"mount\"\nteardown = \"unmount\"\n",
                "needs `name`",
            ),
            (
                "setup-without-teardown",
                "[release]\n[[release.credential]]\nname = \"store\"\nsetup = \"mount\"\n",
                "needs `teardown`",
            ),
        ] {
            let error = must_fail(
                config_for(&format!(
                    "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n{release}"
                ))
                .validate(&unit_ids, &[], &BTreeSet::new()),
                name,
            );
            assert!(
                error.to_string().contains(expected),
                "`{name}` must name the problem: {error}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    /// A complete binding contract parses and validates: producer, modes,
    /// archive members, retention, and paired credentials.
    #[test]
    fn release_bindings_parse_and_validate() {
        let root = scanned_root("release-bindings-parse");
        let shape = shape_for(&root);
        let unit_ids = shape.unit_ids().map(str::to_owned).collect::<Vec<_>>();
        let parsed = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [release]\nenabled = true\nkind = \"rust-binary\"\npackage = \"example\"\n\
             binary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n\
             producer_workflow = \"CI\"\nproducer_conclusion = \"success\"\n\
             modes = [\"validate\", \"build\", \"rehearse\"]\n\
             archive_members = [\"example-role\"]\narchive_checksum = \"sha256\"\n\
             archive_retention_days = 14\n\
             [[release.credential]]\nname = \"store\"\nsetup = \"mount\"\nteardown = \"unmount\"\n",
        );
        must(
            parsed.validate(&unit_ids, &[], &BTreeSet::new()),
            "complete bindings must validate",
        );
        let release = parsed.release();
        assert_eq!(release.producer_workflow(), Some("CI"));
        assert_eq!(
            release.modes(),
            &[
                "validate".to_owned(),
                "build".to_owned(),
                "rehearse".to_owned()
            ]
        );
        assert_eq!(release.archive_members(), &["example-role".to_owned()]);
        assert_eq!(release.archive_retention_days(), Some(14));
        assert_eq!(release.credentials().len(), 1);
        assert_eq!(release.credentials()[0].teardown(), Some("unmount"));
        let _ = fs::remove_dir_all(root);
    }

    /// The `[release]` registry triple: a complete docker triple validates;
    /// partial triples, bad shapes, and non-docker triples fail closed
    /// naming the gap.
    #[test]
    fn release_registry_auth_must_be_a_complete_docker_triple() {
        let base = "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n[release]\nenabled = true\n";
        let contract = "kind = \"docker\"\nimage = \"example/app\"\nplatforms = [\"linux/amd64\", \"linux/arm64\"]\n";
        let triple = "registry = \"docker.io\"\nregistry_username_secret = \"REGISTRY_USERNAME\"\nregistry_password_secret = \"REGISTRY_PASSWORD\"\n";
        let valid = config_for(&format!("{base}{contract}{triple}"));
        must(
            valid.validate(&[], &[], &BTreeSet::new()),
            "a complete docker triple must validate",
        );
        assert_eq!(valid.release().registry(), Some("docker.io"));
        assert_eq!(
            valid.release().registry_username_secret(),
            Some("REGISTRY_USERNAME")
        );
        assert_eq!(
            valid.release().registry_password_secret(),
            Some("REGISTRY_PASSWORD")
        );
        let binary_contract = "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n";
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "registry without secrets",
                format!("{base}{contract}registry = \"docker.io\"\n"),
                "[release] registry_username_secret is missing",
            ),
            (
                "uppercase host",
                format!(
                    "{base}{contract}{}",
                    triple.replace("docker.io", "Docker.io")
                ),
                "[release] registry must be a lowercase host",
            ),
            (
                "lowercase secret",
                format!(
                    "{base}{contract}{}",
                    triple.replace("REGISTRY_USERNAME", "registry_username")
                ),
                "must be an uppercase secret name",
            ),
            (
                "automatic token as password",
                format!(
                    "{base}{contract}{}",
                    triple.replace("REGISTRY_PASSWORD", "GITHUB_TOKEN")
                ),
                "must name a dedicated secret, not GITHUB_TOKEN",
            ),
            (
                "triple on a binary publisher",
                format!("{base}{binary_contract}{triple}"),
                "renders only for kind `docker`",
            ),
            (
                "triple without a publisher",
                format!("{base}{triple}"),
                "needs `kind = \"docker\"`",
            ),
        ];
        for (name, text, expected) in cases {
            let parsed = config_for(&text);
            let error = must_fail(
                parsed.validate(&[], &[], &BTreeSet::new()),
                &format!("{name} must fail"),
            );
            assert!(
                error.to_string().contains(expected),
                "{name} must name the problem: {error}"
            );
        }
    }

    /// The smallest config that enables Renovate: trusted Velnor runners plus
    /// the writer declare row. Tests append contract fields to `[renovate]`.
    const RENOVATE_CONTRACT_CONFIG: &str = "schema = 2\n\n\
         [generator]\n\
         repository = \"example/fixture\"\n\n\
         [workflow]\n\
         providers = [\"velnor\"]\n\n\
         [workflow.selectors.velnor]\n\
         runs_on = [\"self-hosted\", \"example-lane\"]\n\n\
         [renovate]\n\
         enabled = true\n\
         reason = \"Self-hosted Renovate for repository dependencies.\"\n\n\
         [[declare]]\n\
         primitive = \"renovate\"\n\
         file = \"renovate.yml\"\n";

    fn validate_renovate_contract(extra: &str) -> Result<RepoGenerationConfig, GeneratorError> {
        let text = RENOVATE_CONTRACT_CONFIG.replace(
            "reason = \"Self-hosted Renovate for repository dependencies.\"\n",
            &format!("reason = \"Self-hosted Renovate for repository dependencies.\"\n{extra}"),
        );
        let config = config_for(&text);
        config.validate(&[], &[], &BTreeSet::new()).map(|()| config)
    }

    #[test]
    fn renovate_repository_targets_must_be_slugs() {
        for repository in ["example/fixture", "Example-Name.Slug_1/repo.name-2", "a/b"] {
            must(
                validate_renovate_repository_target(repository),
                "valid repository target",
            );
        }
        for repository in [
            "noslash",
            "/empty-owner",
            "empty-name/",
            "three/slashes/here",
            "a b/c",
            "a/../c",
            ".hidden/repo",
            "owner/repo.git",
        ] {
            let error = must_fail(
                validate_renovate_repository_target(repository),
                "invalid repository target must fail",
            );
            assert!(
                error
                    .to_string()
                    .contains("must be `owner/repository` slugs"),
                "{error}"
            );
        }
        must(
            validate_renovate_contract("repositories = [\"example/one\", \"example/two\"]\n"),
            "declared repository targets",
        );
        let error = must_fail(
            validate_renovate_contract("repositories = [\"bogus\"]\n"),
            "a non-slug repository target must fail",
        );
        assert!(
            error
                .to_string()
                .contains("must be `owner/repository` slugs"),
            "{error}"
        );
        let error = must_fail(
            validate_renovate_contract("repositories = [\"example/one\", \"example/one\"]\n"),
            "a duplicated repository target must fail",
        );
        assert!(
            error.to_string().contains("names `example/one` twice"),
            "{error}"
        );
    }

    #[test]
    fn renovate_git_author_must_name_an_email() {
        for author in ["Renovate Bot <bot@example.com>", "R <r@example.co>"] {
            must(validate_renovate_git_author(author), "valid git author");
        }
        for author in [
            "no-email",
            "Renovate Bot",
            "Renovate Bot <>",
            "<bot@example.com>",
            "Renovate Bot <not-an-email>",
            "Renovate Bot <bot@host>",
            "Renovate Bot <bot @example.com>",
            "Renovate\nBot <bot@example.com>",
        ] {
            let error = must_fail(
                validate_renovate_git_author(author),
                "invalid git author must fail",
            );
            assert!(
                error.to_string().contains("must be `Name <email>`"),
                "{error}"
            );
        }
    }

    #[test]
    fn renovate_signoff_requires_an_author() {
        let error = must_fail(
            validate_renovate_contract("signoff = true\n"),
            "signoff without an author must fail",
        );
        assert!(
            error
                .to_string()
                .contains("signoff = true requires `author`"),
            "{error}"
        );
        must(
            validate_renovate_contract(
                "author = \"Renovate Bot <bot@example.com>\"\nsignoff = true\n",
            ),
            "signoff with an author",
        );
    }

    #[test]
    fn renovate_allowed_commands_must_be_single_line_patterns() {
        for command in ["^npm install --package-lock-only$", "^echo "] {
            must(
                validate_renovate_allowed_command(command),
                "valid allowed command",
            );
        }
        for command in ["", "first\nsecond", "null\0byte"] {
            let error = must_fail(
                validate_renovate_allowed_command(command),
                "invalid allowed command must fail",
            );
            assert!(
                error
                    .to_string()
                    .contains("must be single-line command patterns"),
                "{error}"
            );
        }
        let error = must_fail(
            validate_renovate_contract("allowed_commands = [\"^echo \", \"^echo \"]\n"),
            "a duplicated allowed command must fail",
        );
        assert!(error.to_string().contains("twice"), "{error}");
    }

    #[test]
    fn renovate_extra_schedules_must_be_unique_crons() {
        must(
            validate_renovate_contract("schedules = [\"0 18 * * *\"]\n"),
            "a valid extra schedule",
        );
        let error = must_fail(
            validate_renovate_contract("schedules = [\"hourly\"]\n"),
            "a non-cron extra schedule must fail",
        );
        assert!(
            error
                .to_string()
                .contains("must be a 5-field cron expression"),
            "{error}"
        );
        let error = must_fail(
            validate_renovate_contract("schedules = [\"0 18 * * *\", \"0 18 * * *\"]\n"),
            "a duplicated extra schedule must fail",
        );
        assert!(error.to_string().contains("twice"), "{error}");
        let error = must_fail(
            validate_renovate_contract("schedule = \"0 7 * * *\"\nschedules = [\"0 7 * * *\"]\n"),
            "an extra schedule repeating the primary must fail",
        );
        assert!(
            error.to_string().contains("repeats the primary schedule"),
            "{error}"
        );
    }

    #[test]
    fn renovate_host_rules_secret_must_be_a_secret_name() {
        must(
            validate_renovate_contract("host_rules_secret = \"RENOVATE_HOST_RULES_JSON\"\n"),
            "a valid host-rules secret name",
        );
        for secret in ["GITHUB_TOKEN", "lowercase", ""] {
            let error = must_fail(
                validate_renovate_contract(&format!("host_rules_secret = \"{secret}\"\n")),
                "an invalid host-rules secret name must fail",
            );
            assert!(
                error.to_string().contains("[renovate] host_rules_secret"),
                "{error}"
            );
        }
    }

    #[test]
    fn maintenance_overrides_must_be_shaped() {
        let valid = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [maintenance]\nschedule = \"17 4 * * *\"\nproducers = [\"ci-main.yml\"]\nmax_deletes = 50\n",
        );
        must(
            valid.validate(&[], &[], &BTreeSet::new()),
            "valid maintenance overrides",
        );
        for (name, body) in [
            ("cron", "schedule = \"hourly\"\n"),
            (
                "empty producers",
                "schedule = \"17 4 * * *\"\nproducers = []\n",
            ),
            ("producer path", "producers = [\"../ci-main.yml\"]\n"),
            ("producer extension", "producers = [\"ci-main.yaml\"]\n"),
            ("producer bare", "producers = [\"ci-main\"]\n"),
            (
                "duplicate producers",
                "producers = [\"ci-main.yml\", \"ci-main.yml\"]\n",
            ),
            ("zero bound", "max_deletes = 0\n"),
            ("oversized bound", "max_deletes = 5001\n"),
        ] {
            let config = config_for(&format!(
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n[maintenance]\n{body}"
            ));
            let error = must_fail(
                config.validate(&[], &[], &BTreeSet::new()),
                "invalid maintenance override must fail",
            );
            assert!(
                error.to_string().contains("[maintenance]"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn maintenance_unknown_field_fails_closed() {
        let error = must_some_error(
            toml::from_str::<RepoGenerationConfig>(
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [maintenance]\nschedule = \"17 4 * * *\"\nunknown = true\n",
            )
            .err(),
            "unknown maintenance field must fail",
        );
        assert!(error.contains("unknown field"), "{error}");
    }

    #[test]
    fn policy_dco_requires_the_external_dco_check() {
        let enforced = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [policy]\ndco_required = true\nruleset_external_status_checks = [\"DCO\"]\n",
        );
        must(
            enforced.validate(&[], &[], &BTreeSet::new()),
            "DCO required with the DCO check",
        );
        let unenforced = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [policy]\ndco_required = true\n",
        );
        let error = must_fail(
            unenforced.validate(&[], &[], &BTreeSet::new()),
            "DCO required without the DCO check must fail",
        );
        assert!(
            error
                .to_string()
                .contains("requires `DCO` in ruleset_external_status_checks"),
            "{error}"
        );
    }

    #[test]
    fn policy_action_pin_admission_names_the_reviewed_allowlist() {
        let admitted = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [policy]\naction_pin_admission = \"reviewed-allowlist\"\n",
        );
        must(
            admitted.validate(&[], &[], &BTreeSet::new()),
            "the reviewed allowlist admission",
        );
        let invented = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [policy]\naction_pin_admission = \"strict\"\n",
        );
        let error = must_fail(
            invented.validate(&[], &[], &BTreeSet::new()),
            "an unimplemented admission must fail",
        );
        assert!(
            error.to_string().contains("must be `reviewed-allowlist`"),
            "{error}"
        );
    }

    fn product_row(outputs: Option<Vec<String>>) -> UnitSection {
        UnitSection {
            products: vec![ProductSection {
                name: Some("xcframework".to_owned()),
                task: Some("build-xcframework".to_owned()),
                env: None,
                outputs,
                inputs: None,
            }],
            ..UnitSection::default()
        }
    }

    #[test]
    fn named_products_carries_declared_outputs() {
        let products = must(
            product_row(Some(vec!["native/out/lib.xcframework".to_owned()]))
                .named_products("rust-ffi"),
            "declared outputs parse",
        );
        assert_eq!(
            products[0].outputs,
            vec!["native/out/lib.xcframework".to_owned()]
        );
    }

    #[test]
    fn named_products_carries_declared_inputs() {
        let mut row = product_row(None);
        row.products[0].inputs = Some(vec![
            "libs/ffi/**/*.rs".to_owned(),
            "libs/ffi/boltffi.toml".to_owned(),
        ]);
        let products = must(row.named_products("rust-ffi"), "declared inputs parse");
        assert_eq!(
            products[0].inputs,
            vec![
                "libs/ffi/**/*.rs".to_owned(),
                "libs/ffi/boltffi.toml".to_owned(),
            ]
        );
        assert!(products[0].inputs_unknown.is_empty());
    }

    #[test]
    fn named_products_rejects_escaping_input() {
        let mut row = product_row(None);
        row.products[0].inputs = Some(vec!["../escape/**".to_owned()]);
        let error = must_fail(row.named_products("rust-ffi"), "escaping input fails");
        assert!(
            error
                .to_string()
                .contains("not a repo-relative path or glob"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn named_products_rejects_escaping_output() {
        let error = must_fail(
            product_row(Some(vec!["../escape/lib.a".to_owned()])).named_products("rust-ffi"),
            "an escaping output must fail",
        );
        assert!(error.to_string().contains("normal form"), "{error}");
    }
}

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

mod canonical;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{content_digest_bytes, GeneratorError};

/// Location of the repository-owned generation config, relative to the
/// repository root.
pub(crate) const GENERATION_CONFIG_PATH: &str = ".github-gen/velnor-workflow.toml";

/// The only accepted `schema` value. Rejecting every other value keeps the
/// config contract explicit instead of guessing at future layouts.
const CONFIG_SCHEMA: i64 = 1;

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

fn parse(path: &Path, bytes: &[u8]) -> Result<RepoGenerationConfig, GeneratorError> {
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
    units: Vec<UnitSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    static_files: Vec<StaticFileSection>,
    #[serde(default)]
    declare: Vec<DeclareRow>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneratorSection {
    /// `owner/repository` slug this config belongs to.
    repository: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowSection {
    /// GitHub-hosted runner label for hosted lanes.
    github_runner: Option<String>,
    /// Generated runner lanes. Absent keeps the generator's current default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runners: Option<String>,
    /// Velnor runner labels for self-hosted lanes. A surface that renders
    /// self-hosted jobs without them is a configuration error, never an empty
    /// `runs-on`.
    velnor_labels: Option<Vec<String>>,
    /// Velnor runner group for self-hosted lanes.
    velnor_runner_group: Option<String>,
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
    github_pr_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    github_full_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    velnor_pr_commands: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    velnor_full_commands: Option<Vec<String>>,
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
    /// Admission rule for actions that are not pinned to a full commit SHA.
    action_pin_admission: Option<String>,
    /// Emit `config-variables: null` in the generated actionlint config.
    actionlint_config_variables_null: Option<bool>,
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

    pub(crate) fn github_pr_commands(&self) -> Option<&[String]> {
        self.github_pr_commands.as_deref()
    }

    pub(crate) fn github_full_commands(&self) -> Option<&[String]> {
        self.github_full_commands.as_deref()
    }

    pub(crate) fn velnor_pr_commands(&self) -> Option<&[String]> {
        self.velnor_pr_commands.as_deref()
    }

    pub(crate) fn velnor_full_commands(&self) -> Option<&[String]> {
        self.velnor_full_commands.as_deref()
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
}

impl UnitCacheSection {
    pub(crate) fn key_files(&self) -> Option<&[String]> {
        self.key_files.as_deref()
    }

    pub(crate) fn paths(&self) -> Option<&[String]> {
        self.paths.as_deref()
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

    /// The GitHub-hosted runner label, when the config declares one.
    pub(crate) fn github_runner(&self) -> Option<&str> {
        self.workflow.github_runner.as_deref()
    }

    /// The declared generated runner lanes, if any.
    pub(crate) fn runners(&self) -> Option<&str> {
        self.workflow.runners.as_deref()
    }

    /// The declared self-hosted runner labels.
    pub(crate) fn velnor_labels(&self) -> Option<&[String]> {
        self.workflow.velnor_labels.as_deref()
    }

    /// The declared self-hosted runner group.
    pub(crate) fn velnor_runner_group(&self) -> Option<&str> {
        self.workflow.velnor_runner_group.as_deref()
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

    /// The declared unit rows, in the order the config declares them.
    pub(crate) fn units(&self) -> &[UnitSection] {
        &self.units
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
        validate_units(&self.units)?;
        validate_unit_references(
            &self.units,
            self.workflow.version_bump_units.as_deref(),
            unit_ids,
        )?;
        validate_static_files(&self.static_files)?;
        self.validate_release()?;
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
    if workflow.github_runner.as_deref().is_some_and(str::is_empty) {
        return Err(GeneratorError::usage(
            "[workflow] github_runner must not be empty",
        ));
    }
    if let Some(runners) = workflow.runners.as_deref()
        && !matches!(runners, "github" | "velnor" | "both")
    {
        return Err(GeneratorError::usage(format!(
            "[workflow] runners must be one of: github, velnor, both; found `{runners}`"
        )));
    }
    if let Some(labels) = &workflow.velnor_labels {
        if labels.is_empty() {
            return Err(GeneratorError::usage(
                "[workflow] velnor_labels must not be empty",
            ));
        }
        if labels.iter().any(String::is_empty) {
            return Err(GeneratorError::usage(
                "[workflow] velnor_labels must not contain empty labels",
            ));
        }
    }
    if workflow
        .velnor_runner_group
        .as_deref()
        .is_some_and(str::is_empty)
    {
        return Err(GeneratorError::usage(
            "[workflow] velnor_runner_group must not be empty",
        ));
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

/// Unit rows either override a scanned unit by id or add one. Two rows for one
/// id would make the effective contract depend on which one the reader trusts,
/// so the second row is refused instead of merged.
fn validate_units(units: &[UnitSection]) -> Result<(), GeneratorError> {
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
        if let Some(cache) = &row.cache
            && (cache.key_files.as_ref().is_none_or(std::vec::Vec::is_empty)
                || cache.paths.as_ref().is_none_or(std::vec::Vec::is_empty))
        {
            return Err(GeneratorError::usage(format!(
                    "[[unit]] {id} declares `[unit.cache]` without both `key_files` and `paths`; a partial cache contract cannot be keyed"
                )));
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
const RELEASE_KINDS: &[&str] = &["crates", "rust-binary", "pages"];

/// A `kind` the renderer does not implement has no rendered `release.yml`: it
/// is accepted only from a repository that renders its own publisher verbatim
/// as a `static-workflow` row, and is a configuration error anywhere else.
impl RepoGenerationConfig {
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
        must(
            crate::scan::scan_shape(root, crate::RunnerMode::Both, "main", &[]),
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
            "schema = 1\n\
             \n\
             [generator]\n\
             repository = \"example/fixture\"\n\
             \n\
             [workflow]\n\
             github_runner = \"ubuntu-24.04\"\n\
             velnor_labels = [\"self-hosted\", \"example-runner-label\"]\n\
             velnor_runner_group = \"example-runner-group\"\n\
             default_branch = \"trunk\"\n\
             \n\
             [scan]\n\
             exclude = [\"fleet/**\", \"docs/**\"]\n\
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
            config.validate(&unit_ids, &package_update_blocks()),
            "validate full config",
        );
        assert_eq!(config.schema, Some(1));
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
    fn workflow_runners_accepts_lowercase_modes() {
        for runners in ["github", "velnor", "both"] {
            let config = config_for(&format!(
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nrunners = \"{runners}\"\n"
            ));
            assert_eq!(config.runners(), Some(runners));
            must(
                config.validate(&[], &[]),
                "validate accepted workflow runner mode",
            );
        }
    }

    #[test]
    fn workflow_runners_is_optional_without_changing_canonical_shape() {
        let config = config_for("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n");
        assert_eq!(config.runners(), None);
        let canonical = must(
            config.canonical_json(),
            "canonicalize default workflow config",
        );
        assert!(!canonical.contains("\"runners\""), "{canonical}");
    }

    #[test]
    fn workflow_runners_rejects_unknown_or_non_lowercase_modes() {
        for runners in ["GitHub", "VELNOR", "Both", "hosted"] {
            let config = config_for(&format!(
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nrunners = \"{runners}\"\n"
            ));
            let error = must_fail(
                config.validate(&[], &[]),
                "invalid workflow runner mode must fail validation",
            );
            assert!(
                error
                    .to_string()
                    .contains("[workflow] runners must be one of"),
                "unexpected error for {runners}: {error}"
            );
        }
    }

    #[test]
    fn workflow_runners_keeps_unknown_fields_denied() {
        let error = must_fail(
            toml::from_str::<RepoGenerationConfig>(
                "schema = 1\n\n[workflow]\nrunner = \"velnor\"\n",
            ),
            "unknown workflow fields must be rejected",
        );
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn declaration_order_is_part_of_the_digest() {
        let leading = "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                       [[declare]]\nprimitive = \"a\"\nfile = \"a.yml\"\n\n\
                       [[declare]]\nprimitive = \"b\"\nfile = \"b.yml\"\n";
        let trailing = "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
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
    fn schema_must_be_exactly_one() {
        let root = scanned_root("schema");
        let path = root.join(GENERATION_CONFIG_PATH);
        must(
            fs::create_dir_all(path.parent().unwrap_or(&root)),
            "create config dir",
        );
        must(
            fs::write(
                &path,
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n",
            ),
            "write future schema",
        );
        let error = must_some_error(load(&path).err(), "future schema must fail");
        assert!(
            error.contains("schema 2"),
            "error must name the schema: {error}"
        );
        must(
            fs::write(&path, "[generator]\nrepository = \"example/fixture\"\n"),
            "write config without schema",
        );
        let missing = must_some_error(load(&path).err(), "missing schema must fail");
        assert!(
            missing.contains("missing `schema = 1`"),
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
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[declare]]\nprimitive = \"rust-crate\"\nunits = [\"not-a-unit\"]\nfile = \"rust.yml\"\n",
        );
        let error = must_some_error(
            config.validate(&unit_ids, &package_update_blocks()).err(),
            "unknown unit must fail",
        );
        assert!(error.contains("not-a-unit"), "error names the row: {error}");
        assert!(
            error.contains(&format!("available units: {available}")),
            "error lists the scanned units: {error}"
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
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [[declare]]\nprimitive = \"rust-crate\"\nfile = \"{file}\"\n"
            ));
            let error = must_some_error(
                config.validate(&unit_ids, &package_update_blocks()).err(),
                "declared file must fail validation",
            );
            assert!(
                error.contains("bare `.yml` workflow file name"),
                "{file} must be rejected: {error}"
            );
        }
        let separator = config_for(concat!(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n",
            "[[declare]]\nprimitive = \"rust-crate\"\nfile = 'back\\slash.yml'\n",
        ));
        let error = must_some_error(
            separator
                .validate(&unit_ids, &package_update_blocks())
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
                "schema = 1\n\n[generator]\nrepository = \"{repository}\"\n"
            ));
            let error = must_some_error(
                config.validate(&unit_ids, &package_update_blocks()).err(),
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
        let config = config_for("schema = 1\n");
        let error = must_some_error(
            config.validate(&unit_ids, &package_update_blocks()).err(),
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
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[declare]]\nprimitive = \"rust-crate\"\nfile = \"rust.yml\"\n\
             [declare.args]\nanything = { goes = [\"here\", 3, true] }\n",
        );
        must(
            opaque.validate(&unit_ids, &package_update_blocks()),
            "opaque args validate",
        );
        let float = config_for(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
             [[declare]]\nprimitive = \"rust-crate\"\nfile = \"rust.yml\"\n\
             [declare.args]\nratio = 1.5\n",
        );
        let error = must_some_error(
            float.validate(&unit_ids, &package_update_blocks()).err(),
            "float args must fail",
        );
        assert!(error.contains("float"), "{error}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error = match toml::from_str::<RepoGenerationConfig>(
            "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
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
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [workflow]\npackage_update_channels = {{{grants}}}\n"
            ));
            let error = must_some_error(
                config.validate(&[], &blocks).err(),
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
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n\
                 [workflow]\npackage_update_channels = {{{grants}}}\n"
            ));
            must(config.validate(&[], &blocks), "channel grants validate");
        }
    }

    #[test]
    fn absent_config_digests_the_empty_canonical_form() {
        let empty = must(
            config_for("schema = 1\n").canonical_json(),
            "canonicalize minimal",
        );
        assert_ne!(empty, canonical::EMPTY_CANONICAL_FORM);
        assert_ne!(
            must(RepoGenerationConfig::digest(None), "digest absent config"),
            must(
                RepoGenerationConfig::digest(Some(&config_for("schema = 1\n"))),
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
                "schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n",
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
}

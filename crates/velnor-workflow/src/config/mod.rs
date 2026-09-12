//! Repository-owned generation config: the explicit input half of generation.
//!
//! Generation is a pure function of the repository shape, this config, and the
//! generator revision. The config lives in the target repository at
//! `.github-gen/velnor-workflow.toml`, is optional (a repository without one
//! keeps the generator's default behavior), and is fail-closed: a config that
//! fails to parse or validate stops generation instead of being ignored.
//!
//! This phase only carries and digests the config. It does not yet feed the
//! renderer: policy overrides and declared primitives are consumed by later
//! phases, so a value that parses and validates can never change generated
//! bytes here.

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
    /// Velnor runner labels for self-hosted lanes.
    velnor_labels: Option<Vec<String>>,
    /// Velnor runner group for self-hosted lanes.
    velnor_runner_group: Option<String>,
    /// Per-owner-block update channel grants for the rendered
    /// `package-update.yml` matrix; the `default` key covers owner blocks
    /// without their own row. Absent from the canonical form, so configs that
    /// do not use it keep their recorded digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_update_channels: Option<BTreeMap<String, Vec<String>>>,
    /// Overrides the resolved default branch used for branch gates.
    default_branch: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanSection {
    /// Repository paths the scan must ignore. The list is consumed when
    /// declared primitives land; it is carried and digested until then.
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
        for row in &self.declare {
            validate_declare_row(row, unit_ids)?;
        }
        validate_excludes(&self.scan.exclude)?;
        validate_package_update_channels(
            self.workflow.package_update_channels.as_ref(),
            package_update_blocks,
        )?;
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
        root
    }

    fn shape_for(root: &Path) -> crate::scan::RepositoryShape {
        must(
            crate::scan::scan_shape(root, crate::RunnerMode::Both, "main"),
            "scan config test repository",
        )
    }

    fn config_for(text: &str) -> RepoGenerationConfig {
        must(
            toml::from_str::<RepoGenerationConfig>(text),
            "parse config under test",
        )
    }

    /// The owner blocks the rendered `package-update.yml` really declares, so
    /// the channel-grant rules are tested against the render surface itself.
    fn package_update_blocks() -> Vec<&'static str> {
        crate::apt_package_update_owner_blocks(crate::APT_PACKAGE_UPDATE_WORKFLOW_TEMPLATE)
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

//! Rust replacement for the generated CI, policy, and release shell helpers.
//!
//! The generator still records project commands as data in `project.toml`.
//! This module owns parsing, dependency ordering, affected-unit selection,
//! policy validation, and release packaging. Project commands are the only
//! shell boundary left: they are explicit repository inputs and run under the
//! same Bash contract GitHub Actions provides for `run` steps.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};

use sha2::{Digest, Sha256};

use super::GeneratorError;

const DEFAULT_CONFIG: &str = ".github/ci/project.toml";
const IMMUTABLE_POLICY_WORKFLOW: &str =
    "tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml";
const TRUSTED_POLICY_REVISION_ENV: &str = "VELNOR_WORKFLOW_POLICY_REVISION";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

#[expect(
    dead_code,
    reason = "runtime preserves the complete generated TOML contract while commands consume only their relevant fields"
)]
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CiConfig {
    schema: u32,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    profile: String,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    default_branch: String,
    #[serde(default)]
    runners: String,
    #[serde(default)]
    analysis: Analysis,
    #[serde(default)]
    workflow: Workflow,
    #[serde(default)]
    release: Release,
    #[serde(default)]
    unit: Vec<CiUnit>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Analysis {
    method: String,
    detected: Vec<String>,
    limitations: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Workflow {
    github_runner: String,
    velnor_labels: Vec<String>,
    files: Vec<String>,
    notes: Vec<String>,
    #[serde(default)]
    version_bump_units: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Release {
    enabled: bool,
    reason: String,
    kind: String,
    package: String,
    packages: Vec<String>,
    binary: String,
    targets: Vec<String>,
    image: String,
    source_repository: String,
    consumer_repository: String,
    artifact_path: String,
    description: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Cache {
    key_files: Vec<String>,
    paths: Vec<String>,
}

#[expect(
    dead_code,
    reason = "runtime preserves the complete generated unit contract while execution consumes selected fields"
)]
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CiUnit {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    root: String,
    watch: Vec<String>,
    github_pr_commands: Vec<String>,
    github_full_commands: Vec<String>,
    velnor_pr_commands: Vec<String>,
    velnor_full_commands: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    tool_version: Option<String>,
    #[serde(default)]
    cache: Option<Cache>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunnerLane {
    Github,
    Velnor,
}

impl RunnerLane {
    /// Velnor's job container advertises its authoritative execution backend;
    /// hosted GitHub jobs and local invocations deliberately fall back to the
    /// GitHub command set.
    fn from_execution_backend(backend: Option<&str>) -> Self {
        if backend.is_some_and(|value| {
            value.trim().eq_ignore_ascii_case("docker")
                || value.trim().eq_ignore_ascii_case("microvm")
        }) {
            Self::Velnor
        } else {
            Self::Github
        }
    }

    fn current() -> Self {
        Self::from_execution_backend(env::var("VELNOR_EXECUTION_BACKEND").ok().as_deref())
    }
}

impl CiUnit {
    fn commands(&self, lane: RunnerLane, scope: Scope) -> &[String] {
        match (lane, scope) {
            (RunnerLane::Github, Scope::Affected) => &self.github_pr_commands,
            (RunnerLane::Github, Scope::Full) => &self.github_full_commands,
            (RunnerLane::Velnor, Scope::Affected) => &self.velnor_pr_commands,
            (RunnerLane::Velnor, Scope::Full) => &self.velnor_full_commands,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scope {
    Affected,
    Full,
}

impl Scope {
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "affected" => Ok(Self::Affected),
            "full" => Ok(Self::Full),
            _ => Err(GeneratorError::usage(format!(
                "unsupported CI scope: {value}"
            ))),
        }
    }
}

/// Dispatch the binary-only subcommands. `false` means the arguments belong
/// to the workflow generator CLI proper.
pub(crate) fn try_run(arguments: &[OsString]) -> Result<bool, GeneratorError> {
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else {
        return Ok(false);
    };
    match command {
        "plan" => {
            let options = parse_options(&arguments[1..], &["config"])?;
            plan(&resolve_config_path(options.get("config")))?;
            Ok(true)
        }
        "run" => {
            let options = parse_options(&arguments[1..], &["config", "scope", "unit"])?;
            let root = env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
            let config = resolve_config_path(options.get("config"));
            let scope = options
                .get("scope")
                .map_or(Ok(Scope::Full), |value| Scope::parse(value))?;
            run_units(
                &root,
                &config,
                scope,
                options.get("unit").map(String::as_str),
            )?;
            Ok(true)
        }
        "test-crates" => {
            let options = parse_options(&arguments[1..], &["config"])?;
            let root = env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
            test_crates(&root, &resolve_config_path(options.get("config")), None)?;
            Ok(true)
        }
        "policy" => {
            let options = parse_options(
                &arguments[1..],
                &["workflow-root", "approved-policy-revision"],
            )?;
            let root = options
                .get("workflow-root")
                .map_or_else(
                    || env::var_os("WORKFLOW_ROOT").map(PathBuf::from),
                    |value| Some(PathBuf::from(value)),
                )
                .or_else(|| env::var_os("GITHUB_WORKSPACE").map(PathBuf::from))
                .or_else(|| env::current_dir().ok())
                .ok_or_else(|| GeneratorError::usage("resolve workflow root"))?;
            let trusted_revision = options
                .get("approved-policy-revision")
                .cloned()
                .or_else(|| env::var(TRUSTED_POLICY_REVISION_ENV).ok())
                .ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "{TRUSTED_POLICY_REVISION_ENV} or --approved-policy-revision is required"
                    ))
                })?;
            enforce_policy_with_revision(&root, &trusted_revision)?;
            Ok(true)
        }
        "release" => {
            release(&arguments[1..])?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_options(
    arguments: &[OsString],
    allowed: &[&str],
) -> Result<BTreeMap<String, String>, GeneratorError> {
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < arguments.len() {
        let raw = arguments[index]
            .to_str()
            .ok_or_else(|| GeneratorError::usage("CI argument must be valid UTF-8"))?;
        let (name, inline) = raw
            .strip_prefix("--")
            .and_then(|value| {
                value
                    .split_once('=')
                    .map_or(Some((value, None)), |(n, v)| Some((n, Some(v))))
            })
            .ok_or_else(|| GeneratorError::usage(format!("unsupported CI argument: {raw}")))?;
        if !allowed.contains(&name) {
            return Err(GeneratorError::usage(format!(
                "unsupported CI argument: --{name}"
            )));
        }
        let value = match inline {
            Some(value) if !value.is_empty() => value.to_owned(),
            Some(_) => return Err(GeneratorError::usage(format!("--{name} needs a value"))),
            None => {
                index += 1;
                arguments
                    .get(index)
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| GeneratorError::usage(format!("--{name} needs a value")))?
                    .to_owned()
            }
        };
        if options.insert(name.to_owned(), value).is_some() {
            return Err(GeneratorError::usage(format!("duplicate option: --{name}")));
        }
        index += 1;
    }
    Ok(options)
}

fn resolve_config_path(config: Option<&String>) -> PathBuf {
    config.map_or_else(|| PathBuf::from(DEFAULT_CONFIG), PathBuf::from)
}

fn read_config(path: &Path) -> Result<CiConfig, GeneratorError> {
    let contents = fs::read_to_string(path)
        .map_err(|error| GeneratorError::io("read CI configuration", path, &error))?;
    let config: CiConfig = toml::from_str(&contents).map_err(|error| {
        GeneratorError::usage(format!(
            "parse CI configuration {}: {error}",
            path.display()
        ))
    })?;
    if config.schema != 2 {
        return Err(GeneratorError::usage(format!(
            "unsupported CI configuration schema: {}",
            config.schema
        )));
    }
    validate_config(&config)
}

fn validate_config(config: &CiConfig) -> Result<CiConfig, GeneratorError> {
    if config.unit.is_empty() {
        return Err(GeneratorError::usage("CI configuration declares no units"));
    }
    let mut known = BTreeSet::new();
    for unit in &config.unit {
        if !is_unit_id(&unit.id) {
            return Err(GeneratorError::usage(format!(
                "invalid CI unit id: {}",
                unit.id
            )));
        }
        if !known.insert(unit.id.clone()) {
            return Err(GeneratorError::usage(format!(
                "duplicate CI unit id: {}",
                unit.id
            )));
        }
        if unit.watch.is_empty()
            || unit.github_pr_commands.is_empty()
            || unit.github_full_commands.is_empty()
            || unit.velnor_pr_commands.is_empty()
            || unit.velnor_full_commands.is_empty()
        {
            return Err(GeneratorError::usage(format!(
                "CI unit must declare watch plus GitHub and Velnor PR/full commands: {}",
                unit.id
            )));
        }
    }
    for unit in &config.unit {
        for dependency in &unit.depends_on {
            if !known.contains(dependency) {
                return Err(GeneratorError::usage(format!(
                    "{} depends on unknown unit: {}",
                    unit.id, dependency
                )));
            }
            if dependency == &unit.id {
                return Err(GeneratorError::usage(format!(
                    "unit cannot depend on itself: {}",
                    unit.id
                )));
            }
        }
    }
    let mut version_bump_units = BTreeSet::new();
    for unit in &config.workflow.version_bump_units {
        if !known.contains(unit) {
            return Err(GeneratorError::usage(format!(
                "workflow.version_bump_units names unknown unit: {unit}"
            )));
        }
        if !version_bump_units.insert(unit) {
            return Err(GeneratorError::usage(format!(
                "workflow.version_bump_units contains duplicate unit: {unit}"
            )));
        }
    }
    Ok(config.clone())
}

fn is_unit_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value.as_bytes()[0].is_ascii_lowercase()
}

fn plan(config_path: &Path) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    let scope = match scope_for_event()? {
        Some(value) => Scope::parse(&value)?,
        None => Scope::Full,
    };
    let root = env::current_dir()
        .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
    let base = env::var("BASE_SHA").unwrap_or_default();
    let head = env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned());
    let selection = selection_for_diff(&root, &config, scope, &base, &head)?;
    let units = selection
        .units
        .into_iter()
        .map(|unit| unit.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let full_units = selection
        .full_units
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    if let Some(path) = env::var_os("VELNOR_SELECTION_FILE") {
        write_selection_file(
            &PathBuf::from(path),
            &base,
            &head,
            scope,
            &units,
            &full_units,
        )?;
    }
    if let Some(output) = env::var_os("GITHUB_OUTPUT") {
        let output_path = PathBuf::from(output);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_path)
            .map_err(|error| GeneratorError::io("open GitHub output", &output_path, &error))?;
        writeln!(file, "scope={}", scope_name(scope))
            .map_err(|error| GeneratorError::io("write GitHub output", &output_path, &error))?;
        writeln!(file, "units={units}")
            .map_err(|error| GeneratorError::io("write GitHub output", &output_path, &error))?;
        writeln!(file, "full_units={full_units}")
            .map_err(|error| GeneratorError::io("write GitHub output", &output_path, &error))?;
    }
    println!("scope={}", scope_name(scope));
    println!("units={units}");
    println!("full_units={full_units}");
    Ok(())
}

fn scope_for_event() -> Result<Option<String>, GeneratorError> {
    let event = env::var("EVENT_NAME").unwrap_or_default();
    let override_scope = env::var("CI_SCOPE_OVERRIDE").ok();
    scope_for_event_values(&event, override_scope.as_deref())
}

pub(crate) fn scope_for_event_values(
    event: &str,
    override_scope: Option<&str>,
) -> Result<Option<String>, GeneratorError> {
    match event {
        "push" | "workflow_dispatch" | "schedule" => {
            if override_scope.is_some_and(|scope| scope != "full") {
                return Err(GeneratorError::usage(
                    "trusted events require full CI scope",
                ));
            }
            Ok(Some("full".to_owned()))
        }
        "pull_request" => Ok(override_scope
            .map(ToOwned::to_owned)
            .or_else(|| Some("affected".to_owned()))),
        "" => Ok(override_scope.map(ToOwned::to_owned)),
        _ => Ok(Some("full".to_owned())),
    }
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Affected => "affected",
        Scope::Full => "full",
    }
}

#[cfg(test)]
mod runner_lane_tests {
    use super::{collect_manifests, expand_affected_units, CiUnit, RunnerLane, Scope};
    use std::path::Path;

    #[test]
    fn github_is_the_default_lane_and_velnor_backend_selects_velnor() {
        assert_eq!(RunnerLane::from_execution_backend(None), RunnerLane::Github);
        assert_eq!(
            RunnerLane::from_execution_backend(Some("github-hosted")),
            RunnerLane::Github
        );
        assert_eq!(
            RunnerLane::from_execution_backend(Some("docker")),
            RunnerLane::Velnor
        );
        assert_eq!(
            RunnerLane::from_execution_backend(Some("MICROVM")),
            RunnerLane::Velnor
        );
        assert_eq!(
            RunnerLane::Github,
            RunnerLane::from_execution_backend(Some("self-hosted"))
        );
        assert_eq!(
            RunnerLane::Github,
            RunnerLane::from_execution_backend(Some("unknown"))
        );
    }

    #[test]
    fn lane_and_scope_select_the_matching_command_array() {
        let unit = CiUnit {
            id: "docker".to_owned(),
            label: "Docker".to_owned(),
            kind: "docker".to_owned(),
            root: ".".to_owned(),
            watch: vec!["Dockerfile".to_owned()],
            github_pr_commands: vec!["github-pr".to_owned()],
            github_full_commands: vec!["github-full".to_owned()],
            velnor_pr_commands: vec!["velnor-pr".to_owned()],
            velnor_full_commands: vec!["velnor-full".to_owned()],
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
        };
        assert_eq!(
            unit.commands(RunnerLane::Github, Scope::Affected),
            &["github-pr".to_owned()]
        );
        assert_eq!(
            unit.commands(RunnerLane::Github, Scope::Full),
            &["github-full".to_owned()]
        );
        assert_eq!(
            unit.commands(RunnerLane::Velnor, Scope::Affected),
            &["velnor-pr".to_owned()]
        );
        assert_eq!(
            unit.commands(RunnerLane::Velnor, Scope::Full),
            &["velnor-full".to_owned()]
        );
    }

    #[test]
    fn affected_closure_does_not_follow_dependents_of_added_prerequisites() {
        let units = vec![
            CiUnit {
                id: "base".to_owned(),
                label: "base".to_owned(),
                kind: "rust".to_owned(),
                root: ".".to_owned(),
                watch: Vec::new(),
                github_pr_commands: vec!["base".to_owned()],
                github_full_commands: vec!["base".to_owned()],
                velnor_pr_commands: vec!["base".to_owned()],
                velnor_full_commands: vec!["base".to_owned()],
                depends_on: Vec::new(),
                tool_version: None,
                cache: None,
            },
            CiUnit {
                id: "changed".to_owned(),
                label: "changed".to_owned(),
                kind: "rust".to_owned(),
                root: ".".to_owned(),
                watch: Vec::new(),
                github_pr_commands: vec!["changed".to_owned()],
                github_full_commands: vec!["changed".to_owned()],
                velnor_pr_commands: vec!["changed".to_owned()],
                velnor_full_commands: vec!["changed".to_owned()],
                depends_on: vec!["base".to_owned()],
                tool_version: None,
                cache: None,
            },
            CiUnit {
                id: "sibling".to_owned(),
                label: "sibling".to_owned(),
                kind: "rust".to_owned(),
                root: ".".to_owned(),
                watch: Vec::new(),
                github_pr_commands: vec!["sibling".to_owned()],
                github_full_commands: vec!["sibling".to_owned()],
                velnor_pr_commands: vec!["sibling".to_owned()],
                velnor_full_commands: vec!["sibling".to_owned()],
                depends_on: vec!["base".to_owned()],
                tool_version: None,
                cache: None,
            },
            CiUnit {
                id: "leaf".to_owned(),
                label: "leaf".to_owned(),
                kind: "rust".to_owned(),
                root: ".".to_owned(),
                watch: Vec::new(),
                github_pr_commands: vec!["leaf".to_owned()],
                github_full_commands: vec!["leaf".to_owned()],
                velnor_pr_commands: vec!["leaf".to_owned()],
                velnor_full_commands: vec!["leaf".to_owned()],
                depends_on: vec!["changed".to_owned()],
                tool_version: None,
                cache: None,
            },
        ];
        let selected = expand_affected_units(&units, ["changed".to_owned()].into_iter().collect());
        assert_eq!(
            selected,
            ["base", "changed", "leaf"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    }

    #[test]
    fn crate_test_collection_matches_scanner_fixture_exclusions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut manifests = Vec::new();
        assert!(collect_manifests(root, root, &mut manifests).is_ok());
        assert!(manifests.iter().all(|path| {
            path.strip_prefix(root).is_ok_and(|relative| {
                !super::super::is_test_support_path(&relative.to_string_lossy())
            })
        }));
    }
}

pub(crate) fn run_units(
    root: &Path,
    config_path: &Path,
    scope: Scope,
    only_unit: Option<&str>,
) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    if matches!(
        env::var("EVENT_NAME").as_deref(),
        Ok("push" | "workflow_dispatch" | "schedule")
    ) && scope != Scope::Full
    {
        return Err(GeneratorError::usage(
            "trusted events require full CI scope",
        ));
    }
    let selection_file = env::var_os("VELNOR_SELECTION_FILE").map(PathBuf::from);
    let (selected, full_units) = if let Some(path) = selection_file {
        let selection = read_selection_file(&path)?;
        validate_selection_sha(&selection)?;
        if selection.scope != scope {
            return Err(GeneratorError::usage(format!(
                "CI selection artifact scope mismatch: plan={} job={}",
                scope_name(selection.scope),
                scope_name(scope)
            )));
        }
        let selected = ordered_units(&config.unit, Some(&selection.units))?;
        if selected.len() != selection.units.len() {
            return Err(GeneratorError::usage(
                "CI selection artifact names an unknown unit",
            ));
        }
        if selection
            .full_units
            .iter()
            .any(|unit| !selection.units.contains(unit))
        {
            return Err(GeneratorError::usage(
                "CI selection artifact marks an unselected unit as full",
            ));
        }
        (selected, selection.full_units)
    } else {
        let selection = selection_for_current_diff(root, &config, scope)?;
        (selection.units, selection.full_units)
    };
    let selected = match only_unit {
        Some(id) => {
            if !selected.iter().any(|unit| unit.id == id) {
                return Ok(());
            }
            selected
                .into_iter()
                .filter(|unit| unit.id == id)
                .collect::<Vec<_>>()
        }
        None => selected,
    };
    run_layers(root, &selected, scope, &full_units)
}

fn selection_for_current_diff<'a>(
    root: &Path,
    config: &'a CiConfig,
    scope: Scope,
) -> Result<UnitSelection<'a>, GeneratorError> {
    let base = env::var("BASE_SHA").unwrap_or_default();
    let head = env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned());
    selection_for_diff(root, config, scope, &base, &head)
}

struct UnitSelection<'a> {
    units: Vec<&'a CiUnit>,
    full_units: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedSelection {
    base_sha: String,
    head_sha: String,
    scope: Scope,
    units: BTreeSet<String>,
    full_units: BTreeSet<String>,
}

const SELECTION_FILE_VERSION: &str = "1";

fn write_selection_file(
    path: &Path,
    base_sha: &str,
    head_sha: &str,
    scope: Scope,
    units: &str,
    full_units: &str,
) -> Result<(), GeneratorError> {
    let contents = format!(
        "version={SELECTION_FILE_VERSION}\nbase_sha={base_sha}\nhead_sha={head_sha}\nscope={}\nunits={units}\nfull_units={full_units}\n",
        scope_name(scope)
    );
    fs::write(path, contents)
        .map_err(|error| GeneratorError::io("write CI selection", path, &error))
}

fn read_selection_file(path: &Path) -> Result<PlannedSelection, GeneratorError> {
    let contents = fs::read_to_string(path)
        .map_err(|error| GeneratorError::io("read CI selection", path, &error))?;
    let mut fields = BTreeMap::new();
    for line in contents.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| GeneratorError::usage("malformed CI selection artifact"))?;
        if fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(GeneratorError::usage(
                "CI selection artifact contains duplicate fields",
            ));
        }
    }
    if fields.remove("version").as_deref() != Some(SELECTION_FILE_VERSION) {
        return Err(GeneratorError::usage(
            "unsupported CI selection artifact version",
        ));
    }
    let base_sha = fields
        .remove("base_sha")
        .ok_or_else(|| GeneratorError::usage("CI selection artifact is missing base_sha"))?;
    let head_sha = fields
        .remove("head_sha")
        .ok_or_else(|| GeneratorError::usage("CI selection artifact is missing head_sha"))?;
    let scope = Scope::parse(
        &fields
            .remove("scope")
            .ok_or_else(|| GeneratorError::usage("CI selection artifact is missing scope"))?,
    )?;
    let units = parse_selection_ids(
        &fields
            .remove("units")
            .ok_or_else(|| GeneratorError::usage("CI selection artifact is missing units"))?,
    )?;
    let full_units =
        parse_selection_ids(&fields.remove("full_units").ok_or_else(|| {
            GeneratorError::usage("CI selection artifact is missing full_units")
        })?)?;
    if !fields.is_empty() {
        return Err(GeneratorError::usage(
            "CI selection artifact contains unknown fields",
        ));
    }
    Ok(PlannedSelection {
        base_sha,
        head_sha,
        scope,
        units,
        full_units,
    })
}

fn parse_selection_ids(value: &str) -> Result<BTreeSet<String>, GeneratorError> {
    let mut ids = BTreeSet::new();
    for value in value.split(',').filter(|value| !value.is_empty()) {
        if !is_unit_id(value) {
            return Err(GeneratorError::usage(format!(
                "invalid unit id in CI selection artifact: {value}"
            )));
        }
        if !ids.insert(value.to_owned()) {
            return Err(GeneratorError::usage(format!(
                "duplicate unit id in CI selection artifact: {value}"
            )));
        }
    }
    Ok(ids)
}

fn validate_selection_sha(selection: &PlannedSelection) -> Result<(), GeneratorError> {
    let job_base = env::var("BASE_SHA").unwrap_or_default();
    let job_head = env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned());
    if selection.base_sha != job_base || selection.head_sha != job_head {
        println!(
            "::warning::CI selection artifact SHA mismatch: plan base SHA `{}` vs job base SHA `{}`; plan head SHA `{}` vs job head SHA `{}`",
            selection.base_sha, job_base, selection.head_sha, job_head
        );
        return Err(GeneratorError::usage(
            "CI selection artifact does not match this job checkout",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn selected_units_for_diff<'a>(
    root: &Path,
    config: &'a CiConfig,
    scope: Scope,
    base: &str,
    head: &str,
) -> Result<Vec<&'a CiUnit>, GeneratorError> {
    Ok(selection_for_diff(root, config, scope, base, head)?.units)
}

fn selection_for_diff<'a>(
    root: &Path,
    config: &'a CiConfig,
    scope: Scope,
    base: &str,
    head: &str,
) -> Result<UnitSelection<'a>, GeneratorError> {
    if scope == Scope::Full {
        return full_selection(config);
    }
    if base.is_empty() || base.chars().all(|character| character == '0') {
        return full_selection(config);
    }
    let Some(changed) = git_changed_files(root, base, head)? else {
        return full_selection(config);
    };
    if changed.is_empty() {
        return Ok(UnitSelection {
            units: Vec::new(),
            full_units: BTreeSet::new(),
        });
    }
    if changed.iter().any(|file| file.starts_with(".github/")) {
        return full_selection(config);
    }
    if version_bump_matches(
        root,
        base,
        head,
        &changed,
        &config.workflow.version_bump_units,
    )? {
        let allowlist = config
            .workflow
            .version_bump_units
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        return Ok(UnitSelection {
            units: ordered_units(&config.unit, Some(&allowlist))?,
            full_units: allowlist,
        });
    }
    let matchers = config
        .unit
        .iter()
        .map(|unit| {
            let mut builder = GlobSetBuilder::new();
            for pattern in &unit.watch {
                let glob = Glob::new(pattern).map_err(|error| {
                    GeneratorError::usage(format!("invalid watch pattern {pattern}: {error}"))
                })?;
                builder.add(glob);
            }
            let matcher = builder
                .build()
                .map_err(|error| GeneratorError::usage(format!("build watch matcher: {error}")))?;
            Ok::<_, GeneratorError>((unit, matcher))
        })
        .collect::<Result<Vec<(&CiUnit, GlobSet)>, _>>()?;
    let mut selected = BTreeSet::new();
    for file in &changed {
        let mut matched = false;
        for (unit, matcher) in &matchers {
            if matcher.is_match(file) {
                selected.insert(unit.id.clone());
                matched = true;
            }
        }
        if !matched {
            return full_selection(config);
        }
    }
    let (selected, full_units) = expand_affected_units_with_full(&config.unit, selected);
    Ok(UnitSelection {
        units: ordered_units(&config.unit, Some(&selected))?,
        full_units,
    })
}

fn full_selection<'a>(config: &'a CiConfig) -> Result<UnitSelection<'a>, GeneratorError> {
    Ok(UnitSelection {
        units: ordered_units(&config.unit, None)?,
        full_units: config.unit.iter().map(|unit| unit.id.clone()).collect(),
    })
}

fn version_bump_matches(
    root: &Path,
    base: &str,
    head: &str,
    changed: &[String],
    allowlist: &[String],
) -> Result<bool, GeneratorError> {
    if allowlist.is_empty() || changed.is_empty() {
        return Ok(false);
    }
    let mut diff_files = Vec::new();
    for file in changed {
        if file == "Cargo.lock" {
            diff_files.push(file.clone());
            continue;
        }
        let Some(crate_name) = file
            .strip_prefix("crates/")
            .and_then(|value| value.strip_suffix("/Cargo.toml"))
        else {
            return Ok(false);
        };
        let unit_id = if crate_name == "velnorctl" {
            "rust-velnorctl".to_owned()
        } else {
            format!("rust-{crate_name}")
        };
        if !allowlist.iter().any(|allowed| allowed == &unit_id) {
            return Ok(false);
        }
        diff_files.push(file.clone());
    }
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["diff", "--unified=0"])
        .arg(format!("{base}...{head}"))
        .arg("--");
    for file in &diff_files {
        command.arg(file);
    }
    let output = command
        .output()
        .map_err(|error| GeneratorError::usage(format!("run version diff: {error}")))?;
    if !output.status.success() {
        return Ok(false);
    }
    let mut changed_lines = 0;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let Some(content) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) else {
            continue;
        };
        changed_lines += 1;
        if !content.trim_start().starts_with("version = \"") {
            return Ok(false);
        }
    }
    Ok(changed_lines > 0)
}

fn expand_affected_units_with_full(
    units: &[CiUnit],
    changed: BTreeSet<String>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut affected = changed.clone();
    let mut pending = changed.into_iter().collect::<Vec<_>>();
    while let Some(changed_id) = pending.pop() {
        for unit in units {
            if unit
                .depends_on
                .iter()
                .any(|dependency| dependency == &changed_id)
                && affected.insert(unit.id.clone())
            {
                pending.push(unit.id.clone());
            }
        }
    }
    let full_units = affected.clone();
    let mut required = affected;
    let mut pending = required.iter().cloned().collect::<Vec<_>>();
    while let Some(unit_id) = pending.pop() {
        let Some(unit) = units.iter().find(|unit| unit.id == unit_id) else {
            continue;
        };
        for dependency in &unit.depends_on {
            if required.insert(dependency.clone()) {
                pending.push(dependency.clone());
            }
        }
    }
    (required, full_units)
}

#[cfg(test)]
fn expand_affected_units(units: &[CiUnit], changed: BTreeSet<String>) -> BTreeSet<String> {
    expand_affected_units_with_full(units, changed).0
}

fn git_changed_files(
    root: &Path,
    base: &str,
    head: &str,
) -> Result<Option<Vec<String>>, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-only"])
        .arg(format!("{base}...{head}"))
        .output()
        .map_err(|error| GeneratorError::usage(format!("run git diff: {error}")))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
    ))
}

fn ordered_units<'a>(
    units: &'a [CiUnit],
    selected: Option<&BTreeSet<String>>,
) -> Result<Vec<&'a CiUnit>, GeneratorError> {
    let wanted = units
        .iter()
        .filter(|unit| selected.is_none_or(|ids| ids.contains(&unit.id)))
        .collect::<Vec<_>>();
    let wanted_ids = wanted
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut emitted = BTreeSet::new();
    let mut result = Vec::with_capacity(wanted.len());
    while result.len() < wanted.len() {
        let mut progress = false;
        for unit in &wanted {
            if emitted.contains(&unit.id)
                || unit.depends_on.iter().any(|dependency| {
                    wanted_ids.contains(dependency.as_str()) && !emitted.contains(dependency)
                })
            {
                continue;
            }
            emitted.insert(unit.id.clone());
            result.push(*unit);
            progress = true;
        }
        if !progress {
            return Err(GeneratorError::usage(
                "CI dependency graph contains a cycle",
            ));
        }
    }
    Ok(result)
}

fn run_layers(
    root: &Path,
    units: &[&CiUnit],
    run_scope: Scope,
    full_units: &BTreeSet<String>,
) -> Result<(), GeneratorError> {
    let runner_lane = RunnerLane::current();
    let mut finished = BTreeSet::new();
    while finished.len() < units.len() {
        let ready = units
            .iter()
            .filter(|unit| {
                !finished.contains(&unit.id)
                    && unit
                        .depends_on
                        .iter()
                        .filter(|dependency| {
                            units.iter().any(|candidate| candidate.id == **dependency)
                        })
                        .all(|dependency| finished.contains(dependency))
            })
            .copied()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err(GeneratorError::usage(
                "CI dependency graph contains a cycle or invalid ordering",
            ));
        }
        let (sender, receiver) = mpsc::channel();
        thread::scope(|thread_scope| {
            for unit in ready.iter().copied() {
                let sender = sender.clone();
                let commands = if full_units.contains(&unit.id) {
                    unit.commands(runner_lane, run_scope).to_vec()
                } else {
                    prerequisite_commands(unit, runner_lane, run_scope)
                };
                thread_scope.spawn(move || {
                    let result = run_unit(root, unit, &commands);
                    let _ = sender.send((unit.id.clone(), result));
                });
            }
        });
        drop(sender);
        for (id, result) in receiver {
            result?;
            finished.insert(id);
        }
    }
    Ok(())
}

fn prerequisite_commands(unit: &CiUnit, lane: RunnerLane, scope: Scope) -> Vec<String> {
    if unit.kind != "rust" {
        return Vec::new();
    }
    unit.commands(lane, scope)
        .iter()
        .filter(|command| command.contains(" clippy "))
        .map(|command| {
            command
                .replacen(" clippy ", " check ", 1)
                .replace(" -- -D warnings", "")
        })
        .collect()
}

fn run_unit(root: &Path, unit: &CiUnit, commands: &[String]) -> Result<(), GeneratorError> {
    for command in commands {
        println!("::group::{}: {}", unit.id, command);
        let status = Command::new("bash")
            .args(["-euo", "pipefail", "-c", command])
            .current_dir(root)
            .stdin(Stdio::null())
            .status()
            .map_err(|error| {
                GeneratorError::usage(format!("run CI command {}: {error}", unit.id))
            })?;
        println!("::endgroup::");
        if !status.success() {
            return Err(GeneratorError::usage(format!(
                "CI command failed for unit {} with {status}",
                unit.id
            )));
        }
    }
    Ok(())
}

pub(crate) fn test_crates(
    root: &Path,
    config_path: &Path,
    cargo_program: Option<&Path>,
) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    let mut manifests = Vec::new();
    collect_manifests(root, root, &mut manifests)?;
    manifests.sort();
    let locked = root.join("Cargo.lock").is_file();
    let nextest = config
        .analysis
        .detected
        .iter()
        .any(|item| item == "cargo-nextest-policy");
    for manifest in manifests {
        let contents = fs::read_to_string(&manifest)
            .map_err(|error| GeneratorError::io("read Cargo manifest", &manifest, &error))?;
        let value: toml::Value = toml::from_str(&contents).map_err(|error| {
            GeneratorError::usage(format!(
                "parse Cargo manifest {}: {error}",
                manifest.display()
            ))
        })?;
        if !value.get("package").is_some_and(toml::Value::is_table) {
            continue;
        }
        println!("::group::cargo tests: {}", manifest.display());
        let default_cargo =
            if env::var(super::MR_BOXINGTON_ENABLED_ENV).is_ok_and(|value| value == "1") {
                Path::new("mbx")
            } else {
                Path::new("cargo")
            };
        let mut command = Command::new(cargo_program.unwrap_or(default_cargo));
        command
            .arg(if nextest { "nextest" } else { "test" })
            .arg(if nextest { "run" } else { "--all-features" });
        if nextest {
            command.arg("--all-features");
        }
        if locked {
            command.arg("--locked");
        }
        command.arg("--manifest-path").arg(&manifest);
        let status = command
            .current_dir(root)
            .status()
            .map_err(|error| GeneratorError::usage(format!("run Cargo tests: {error}")))?;
        println!("::endgroup::");
        if !status.success() {
            return Err(GeneratorError::usage(format!(
                "Cargo tests failed: {}",
                manifest.display()
            )));
        }
    }
    Ok(())
}

fn collect_manifests(
    root: &Path,
    directory: &Path,
    manifests: &mut Vec<PathBuf>,
) -> Result<(), GeneratorError> {
    for entry in fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read CI source directory", directory, &error))?
    {
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("read directory entry: {error}")))?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if entry
            .file_type()
            .map_err(|error| GeneratorError::usage(format!("inspect {}: {error}", path.display())))?
            .is_dir()
        {
            if matches!(relative.to_str(), Some(".git" | "target")) {
                continue;
            }
            collect_manifests(root, &path, manifests)?;
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml")
            && relative
                .to_str()
                .is_none_or(|path| !super::is_test_support_path(path))
        {
            manifests.push(path);
        }
    }
    Ok(())
}

pub(crate) fn enforce_policy_with_revision(
    root: &Path,
    trusted_revision: &str,
) -> Result<(), GeneratorError> {
    if !is_full_sha(trusted_revision) {
        return Err(GeneratorError::usage(format!(
            "{TRUSTED_POLICY_REVISION_ENV} must be a full 40-character SHA"
        )));
    }
    let workflows = root.join(".github/workflows");
    let entries = fs::read_dir(&workflows)
        .map_err(|error| GeneratorError::io("read workflow directory", &workflows, &error))?;
    let policy_entrypoint = workflows.join("ci-policy.yml");
    let mut found_policy_entrypoint = false;
    let mut failures = 0;
    for entry in entries {
        let path = entry
            .map_err(|error| GeneratorError::usage(format!("read workflow entry: {error}")))?
            .path();
        if !path.is_file()
            || !matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("yml" | "yaml")
            )
        {
            continue;
        }
        if path == policy_entrypoint {
            found_policy_entrypoint = true;
        }
        let content = fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read workflow", &path, &error))?;
        let document: Value = serde_yaml::from_str(&content).map_err(|error| {
            GeneratorError::usage(format!("parse workflow {}: {error}", path.display()))
        })?;
        let Some(workflow) = document.as_mapping() else {
            policy_failure(
                &path,
                "workflow document must be a YAML mapping",
                &mut failures,
            );
            continue;
        };
        inspect_workflow(workflow, &path, trusted_revision, &mut failures);
    }
    if !found_policy_entrypoint {
        policy_failure(
            &policy_entrypoint,
            "required base-owned ci-policy.yml entrypoint is missing",
            &mut failures,
        );
    }
    if failures > 0 {
        return Err(GeneratorError::usage(format!(
            "workflow policy rejected {failures} finding(s)"
        )));
    }
    Ok(())
}

fn inspect_workflow(workflow: &Mapping, path: &Path, trusted_revision: &str, failures: &mut usize) {
    let approved_policy_entrypoint =
        is_approved_policy_entrypoint(path, workflow, trusted_revision);
    for (key, value) in workflow {
        let key = key.as_str();
        match key {
            "on" => {
                if contains_exact_yaml_value(value, "pull_request_target")
                    && !approved_policy_entrypoint
                {
                    policy_failure(path, "pull_request_target is forbidden", failures);
                }
            }
            "jobs" => inspect_jobs(value, path, trusted_revision, failures),
            _ => inspect_yaml_value(value, path, None, false, trusted_revision, failures),
        }
    }
}

fn is_approved_policy_entrypoint(path: &Path, workflow: &Mapping, trusted_revision: &str) -> bool {
    if !path.ends_with(Path::new(".github/workflows/ci-policy.yml"))
        || mapping_value(workflow, "name").and_then(Value::as_str) != Some("Velnor workflow policy")
    {
        return false;
    }
    let Some(on) = mapping_value(workflow, "on").and_then(Value::as_mapping) else {
        return false;
    };
    if on.len() != 1 {
        return false;
    }
    let Some(event) = mapping_value(on, "pull_request_target").and_then(Value::as_mapping) else {
        return false;
    };
    let Some(types) = mapping_value(event, "types").and_then(Value::as_sequence) else {
        return false;
    };
    let expected_types = ["opened", "synchronize", "reopened"];
    if types.len() != expected_types.len()
        || types
            .iter()
            .zip(expected_types)
            .any(|(value, expected)| value.as_str() != Some(expected))
    {
        return false;
    }
    let Some(jobs) = mapping_value(workflow, "jobs").and_then(Value::as_mapping) else {
        return false;
    };
    let Some(policy) = mapping_value(jobs, "policy").and_then(Value::as_mapping) else {
        return false;
    };
    if jobs.len() != 1
        || mapping_value(policy, "name").and_then(Value::as_str) != Some("Policy")
        || mapping_value(policy, "uses")
            .and_then(Value::as_str)
            .is_none_or(|value| !is_approved_policy_reusable(value, trusted_revision))
        || !has_policy_revision_input(policy, trusted_revision)
    {
        return false;
    }
    let Some(permissions) = mapping_value(policy, "permissions").and_then(Value::as_mapping) else {
        return false;
    };
    permissions.len() == 1
        && mapping_value(permissions, "contents").and_then(Value::as_str) == Some("read")
        && policy
            .keys()
            .all(|key| matches!(key.as_str(), "name" | "uses" | "with" | "permissions"))
}

fn inspect_jobs(value: &Value, path: &Path, trusted_revision: &str, failures: &mut usize) {
    let Some(jobs) = value.as_mapping() else {
        policy_failure(path, "jobs must be a YAML mapping", failures);
        return;
    };
    for (job_id, job) in jobs {
        let Some(job) = job.as_mapping() else {
            let name = job_id.as_str();
            policy_failure(
                path,
                &format!("job {name} must be a YAML mapping"),
                failures,
            );
            continue;
        };
        if mapping_value(job, "uses")
            .and_then(Value::as_str)
            .is_some_and(|value| is_approved_policy_reusable(value, trusted_revision))
            && !has_policy_revision_input(job, trusted_revision)
        {
            policy_failure(
                path,
                "approved policy workflow call must pass the trusted policy-revision input",
                failures,
            );
        }
        let trusted_gate = mapping_value(job, "if")
            .and_then(Value::as_str)
            .is_some_and(has_trusted_runner_gate);
        let matrix = mapping_value(job, "strategy")
            .and_then(Value::as_mapping)
            .and_then(|strategy| mapping_value(strategy, "matrix"))
            .and_then(Value::as_mapping);
        inspect_mapping(job, path, matrix, trusted_gate, trusted_revision, failures);
    }
}

fn inspect_yaml_value(
    value: &Value,
    path: &Path,
    matrix: Option<&Mapping>,
    trusted_gate: bool,
    trusted_revision: &str,
    failures: &mut usize,
) {
    match value {
        Value::Mapping(mapping) => {
            inspect_mapping(
                mapping,
                path,
                matrix,
                trusted_gate,
                trusted_revision,
                failures,
            );
        }
        Value::Sequence(sequence) => {
            for item in sequence {
                inspect_yaml_value(item, path, matrix, trusted_gate, trusted_revision, failures);
            }
        }
        Value::Tagged(tagged) => {
            inspect_yaml_value(
                tagged.value(),
                path,
                matrix,
                trusted_gate,
                trusted_revision,
                failures,
            );
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn inspect_mapping(
    mapping: &Mapping,
    path: &Path,
    matrix: Option<&Mapping>,
    trusted_gate: bool,
    trusted_revision: &str,
    failures: &mut usize,
) {
    for (key, value) in mapping {
        let key = key.as_str();
        match key {
            "pull_request_target" => {
                policy_failure(path, "pull_request_target is forbidden", failures);
            }
            "uses" => inspect_uses(value, path, trusted_revision, failures),
            "runs-on" => inspect_runner(value, path, matrix, trusted_gate, failures),
            _ => inspect_yaml_value(
                value,
                path,
                matrix,
                trusted_gate,
                trusted_revision,
                failures,
            ),
        }
    }
}

fn inspect_uses(value: &Value, path: &Path, trusted_revision: &str, failures: &mut usize) {
    let Some(action) = value.as_str() else {
        policy_failure(path, "uses must be a scalar reference", failures);
        return;
    };
    if is_approved_local_reusable(action) || is_approved_policy_reusable(action, trusted_revision) {
        return;
    }
    let reference_path = action.split_once('@').map_or(action, |(path, _)| path);
    if reference_path.contains("/.github/workflows/") {
        policy_failure(
            path,
            &format!("reusable workflow must be an approved local generated workflow: {action}"),
            failures,
        );
    } else if !is_full_sha_reference(action) {
        policy_failure(
            path,
            &format!("action is not a full SHA pin: {action}"),
            failures,
        );
    }
}

fn is_approved_policy_reusable(value: &str, trusted_revision: &str) -> bool {
    value.split_once('@').is_some_and(|(path, reference)| {
        path == IMMUTABLE_POLICY_WORKFLOW && reference == trusted_revision && is_full_sha(reference)
    })
}

fn has_policy_revision_input(policy: &Mapping, trusted_revision: &str) -> bool {
    let Some(with) = mapping_value(policy, "with").and_then(Value::as_mapping) else {
        return false;
    };
    with.len() == 1
        && mapping_value(with, "policy-revision")
            .and_then(Value::as_str)
            .is_some_and(|value| value == trusted_revision && is_full_sha(value))
}

fn is_approved_local_reusable(value: &str) -> bool {
    let Some(name) = value.strip_prefix("./.github/workflows/ci-") else {
        return false;
    };
    !name.is_empty()
        && Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("yml"))
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains('@')
}

fn is_full_sha_reference(value: &str) -> bool {
    value
        .split_once('@')
        .is_some_and(|(action, reference)| !action.is_empty() && is_full_sha(reference))
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || HEX_DIGITS.contains(&byte))
}

#[derive(Clone, Copy, Debug, Default)]
struct RunnerAnalysis {
    self_hosted: bool,
    dynamic: bool,
    invalid: bool,
}

impl RunnerAnalysis {
    fn merge(&mut self, other: Self) {
        self.self_hosted |= other.self_hosted;
        self.dynamic |= other.dynamic;
        self.invalid |= other.invalid;
    }
}

fn inspect_runner(
    value: &Value,
    path: &Path,
    matrix: Option<&Mapping>,
    trusted_gate: bool,
    failures: &mut usize,
) {
    let mut resolving = BTreeSet::new();
    let analysis = analyze_runner(value, matrix, &mut resolving);
    if analysis.invalid {
        policy_failure(path, "runs-on must contain only string labels", failures);
    }
    if analysis.dynamic {
        policy_failure(
            path,
            "runs-on contains an unresolved or dynamic runner label",
            failures,
        );
    }
    if analysis.self_hosted && !trusted_gate {
        policy_failure(
            path,
            "self-hosted jobs require a default-branch trusted-event gate",
            failures,
        );
    }
}

fn analyze_runner(
    value: &Value,
    matrix: Option<&Mapping>,
    resolving: &mut BTreeSet<String>,
) -> RunnerAnalysis {
    match value {
        Value::String(label) => {
            if let Some(field) = matrix_field_reference(label) {
                if !resolving.insert(field.to_owned()) {
                    return RunnerAnalysis {
                        dynamic: true,
                        ..RunnerAnalysis::default()
                    };
                }
                let Some(values) = matrix_values(matrix, field) else {
                    resolving.remove(field);
                    return RunnerAnalysis {
                        dynamic: true,
                        ..RunnerAnalysis::default()
                    };
                };
                let mut result = RunnerAnalysis::default();
                for value in values {
                    result.merge(analyze_runner(value, matrix, resolving));
                }
                resolving.remove(field);
                result
            } else if label.contains("${{") {
                RunnerAnalysis {
                    dynamic: true,
                    ..RunnerAnalysis::default()
                }
            } else if contains_self_hosted_label(label) {
                RunnerAnalysis {
                    self_hosted: true,
                    ..RunnerAnalysis::default()
                }
            } else {
                RunnerAnalysis::default()
            }
        }
        Value::Sequence(sequence) => {
            let mut result = RunnerAnalysis::default();
            for value in sequence {
                result.merge(analyze_runner(value, matrix, resolving));
            }
            result
        }
        Value::Mapping(mapping) => {
            let mut result = RunnerAnalysis {
                dynamic: true,
                ..RunnerAnalysis::default()
            };
            if let Some(labels) = mapping_value(mapping, "labels") {
                result.merge(analyze_runner(labels, matrix, resolving));
            } else {
                result.invalid = true;
            }
            result
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => RunnerAnalysis {
            invalid: true,
            ..RunnerAnalysis::default()
        },
        Value::Tagged(tagged) => analyze_runner(tagged.value(), matrix, resolving),
    }
}

fn matrix_field_reference(value: &str) -> Option<&str> {
    let expression = value.trim().strip_prefix("${{")?.strip_suffix("}}")?.trim();
    let field = expression.strip_prefix("matrix.")?;
    (!field.is_empty()
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then_some(field)
}

fn matrix_values<'a>(matrix: Option<&'a Mapping>, field: &str) -> Option<Vec<&'a Value>> {
    let matrix = matrix?;
    let mut values = Vec::new();
    let direct = mapping_value(matrix, field);
    if let Some(value) = direct {
        let sequence = value.as_sequence()?;
        values.extend(sequence);
    }
    if let Some(include) = mapping_value(matrix, "include") {
        let include = include.as_sequence()?;
        for item in include {
            let item = item.as_mapping()?;
            if let Some(value) = mapping_value(item, field) {
                values.push(value);
            } else if direct.is_none() {
                return None;
            }
        }
    }
    (!values.is_empty()).then_some(values)
}

fn mapping_value<'a>(mapping: &'a Mapping, name: &str) -> Option<&'a Value> {
    mapping
        .iter()
        .find_map(|(key, value)| (key.as_str() == name).then_some(value))
}

fn contains_exact_yaml_value(value: &Value, target: &str) -> bool {
    match value {
        Value::String(value) => value == target,
        Value::Mapping(mapping) => mapping
            .iter()
            .any(|(key, value)| key.as_str() == target || contains_exact_yaml_value(value, target)),
        Value::Sequence(sequence) => sequence
            .iter()
            .any(|value| contains_exact_yaml_value(value, target)),
        Value::Tagged(tagged) => contains_exact_yaml_value(tagged.value(), target),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn contains_self_hosted_label(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("self-hosted") || value.contains("velnor")
}

fn has_trusted_runner_gate(value: &str) -> bool {
    let value = value.trim();
    let value = value
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map_or(value, str::trim)
        .split_whitespace()
        .collect::<String>();
    let marker = "github.ref=='refs/heads/";
    let Some(start) = value.find(marker).map(|start| start + marker.len()) else {
        return false;
    };
    let Some(end) = value[start..].find('\'').map(|end| start + end) else {
        return false;
    };
    let branch = &value[start..end];
    if !valid_branch(branch) {
        return false;
    }
    let ci_gate = format!(
        "github.ref=='refs/heads/{branch}'&&(github.event_name=='push'||github.event_name=='schedule'||github.event_name=='workflow_dispatch')"
    );
    let release_gate = format!(
        "(github.event_name=='push'&&(github.ref_type=='tag'||github.ref=='refs/heads/{branch}'))||github.event_name=='schedule'||(github.event_name=='workflow_dispatch'&&github.ref=='refs/heads/{branch}')"
    );
    value == ci_gate || value == release_gate
}

fn policy_failure(path: &Path, message: &str, failures: &mut usize) {
    eprintln!("{}: {message}", path.display());
    *failures += 1;
}

fn release(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else {
        return Err(GeneratorError::usage(
            "usage: release verify-tag | release package-binary ...",
        ));
    };
    match command {
        "verify-tag" => verify_tag(&arguments[1..]),
        "package-binary" => package_binary(&arguments[1..]),
        _ => Err(GeneratorError::usage(format!(
            "unsupported release command: {command}"
        ))),
    }
}

fn verify_tag(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["branch", "package"])?;
    let reference = env::var("GITHUB_REF").unwrap_or_default();
    let tag = reference
        .strip_prefix("refs/tags/v")
        .filter(|value| is_semver(value))
        .ok_or_else(|| GeneratorError::usage("release requires a semver v* tag"))?;
    if env::var("GITHUB_REF_TYPE").unwrap_or_else(|_| "tag".to_owned()) != "tag" {
        return Err(GeneratorError::usage("release ref is not a tag"));
    }
    let branch = options
        .get("branch")
        .map(String::as_str)
        .ok_or_else(|| GeneratorError::usage("release requires --branch"))?;
    if !valid_branch(branch) {
        return Err(GeneratorError::usage("invalid release branch"));
    }
    let branch_reference = format!("refs/remotes/origin/{branch}");
    let tag_commit = git_revision(&reference)?;
    let branch_commit = git_revision(&branch_reference)?;
    if tag_commit != branch_commit {
        return Err(GeneratorError::usage(format!(
            "release tag must equal current origin/{branch} tip"
        )));
    }
    if let Some(package) = options.get("package") {
        if !valid_package(package) {
            return Err(GeneratorError::usage("invalid release package"));
        }
        let metadata = Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
            .output()
            .map_err(|error| GeneratorError::usage(format!("cargo metadata failed: {error}")))?;
        if !metadata.status.success() {
            return Err(GeneratorError::usage("cargo metadata failed"));
        }
        let document: serde_json::Value = serde_json::from_slice(&metadata.stdout)
            .map_err(|error| GeneratorError::usage(format!("parse cargo metadata: {error}")))?;
        let found = document["packages"].as_array().is_some_and(|packages| {
            packages.iter().any(|item| {
                item["name"].as_str() == Some(package) && item["version"].as_str() == Some(tag)
            })
        });
        if !found {
            return Err(GeneratorError::usage(
                "release tag version does not match the declared package",
            ));
        }
    }
    println!("{tag}");
    Ok(())
}

fn package_binary(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["target", "version", "package", "binary"])?;
    let target = required_option(&options, "target")?;
    let version = required_option(&options, "version")?;
    let package = required_option(&options, "package")?;
    let binary = required_option(&options, "binary")?;
    if !valid_target(target)
        || !is_artifact_version(version)
        || !valid_package(package)
        || !valid_binary(binary)
    {
        return Err(GeneratorError::usage(
            "invalid target, version, package, or binary",
        ));
    }
    let root = env::current_dir()
        .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
    let source = root
        .join("target")
        .join(target)
        .join("release")
        .join(binary);
    if !source.is_file() {
        return Err(GeneratorError::usage(format!(
            "built binary is missing: {}",
            source.display()
        )));
    }
    let dist = root.join("dist");
    fs::create_dir_all(&dist)
        .map_err(|error| GeneratorError::io("create release directory", &dist, &error))?;
    let archive = dist.join(format!("{binary}-{version}-{target}.tar.gz"));
    let status = Command::new("tar")
        .arg("-C")
        .arg(source.parent().unwrap_or(&root))
        .arg("-czf")
        .arg(&archive)
        .arg(binary)
        .status()
        .map_err(|error| GeneratorError::usage(format!("package binary: {error}")))?;
    if !status.success() {
        return Err(GeneratorError::usage("tar failed while packaging binary"));
    }
    let digest = sha256_file(&archive)?;
    let checksum = archive.with_extension("tar.gz.sha256");
    fs::write(
        &checksum,
        format!(
            "{digest}  {}\n",
            archive
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
        ),
    )
    .map_err(|error| GeneratorError::io("write release checksum", &checksum, &error))?;
    println!("{}", archive.display());
    Ok(())
}

fn required_option<'a>(
    options: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, GeneratorError> {
    options
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| GeneratorError::usage(format!("--{name} needs a value")))
}

fn git_revision(reference: &str) -> Result<String, GeneratorError> {
    let revision_reference = format!("{reference}^{{commit}}");
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &revision_reference])
        .output()
        .map_err(|error| GeneratorError::usage(format!("run git: {error}")))?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "git could not resolve release reference {reference}"
        )));
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GeneratorError::usage(format!(
            "git returned an invalid revision for {reference}"
        )));
    }
    Ok(revision)
}

fn sha256_file(path: &Path) -> Result<String, GeneratorError> {
    let contents =
        fs::read(path).map_err(|error| GeneratorError::io("read release archive", path, &error))?;
    let digest = Sha256::digest(contents);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    Ok(output)
}

fn is_semver(value: &str) -> bool {
    let (core, suffix) = value
        .split_once('-')
        .map_or((value, None), |(core, suffix)| (core, Some(suffix)));
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && suffix.is_none_or(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        })
}

fn is_artifact_version(value: &str) -> bool {
    value == "preview" || is_semver(value)
}

fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
}

fn valid_target(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_package(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_binary(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const CHECKOUT_SHA: &str = "3d3c42e5aac5ba805825da76410c181273ba90b1";
    const POLICY_REVISION: &str = "a1cbfcbe5ab179032e37125f0383cdcae8183c8c";

    fn policy_fixture(
        name: &str,
        workflow: &str,
        runners: &str,
    ) -> Result<std::path::PathBuf, Box<dyn Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-policy-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join(".github/workflows"))?;
        std::fs::create_dir_all(root.join(".github/ci"))?;
        std::fs::write(root.join(".github/workflows/policy.yml"), workflow)?;
        std::fs::write(
            root.join(".github/workflows/ci-policy.yml"),
            format!("name: Velnor workflow policy\non:\n  pull_request_target:\n    types: [opened, synchronize, reopened]\npermissions:\n  contents: read\njobs:\n  policy:\n    name: Policy\n    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@{POLICY_REVISION}\n    with:\n      policy-revision: {POLICY_REVISION}\n    permissions:\n      contents: read\n"),
        )?;
        std::fs::write(
            root.join(".github/ci/project.toml"),
            format!("runners = \"{runners}\"\n"),
        )?;
        Ok(root)
    }

    fn run_policy(root: std::path::PathBuf) -> Result<bool, Box<dyn Error>> {
        let result = enforce_policy_with_revision(&root, POLICY_REVISION).is_ok();
        std::fs::remove_dir_all(root)?;
        Ok(result)
    }

    fn selection_config() -> CiConfig {
        let unit = |id: &str, watch: &[&str], depends_on: &[&str]| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: "rust".to_owned(),
            root: ".".to_owned(),
            watch: watch.iter().map(|value| (*value).to_owned()).collect(),
            github_pr_commands: vec!["true".to_owned()],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["true".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            depends_on: depends_on.iter().map(|value| (*value).to_owned()).collect(),
            tool_version: None,
            cache: None,
        };
        CiConfig {
            schema: 2,
            repository: "example/repository".to_owned(),
            profile: "rust-workspace".to_owned(),
            verified: true,
            default_branch: "main".to_owned(),
            runners: "github".to_owned(),
            analysis: Analysis::default(),
            workflow: Workflow::default(),
            release: Release::default(),
            unit: vec![
                unit("base", &["crates/base/**"], &[]),
                unit("app", &["crates/app/**"], &["base"]),
                unit("consumer", &["crates/consumer/**"], &["app"]),
                unit("docs", &["docs/**"], &[]),
            ],
        }
    }

    fn selection_git_fixture(
        name: &str,
        changed: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-selection-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        let init = |args: &[&str]| -> Result<(), Box<dyn Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?;
            assert!(status.success(), "git command failed: {args:?}");
            Ok(())
        };
        init(&["init", "-q"])?;
        init(&["config", "user.email", "test@example.invalid"])?;
        init(&["config", "user.name", "Velnor test"])?;
        for path in [
            "crates/base/src/lib.rs",
            "crates/app/src/lib.rs",
            "crates/consumer/src/lib.rs",
            "docs/index.md",
        ] {
            let path = root.join(path);
            let parent = path.parent().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "fixture parent")
            })?;
            std::fs::create_dir_all(parent)?;
            std::fs::write(path, "initial\n")?;
        }
        init(&["add", "."])?;
        init(&["commit", "-qm", "base"])?;
        let base = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        let changed_path = root.join(changed);
        let parent = changed_path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "changed parent")
        })?;
        std::fs::create_dir_all(parent)?;
        std::fs::write(changed_path, "changed\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "change"])?;
        let head = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        Ok((root, base, head))
    }

    fn stale_base_selection_git_fixture(
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-stale-base-selection-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("crates/base/src"))?;
        std::fs::create_dir_all(root.join("docs"))?;
        let init = |args: &[&str]| -> Result<(), Box<dyn Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?;
            assert!(status.success(), "git command failed: {args:?}");
            Ok(())
        };
        init(&["init", "-q"])?;
        init(&["config", "user.email", "test@example.invalid"])?;
        init(&["config", "user.name", "Velnor test"])?;
        std::fs::write(root.join("crates/base/src/lib.rs"), "initial\n")?;
        std::fs::write(root.join("docs/index.md"), "initial\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "base"])?;
        let initial = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();

        init(&["checkout", "-q", "-b", "base-line"])?;
        std::fs::write(root.join("docs/index.md"), "base-only\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "base-only"])?;
        let base = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();

        init(&["checkout", "-q", "-b", "feature", &initial])?;
        std::fs::write(root.join("crates/base/src/lib.rs"), "feature-only\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "feature-only"])?;
        let head = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        Ok((root, base, head))
    }

    fn lockfile_version_selection_git_fixture(
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-version-selection-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("crates/runner"))?;
        std::fs::create_dir_all(root.join("crates/ctl"))?;
        let init = |args: &[&str]| -> Result<(), Box<dyn Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?;
            assert!(status.success(), "git command failed: {args:?}");
            Ok(())
        };
        init(&["init", "-q"])?;
        init(&["config", "user.email", "test@example.invalid"])?;
        init(&["config", "user.name", "Velnor test"])?;
        std::fs::write(
            root.join("Cargo.lock"),
            "version = \"1\"\nchecksum = \"unchanged\"\n",
        )?;
        std::fs::write(
            root.join("crates/runner/Cargo.toml"),
            "version = \"0.1.0\"\n",
        )?;
        std::fs::write(root.join("crates/ctl/Cargo.toml"), "version = \"0.1.0\"\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "base"])?;
        let base = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        std::fs::write(
            root.join("Cargo.lock"),
            "version = \"2\"\nchecksum = \"unchanged\"\n",
        )?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "version bump"])?;
        let head = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        Ok((root, base, head))
    }

    fn selected_ids(units: Vec<&CiUnit>) -> Vec<&str> {
        units.into_iter().map(|unit| unit.id.as_str()).collect()
    }

    #[test]
    fn selection_artifact_round_trips_scope_and_sha() -> Result<(), Box<dyn Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "velnor-workflow-selection-artifact-{}-{id}",
            std::process::id()
        ));
        write_selection_file(
            &path,
            "base-sha",
            "head-sha",
            Scope::Affected,
            "base,app",
            "app",
        )?;
        let selection = read_selection_file(&path)?;
        assert_eq!(selection.base_sha, "base-sha");
        assert_eq!(selection.head_sha, "head-sha");
        assert_eq!(selection.scope, Scope::Affected);
        assert_eq!(
            selection.units,
            ["app", "base"].into_iter().map(str::to_owned).collect()
        );
        assert_eq!(
            selection.full_units,
            ["app"].into_iter().map(str::to_owned).collect()
        );
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn prerequisite_tier_rewrites_rust_clippy_to_check() {
        let unit = CiUnit {
            id: "rust-app".to_owned(),
            label: "rust-app".to_owned(),
            kind: "rust".to_owned(),
            root: ".".to_owned(),
            watch: vec!["crates/app/**".to_owned()],
            github_pr_commands: vec![
                "cargo fmt --check".to_owned(),
                "cargo clippy --locked --no-deps --all-targets -- -D warnings".to_owned(),
                "cargo nextest run --locked".to_owned(),
            ],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec![
                "mbx clippy --locked --no-deps --all-targets -- -D warnings".to_owned()
            ],
            velnor_full_commands: vec!["true".to_owned()],
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
        };
        assert_eq!(
            prerequisite_commands(&unit, RunnerLane::Github, Scope::Affected),
            vec!["cargo check --locked --no-deps --all-targets"]
        );
        assert_eq!(
            prerequisite_commands(&unit, RunnerLane::Velnor, Scope::Affected),
            vec!["mbx check --locked --no-deps --all-targets"]
        );
    }

    #[test]
    fn affected_selection_expands_dependency_and_dependent_closure() -> Result<(), Box<dyn Error>> {
        let (root, base, head) = selection_git_fixture("closure", "crates/base/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            ["app", "base", "consumer"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        std::fs::remove_dir_all(root)?;

        let (root, base, head) = selection_git_fixture("prerequisite", "crates/app/src/lib.rs")?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            ["app", "consumer"].into_iter().map(str::to_owned).collect()
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_falls_back_to_full_for_global_or_unmatched_changes(
    ) -> Result<(), Box<dyn Error>> {
        for (name, changed) in [
            ("github", ".github/workflows/ci.yml"),
            ("unmatched", "README.md"),
        ] {
            let (root, base, head) = selection_git_fixture(name, changed)?;
            let config = selection_config();
            let units = selected_units_for_diff(&root, &config, Scope::Affected, &base, &head)?;
            assert_eq!(selected_ids(units), vec!["base", "app", "consumer", "docs"]);
            std::fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn affected_selection_is_empty_for_an_empty_diff() -> Result<(), Box<dyn Error>> {
        let (root, base, _) = selection_git_fixture("empty", "crates/base/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &base)?;
        assert!(selection.units.is_empty());
        assert!(selection.full_units.is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_falls_back_to_full_for_a_stale_base() -> Result<(), Box<dyn Error>> {
        let (root, _, head) = selection_git_fixture("stale-base", "crates/base/src/lib.rs")?;
        let config = selection_config();
        let units = selected_units_for_diff(
            &root,
            &config,
            Scope::Affected,
            "0000000000000000000000000000000000000001",
            &head,
        )?;
        assert_eq!(selected_ids(units), vec!["base", "app", "consumer", "docs"]);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_uses_merge_base_for_a_stale_branch_base() -> Result<(), Box<dyn Error>> {
        let (root, base, head) = stale_base_selection_git_fixture()?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert!(!selection.full_units.contains("docs"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cargo_version_bump_uses_the_explicit_allowlist_ids() -> Result<(), Box<dyn Error>> {
        let (root, base, head) = lockfile_version_selection_git_fixture()?;
        let command_unit = |id: &str, watch: &[&str]| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: "rust".to_owned(),
            root: ".".to_owned(),
            watch: watch.iter().map(|value| (*value).to_owned()).collect(),
            github_pr_commands: vec!["true".to_owned()],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["true".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
        };
        let mut config = selection_config();
        config.workflow.version_bump_units = vec![
            "rust-runner".to_owned(),
            "rust-ctl".to_owned(),
            "docker".to_owned(),
        ];
        config.unit = vec![
            command_unit("rust-runner", &["crates/runner/**"]),
            command_unit("rust-ctl", &["crates/ctl/**"]),
            command_unit("docker", &["Dockerfile"]),
            command_unit("unrelated", &["unrelated/**"]),
        ];
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["rust-runner", "rust-ctl", "docker"]
        );
        assert_eq!(
            selection.full_units,
            ["docker", "rust-ctl", "rust-runner"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn policy_parsing_ignores_comments_and_literal_run_content() -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Comments
on: pull_request
# pull_request_target:
# uses: actions/checkout@v4
# runs-on: [self-hosted, velnor]
jobs:
  verify:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
      - run: |
          echo 'pull_request_target:'
          echo 'uses: actions/checkout@v4'
          echo 'runs-on: [self-hosted, velnor]'
";
        let root = policy_fixture("comments", workflow, "github")?;
        assert!(run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_rejects_structural_forbidden_trigger_and_external_reusable_workflow(
    ) -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Forbidden
on:
  pull_request_target:
jobs:
  call:
    uses: acme/ci/.github/workflows/ci.yml@0123456789012345678901234567890123456789
";
        let root = policy_fixture("forbidden", workflow, "github")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_allows_only_full_sha_for_immutable_policy_workflow() -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Policy caller
on: pull_request
jobs:
  policy:
    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@a1cbfcbe5ab179032e37125f0383cdcae8183c8c
    with:
      policy-revision: a1cbfcbe5ab179032e37125f0383cdcae8183c8c
";
        let root = policy_fixture("approved-policy", workflow, "github")?;
        assert!(run_policy(root)?);

        let workflow = r"
name: Policy caller
on: pull_request
jobs:
  policy:
    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@main
";
        let root = policy_fixture("floating-policy", workflow, "github")?;
        assert!(!run_policy(root)?);

        let workflow = r"
name: Policy caller
on: pull_request
jobs:
  policy:
    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@13f5567b0a5d2f61e9f47dcf11dc7d2f8b8d4a33
";
        let root = policy_fixture("wrong-policy-pin", workflow, "github")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_requires_the_base_owned_entrypoint_file() -> Result<(), Box<dyn Error>> {
        let root = policy_fixture(
            "missing-policy-entrypoint",
            "name: Valid\non: pull_request\njobs:\n  verify:\n    runs-on: ubuntu-24.04\n",
            "github",
        )?;
        std::fs::remove_file(root.join(".github/workflows/ci-policy.yml"))?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_allows_only_the_exact_base_owned_policy_entrypoint() -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Velnor workflow policy
on:
  pull_request_target:
    types: [opened, synchronize, reopened]
permissions:
  contents: read
jobs:
  policy:
    name: Policy
    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@a1cbfcbe5ab179032e37125f0383cdcae8183c8c
    with:
      policy-revision: a1cbfcbe5ab179032e37125f0383cdcae8183c8c
    permissions:
      contents: read
";
        let root = policy_fixture("approved-policy-entrypoint", workflow, "github")?;
        std::fs::rename(
            root.join(".github/workflows/policy.yml"),
            root.join(".github/workflows/ci-policy.yml"),
        )?;
        assert!(run_policy(root)?);

        let workflow = r"
name: Velnor workflow policy
on:
  pull_request_target:
    types: [opened, synchronize, reopened]
jobs:
  policy:
    name: Policy
    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@13f5567b0a5d2f61e9f47dcf11dc7d2f8b8d4a33
    permissions:
      contents: read
  bypass:
    runs-on: ubuntu-24.04
    steps:
      - run: true
";
        let root = policy_fixture("bypassed-policy-entrypoint", workflow, "github")?;
        std::fs::rename(
            root.join(".github/workflows/policy.yml"),
            root.join(".github/workflows/ci-policy.yml"),
        )?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_requires_full_sha_for_external_actions_and_allows_approved_local_calls(
    ) -> Result<(), Box<dyn Error>> {
        let valid = format!(
            "name: Valid\non: pull_request\njobs:\n  call:\n    uses: ./.github/workflows/ci-rust.yml\n  verify:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@{CHECKOUT_SHA}\n"
        );
        let root = policy_fixture("uses-valid", &valid, "github")?;
        assert!(run_policy(root)?);

        let invalid = "name: Invalid\non: pull_request\njobs:\n  verify:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n";
        let root = policy_fixture("uses-invalid", invalid, "github")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_resolves_static_matrix_runner_and_preserves_trusted_self_hosted_gate(
    ) -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Matrix
on: push
jobs:
  verify:
    if: ${{ github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') }}
    strategy:
      matrix:
        include:
          - runner: [self-hosted, velnor]
    runs-on: ${{ matrix.runner }}
";
        let root = policy_fixture("matrix-trusted", workflow, "github")?;
        assert!(run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_rejects_untrusted_matrix_self_hosted_runner() -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Matrix
on: pull_request
jobs:
  verify:
    strategy:
      matrix:
        include:
          - runner: [self-hosted, velnor]
    runs-on: ${{ matrix.runner }}
";
        let root = policy_fixture("matrix-untrusted", workflow, "github")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_rejects_noncanonical_gate_that_mentions_trusted_fragments(
    ) -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: SpoofedGate
on: pull_request
jobs:
  verify:
    if: ${{ github.event_name == 'pull_request' || (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')) }}
    runs-on: [self-hosted, velnor]
";
        let root = policy_fixture("spoofed-gate", workflow, "velnor")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_rejects_unresolved_dynamic_runner_even_when_matrix_is_present(
    ) -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Dynamic
on: pull_request
jobs:
  verify:
    strategy:
      matrix:
        include:
          - runner: ${{ inputs.runner }}
    runs-on: ${{ matrix.runner }}
";
        let root = policy_fixture("matrix-dynamic", workflow, "velnor")?;
        assert!(!run_policy(root)?);

        let workflow = r"
name: Dynamic
on: pull_request
jobs:
  verify:
    runs-on: ${{ needs.select.outputs.runner }}
";
        let root = policy_fixture("runner-dynamic", workflow, "velnor")?;
        assert!(!run_policy(root)?);
        Ok(())
    }
}

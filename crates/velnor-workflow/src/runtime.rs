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

use super::primitives::snapshot::{
    plan_evictions, CacheEntry as SnapshotCacheEntry, RetentionPolicy,
};
use super::{lanes_support_unit_kind, GeneratorError, RunnerMode, UnitKind};

const DEFAULT_CONFIG: &str = ".github/ci/project.toml";
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
    automatic: String,
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

#[expect(
    dead_code,
    reason = "runtime preserves the complete generated workflow contract while execution consumes selected fields"
)]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Workflow {
    github_runner: String,
    #[serde(default)]
    macos_runner: String,
    velnor_labels: Vec<String>,
    files: Vec<String>,
    notes: Vec<String>,
    #[serde(default)]
    version_bump_units: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct VelnorPolicyContract {
    runners: String,
    default_branch: String,
    velnor_labels: Vec<String>,
    velnor_runner_group: Option<String>,
    pull_request_on_velnor: bool,
}

impl VelnorPolicyContract {
    fn requires_approved_runner(&self) -> bool {
        self.pull_request_on_velnor
    }

    fn approved_runner_configured(&self) -> bool {
        let labels = self
            .velnor_labels
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        super::estate::approved_velnor_runner_contract_matches(
            &labels,
            self.velnor_runner_group.as_deref(),
        )
    }

    fn configured_runner(&self) -> String {
        let labels = self
            .velnor_labels
            .iter()
            .map(|label| super::yaml_scalar(label))
            .collect::<Vec<_>>()
            .join(", ");
        let labels = format!("[{labels}]");
        self.velnor_runner_group
            .as_deref()
            .map_or(labels.clone(), |group| {
                format!(
                    "{{ group: {}, labels: {labels} }}",
                    super::yaml_scalar(group)
                )
            })
    }
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
        "cache-plan" => {
            let options = parse_options(&arguments[1..], &["entries", "now", "mode"])?;
            let mode = options.get("mode").map_or("plan", String::as_str);
            if !matches!(mode, "plan" | "budget") {
                return Err(GeneratorError::usage(format!(
                    "unsupported cache-plan mode: {mode}; use --mode=plan or --mode=budget"
                )));
            }
            if mode == "budget" {
                println!("{}", RetentionPolicy::default_policy().total_bytes);
                return Ok(true);
            }
            cache_plan(
                options.get("entries").map(String::as_str),
                options.get("now").map(String::as_str),
            )?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// One Actions cache entry as the maintenance job collects it from the API.
#[derive(Deserialize)]
struct CacheEntryRecord {
    #[serde(deserialize_with = "deserialize_cache_id")]
    id: String,
    key: String,
    size_in_bytes: u64,
    created_at: String,
}

/// Accept the Actions API's numeric cache id and carry it losslessly as the
/// planner's string identity. The API has shipped both JSON shapes; treating
/// its current numeric encoding as invalid turns retention into a cold-cache
/// failure instead of the bounded maintenance it exists to perform.
fn deserialize_cache_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CacheIdVisitor;

    impl serde::de::Visitor<'_> for CacheIdVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a numeric or string Actions cache id")
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value.to_owned())
        }
    }

    deserializer.deserialize_any(CacheIdVisitor)
}

/// Compute the retention eviction plan for the Actions cache account: the same
/// [`RetentionPolicy`] and the same `plan_evictions` the generator's tests
/// exercise, run against the account snapshot the maintenance job collects.
/// The plan is the workflow's only eviction decision — the job applies it
/// verbatim and records every eviction's class and reason in the summary.
///
/// `--now` pins the clock for tests; a live run uses the system clock.
fn cache_plan(entries_path: Option<&str>, now: Option<&str>) -> Result<(), GeneratorError> {
    let entries_text = if let Some(path) = entries_path {
        fs::read_to_string(path).map_err(|error| {
            GeneratorError::io("read cache account snapshot", Path::new(path), &error)
        })?
    } else {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).map_err(|error| {
            GeneratorError::usage(format!("read cache account snapshot: {error}"))
        })?;
        text
    };
    let records: Vec<CacheEntryRecord> = serde_json::from_str(&entries_text).map_err(|error| {
        GeneratorError::usage(format!(
            "the cache account snapshot is not an array of cache records: {error}"
        ))
    })?;
    let entries = records
        .into_iter()
        .map(|record| SnapshotCacheEntry {
            id: record.id,
            key: record.key,
            size_in_bytes: record.size_in_bytes,
            created_at: record.created_at,
        })
        .collect::<Vec<_>>();
    let now_epoch = match now {
        Some(pinned) => parse_pinned_epoch(pinned)?,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| GeneratorError::usage(format!("resolve current time: {error}")))?
            .as_secs()
            .cast_signed(),
    };
    let plan = plan_evictions(&entries, &RetentionPolicy::default_policy(), now_epoch);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, &plan)
        .map_err(|error| GeneratorError::usage(format!("write eviction plan: {error}")))?;
    writeln!(handle)
        .map_err(|error| GeneratorError::usage(format!("write eviction plan: {error}")))?;
    Ok(())
}

/// Parse the pinned `--now` clock as seconds since the epoch, so a test run
/// names a reproducible instant.
fn parse_pinned_epoch(pinned: &str) -> Result<i64, GeneratorError> {
    pinned.parse::<i64>().map_err(|_| {
        GeneratorError::usage(format!(
            "unsupported --now value {pinned}: name seconds since the epoch"
        ))
    })
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
    let lanes = plan_lanes()?;
    let selection = selection_for_lanes(
        &config,
        selection_for_diff(&root, &config, scope, &base, &head)?,
        lanes,
    );
    let units = selection
        .units
        .iter()
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
        write_kind_matrices(&mut file, &config, &selection, &output_path)?;
    }
    println!("scope={}", scope_name(scope));
    println!("units={units}");
    println!("full_units={full_units}");
    Ok(())
}

fn write_kind_matrices(
    file: &mut fs::File,
    config: &CiConfig,
    selection: &UnitSelection<'_>,
    output_path: &Path,
) -> Result<(), GeneratorError> {
    let selected = selection
        .units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut kinds = BTreeSet::new();
    for unit in &config.unit {
        if !unit.kind.is_empty() {
            kinds.insert(unit.kind.as_str());
        }
    }
    for kind in kinds {
        let entries = config
            .unit
            .iter()
            .filter(|unit| unit.kind == kind && selected.contains(unit.id.as_str()))
            .map(|unit| {
                let label = if unit.label.is_empty() {
                    unit.id.clone()
                } else {
                    unit.label.clone()
                };
                serde_json::json!({ "unit": unit.id, "label": label })
            })
            .collect::<Vec<_>>();
        let json = serde_json::to_string(&entries)
            .map_err(|error| GeneratorError::usage(format!("serialize {kind} matrix: {error}")))?;
        writeln!(file, "{kind}_matrix={json}")
            .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
    }
    Ok(())
}

fn scope_for_event() -> Result<Option<String>, GeneratorError> {
    let event = env::var("EVENT_NAME").unwrap_or_default();
    let override_scope = env::var("CI_SCOPE_OVERRIDE")
        .ok()
        .filter(|value| !value.is_empty());
    scope_for_event_values(&event, override_scope.as_deref())
}

pub(crate) fn scope_for_event_values(
    event: &str,
    override_scope: Option<&str>,
) -> Result<Option<String>, GeneratorError> {
    match event {
        // Merge-queue validation is a trusted event like push and schedule:
        // the ephemeral merge ref has no pull_request base to diff against,
        // so it always runs full scope.
        "push" | "schedule" | "merge_group" => {
            if override_scope.is_some_and(|scope| scope != "full") {
                return Err(GeneratorError::usage(
                    "trusted events require full CI scope",
                ));
            }
            Ok(Some("full".to_owned()))
        }
        "workflow_dispatch" => match override_scope {
            None | Some("full") => Ok(Some("full".to_owned())),
            Some("affected") => Ok(Some("affected".to_owned())),
            Some(other) => Err(GeneratorError::usage(format!(
                "unsupported CI scope override `{other}`"
            ))),
        },
        "pull_request" => Ok(override_scope
            .map(ToOwned::to_owned)
            .or_else(|| Some("affected".to_owned()))),
        "" => Ok(override_scope.map(ToOwned::to_owned)),
        other => Err(GeneratorError::usage(format!(
            "unsupported CI event `{other}`"
        ))),
    }
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Affected => "affected",
        Scope::Full => "full",
    }
}

/// The lanes admitted for this plan, from the `VELNOR_LANES` environment the
/// Both-mode plan step sets to `needs.lane-admission.outputs.lanes`.
/// Absent or empty means `both`: single-lane aggregates and local runs plan
/// without lane filtering, exactly as before.
fn plan_lanes() -> Result<RunnerMode, GeneratorError> {
    plan_lanes_for_value(env::var("VELNOR_LANES").unwrap_or_default().as_str())
}

fn plan_lanes_for_value(value: &str) -> Result<RunnerMode, GeneratorError> {
    match value.trim() {
        "" | "both" => Ok(RunnerMode::Both),
        "velnor" => Ok(RunnerMode::Velnor),
        "github" => Ok(RunnerMode::Github),
        other => Err(GeneratorError::usage(format!(
            "unsupported CI lanes `{other}`: use velnor, github, or both"
        ))),
    }
}

/// Narrow a diff selection to the units the admitted `lanes` can execute.
/// A velnor-only dispatch drops whatever the Velnor lane cannot run (Swift
/// units); the required gate already tolerates `skipped` for unselected
/// units, so the excluded callers stay green instead of failing a selection
/// they can never satisfy. Units of an unknown kind are kept: dropping a
/// unit the planner does not recognize would silently skip verification.
fn selection_for_lanes<'a>(
    config: &'a CiConfig,
    selection: UnitSelection<'a>,
    lanes: RunnerMode,
) -> UnitSelection<'a> {
    let kinds = config
        .unit
        .iter()
        .map(|unit| (unit.id.as_str(), unit.kind.as_str()))
        .collect::<BTreeMap<_, _>>();
    let supported = |kind: &str| {
        UnitKind::from_prefix(kind).is_none_or(|parsed| lanes_support_unit_kind(lanes, parsed))
    };
    UnitSelection {
        units: selection
            .units
            .into_iter()
            .filter(|unit| supported(unit.kind.as_str()))
            .collect(),
        full_units: selection
            .full_units
            .into_iter()
            .filter(|id| kinds.get(id.as_str()).is_none_or(|kind| supported(kind)))
            .collect(),
    }
}

#[cfg(test)]
mod scope_event_tests {
    use super::scope_for_event_values;
    use crate::GeneratorError;

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must<T>(result: Result<T, GeneratorError>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> GeneratorError {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error,
        }
    }

    #[test]
    fn merge_group_is_a_trusted_full_scope_event() {
        assert_eq!(
            must(scope_for_event_values("merge_group", None), "resolve scope").as_deref(),
            Some("full")
        );
        assert_eq!(
            must(
                scope_for_event_values("merge_group", Some("full")),
                "resolve explicit full scope"
            )
            .as_deref(),
            Some("full")
        );
        let error = must_fail(
            scope_for_event_values("merge_group", Some("affected")),
            "merge_group must reject a narrowed scope",
        );
        assert!(
            error
                .to_string()
                .contains("trusted events require full CI scope"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_events_are_rejected_and_empty_keeps_local_passthrough() {
        let error = must_fail(
            scope_for_event_values("release", None),
            "unknown events must fail closed",
        );
        assert!(
            error.to_string().contains("unsupported CI event `release`"),
            "unexpected error: {error}"
        );
        assert_eq!(
            must(scope_for_event_values("", None), "resolve empty scope"),
            None
        );
        assert_eq!(
            must(
                scope_for_event_values("", Some("affected")),
                "resolve empty override"
            )
            .as_deref(),
            Some("affected")
        );
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
    let selection_file = env::var_os("VELNOR_SELECTION_FILE").map_or_else(
        || root.join(".velnor-ci-selection/velnor-ci-selection"),
        PathBuf::from,
    );
    run_units_with_selection_file(root, config_path, scope, only_unit, &selection_file)
}

pub(crate) fn run_units_with_selection_file(
    root: &Path,
    config_path: &Path,
    scope: Scope,
    only_unit: Option<&str>,
    selection_file: &Path,
) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    if matches!(env::var("EVENT_NAME").as_deref(), Ok("push" | "schedule")) && scope != Scope::Full
    {
        return Err(GeneratorError::usage(
            "trusted events require full CI scope",
        ));
    }
    let selection = read_selection_file(selection_file)?;
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
    let full_units = selection.full_units;
    let selected = select_units_for_job(selected, only_unit)?;
    run_layers(root, &selected, scope, &full_units)
}

fn select_units_for_job<'a>(
    selected: Vec<&'a CiUnit>,
    only_unit: Option<&str>,
) -> Result<Vec<&'a CiUnit>, GeneratorError> {
    match only_unit {
        Some(id) => {
            if !selected.iter().any(|unit| unit.id == id) {
                return Err(GeneratorError::usage(format!(
                    "CI selection artifact does not include requested unit `{id}`"
                )));
            }
            Ok(selected.into_iter().filter(|unit| unit.id == id).collect())
        }
        None => Ok(selected),
    }
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

fn full_selection(config: &CiConfig) -> Result<UnitSelection<'_>, GeneratorError> {
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
        let unit_id = format!("rust-{crate_name}");
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
            let command = command
                .replacen(" clippy ", " check ", 1)
                .replace(" -- -D warnings", "");
            if lane == RunnerLane::Velnor {
                // `mbx check` does not expose Cargo's `--no-deps` flag.
                command.replace(" --no-deps", "")
            } else {
                command
            }
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
        command.arg(if nextest { "nextest" } else { "test" });
        if nextest {
            command.arg("run");
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
    let velnor_policy = configured_velnor_policy(root)?;
    let mut found_policy_entrypoint = false;
    let mut failures = PolicyFindings::default();
    // GitHub rejects a workflow whose YAML carries duplicate keys, so the
    // auditor must fail closed on exactly the inputs GitHub refuses instead of
    // silently auditing the last-key-wins rewrite of them.
    let parser = serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
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
        let document: Value =
            serde_yaml::from_str_with_config(&content, &parser).map_err(|error| {
                GeneratorError::usage(format!("parse workflow {}: {error}", path.display()))
            })?;
        let Some(workflow) = document.as_mapping() else {
            failures.record(&path, "workflow document must be a YAML mapping");
            continue;
        };
        inspect_workflow(
            workflow,
            &path,
            trusted_revision,
            &velnor_policy,
            &mut failures,
        );
    }
    if !found_policy_entrypoint {
        failures.record(
            &policy_entrypoint,
            "required base-owned ci-policy.yml entrypoint is missing",
        );
    }
    if !failures.lines.is_empty() {
        return Err(GeneratorError::usage(format!(
            "workflow policy rejected {} finding(s):\n{}",
            failures.lines.len(),
            failures.lines.join("\n")
        )));
    }
    Ok(())
}

fn inspect_workflow(
    workflow: &Mapping,
    path: &Path,
    trusted_revision: &str,
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    let approved_policy_entrypoint =
        is_approved_policy_entrypoint(path, workflow, trusted_revision, velnor_policy);
    for (key, value) in workflow {
        let key = key.as_str();
        match key {
            "on" => {
                if contains_exact_yaml_value(value, "pull_request_target")
                    && !approved_policy_entrypoint
                {
                    failures.record(path, "pull_request_target is forbidden");
                }
            }
            "jobs" => inspect_jobs(value, path, trusted_revision, velnor_policy, failures),
            _ => inspect_yaml_value(
                value,
                path,
                None,
                false,
                trusted_revision,
                velnor_policy.requires_approved_runner(),
                failures,
            ),
        }
    }
}

fn is_approved_policy_entrypoint(
    path: &Path,
    workflow: &Mapping,
    trusted_revision: &str,
    velnor_policy: &VelnorPolicyContract,
) -> bool {
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
    jobs.len() == 1 && is_approved_inline_policy_job(policy, trusted_revision, velnor_policy)
}

/// Whether a job is exactly the inline policy job the generator renders for
/// `revision`: same key set, same runner, same permissions, and the same four
/// steps, compared as parsed YAML against the generator's own rendering. Any
/// drift — a changed pin, an extra step, a rewritten install command — fails
/// the comparison, so a tree can only carry the audited transport.
fn is_approved_inline_policy_job(
    job: &Mapping,
    trusted_revision: &str,
    velnor_policy: &VelnorPolicyContract,
) -> bool {
    let parser = serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
    crate::POLICY_JOB_NAMES.iter().any(|name| {
        let hosted = format!(
            "jobs:\n{}",
            crate::inline_policy_job(name, trusted_revision)
        );
        let Ok(parsed) = serde_yaml::from_str_with_config::<Value>(&hosted, &parser) else {
            return false;
        };
        let Some(canonical) = parsed
            .get("jobs")
            .and_then(Value::as_mapping)
            .and_then(|jobs| jobs.get("policy"))
        else {
            return false;
        };
        if canonical == &Value::Mapping(job.to_owned()) {
            return true;
        }

        // Velnor policy is intentionally a separate approved shape: it uses
        // the local cache backend and a self-hosted runner, and the safe
        // event gate is mandatory. Runner labels and the default branch are
        // repository configuration, so compare those two lane fields through
        // the generic static-runner/trusted-gate validators below while the
        // remaining job structure stays an exact generator comparison.
        if !mapping_value(job, "if")
            .and_then(Value::as_str)
            .is_some_and(|condition| has_safe_runner_gate(condition, job, velnor_policy))
        {
            return false;
        }
        if !is_static_self_hosted_runner(job, velnor_policy) {
            return false;
        }

        let canonical_gate = format!(
            "    if: ${{{{ github.event_name == 'pull_request_target' || (github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')) }}}}\n",
            if velnor_policy.default_branch.is_empty() {
                "main"
            } else {
                &velnor_policy.default_branch
            }
        );
        let runner = velnor_policy.configured_runner();
        let velnor = format!(
            "jobs:\n{}",
            crate::inline_policy_job_for_lane(
                name,
                trusted_revision,
                &runner,
                "local",
                Some(&canonical_gate),
            )
        );
        let Ok(parsed) = serde_yaml::from_str_with_config::<Value>(&velnor, &parser) else {
            return false;
        };
        let Some(canonical) = parsed
            .get("jobs")
            .and_then(Value::as_mapping)
            .and_then(|jobs| jobs.get("policy"))
        else {
            return false;
        };

        strip_inline_policy_lane_fields(canonical)
            == strip_inline_policy_lane_fields(&Value::Mapping(job.to_owned()))
    })
}

fn is_static_self_hosted_runner(job: &Mapping, velnor_policy: &VelnorPolicyContract) -> bool {
    let Some(runs_on) = mapping_value(job, "runs-on") else {
        return false;
    };
    let mut resolving = BTreeSet::new();
    let analysis = analyze_runner(runs_on, None, &mut resolving);
    analysis.self_hosted
        && !analysis.dynamic
        && !analysis.invalid
        && (!velnor_policy.requires_approved_runner() || is_approved_velnor_runner(runs_on))
}

fn has_safe_runner_gate(
    condition: &str,
    job: &Mapping,
    velnor_policy: &VelnorPolicyContract,
) -> bool {
    if has_trusted_runner_gate(condition) {
        return true;
    }
    velnor_policy.pull_request_on_velnor
        && is_generated_velnor_pr_gate(condition, &velnor_policy.default_branch)
        && is_static_self_hosted_runner(job, velnor_policy)
}

fn generation_workflow(root: &Path) -> Result<Option<toml::Value>, GeneratorError> {
    let path = root.join(".github-gen/velnor-workflow.toml");
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GeneratorError::io(
                "read generation workflow config",
                &path,
                &error,
            ));
        }
    };
    let value = toml::from_str::<toml::Value>(&content).map_err(|error| {
        GeneratorError::usage(format!("parse workflow config {}: {error}", path.display()))
    })?;
    Ok(value.get("workflow").cloned())
}

fn configured_velnor_policy(root: &Path) -> Result<VelnorPolicyContract, GeneratorError> {
    let path = root.join(DEFAULT_CONFIG);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(VelnorPolicyContract::default());
        }
        Err(error) => return Err(GeneratorError::io("read workflow config", &path, &error)),
    };
    let value = toml::from_str::<toml::Value>(&content).map_err(|error| {
        GeneratorError::usage(format!("parse workflow config {}: {error}", path.display()))
    })?;
    let runtime_workflow = value.get("workflow").and_then(toml::Value::as_table);
    let labels = runtime_workflow
        .and_then(|workflow| workflow.get("velnor_labels"))
        .map(|labels| {
            labels
                .as_array()
                .ok_or_else(|| GeneratorError::usage("[workflow] velnor_labels must be an array"))?
                .iter()
                .map(|label| {
                    label.as_str().map(str::to_owned).ok_or_else(|| {
                        GeneratorError::usage("[workflow] velnor_labels must contain only strings")
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let generation_workflow = generation_workflow(root)?;
    let generation_workflow = generation_workflow.as_ref().and_then(toml::Value::as_table);
    let group = generation_workflow
        .and_then(|workflow| workflow.get("velnor_runner_group"))
        .map(|group| {
            group.as_str().map(str::to_owned).ok_or_else(|| {
                GeneratorError::usage("[workflow] velnor_runner_group must be a string")
            })
        })
        .transpose()?;
    let pull_request_on_velnor = generation_workflow
        .and_then(|workflow| workflow.get("pull_request_on_velnor"))
        .map(|enabled| {
            enabled.as_bool().ok_or_else(|| {
                GeneratorError::usage("[workflow] pull_request_on_velnor must be a boolean")
            })
        })
        .transpose()?
        .unwrap_or(false);
    let policy = VelnorPolicyContract {
        runners: value
            .get("runners")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        default_branch: value
            .get("default_branch")
            .and_then(toml::Value::as_str)
            .unwrap_or("main")
            .to_owned(),
        velnor_labels: labels,
        velnor_runner_group: group,
        pull_request_on_velnor,
    };
    if policy.pull_request_on_velnor
        && (!matches!(policy.runners.as_str(), "velnor" | "both")
            || !policy.approved_runner_configured())
    {
        return Err(GeneratorError::usage(
            "workflow policy rejected pull_request_on_velnor: config must declare the exact approved Velnor runner group and labels and include the Velnor lane",
        ));
    }
    Ok(policy)
}

fn normalize_gate_expression(value: &str) -> String {
    // Both-mode aggregates name the manual lane selector `lanes` while
    // single-lane modes name it `runner`; the gate shape the policy matchers
    // below recognize is identical, so canonicalize the spelling first.
    let value = value.replace("github.event.inputs.lanes", "github.event.inputs.runner");
    let value = value.trim();
    let value = value
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map_or(value, str::trim)
        .split_whitespace()
        .collect::<String>();
    if let Some(inner) = value
        .strip_prefix("always()&&(")
        .and_then(|value| value.strip_suffix(')'))
    {
        inner.to_owned()
    } else if let Some(inner) = value.strip_prefix("always()&&") {
        inner.to_owned()
    } else {
        value
    }
}

fn is_generated_velnor_pr_gate(value: &str, default_branch: &str) -> bool {
    let normalized = normalize_gate_expression(value);
    let value = strip_reusable_unit_selector(&normalized).unwrap_or(&normalized);
    if !valid_branch(default_branch) {
        return false;
    }
    let automatic = format!(
        "github.event_name=='pull_request'&&github.event.pull_request.head.repo.full_name==github.repository||(github.ref=='refs/heads/{default_branch}'&&(github.event_name=='push'||github.event_name=='schedule'))"
    );
    let explicit_dispatch = format!(
        "(github.ref=='refs/heads/{default_branch}'&&(github.event_name=='workflow_dispatch'&&(github.event.inputs.runner=='velnor'||github.event.inputs.runner=='both')))"
    );
    let default_dispatch = format!(
        "(github.ref=='refs/heads/{default_branch}'&&(github.event_name=='workflow_dispatch'&&(github.event.inputs.runner=='velnor'||github.event.inputs.runner=='both'||github.event.inputs.runner=='')))"
    );
    value == format!("{automatic}||{explicit_dispatch}")
        || value == format!("{automatic}||{default_dispatch}")
}

fn strip_inline_policy_lane_fields(value: &Value) -> Value {
    let Some(mapping) = value.as_mapping() else {
        return value.clone();
    };
    let mut stripped = Mapping::new();
    for (key, value) in mapping {
        if key != "if" && key != "runs-on" {
            stripped.insert(key.clone(), value.clone());
        }
    }
    Value::Mapping(stripped)
}

/// Whether a job claims the inline policy transport. The first step's name is
/// generator-owned, so anything wearing it must be the exact approved job;
/// anything else is judged by the generic rules.
fn claims_inline_policy_transport(job: &Mapping) -> bool {
    mapping_value(job, "steps")
        .and_then(Value::as_sequence)
        .and_then(|steps| steps.first())
        .and_then(Value::as_mapping)
        .and_then(|step| mapping_value(step, "name").and_then(Value::as_str))
        == Some("Checkout caller workflow data")
}

fn inspect_jobs(
    value: &Value,
    path: &Path,
    trusted_revision: &str,
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    let Some(jobs) = value.as_mapping() else {
        failures.record(path, "jobs must be a YAML mapping");
        return;
    };
    for (job_id, job) in jobs {
        let Some(job) = job.as_mapping() else {
            let name = job_id.as_str();
            failures.record(path, &format!("job {name} must be a YAML mapping"));
            continue;
        };
        if claims_inline_policy_transport(job)
            && !is_approved_inline_policy_job(job, trusted_revision, velnor_policy)
        {
            failures.record(path, &format!(
                    "inline policy job must match the approved generated shape for revision {trusted_revision}"
                ));
        }
        let trusted_gate = mapping_value(job, "if")
            .and_then(Value::as_str)
            .is_some_and(|condition| has_safe_runner_gate(condition, job, velnor_policy));
        let matrix = mapping_value(job, "strategy")
            .and_then(Value::as_mapping)
            .and_then(|strategy| mapping_value(strategy, "matrix"))
            .and_then(Value::as_mapping);
        inspect_mapping(
            job,
            path,
            matrix,
            trusted_gate,
            trusted_revision,
            velnor_policy.requires_approved_runner(),
            failures,
        );
    }
}

fn inspect_yaml_value(
    value: &Value,
    path: &Path,
    matrix: Option<&Mapping>,
    trusted_gate: bool,
    trusted_revision: &str,
    require_approved_runner: bool,
    failures: &mut PolicyFindings,
) {
    match value {
        Value::Mapping(mapping) => {
            inspect_mapping(
                mapping,
                path,
                matrix,
                trusted_gate,
                trusted_revision,
                require_approved_runner,
                failures,
            );
        }
        Value::Sequence(sequence) => {
            for item in sequence {
                inspect_yaml_value(
                    item,
                    path,
                    matrix,
                    trusted_gate,
                    trusted_revision,
                    require_approved_runner,
                    failures,
                );
            }
        }
        Value::Tagged(tagged) => {
            inspect_yaml_value(
                tagged.value(),
                path,
                matrix,
                trusted_gate,
                trusted_revision,
                require_approved_runner,
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
    require_approved_runner: bool,
    failures: &mut PolicyFindings,
) {
    for (key, value) in mapping {
        let key = key.as_str();
        match key {
            "pull_request_target" => {
                failures.record(path, "pull_request_target is forbidden");
            }
            "uses" => inspect_uses(value, path, failures),
            "runs-on" => inspect_runner(
                value,
                path,
                matrix,
                trusted_gate,
                require_approved_runner,
                failures,
            ),
            _ => inspect_yaml_value(
                value,
                path,
                matrix,
                trusted_gate,
                trusted_revision,
                require_approved_runner,
                failures,
            ),
        }
    }
}

fn inspect_uses(value: &Value, path: &Path, failures: &mut PolicyFindings) {
    let Some(action) = value.as_str() else {
        failures.record(path, "uses must be a scalar reference");
        return;
    };
    if is_approved_local_reusable(action) {
        return;
    }
    if is_approved_local_action(action) {
        return;
    }
    let reference_path = action.split_once('@').map_or(action, |(path, _)| path);
    if reference_path.contains("/.github/workflows/") {
        if is_approved_fleet_reusable(action) {
            return;
        }
        if action.starts_with("./") && action.contains('@') {
            failures.record(
                path,
                &format!(
                    "same-repository reusable workflow call at a pinned revision wedges push/schedule scheduling; inline the approved steps instead: {action}"
                ),
            );
        } else {
            failures.record(
                path,
                &format!(
                    "reusable workflow must be an approved local generated workflow: {action}"
                ),
            );
        }
    } else if !is_full_sha_reference(action) {
        failures.record(path, &format!("action is not a full SHA pin: {action}"));
    }
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

/// A repository-local composite action, pinned by the audited tree itself:
/// the policy audits the pull-request head tree, so a tampered local action
/// is reviewed code exactly like an inline `run:` step, and a tampered
/// reference outside `.github/actions/` stays rejected below.
fn is_approved_local_action(value: &str) -> bool {
    let Some(path) = value.strip_prefix("./.github/actions/") else {
        return false;
    };
    !path.is_empty()
        && !path.contains('@')
        && !path.contains('\\')
        && !path.split('/').any(|segment| segment == "..")
}

fn is_approved_fleet_reusable(value: &str) -> bool {
    let Some((path, reference)) = value.split_once('@') else {
        return false;
    };
    if !is_full_sha(reference) {
        return false;
    }
    let mut segments = path.split('/');
    let (Some(owner), Some(repository), Some(dot_github), Some(workflows), Some(file)) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return false;
    };
    segments.next().is_none()
        && crate::estate::FLEET_VELNOR_ACTION_OWNERS.contains(&owner)
        && repository == "velnor-actions"
        && dot_github == ".github"
        && workflows == "workflows"
        && !file.is_empty()
        && Path::new(file)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("yml"))
        && !file.contains('\\')
        && !file.contains("..")
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
    require_approved_runner: bool,
    failures: &mut PolicyFindings,
) {
    let mut resolving = BTreeSet::new();
    let analysis = analyze_runner(value, matrix, &mut resolving);
    if analysis.invalid {
        failures.record(
            path,
            "runs-on must contain only static labels or a static runner-group mapping",
        );
    }
    if analysis.dynamic {
        failures.record(
            path,
            "runs-on contains an unresolved or dynamic runner label",
        );
    }
    if analysis.self_hosted && !trusted_gate {
        failures.record(
            path,
            "self-hosted jobs require a default-branch trusted-event gate",
        );
    }
    if require_approved_runner && analysis.self_hosted && !is_approved_velnor_runner(value) {
        failures.record(
            path,
            "opt-in Velnor self-hosted jobs must use the exact approved runner group and labels",
        );
    }
}

fn is_approved_velnor_runner(value: &Value) -> bool {
    let Some(runner) = value.as_mapping() else {
        return false;
    };
    let Some(group) = mapping_value(runner, "group").and_then(Value::as_str) else {
        return false;
    };
    let Some(labels) = mapping_value(runner, "labels").and_then(Value::as_sequence) else {
        return false;
    };
    let Some(labels) = labels.iter().map(Value::as_str).collect::<Option<Vec<_>>>() else {
        return false;
    };
    runner.len() == 2
        && super::estate::approved_velnor_runner_contract_matches(&labels, Some(group))
}

fn normalize_runner_expression(value: &str) -> String {
    value
        .trim()
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map_or_else(
            || value.split_whitespace().collect(),
            |inner| inner.split_whitespace().collect(),
        )
}

fn is_approved_dynamic_runner(label: &str) -> bool {
    let normalized = normalize_runner_expression(label);
    crate::estate::APPROVED_DYNAMIC_RUNNERS
        .iter()
        .any(|shape| normalized == **shape)
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
                if is_approved_dynamic_runner(label) {
                    RunnerAnalysis::default()
                } else {
                    RunnerAnalysis {
                        dynamic: true,
                        ..RunnerAnalysis::default()
                    }
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
            let mut result = RunnerAnalysis::default();
            let Some(group) = mapping_value(mapping, "group") else {
                return RunnerAnalysis {
                    invalid: true,
                    ..result
                };
            };
            match group {
                Value::String(group) if !group.is_empty() && !group.contains("${{") => {}
                Value::String(_) => result.dynamic = true,
                _ => result.invalid = true,
            }
            for (key, value) in mapping {
                match key.as_str() {
                    "group" => {}
                    "labels" => result.merge(analyze_runner(value, matrix, resolving)),
                    _ => result.invalid = true,
                }
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
    // Both-mode aggregates name the manual lane selector `lanes` while
    // single-lane modes name it `runner`; canonicalize first, exactly like
    // `normalize_gate_expression`, so adopted-estate `lanes` gates match the
    // same trusted shapes.
    let value = value
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map_or(value, str::trim)
        .split_whitespace()
        .collect::<String>()
        .replace("github.event.inputs.lanes", "github.event.inputs.runner");
    if value == "github.event_name=='pull_request_target'"
        || value == "always()&&github.event_name=='pull_request_target'"
    {
        return true;
    }
    if let Some(trusted) = value
        .strip_prefix("github.event_name=='pull_request_target'||(")
        .and_then(|value| value.strip_suffix(')'))
    {
        return has_trusted_runner_gate(trusted);
    }
    if let Some(trusted) = strip_reusable_unit_selector(&value) {
        return has_trusted_runner_gate(trusted);
    }
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
    let velnor_lane_gate = format!(
        "github.ref=='refs/heads/{branch}'&&(github.event_name=='push'||github.event_name=='schedule'||(github.event_name=='workflow_dispatch'&&(github.event.inputs.runner=='velnor'||github.event.inputs.runner=='both')))"
    );
    let velnor_dispatch_only_gate = format!(
        "github.ref=='refs/heads/{branch}'&&github.event_name=='workflow_dispatch'&&(github.event.inputs.runner=='velnor'||github.event.inputs.runner=='both')"
    );
    value == ci_gate
        || value == format!("always()&&{ci_gate}")
        || value == velnor_lane_gate
        || value == format!("always()&&{velnor_lane_gate}")
        || value == velnor_dispatch_only_gate
        || value == format!("always()&&{velnor_dispatch_only_gate}")
        || value == release_gate
        || value
            .strip_suffix(&format!("&&{ci_gate}"))
            .is_some_and(is_safe_trusted_gate_conjunction)
        || value
            .strip_prefix(&format!("{ci_gate}&&"))
            .is_some_and(is_safe_trusted_gate_conjunction)
        || value
            .strip_suffix(&format!("&&{release_gate}"))
            .is_some_and(is_safe_trusted_gate_conjunction)
        || value
            .strip_prefix(&format!("{release_gate}&&"))
            .is_some_and(is_safe_trusted_gate_conjunction)
}

fn strip_reusable_unit_selector(value: &str) -> Option<&str> {
    strip_inputs_unit_selector(value).or_else(|| strip_selected_units_selector(value))
}

fn strip_inputs_unit_selector(value: &str) -> Option<&str> {
    let value = value.strip_prefix("inputs.unit=='")?;
    let separator = value.find("'&&(")?;
    let unit = &value[..separator];
    if !is_unit_id(unit) {
        return None;
    }
    value[separator + "'&&(".len()..].strip_suffix(')')
}

fn strip_selected_units_selector(value: &str) -> Option<&str> {
    // contains(format(',{0},',inputs.selected_units),',unit,')&&(gate)
    const PREFIX: &str = "contains(format(',{0},',inputs.selected_units),'";
    let rest = value.strip_prefix(PREFIX)?;
    let rest = rest.strip_prefix(',')?;
    let separator = rest.find(",')&&(")?;
    let unit = &rest[..separator];
    if !is_unit_id(unit) {
        return None;
    }
    rest[separator + ",')&&(".len()..].strip_suffix(')')
}

fn is_safe_trusted_gate_conjunction(value: &str) -> bool {
    !value.is_empty() && !value.contains("||") && !value.contains("github.ref")
}

/// Every policy finding recorded while auditing one repository tree: the
/// failed audit reports them in its error, so the job log carries the cause
/// without depending on stderr interleaving.
#[derive(Default)]
struct PolicyFindings {
    lines: Vec<String>,
}

impl PolicyFindings {
    fn record(&mut self, path: &Path, message: &str) {
        eprintln!("{}: {message}", path.display());
        self.lines.push(format!("{}: {message}", path.display()));
    }
}

fn release(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else {
        return Err(GeneratorError::usage(
            "usage: release verify-tag | release package-binary | release package-deb | release package-guest | release verify-feed | release update-feed",
        ));
    };
    match command {
        "verify-tag" => verify_tag(&arguments[1..]),
        "package-binary" => package_binary(&arguments[1..]),
        "package-deb" => package_deb(&arguments[1..]),
        "package-guest" => package_guest(&arguments[1..]),
        "verify-feed" => verify_feed(&arguments[1..]),
        "update-feed" => update_feed(&arguments[1..]),
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

fn package_deb(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["package", "version", "guest", "target"])?;
    let package = required_option(&options, "package")?;
    let version = required_option(&options, "version")?;
    if !valid_package(package) || !is_artifact_version(version) {
        return Err(GeneratorError::usage("invalid package or version"));
    }
    if let Some(guest) = options.get("guest") {
        let guest = Path::new(guest);
        if !guest.is_dir() {
            return Err(GeneratorError::usage(format!(
                "guest payload directory is missing: {}",
                guest.display()
            )));
        }
        let dest = cargo_package_manifest_dir(package)?.join("release/microvm");
        copy_dir_files(guest, &dest)?;
    }
    let mut command = Command::new("cargo");
    command.args([
        "deb",
        "--no-strip",
        "--package",
        package,
        "--deb-version",
        version,
    ]);
    if let Some(target) = options.get("target") {
        if !valid_target(target) {
            return Err(GeneratorError::usage("invalid package target"));
        }
        command.args(["--target", target]);
    }
    let status = command
        .status()
        .map_err(|error| GeneratorError::usage(format!("cargo deb: {error}")))?;
    if !status.success() {
        return Err(GeneratorError::usage("cargo deb failed"));
    }
    collect_debian_packages(options.get("target").map(String::as_str))
}

fn cargo_package_manifest_dir(package: &str) -> Result<PathBuf, GeneratorError> {
    let metadata = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .output()
        .map_err(|error| GeneratorError::usage(format!("cargo metadata failed: {error}")))?;
    if !metadata.status.success() {
        return Err(GeneratorError::usage("cargo metadata failed"));
    }
    let document: serde_json::Value = serde_json::from_slice(&metadata.stdout)
        .map_err(|error| GeneratorError::usage(format!("parse cargo metadata: {error}")))?;
    document["packages"]
        .as_array()
        .and_then(|packages| {
            packages.iter().find_map(|item| {
                (item["name"].as_str() == Some(package)).then(|| {
                    item["manifest_path"]
                        .as_str()
                        .map(PathBuf::from)
                        .and_then(|path| path.parent().map(Path::to_path_buf))
                })
            })
        })
        .flatten()
        .ok_or_else(|| GeneratorError::usage(format!("cargo package `{package}` was not found")))
}

fn copy_dir_files(src: &Path, dest: &Path) -> Result<(), GeneratorError> {
    fs::create_dir_all(dest)
        .map_err(|error| GeneratorError::io("create guest package directory", dest, &error))?;
    let entries =
        fs::read_dir(src).map_err(|error| GeneratorError::io("read guest payload", src, &error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| GeneratorError::io("read guest payload entry", src, &error))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| GeneratorError::io("stat guest payload entry", &from, &error))?;
        if file_type.is_dir() {
            copy_dir_files(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .map_err(|error| GeneratorError::io("copy guest payload file", &to, &error))?;
        }
    }
    Ok(())
}

fn collect_debian_packages(target: Option<&str>) -> Result<(), GeneratorError> {
    let dist = Path::new("dist");
    fs::create_dir_all(dist)
        .map_err(|error| GeneratorError::io("create release directory", dist, &error))?;
    let mut sources = vec![PathBuf::from("target/debian")];
    if let Some(target) = target {
        sources.push(PathBuf::from("target").join(target).join("debian"));
    }
    let mut copied = 0_usize;
    for source in sources {
        let Ok(entries) = fs::read_dir(&source) else {
            continue;
        };
        for entry in entries {
            let entry =
                entry.map_err(|error| GeneratorError::io("read debian output", &source, &error))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("deb") {
                continue;
            }
            let Some(name) = path.file_name() else {
                continue;
            };
            fs::copy(&path, dist.join(name))
                .map_err(|error| GeneratorError::io("copy debian package", &path, &error))?;
            copied += 1;
        }
    }
    if copied == 0 {
        return Err(GeneratorError::usage(
            "cargo deb produced no .deb under target/debian",
        ));
    }
    Ok(())
}

fn package_guest(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["arch", "package", "bin", "agent"])?;
    let arch = required_option(&options, "arch")?;
    if !matches!(arch, "x86_64" | "aarch64") {
        return Err(GeneratorError::usage(
            "guest arch must be x86_64 or aarch64",
        ));
    }
    let pins = Path::new("microvm/pins.json");
    if pins.is_file() {
        let package = required_option(&options, "package")?;
        let bin = required_option(&options, "bin")?;
        let agent = required_option(&options, "agent")?;
        if !valid_package(package) || !valid_binary(bin) || !valid_binary(agent) {
            return Err(GeneratorError::usage(
                "invalid guest package, bin, or agent",
            ));
        }
        let status = Command::new("bash")
            .arg("-c")
            .arg(
                r#"
set -euo pipefail
url="$(jq -er '.kernel_tarball' microvm/pins.json)"
sha="$(jq -er '.kernel_tarball_sha256' microvm/pins.json)"
curl --fail --show-error --silent --location --http1.1 \
  --connect-timeout 30 --max-time 900 \
  -o linux.tar.xz "$url"
echo "$sha  linux.tar.xz" | sha256sum -c -
"#,
            )
            .status()
            .map_err(|error| GeneratorError::usage(format!("download guest kernel: {error}")))?;
        if !status.success() {
            return Err(GeneratorError::usage("guest kernel download failed"));
        }
        fs::create_dir_all("dist/microvm").map_err(|error| {
            GeneratorError::io("create guest output", Path::new("dist/microvm"), &error)
        })?;
        let status = Command::new("cargo")
            .args([
                "run",
                "--locked",
                "--release",
                "--package",
                package,
                "--bin",
                bin,
                "--",
                "build",
                "--arch",
                arch,
                "--out",
                "dist/microvm",
                "--tarball",
                "linux.tar.xz",
                "--guest-agent",
                agent,
            ])
            .status()
            .map_err(|error| GeneratorError::usage(format!("build guest image: {error}")))?;
        if !status.success() {
            return Err(GeneratorError::usage("guest image build failed"));
        }
        return Ok(());
    }
    let script = Path::new("microvm/build.sh");
    if !script.is_file() {
        return Err(GeneratorError::usage(
            "guest image requires microvm/pins.json or a repository-owned microvm/build.sh",
        ));
    }
    let status = Command::new("bash")
        .args(["microvm/build.sh", arch])
        .status()
        .map_err(|error| GeneratorError::usage(format!("build guest image: {error}")))?;
    if !status.success() {
        return Err(GeneratorError::usage("guest image build failed"));
    }
    Ok(())
}

fn verify_feed(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["kind", "package", "coordinate"])?;
    let kind = required_option(&options, "kind")?;
    let package = required_option(&options, "package")?;
    if !valid_package(package) {
        return Err(GeneratorError::usage("invalid feed package"));
    }
    match kind {
        "homebrew" => {
            let formula = Path::new("Formula").join(format!("{package}.rb"));
            if !formula.is_file() {
                return Err(GeneratorError::usage(format!(
                    "homebrew feed requires {}",
                    formula.display()
                )));
            }
        }
        "apt" => {
            if !Path::new("conf/distributions").is_file() && !Path::new("debian").is_dir() {
                return Err(GeneratorError::usage(
                    "apt feed requires conf/distributions or debian/",
                ));
            }
        }
        other => {
            return Err(GeneratorError::usage(format!(
                "unsupported feed kind: {other}"
            )));
        }
    }
    Ok(())
}

fn update_feed(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["kind", "package", "coordinate", "channel"])?;
    verify_feed(arguments)?;
    let kind = required_option(&options, "kind")?;
    let channel = options.get("channel").map_or("stable", String::as_str);
    if !matches!(channel, "stable" | "preview") {
        return Err(GeneratorError::usage("channel must be stable or preview"));
    }
    match kind {
        "homebrew" | "apt" => {
            println!("feed {kind} channel {channel} verified; mutation is GitHub-writer only");
            Ok(())
        }
        other => Err(GeneratorError::usage(format!(
            "unsupported feed kind: {other}"
        ))),
    }
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
    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> GeneratorError {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error,
        }
    }

    #[test]
    fn package_deb_rejects_bad_inputs_before_any_cargo_call() {
        // Every case below fails on option validation, before package-deb
        // shells out: no cargo, no filesystem writes, no network.
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        let error = must_fail(
            package_deb(&args(&["--version", "1.2.3"])),
            "package-deb without --package",
        );
        assert!(
            error.to_string().contains("--package needs a value"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            package_deb(&args(&[
                "--package",
                "velnor-runner",
                "--version",
                "v1.2.3",
            ])),
            "package-deb with a v-prefixed version",
        );
        assert!(
            error.to_string().contains("invalid package or version"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            package_deb(&args(&[
                "--package",
                "velnor-runner",
                "--version",
                "1.2.3",
                "--guest",
                "/nonexistent-velnor-guest-payload",
            ])),
            "package-deb with a missing guest directory",
        );
        assert!(
            error
                .to_string()
                .contains("guest payload directory is missing"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            package_deb(&args(&[
                "--package",
                "velnor-runner",
                "--version",
                "1.2.3",
                "--target",
                "x86_64!",
            ])),
            "package-deb with an invalid target",
        );
        assert!(
            error.to_string().contains("invalid package target"),
            "unexpected error: {error}"
        );
    }

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
            format!(
                "name: Velnor workflow policy\non:\n  pull_request_target:\n    types: [opened, synchronize, reopened]\npermissions:\n  contents: read\njobs:\n{}",
                crate::inline_policy_job("Policy", POLICY_REVISION)
            ),
        )?;
        std::fs::write(
            root.join(".github/ci/project.toml"),
            format!("runners = \"{runners}\"\n"),
        )?;
        Ok(root)
    }

    #[test]
    fn numeric_actions_cache_ids_become_lossless_internal_strings() {
        let record: CacheEntryRecord = must(
            serde_json::from_str(
                r#"{"id":7636963307,"key":"example-docker-seed-Linux-X64",
                "size_in_bytes":1,"created_at":"2026-09-01T00:00:00Z"}"#,
            ),
            "deserialize a numeric Actions cache id",
        );
        assert_eq!(record.id, "7636963307");
        assert_eq!(record.key, "example-docker-seed-Linux-X64");
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
            automatic: "github".to_owned(),
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

    /// A repo-owned project config written for these tests: one docker unit,
    /// a small rust crate graph, and a version-bump allowlist. The selection
    /// engine consumes whatever the file declares, so the fixture stays
    /// independent of any repository the workspace happens to live in.
    const SELECTION_PROJECT_CONFIG: &str = r#"schema = 2
repository = "example/selection"
profile = "generic"
verified = true
default_branch = "main"
runners = "github"

[analysis]
method = "static-filesystem-and-manifest-inspection"
detected = []
limitations = []

[workflow]
github_runner = "ubuntu-24.04"
files = ["ci-docker-docker.yml", "ci-rust-base.yml", "ci-rust-leaf.yml", "ci-pr.yml", "ci-main.yml", "ci-policy.yml", "maintenance.yml", "nightly.yml"]
version_bump_units = ["docker", "rust-bench", "rust-leaf"]

[[unit]]
id = "docker"
kind = "docker"
root = "."
watch = ["Dockerfile"]
github_pr_commands = ["docker build --file 'Dockerfile' --tag local-ci:dockerfile '.'"]
github_full_commands = ["docker build --file 'Dockerfile' --tag local-ci:dockerfile '.'"]
velnor_pr_commands = ["docker build --file 'Dockerfile' --tag local-ci:dockerfile '.'"]
velnor_full_commands = ["docker build --file 'Dockerfile' --tag local-ci:dockerfile '.'"]

[[unit]]
id = "rust-base"
kind = "rust"
root = "crates/base"
watch = ["crates/base/**", "Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'crates/base/Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'crates/base/Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'crates/base/Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'crates/base/Cargo.toml'"]

[[unit]]
id = "rust-leaf"
kind = "rust"
root = "crates/leaf"
depends_on = ["rust-base"]
watch = ["crates/leaf/**", "Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'crates/leaf/Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'crates/leaf/Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'crates/leaf/Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'crates/leaf/Cargo.toml'"]

[[unit]]
id = "rust-bench"
kind = "rust"
root = "crates/bench"
depends_on = ["rust-base"]
watch = ["crates/bench/**", "Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'crates/bench/Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'crates/bench/Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'crates/bench/Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'crates/bench/Cargo.toml'"]
"#;

    fn current_project_selection_git_fixture(
        name: &str,
        changed: &str,
        base_contents: &str,
        head_contents: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-current-project-selection-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        let init = |args: &[&str]| -> Result<String, Box<dyn Error>> {
            let output = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()?;
            assert!(
                output.status.success(),
                "git command failed: {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        init(&["init", "-q"])?;
        init(&["config", "user.email", "test@example.invalid"])?;
        init(&["config", "user.name", "Velnor test"])?;

        let config = root.join(".github/ci/project.toml");
        std::fs::create_dir_all(config.parent().ok_or("project config parent")?)?;
        std::fs::write(&config, SELECTION_PROJECT_CONFIG)?;

        let changed_path = root.join(changed);
        std::fs::create_dir_all(changed_path.parent().ok_or("changed file parent")?)?;
        std::fs::write(&changed_path, base_contents)?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "base"])?;
        let base = init(&["rev-parse", "HEAD"])?;

        std::fs::write(changed_path, head_contents)?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "change"])?;
        let head = init(&["rev-parse", "HEAD"])?;
        Ok((root, base, head))
    }

    fn selected_ids(units: Vec<&CiUnit>) -> Vec<&str> {
        units.into_iter().map(|unit| unit.id.as_str()).collect()
    }

    fn selected_id_set(selection: &UnitSelection<'_>) -> BTreeSet<String> {
        selection.units.iter().map(|unit| unit.id.clone()).collect()
    }

    fn lanes_selection_config() -> CiConfig {
        let unit = |id: &str, kind: &str| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: kind.to_owned(),
            root: ".".to_owned(),
            watch: vec!["**".to_owned()],
            github_pr_commands: vec!["true".to_owned()],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["true".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
        };
        CiConfig {
            schema: 2,
            repository: String::new(),
            profile: String::new(),
            verified: true,
            default_branch: "main".to_owned(),
            runners: "both".to_owned(),
            automatic: "both".to_owned(),
            analysis: Analysis::default(),
            workflow: Workflow::default(),
            release: Release::default(),
            unit: vec![
                unit("rust-app", "rust"),
                unit("swift-app", "swift"),
                unit("future-app", "quantum"),
            ],
        }
    }

    #[test]
    fn plan_lanes_default_to_both_and_reject_unknown_lanes() {
        assert_eq!(
            must(plan_lanes_for_value(""), "empty lanes"),
            RunnerMode::Both
        );
        assert_eq!(
            must(plan_lanes_for_value("both"), "both lanes"),
            RunnerMode::Both
        );
        assert_eq!(
            must(plan_lanes_for_value("velnor"), "velnor lanes"),
            RunnerMode::Velnor
        );
        assert_eq!(
            must(plan_lanes_for_value("github"), "github lanes"),
            RunnerMode::Github
        );
        let result = plan_lanes_for_value("self-hosted");
        assert!(
            result.as_ref().is_err_and(|error| error
                .to_string()
                .contains("unsupported CI lanes `self-hosted`")),
            "unknown lanes must fail closed: {result:?}"
        );
    }

    #[test]
    fn velnor_only_selection_drops_swift_but_keeps_unknown_kinds() {
        let config = lanes_selection_config();
        let narrowed = selection_for_lanes(
            &config,
            must(full_selection(&config), "full selection"),
            RunnerMode::Velnor,
        );
        let ids = narrowed
            .units
            .iter()
            .map(|unit| unit.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["rust-app", "future-app"]);
        assert_eq!(
            narrowed.full_units,
            ["future-app", "rust-app"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        for lanes in [RunnerMode::Github, RunnerMode::Both] {
            let narrowed = selection_for_lanes(
                &config,
                must(full_selection(&config), "full selection"),
                lanes,
            );
            let ids = narrowed
                .units
                .iter()
                .map(|unit| unit.id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                vec!["rust-app", "swift-app", "future-app"],
                "{lanes:?}"
            );
            assert_eq!(narrowed.full_units.len(), 3, "{lanes:?}");
        }
    }

    #[test]
    fn velnor_only_selection_prunes_full_marks_of_excluded_units() {
        let config = lanes_selection_config();
        let units = config.unit.iter().collect::<Vec<_>>();
        let selection = UnitSelection {
            units,
            full_units: ["swift-app"].into_iter().map(str::to_owned).collect(),
        };
        let narrowed = selection_for_lanes(&config, selection, RunnerMode::Velnor);
        assert!(narrowed.full_units.is_empty());
        assert!(narrowed.units.iter().all(|unit| unit.id != "swift-app"));
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
    fn unselected_unit_fails_closed_instead_of_succeeding_as_a_noop() {
        let config = selection_config();
        let selected = config.unit.iter().collect::<Vec<_>>();
        let result = super::select_units_for_job(selected, Some("not-selected"));
        assert!(
            result.as_ref().is_err_and(|error| error
                .to_string()
                .contains("does not include requested unit `not-selected`")),
            "an unselected unit must fail closed with a useful error: {result:?}"
        );
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
            vec!["mbx check --locked --all-targets"]
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
    fn current_project_cargo_lock_only_change_selects_exact_allowlist_units(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, head) = current_project_selection_git_fixture(
            "cargo-lock-only",
            "Cargo.lock",
            "[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
            "[[package]]\nname = \"fixture\"\nversion = \"0.1.1\"\n",
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected = ["docker", "rust-bench", "rust-leaf"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(selected_id_set(&selection), expected);
        assert_eq!(selection.full_units, expected);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn current_project_leaf_change_selects_exact_dependency_closure() -> Result<(), Box<dyn Error>>
    {
        let (root, base, head) = current_project_selection_git_fixture(
            "leaf-source",
            "crates/leaf/src/lib.rs",
            "pub fn fixture() {}\n",
            "pub fn fixture() { let _ = 1; }\n",
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected_selected = ["rust-base", "rust-leaf"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        // `full_units` carries the affected set (the changed unit and its
        // dependents); the dependency closure lives on the selected side.
        let expected_full = ["rust-leaf"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(selected_id_set(&selection), expected_selected);
        assert_eq!(selection.full_units, expected_full);
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

    /// GitHub rejects workflows whose YAML carries duplicate keys, so the
    /// auditor must reject them too instead of auditing a last-key-wins
    /// rewrite of the document GitHub refuses to run.
    #[test]
    fn policy_rejects_duplicate_yaml_keys_github_would_reject() -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Duplicate keys
on: push
jobs:
  verify:
    runs-on: ubuntu-24.04
    steps:
      - run: true
  verify:
    runs-on: ubuntu-24.04
    steps:
      - run: true
";
        let root = policy_fixture("duplicate-keys", workflow, "github")?;
        let failures = enforce_policy_with_revision(&root, POLICY_REVISION)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            failures.contains("duplicate key: verify"),
            "rejection must come from the strict parser, not a later finding: {failures}"
        );
        std::fs::remove_dir_all(root)?;
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
    fn policy_rejects_untrusted_velnor_pull_request_gates() -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Velnor PR
on:
  pull_request:
jobs:
  verify:
    if: ${{ github.event_name == 'pull_request' }}
    runs-on: [self-hosted, example-velnor]
    steps:
      - run: true
";
        let root = policy_fixture("velnor-pull-request", workflow, "velnor")?;
        assert!(!run_policy(root)?);

        let workflow = r"
name: Velnor PR aggregate
on: push
jobs:
  verify:
    if: ${{ always() && (github.event_name == 'pull_request' || (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'))) }}
    runs-on: [self-hosted, example-velnor]
    steps:
      - run: true
";
        let root = policy_fixture("velnor-pull-request-aggregate", workflow, "velnor")?;
        assert!(!run_policy(root)?);

        let workflow = r"
name: Velnor kind reusable
on: workflow_call
jobs:
  verify:
    if: ${{ inputs.unit == 'rust-policy' && (github.event_name == 'pull_request' || (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule'))) }}
    runs-on: [self-hosted, example-velnor]
    steps:
      - run: true
";
        let root = policy_fixture("velnor-kind-reusable", workflow, "velnor")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_accepts_generated_pr_gates_on_configured_velnor_labels() -> Result<(), Box<dyn Error>>
    {
        let group = crate::estate::approved_velnor_runner_group();
        let yaml_labels = crate::estate::approved_velnor_runner_labels().join(", ");
        let toml_labels = crate::estate::approved_velnor_runner_labels()
            .iter()
            .map(|label| format!("\"{label}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let config_text = format!(
            "schema = 2\nrunners = \"velnor\"\ndefault_branch = \"main\"\n\n[workflow]\nvelnor_labels = [{toml_labels}]\n"
        );
        let generation_config = format!(
            "schema = 1\n\n[workflow]\nvelnor_runner_group = \"{group}\"\npull_request_on_velnor = true\n"
        );
        let runner = format!("{{ group: {group}, labels: [{yaml_labels}] }}");
        let gate = "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository || (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule')) || (github.ref == 'refs/heads/main' && (github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both')))";
        let root = policy_fixture(
            "velnor-pr-configured",
            "name: Other\non: push\njobs:\n  noop:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n",
            "velnor",
        )?;
        std::fs::write(root.join(".github/ci/project.toml"), &config_text)?;
        std::fs::create_dir_all(root.join(".github-gen"))?;
        std::fs::write(
            root.join(".github-gen/velnor-workflow.toml"),
            &generation_config,
        )?;
        std::fs::write(
            root.join(".github/workflows/ci-pr.yml"),
            format!(
                r"
name: CI
on:
  pull_request:
jobs:
  plan:
    if: ${{{{ {gate} }}}}
    runs-on: {runner}
    steps:
      - run: true
  ci-required:
    if: ${{{{ always() && ({gate}) }}}}
    runs-on: {runner}
    steps:
      - run: true
",
            ),
        )?;
        std::fs::write(
            root.join(".github/workflows/ci-unit-rust.yml"),
            format!(
                r"
name: rust
on:
  workflow_call:
jobs:
  verify:
    if: ${{{{ inputs.unit == 'rust-policy' && ({gate}) }}}}
    runs-on: {runner}
    steps:
      - run: true
  selected:
    if: ${{{{ contains(format(',{{0}},', inputs.selected_units), ',rust-policy,') && ({gate}) }}}}
    runs-on: {runner}
    steps:
      - run: true
",
            ),
        )?;
        let result = enforce_policy_with_revision(&root, POLICY_REVISION);
        assert!(
            result.is_ok(),
            "accepted opt-in fixture rejected: {result:?}"
        );
        std::fs::remove_dir_all(root)?;

        let mismatched = policy_fixture(
            "velnor-pr-mismatched",
            "name: Other\non: push\njobs:\n  noop:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n",
            "velnor",
        )?;
        std::fs::write(mismatched.join(".github/ci/project.toml"), &config_text)?;
        std::fs::write(
            mismatched.join(".github/workflows/ci-pr.yml"),
            r"
name: CI
on:
  pull_request:
jobs:
  plan:
    if: ${{ github.event_name == 'pull_request' || github.event_name == 'workflow_dispatch' || (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule')) }}
    runs-on: [self-hosted, other-runner]
    steps:
      - run: true
",
        )?;
        assert!(!run_policy(mismatched)?);
        Ok(())
    }

    #[test]
    fn policy_accepts_the_both_mode_lanes_dispatch_gate() -> Result<(), Box<dyn Error>> {
        // Both-mode aggregates name the manual lane selector `lanes`; the
        // gate shape is the generated one, only the input spelling differs.
        // No pull_request arm: Velnor PR gates now require the opt-in plus a
        // same-repository restriction, covered by the opt-in fixtures; this
        // pins `lanes` spelling on the classic trusted shape instead.
        let workflow = r"
name: Velnor lanes reusable
on: workflow_call
jobs:
  verify:
    if: ${{ inputs.unit == 'rust-policy' && (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && (github.event.inputs.lanes == 'velnor' || github.event.inputs.lanes == 'both')))) }}
    runs-on: [self-hosted, example-velnor]
    steps:
      - run: true
";
        let root = policy_fixture("velnor-lanes-reusable", workflow, "velnor")?;
        std::fs::write(
            root.join(".github/ci/project.toml"),
            "schema = 2\nrunners = \"velnor\"\n\n[workflow]\nvelnor_labels = [\"self-hosted\", \"example-velnor\"]\n",
        )?;
        assert!(run_policy(root)?);

        let workflow = r"
name: Velnor lanes dispatch
on: workflow_call
jobs:
  verify:
    if: ${{ github.event_name == 'workflow_dispatch' && (github.event.inputs.lanes == 'velnor' || github.event.inputs.lanes == 'both') }}
    runs-on: [self-hosted, example-velnor]
    steps:
      - run: true
";
        let root = policy_fixture("velnor-lanes-dispatch", workflow, "velnor")?;
        std::fs::write(
            root.join(".github/ci/project.toml"),
            "schema = 2\nrunners = \"velnor\"\n\n[workflow]\nvelnor_labels = [\"self-hosted\", \"example-velnor\"]\n",
        )?;
        // A dispatch-only gate stays rejected: manual dispatch must ride with
        // the automatic arms, never alone on the self-hosted lane.
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_rejects_fork_pull_request_gate_even_with_approved_runner(
    ) -> Result<(), Box<dyn Error>> {
        let group = crate::estate::approved_velnor_runner_group();
        let labels = crate::estate::approved_velnor_runner_labels();
        let labels_toml = labels
            .iter()
            .map(|label| format!("\"{label}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let labels_yaml = labels.join(", ");
        let config = format!(
            "schema = 2\nrunners = \"velnor\"\ndefault_branch = \"main\"\n\n[workflow]\nvelnor_labels = [{labels_toml}]\n"
        );
        let generation_config = format!(
            "schema = 1\n\n[workflow]\nvelnor_runner_group = \"{group}\"\npull_request_on_velnor = true\n"
        );
        let runner = format!("{{ group: {group}, labels: [{labels_yaml}] }}");
        let root = policy_fixture(
            "velnor-pr-fork-gate",
            &format!(
                r"
name: CI
on:
  pull_request:
jobs:
  verify:
    if: ${{{{ github.event_name == 'pull_request' }}}}
    runs-on: {runner}
    steps:
      - run: true
",
            ),
            "velnor",
        )?;
        std::fs::write(root.join(".github/ci/project.toml"), config)?;
        std::fs::create_dir_all(root.join(".github-gen"))?;
        std::fs::write(
            root.join(".github-gen/velnor-workflow.toml"),
            generation_config,
        )?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_accepts_the_generated_inline_transport_in_every_variant() -> Result<(), Box<dyn Error>>
    {
        // The base-owned entrypoint carries the `Policy` variant.
        let workflow = format!(
            "name: Advisory caller\non: push\njobs:\n{}",
            crate::inline_policy_job("Advisory policy", POLICY_REVISION)
        );
        let root = policy_fixture("inline-advisory", &workflow, "github")?;
        assert!(run_policy(root)?);

        // A drifted inline job — the runtime pin rewritten to another revision —
        // must fail closed against the trusted revision.
        let workflow = format!(
            "name: Advisory caller\non: push\njobs:\n{}",
            crate::inline_policy_job(
                "Advisory policy",
                "13f5567b0a5d2f61e9f47dcf11dc7d2f8b8d4a33"
            )
        );
        let root = policy_fixture("inline-wrong-pin", &workflow, "github")?;
        assert!(!run_policy(root)?);

        // So must any other drift inside the approved steps.
        let drifted = crate::inline_policy_job("Policy", POLICY_REVISION)
            .replace("set -euo pipefail", "set -eo pipefail");
        let workflow = format!("name: Drifted\non: push\njobs:\n{drifted}");
        let root = policy_fixture("inline-drifted-step", &workflow, "github")?;
        assert!(!run_policy(root)?);

        // Velnor policy uses the local cache backend and a trusted event gate,
        // while preserving the same pinned revision.
        let trusted_gate = "    if: ${{ github.event_name == 'pull_request_target' || (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')) }}\n";
        let workflow = format!(
            "name: Velnor caller\non: push\njobs:\n{}",
            crate::inline_policy_job_for_lane(
                "Advisory policy",
                POLICY_REVISION,
                "[self-hosted, example-runner]",
                "local",
                Some(trusted_gate),
            )
        );
        let root = policy_fixture("inline-velnor", &workflow, "velnor")?;
        assert!(run_policy(root)?);

        let pull_request_gate = "    if: ${{ github.event_name == 'pull_request' || (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')) }}\n";
        let workflow = format!(
            "name: Velnor pull request policy\non: push\njobs:\n{}",
            crate::inline_policy_job_for_lane(
                "Advisory policy",
                POLICY_REVISION,
                "[self-hosted, example-velnor]",
                "local",
                Some(pull_request_gate),
            )
        );
        let root = policy_fixture("inline-velnor-pull-request", &workflow, "velnor")?;
        assert!(!run_policy(root)?);

        let lane_gate = "    if: ${{ github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both'))) }}\n";
        let workflow = format!(
            "name: Velnor dispatch policy\non: push\njobs:\n{}",
            crate::inline_policy_job_for_lane(
                "Advisory policy",
                POLICY_REVISION,
                "[self-hosted, example-velnor]",
                "local",
                Some(lane_gate),
            )
        );
        let root = policy_fixture("inline-velnor-dispatch", &workflow, "velnor")?;
        assert!(run_policy(root)?);

        let unit_gate = "    if: ${{ inputs.unit == 'rust-policy' && (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both')))) }}\n";
        let workflow = format!(
            "name: Velnor unit policy\non: workflow_call\njobs:\n{}",
            crate::inline_policy_job_for_lane(
                "Advisory policy",
                POLICY_REVISION,
                "[self-hosted, example-velnor]",
                "local",
                Some(unit_gate),
            )
        );
        let root = policy_fixture("inline-velnor-unit", &workflow, "velnor")?;
        assert!(run_policy(root)?);

        // The Velnor lane remains fail-closed for both revision drift and
        // removal of its trusted-event gate.
        let workflow = format!(
            "name: Velnor wrong pin\non: push\njobs:\n{}",
            crate::inline_policy_job_for_lane(
                "Advisory policy",
                "13f5567b0a5d2f61e9f47dcf11dc7d2f8b8d4a33",
                "[self-hosted, example-runner]",
                "local",
                Some(trusted_gate),
            )
        );
        let root = policy_fixture("inline-velnor-wrong-pin", &workflow, "velnor")?;
        assert!(!run_policy(root)?);

        let workflow = format!(
            "name: Velnor untrusted\non: push\njobs:\n{}",
            crate::inline_policy_job_for_lane(
                "Advisory policy",
                POLICY_REVISION,
                "[self-hosted, example-runner]",
                "local",
                Some("    if: ${{ github.event_name == 'push' }}\n"),
            )
        );
        let root = policy_fixture("inline-velnor-untrusted", &workflow, "velnor")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_rejects_the_retired_reusable_policy_transport() -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Reusable policy caller
on: pull_request
jobs:
  policy:
    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@a1cbfcbe5ab179032e37125f0383cdcae8183c8c
    with:
      policy-revision: a1cbfcbe5ab179032e37125f0383cdcae8183c8c
";
        let root = policy_fixture("retained-reusable-policy", workflow, "github")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_names_the_scheduling_hazard_for_same_repository_pinned_reusables(
    ) -> Result<(), Box<dyn Error>> {
        let workflow = r"
name: Pinned same-repository caller
on: push
jobs:
  call:
    uses: ./.github/workflows/ci-rust.yml@13f5567b0a5d2f61e9f47dcf11dc7d2f8b8d4a33
";
        let root = policy_fixture("same-repo-pinned", workflow, "github")?;
        let failures = policy_failures(&root);
        std::fs::remove_dir_all(root)?;
        assert!(
            failures.contains(
                "same-repository reusable workflow call at a pinned revision wedges push/schedule scheduling"
            ),
            "{failures}"
        );
        Ok(())
    }

    /// The rendered inline policy job never calls a reusable workflow through
    /// the pinned `tailrocks/velnor` provider path — the transport that wedges
    /// push/schedule scheduling — and always installs exactly the revision it
    /// audits with.
    #[test]
    fn inline_policy_job_carries_no_reusable_call_and_one_runtime_pin() {
        for name in crate::POLICY_JOB_NAMES {
            let job = crate::inline_policy_job(name, POLICY_REVISION);
            assert!(
                !job.contains("uses: tailrocks/velnor/.github/workflows/"),
                "{name} must not call a reusable workflow through the pinned provider path"
            );
            let rev_uses = job.matches(&format!("--rev {POLICY_REVISION}")).count();
            assert_eq!(
                rev_uses, 1,
                "{name} must pin the install to the policy revision once"
            );
            let revision_env = format!("{TRUSTED_POLICY_REVISION_ENV}: {POLICY_REVISION}");
            assert!(
                job.contains(&revision_env),
                "{name} must audit with the same revision it installs"
            );
            assert_eq!(
                job.matches(POLICY_REVISION).count(),
                3,
                "{name} must use the policy revision for the cache key, the install pin, and the audit env"
            );
        }
    }

    /// Every parsed failure the audit reports for the fixture tree, so tests
    /// can assert the message names its cause.
    fn policy_failures(root: &std::path::Path) -> String {
        enforce_policy_with_revision(root, POLICY_REVISION)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
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
        let workflow = format!(
            "name: Velnor workflow policy\non:\n  pull_request_target:\n    types: [opened, synchronize, reopened]\npermissions:\n  contents: read\njobs:\n{}",
            crate::inline_policy_job("Policy", POLICY_REVISION)
        );
        let root = policy_fixture("approved-policy-entrypoint", &workflow, "github")?;
        std::fs::rename(
            root.join(".github/workflows/policy.yml"),
            root.join(".github/workflows/ci-policy.yml"),
        )?;
        assert!(run_policy(root)?);

        let workflow = format!(
            "name: Velnor workflow policy\non:\n  pull_request_target:\n    types: [opened, synchronize, reopened]\njobs:\n{}  bypass:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n",
            crate::inline_policy_job("Policy", POLICY_REVISION)
        );
        let root = policy_fixture("bypassed-policy-entrypoint", &workflow, "github")?;
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
    fn policy_accepts_adopted_lane_selection_runners() -> Result<(), Box<dyn Error>> {
        let lane = format!(
            "${{{{ ((github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') || github.event_name == 'pull_request' || github.event_name == 'push') && 'ubuntu-26.04' || {} }}}}",
            crate::estate::LEGACY_VELNOR_RUNNER_SELECTOR
        );
        let valid = format!(
            "name: Valid\non: pull_request\njobs:\n  verify:\n    runs-on: {lane}\n    steps:\n      - uses: actions/checkout@{CHECKOUT_SHA}\n"
        );
        let root = policy_fixture("lane-valid", &valid, "both")?;
        assert!(run_policy(root)?);

        // A lane selector that routes pull requests anywhere but the hosted
        // label stays rejected: untrusted pull requests must never resolve
        // to the persistent pool.
        let unsafe_lane = format!(
            "${{{{ ((github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') || github.event_name == 'push') && 'ubuntu-26.04' || {} }}}}",
            crate::estate::LEGACY_VELNOR_RUNNER_SELECTOR
        );
        let invalid = format!(
            "name: Invalid\non: pull_request\njobs:\n  verify:\n    runs-on: {unsafe_lane}\n    steps:\n      - uses: actions/checkout@{CHECKOUT_SHA}\n"
        );
        let root = policy_fixture("lane-unsafe", &invalid, "both")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_accepts_matrix_lane_selection_runners() -> Result<(), Box<dyn Error>> {
        let valid = format!(
            "name: Valid\non: pull_request\njobs:\n  build:\n    runs-on: ${{{{ fromJSON(matrix.config.runner) }}}}\n    steps:\n      - uses: actions/checkout@{CHECKOUT_SHA}\n"
        );
        let root = policy_fixture("matrix-valid", &valid, "both")?;
        assert!(run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_accepts_tree_pinned_local_actions() -> Result<(), Box<dyn Error>> {
        let valid = format!(
            "name: Valid\non: pull_request\njobs:\n  verify:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: ./.github/actions/cache-cargo-registry\n      - uses: actions/checkout@{CHECKOUT_SHA}\n"
        );
        let root = policy_fixture("local-action-valid", &valid, "github")?;
        assert!(run_policy(root)?);

        let invalid = "name: Invalid\non: pull_request\njobs:\n  verify:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: ./../actions/cache-cargo-registry\n";
        let root = policy_fixture("local-action-escape", invalid, "github")?;
        assert!(!run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_accepts_sha_pinned_fleet_reusables() -> Result<(), Box<dyn Error>> {
        let sha = "851ef541d67f9cabebf2ddb2a2a02f51f6c54130";
        let owner = crate::estate::FLEET_VELNOR_ACTION_OWNERS[0];
        let valid = format!(
            "name: Valid\non: pull_request\njobs:\n  sign:\n    uses: {owner}/velnor-actions/.github/workflows/package-signer.yml@{sha}\n"
        );
        let root = policy_fixture("fleet-valid", &valid, "github")?;
        assert!(run_policy(root)?);

        let invalid = format!(
            "name: Invalid\non: pull_request\njobs:\n  sign:\n    uses: {owner}/velnor-actions/.github/workflows/package-signer.yml@2026.8.33\n"
        );
        let root = policy_fixture("fleet-unpinned", &invalid, "github")?;
        assert!(!run_policy(root)?);

        let invalid = format!(
            "name: Invalid\non: pull_request\njobs:\n  sign:\n    uses: unknown-owner/velnor-actions/.github/workflows/package-signer.yml@{sha}\n"
        );
        let root = policy_fixture("fleet-owner", &invalid, "github")?;
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
          - runner: [self-hosted, example-label]
    runs-on: ${{ matrix.runner }}
";
        let root = policy_fixture("matrix-trusted", workflow, "github")?;
        assert!(run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_accepts_static_runner_group_mapping_with_trusted_gate() -> Result<(), Box<dyn Error>>
    {
        let workflow = r"
name: Group
on: push
jobs:
  verify:
    if: ${{ github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') }}
    runs-on:
      group: example-trusted
      labels: [self-hosted, example-label]
";
        let root = policy_fixture("runner-group-trusted", workflow, "github")?;
        assert!(run_policy(root)?);

        let workflow = r"
name: Group with lane guard
on: push
jobs:
  verify:
    if: ${{ inputs.consumer_repository != '' && inputs.lane == 'velnor' && github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') }}
    runs-on:
      group: example-trusted
      labels: [self-hosted, example-label]
";
        let root = policy_fixture("runner-group-trusted-with-conjunction", workflow, "github")?;
        assert!(run_policy(root)?);
        Ok(())
    }

    #[test]
    fn policy_rejects_dynamic_or_unknown_runner_group_mapping() -> Result<(), Box<dyn Error>> {
        let dynamic = r"
name: Dynamic group
on: push
jobs:
  verify:
    if: ${{ github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') }}
    runs-on:
      group: ${{ inputs.group }}
      labels: [self-hosted, example-label]
";
        let root = policy_fixture("runner-group-dynamic", dynamic, "github")?;
        assert!(!run_policy(root)?);

        let unknown = r"
name: Unknown group key
on: push
jobs:
  verify:
    if: ${{ github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') }}
    runs-on:
      group: example-trusted
      labels: [self-hosted, example-label]
      environment: production
";
        let root = policy_fixture("runner-group-unknown-key", unknown, "github")?;
        assert!(!run_policy(root)?);
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
          - runner: [self-hosted, example-label]
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
    if: ${{ github.event_name == 'pull_request' || (github.ref == 'refs/heads/main' && github.event_name == 'push') }}
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

    #[test]
    fn policy_ignores_runner_group_lingering_in_the_runtime_contract() -> Result<(), Box<dyn Error>>
    {
        let group = crate::estate::approved_velnor_runner_group();
        let yaml_labels = crate::estate::approved_velnor_runner_labels().join(", ");
        let toml_labels = crate::estate::approved_velnor_runner_labels()
            .iter()
            .map(|label| format!("\"{label}\""))
            .collect::<Vec<_>>()
            .join(", ");
        // A hand-merged group lingers in the runtime contract while the
        // generation input carries no group. The runner group is
        // generation-only, so policy must not see the stale field.
        let config_text = format!(
            "schema = 2\nrunners = \"velnor\"\ndefault_branch = \"main\"\n\n[workflow]\nvelnor_labels = [{toml_labels}]\nvelnor_runner_group = \"{group}\"\n"
        );
        let runner = format!("{{ group: {group}, labels: [{yaml_labels}] }}");
        let gate = "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository || (github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule')) || (github.ref == 'refs/heads/main' && (github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both' || github.event.inputs.runner == '')))";
        let root = policy_fixture(
            "velnor-pr-stale-group",
            "name: Other\non: push\njobs:\n  noop:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n",
            "velnor",
        )?;
        std::fs::write(root.join(".github/ci/project.toml"), &config_text)?;
        std::fs::create_dir_all(root.join(".github-gen"))?;
        std::fs::write(
            root.join(".github-gen/velnor-workflow.toml"),
            "schema = 1\n\n[workflow]\npull_request_on_velnor = true\n",
        )?;
        std::fs::write(
            root.join(".github/workflows/ci-pr.yml"),
            format!(
                r"
name: CI
on:
  pull_request:
jobs:
  plan:
    if: ${{{{ {gate} }}}}
    runs-on: {runner}
    steps:
      - run: true
  ci-required:
    if: ${{{{ always() && ({gate}) }}}}
    runs-on: {runner}
    steps:
      - run: true
",
            ),
        )?;
        let failures = policy_failures(&root);
        assert!(
            failures.contains("approved Velnor runner group"),
            "a stale runtime group must not satisfy the approved-group policy: {failures}"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

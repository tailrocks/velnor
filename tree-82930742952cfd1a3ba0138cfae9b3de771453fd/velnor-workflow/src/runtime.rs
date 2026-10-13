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

use globset::{Glob, GlobSetBuilder};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};

const IMMUTABLE_POLICY_WORKFLOW: &str =
    "tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml";
const IMMUTABLE_POLICY_WORKFLOW_REV: &str = "fe805b984d3e261d3686d7ec670792f8121306bc";
use sha2::{Digest, Sha256};

use super::GeneratorError;

const DEFAULT_CONFIG: &str = ".github/ci/project.toml";
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
    pr_commands: Vec<String>,
    full_commands: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    tool_version: Option<String>,
    #[serde(default)]
    cache: Option<Cache>,
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
            let options = parse_options(&arguments[1..], &["workflow-root"])?;
            let root = options
                .get("workflow-root")
                .map_or_else(
                    || env::var_os("WORKFLOW_ROOT").map(PathBuf::from),
                    |value| Some(PathBuf::from(value)),
                )
                .or_else(|| env::var_os("GITHUB_WORKSPACE").map(PathBuf::from))
                .or_else(|| env::current_dir().ok())
                .ok_or_else(|| GeneratorError::usage("resolve workflow root"))?;
            enforce_policy(&root)?;
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
    if config.schema != 1 {
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
        if unit.watch.is_empty() || unit.pr_commands.is_empty() || unit.full_commands.is_empty() {
            return Err(GeneratorError::usage(format!(
                "CI unit must declare watch, PR, and full commands: {}",
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
    if let Some(output) = env::var_os("GITHUB_OUTPUT") {
        let output_path = PathBuf::from(output);
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_path)
            .map_err(|error| GeneratorError::io("open GitHub output", &output_path, &error))?;
        writeln!(file, "scope={}", scope_name(scope))
            .map_err(|error| GeneratorError::io("write GitHub output", &output_path, &error))?;
    }
    println!("scope={}", scope_name(scope));
    let _ = config;
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
        "pull_request" | "merge_group" => Ok(override_scope
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
    let selected = selected_units(root, &config, scope)?;
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
    run_layers(root, &selected, scope)
}

fn selected_units<'a>(
    root: &Path,
    config: &'a CiConfig,
    scope: Scope,
) -> Result<Vec<&'a CiUnit>, GeneratorError> {
    if scope == Scope::Full {
        return ordered_units(&config.unit, None);
    }
    let base = env::var("BASE_SHA").unwrap_or_default();
    let head = env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned());
    if base.is_empty() || base.chars().all(|character| character == '0') {
        return ordered_units(&config.unit, None);
    }
    let Some(changed) = git_changed_files(root, &base, &head)? else {
        return ordered_units(&config.unit, None);
    };
    if changed.is_empty() {
        return Err(GeneratorError::usage(
            "affected CI scope has no changed files",
        ));
    }
    if changed.iter().any(|file| file.starts_with(".github/")) {
        return ordered_units(&config.unit, None);
    }
    let mut builder = GlobSetBuilder::new();
    for unit in &config.unit {
        for pattern in &unit.watch {
            let glob = Glob::new(pattern).map_err(|error| {
                GeneratorError::usage(format!("invalid watch pattern {pattern}: {error}"))
            })?;
            builder.add(glob);
        }
    }
    let globset = builder
        .build()
        .map_err(|error| GeneratorError::usage(format!("build watch matcher: {error}")))?;
    let mut selected = BTreeSet::new();
    for file in &changed {
        let mut matched = false;
        for unit in &config.unit {
            if unit.watch.iter().any(|pattern| {
                Glob::new(pattern).is_ok_and(|glob| glob.compile_matcher().is_match(file))
            }) {
                selected.insert(unit.id.clone());
                matched = true;
            }
        }
        if !matched && !globset.is_match(file) {
            return ordered_units(&config.unit, None);
        }
    }
    let mut expanded = true;
    while expanded {
        expanded = false;
        for unit in &config.unit {
            if selected.contains(&unit.id) {
                for dependency in &unit.depends_on {
                    expanded |= selected.insert(dependency.clone());
                }
            } else if unit
                .depends_on
                .iter()
                .any(|dependency| selected.contains(dependency))
            {
                expanded |= selected.insert(unit.id.clone());
            }
        }
    }
    ordered_units(&config.unit, Some(&selected))
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
        .arg(format!("{base}..{head}"))
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

fn run_layers(root: &Path, units: &[&CiUnit], run_scope: Scope) -> Result<(), GeneratorError> {
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
                thread_scope.spawn(move || {
                    let commands = match run_scope {
                        Scope::Affected => unit.pr_commands.as_slice(),
                        Scope::Full => unit.full_commands.as_slice(),
                    };
                    let result = run_unit(root, unit, commands);
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
        let mut command = Command::new(cargo_program.unwrap_or_else(|| Path::new("cargo")));
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
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            manifests.push(path);
        }
    }
    Ok(())
}

pub(crate) fn enforce_policy(root: &Path) -> Result<(), GeneratorError> {
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
        inspect_workflow(workflow, &path, &mut failures);
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

fn inspect_workflow(workflow: &Mapping, path: &Path, failures: &mut usize) {
    let approved_policy_entrypoint = is_approved_policy_entrypoint(path, workflow);
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
            "jobs" => inspect_jobs(value, path, failures),
            _ => inspect_yaml_value(value, path, None, false, failures),
        }
    }
}

fn is_approved_policy_entrypoint(path: &Path, workflow: &Mapping) -> bool {
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
            .is_none_or(|value| !is_approved_policy_reusable(value))
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
            .all(|key| matches!(key.as_str(), "name" | "uses" | "permissions"))
}

fn inspect_jobs(value: &Value, path: &Path, failures: &mut usize) {
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
        let trusted_gate = mapping_value(job, "if")
            .and_then(Value::as_str)
            .is_some_and(has_trusted_runner_gate);
        let matrix = mapping_value(job, "strategy")
            .and_then(Value::as_mapping)
            .and_then(|strategy| mapping_value(strategy, "matrix"))
            .and_then(Value::as_mapping);
        inspect_mapping(job, path, matrix, trusted_gate, failures);
    }
}

fn inspect_yaml_value(
    value: &Value,
    path: &Path,
    matrix: Option<&Mapping>,
    trusted_gate: bool,
    failures: &mut usize,
) {
    match value {
        Value::Mapping(mapping) => {
            inspect_mapping(mapping, path, matrix, trusted_gate, failures);
        }
        Value::Sequence(sequence) => {
            for item in sequence {
                inspect_yaml_value(item, path, matrix, trusted_gate, failures);
            }
        }
        Value::Tagged(tagged) => {
            inspect_yaml_value(tagged.value(), path, matrix, trusted_gate, failures);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn inspect_mapping(
    mapping: &Mapping,
    path: &Path,
    matrix: Option<&Mapping>,
    trusted_gate: bool,
    failures: &mut usize,
) {
    for (key, value) in mapping {
        let key = key.as_str();
        match key {
            "pull_request_target" => {
                policy_failure(path, "pull_request_target is forbidden", failures);
            }
            "uses" => inspect_uses(value, path, failures),
            "runs-on" => inspect_runner(value, path, matrix, trusted_gate, failures),
            _ => inspect_yaml_value(value, path, matrix, trusted_gate, failures),
        }
    }
}

fn inspect_uses(value: &Value, path: &Path, failures: &mut usize) {
    let Some(action) = value.as_str() else {
        policy_failure(path, "uses must be a scalar reference", failures);
        return;
    };
    if is_approved_local_reusable(action) || is_approved_policy_reusable(action) {
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

fn is_approved_policy_reusable(value: &str) -> bool {
    value.split_once('@').is_some_and(|(path, reference)| {
        path == IMMUTABLE_POLICY_WORKFLOW && reference == IMMUTABLE_POLICY_WORKFLOW_REV
    })
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
    value.split_once('@').is_some_and(|(action, reference)| {
        !action.is_empty()
            && reference.len() == 40
            && reference
                .chars()
                .all(|character| character.is_ascii_hexdigit())
    })
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
    let branch = options.get("branch").map_or("main", String::as_str);
    if !valid_branch(branch) {
        return Err(GeneratorError::usage("invalid release branch"));
    }
    git_success([
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/remotes/origin/{branch}"),
    ])?;
    git_success([
        "merge-base",
        "--is-ancestor",
        &reference,
        &format!("refs/remotes/origin/{branch}"),
    ])?;
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

fn git_success<const N: usize>(arguments: [&str; N]) -> Result<(), GeneratorError> {
    let status = Command::new("git")
        .args(arguments)
        .status()
        .map_err(|error| GeneratorError::usage(format!("run git: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(GeneratorError::usage("git release verification failed"))
    }
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
            "name: Velnor workflow policy\non:\n  pull_request_target:\n    types: [opened, synchronize, reopened]\npermissions:\n  contents: read\njobs:\n  policy:\n    name: Policy\n    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@fe805b984d3e261d3686d7ec670792f8121306bc\n    permissions:\n      contents: read\n",
        )?;
        std::fs::write(
            root.join(".github/ci/project.toml"),
            format!("runners = \"{runners}\"\n"),
        )?;
        Ok(root)
    }

    fn run_policy(root: std::path::PathBuf) -> Result<bool, Box<dyn Error>> {
        let result = enforce_policy(&root).is_ok();
        std::fs::remove_dir_all(root)?;
        Ok(result)
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
    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@fe805b984d3e261d3686d7ec670792f8121306bc
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
    uses: tailrocks/velnor/.github/workflows/velnor-workflow-policy.yml@fe805b984d3e261d3686d7ec670792f8121306bc
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

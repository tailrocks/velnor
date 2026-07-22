//! Estate CI contract auditor (`VELNOR_PROJECTS_SETUP.md` §2.0–§2.12).

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const INLINE_MATRIX_MARKER: &str = "inputs.lanes == 'both'";
const SHA_LEN: usize = 40;

#[derive(Debug, Args)]
pub struct AuditCiArgs {
    /// Repository checkout to audit.
    #[arg(long, default_value = ".")]
    pub repo_path: PathBuf,
    /// Emit stable JSON findings.
    #[arg(long)]
    pub json: bool,
    /// Warm log file or numeric GitHub Actions run id.
    #[arg(long)]
    pub perf_log: Option<String>,
    /// GitHub repository used when --perf-log is a run id.
    #[arg(long)]
    pub repo: Option<String>,
    /// Comma-separated first-party crates allowed to compile in a warm run.
    #[arg(long, value_delimiter = ',')]
    pub first_party: Vec<String>,
    /// JSON array of repository paths to audit as one estate.
    #[arg(long)]
    pub estate: Option<PathBuf>,
    /// Skip latest-release lookups; floating refs remain errors.
    #[arg(long)]
    pub offline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum Severity {
    Error,
    Warn,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Finding {
    severity: Severity,
    rule: &'static str,
    file: String,
    path: String,
    message: String,
}

impl Finding {
    fn error(
        rule: &'static str,
        file: &str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            rule,
            file: file.to_string(),
            path: path.into(),
            message: message.into(),
        }
    }

    fn warn(
        rule: &'static str,
        file: &str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warn,
            rule,
            file: file.to_string(),
            path: path.into(),
            message: message.into(),
        }
    }

    fn info(
        rule: &'static str,
        file: &str,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Info,
            rule,
            file: file.to_string(),
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct EstateManifest {
    version: u32,
    #[serde(default)]
    defaults: BTreeMap<String, ConcernContract>,
    repositories: Vec<EstateRepository>,
}

#[derive(Debug, Deserialize)]
struct EstateRepository {
    name: String,
    path: PathBuf,
    concerns: BTreeMap<String, ConcernContract>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ConcernClassification {
    Required,
    Applicable,
    NonApplicable,
    RepoSpecific,
}

#[derive(Debug, Deserialize)]
struct ConcernContract {
    classification: ConcernClassification,
    evidence: String,
    #[serde(default)]
    implementations: Vec<ConcernImplementation>,
}

#[derive(Debug, Deserialize)]
struct ConcernImplementation {
    workflow: String,
    #[serde(default)]
    job_ids: Vec<String>,
    #[serde(default)]
    canonical_markers: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct WorkflowAuditProfile {
    workload_override: Option<bool>,
    legacy_uniform_warnings: bool,
}

pub fn audit_ci(args: AuditCiArgs) -> Result<()> {
    let estate = if let Some(estate) = &args.estate {
        let text = fs::read_to_string(estate)
            .with_context(|| format!("read estate file {}", estate.display()))?;
        Some(
            serde_json::from_str::<EstateManifest>(&text)
                .with_context(|| format!("parse estate manifest {}", estate.display()))?,
        )
    } else {
        None
    };
    let mut all = BTreeMap::new();
    if let Some(estate) = &estate {
        if estate.version != 1 {
            bail!(
                "unsupported estate manifest version {} (expected 1)",
                estate.version
            );
        }
        for repo in &estate.repositories {
            let canonical = repo.path.canonicalize().with_context(|| {
                format!(
                    "estate repository {} path {} does not exist",
                    repo.name,
                    repo.path.display()
                )
            })?;
            let workload_files = concern_implementations(repo, &estate.defaults, "lane-selection")
                .map(|concern| {
                    concern
                        .implementations
                        .iter()
                        .map(|implementation| implementation.workflow.as_str())
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            let mut findings =
                audit_repo_profile(&canonical, args.offline, Some(&workload_files), false)?;
            findings.extend(audit_concern_contract(repo, &estate.defaults, &canonical)?);
            findings.sort_by(|left, right| {
                (&left.file, &left.path, left.rule).cmp(&(&right.file, &right.path, right.rule))
            });
            all.insert(repo.name.clone(), findings);
        }
    } else {
        let root = &args.repo_path;
        let canonical = root.canonicalize().unwrap_or(root.clone());
        all.insert(
            canonical.display().to_string(),
            audit_repo(&canonical, args.offline)?,
        );
    }
    if let Some(log) = &args.perf_log {
        let root = args
            .repo_path
            .canonicalize()
            .unwrap_or(args.repo_path.clone());
        let text = read_perf_log(log, args.repo.as_deref())?;
        let first_party = if args.first_party.is_empty() {
            cargo_package_names(&root)
        } else {
            args.first_party.iter().cloned().collect()
        };
        all.entry(root.display().to_string())
            .or_default()
            .extend(audit_perf_log(&text, &first_party));
    }
    let errors = all
        .values()
        .flatten()
        .filter(|finding| finding.severity == Severity::Error)
        .count();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&all)?);
    } else {
        for (repo, findings) in &all {
            println!("audit-ci: {repo}");
            if findings.is_empty() {
                println!("  PASS");
            }
            for finding in findings {
                println!(
                    "  {:?} {} {} {} — {}",
                    finding.severity, finding.rule, finding.file, finding.path, finding.message
                );
            }
        }
    }
    if errors > 0 {
        bail!("audit-ci found {errors} error(s)");
    }
    Ok(())
}

fn audit_concern_contract(
    repo: &EstateRepository,
    defaults: &BTreeMap<String, ConcernContract>,
    root: &Path,
) -> Result<Vec<Finding>> {
    const REQUIRED_CONCERNS: [&str; 14] = [
        "lane-selection",
        "checkout",
        "tool-setup",
        "rust-ci",
        "integration-services",
        "cargo-cache",
        "docker-build",
        "artifacts",
        "docs-pages",
        "preview",
        "release",
        "renovate",
        "required-aggregator",
        "workflow-safety",
    ];
    let mut findings = Vec::new();
    for name in REQUIRED_CONCERNS {
        if !repo.concerns.contains_key(name) && !defaults.contains_key(name) {
            findings.push(Finding::error(
                "missing-required",
                "config/estate-repositories.json",
                format!("$.repositories[{}].concerns.{name}", repo.name),
                "classify this concern with evidence; absence is not non-applicability",
            ));
        }
    }
    for name in REQUIRED_CONCERNS {
        let Some(concern) = repo.concerns.get(name).or_else(|| defaults.get(name)) else {
            continue;
        };
        if concern.evidence.trim().is_empty() {
            findings.push(Finding::error(
                "missing-required",
                "config/estate-repositories.json",
                format!("$.repositories[{}].concerns.{name}.evidence", repo.name),
                "add evidence for this classification",
            ));
        }
        match concern.classification {
            ConcernClassification::NonApplicable | ConcernClassification::RepoSpecific => {
                let rule = match concern.classification {
                    ConcernClassification::NonApplicable => "non-applicable",
                    ConcernClassification::RepoSpecific => "repo-specific",
                    _ => unreachable!(),
                };
                findings.push(Finding::info(
                    rule,
                    "config/estate-repositories.json",
                    format!("$.repositories[{}].concerns.{name}", repo.name),
                    &concern.evidence,
                ));
            }
            ConcernClassification::Required | ConcernClassification::Applicable => {
                if concern.implementations.is_empty() {
                    findings.push(Finding::error(
                        "missing-required",
                        "config/estate-repositories.json",
                        format!(
                            "$.repositories[{}].concerns.{name}.implementations",
                            repo.name
                        ),
                        "required/applicable concern must name every implementing workflow",
                    ));
                    continue;
                }
                for implementation in &concern.implementations {
                    let workflow = &implementation.workflow;
                    let path = root.join(".github/workflows").join(workflow);
                    if !path.is_file() {
                        findings.push(Finding::error(
                            "missing-required",
                            &format!(".github/workflows/{workflow}"),
                            "$",
                            format!(
                                "{name} is classified {:?} but its workflow is absent",
                                concern.classification
                            ),
                        ));
                        continue;
                    }
                    let text = fs::read_to_string(&path)
                        .with_context(|| format!("read concern workflow {}", path.display()))?;
                    let yaml: Value = serde_yaml::from_str(&text)
                        .with_context(|| format!("parse concern workflow {}", path.display()))?;
                    let jobs = object_get(&yaml, "jobs").and_then(Value::as_mapping);
                    for job_id in &implementation.job_ids {
                        if jobs.is_none_or(|jobs| mapping_get(jobs, job_id).is_none()) {
                            findings.push(Finding::error(
                                "canonical-drift",
                                &format!(".github/workflows/{workflow}"),
                                format!("$.jobs.{job_id}"),
                                format!("{name} must use canonical job id {job_id}"),
                            ));
                        }
                    }
                    let marker_sources = if implementation.job_ids.is_empty() {
                        vec![("$".to_string(), text.clone())]
                    } else {
                        let workflow_env =
                            object_get(&yaml, "env").map(compact).unwrap_or_default();
                        implementation
                            .job_ids
                            .iter()
                            .filter_map(|job_id| {
                                jobs.and_then(|jobs| mapping_get(jobs, job_id)).map(|job| {
                                    (
                                        format!("$.jobs.{job_id}"),
                                        format!("{workflow_env}\n{}", compact(job)),
                                    )
                                })
                            })
                            .collect()
                    };
                    for (job_path, source) in marker_sources {
                        let mut marker_offset = 0;
                        for marker in &implementation.canonical_markers {
                            if let Some(relative) = source[marker_offset..].find(marker) {
                                marker_offset += relative + marker.len();
                            } else if source.contains(marker) {
                                findings.push(Finding::error(
                                    "canonical-drift",
                                    &format!(".github/workflows/{workflow}"),
                                    &job_path,
                                    format!("{name} canonical marker {marker:?} is out of order"),
                                ));
                            } else {
                                findings.push(Finding::error(
                                    "canonical-drift",
                                    &format!(".github/workflows/{workflow}"),
                                    &job_path,
                                    format!("{name} is missing canonical marker {marker:?}"),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(findings)
}

fn concern_implementations<'a>(
    repo: &'a EstateRepository,
    defaults: &'a BTreeMap<String, ConcernContract>,
    name: &str,
) -> Option<&'a ConcernContract> {
    repo.concerns.get(name).or_else(|| defaults.get(name))
}

fn audit_repo(root: &Path, offline: bool) -> Result<Vec<Finding>> {
    audit_repo_profile(root, offline, None, true)
}

fn audit_repo_profile(
    root: &Path,
    offline: bool,
    workload_files: Option<&BTreeSet<&str>>,
    legacy_uniform_warnings: bool,
) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    findings.extend(audit_test_runner_surfaces(root)?);
    let mise_path = root.join("mise.toml");
    if mise_path.is_file() {
        let text = fs::read_to_string(&mise_path)
            .with_context(|| format!("read {}", mise_path.display()))?;
        audit_prebuilt_tool_surface("mise.toml", &text, &mut findings);
    }
    if !root.join(".github/AGENTS.md").is_file() {
        findings.push(Finding::error(
            "uniform-agents",
            ".github/AGENTS.md",
            "$",
            "add the shared estate CI instructions",
        ));
    }
    let workflow_dir = root.join(".github/workflows");
    if !workflow_dir.is_dir() {
        findings.push(Finding::error(
            "workflows",
            ".github/workflows",
            "$",
            "add canonical workflows",
        ));
        return Ok(findings);
    }
    let mut latest = BTreeMap::new();
    for path in yaml_files(&workflow_dir)? {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let yaml: Value =
            serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        let workload = workload_files.map(|files| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| files.contains(name))
        });
        audit_workflow(
            &relative,
            &text,
            &yaml,
            offline,
            WorkflowAuditProfile {
                workload_override: workload,
                legacy_uniform_warnings,
            },
            &mut latest,
            &mut findings,
        );
    }
    findings.sort_by(|left, right| {
        (&left.file, &left.path, left.rule).cmp(&(&right.file, &right.path, right.rule))
    });
    Ok(findings)
}

fn audit_test_runner_surfaces(root: &Path) -> Result<Vec<Finding>> {
    let mut files = Vec::new();
    collect_test_runner_files(root, root, &mut files)?;
    files.sort();
    let mut findings = Vec::new();
    for path in files {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if path.extension().is_some_and(|extension| extension == "rs")
                && !trimmed.starts_with("///")
                && !trimmed.starts_with("//!")
            {
                continue;
            }
            if is_cargo_test_instruction(line) {
                findings.push(Finding::error(
                    "test-runner",
                    &relative,
                    format!("line {}", index + 1),
                    "use cargo nextest run; cargo test is forbidden by the estate test-runner contract",
                ));
            }
        }
    }
    Ok(findings)
}

fn collect_test_runner_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry.with_context(|| format!("read entry under {}", directory.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("read file type for {}", path.display()))?;
        if file_type.is_dir() {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if !is_historical_or_generated_directory(relative) {
                collect_test_runner_files(root, &path, files)?;
            }
        } else if file_type.is_file() && is_test_runner_surface(&path) {
            files.push(path);
        }
    }
    Ok(())
}

fn is_historical_or_generated_directory(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(
                ".git"
                    | ".velnor-compare"
                    | "target"
                    | "node_modules"
                    | "plans"
                    | "migrations"
                    | "research"
                    | "validation"
                    | "evidence"
                    | "history"
                    | "benchmarks"
            )
        )
    })
}

fn is_test_runner_surface(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if matches!(name, "compatibility.toml" | "Cargo.lock") {
        return false;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension,
                "sh" | "bash" | "zsh" | "md" | "mdx" | "rs" | "toml" | "yml" | "yaml"
            )
        })
        || matches!(name, "Justfile" | "Makefile")
}

fn is_cargo_test_instruction(line: &str) -> bool {
    let line = line
        .trim_start()
        .strip_prefix("///")
        .or_else(|| line.trim_start().strip_prefix("//!"))
        .or_else(|| line.trim_start().strip_prefix('#'))
        .unwrap_or_else(|| line.trim_start())
        .trim_start_matches([' ', '\t', '`', '>', '-', '*']);
    let Some(index) = line.find("cargo test") else {
        return false;
    };
    let prefix = line[..index].trim();
    prefix.is_empty()
        || matches!(prefix, "rtk" | "rtk proxy" | "mise x --" | "mise exec --")
        || prefix.ends_with("&&")
        || prefix.ends_with('"')
        || prefix
            .split_whitespace()
            .all(|token| token.contains('=') && !token.starts_with('='))
}

fn has_unexplained_sudo(run: &str) -> bool {
    let lines = run.lines().collect::<Vec<_>>();
    lines.iter().enumerate().any(|(index, line)| {
        let line = line.trim();
        let invokes_sudo = line.starts_with("sudo ")
            || line.contains("&& sudo ")
            || line.contains("; sudo ")
            || line.contains("| sudo ");
        invokes_sudo
            && !index.checked_sub(1).is_some_and(|previous| {
                lines[previous]
                    .trim()
                    .starts_with("# velnor-sudo-exception:")
            })
    })
}

fn audit_workflow(
    file: &str,
    text: &str,
    yaml: &Value,
    offline: bool,
    profile: WorkflowAuditProfile,
    latest: &mut BTreeMap<String, Option<String>>,
    findings: &mut Vec<Finding>,
) {
    audit_prebuilt_tool_surface(file, text, findings);
    let workload = profile
        .workload_override
        .unwrap_or_else(|| has_trigger(yaml, "push") || has_trigger(yaml, "pull_request"));
    let file_name = Path::new(file)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(file);
    let canonical_files = [
        "ci.yml",
        "release.yml",
        "docs.yml",
        "preview.yml",
        "renovate.yml",
    ];
    if profile.legacy_uniform_warnings && !canonical_files.contains(&file_name) {
        findings.push(Finding::warn(
            "uniform-workflow-name",
            file,
            "$",
            "rename this executable concern to a canonical workflow filename",
        ));
    }
    if profile.legacy_uniform_warnings && workload && file_name != "ci.yml" {
        findings.push(Finding::warn(
            "uniform-workflow-name",
            file,
            "$",
            "push/pull_request workload workflow should be named ci.yml",
        ));
    }
    if object_get(yaml, "concurrency").is_none() {
        findings.push(Finding::error(
            "concurrency",
            file,
            "$.concurrency",
            "add workflow-level concurrency and cancel-in-progress policy",
        ));
    } else if let Some(group) = object_get(yaml, "concurrency")
        .and_then(|value| object_get(value, "group"))
        .and_then(Value::as_str)
    {
        let globally_serialized = object_get(yaml, "concurrency")
            .and_then(|value| object_get(value, "cancel-in-progress"))
            .and_then(Value::as_bool)
            == Some(false);
        if !group.contains("github.ref") && !globally_serialized {
            findings.push(Finding::warn(
                "uniform-concurrency",
                file,
                "$.concurrency.group",
                "include the workflow identity and github.ref, or set cancel-in-progress false for intentional global writer serialization",
            ));
        }
    }
    if workload
        && !is_native_apple_workflow(yaml)
        && (!has_lanes_input(yaml) || !text.contains(INLINE_MATRIX_MARKER))
    {
        findings.push(Finding::error(
            "lanes",
            file,
            "$.on.workflow_dispatch.inputs.lanes",
            "add lanes choice input and the canonical inline matrix",
        ));
    }
    let Some(jobs) = object_get(yaml, "jobs").and_then(Value::as_mapping) else {
        return;
    };
    for (job_key, job_value) in jobs {
        let job_id = job_key.clone();
        let job_path = format!("$.jobs.{job_id}");
        let canonical_jobs = [
            "rust",
            "integration",
            "audit",
            "build-image",
            "docs",
            "release",
            "ci-required",
        ];
        if profile.legacy_uniform_warnings && workload && !canonical_jobs.contains(&job_id.as_str())
        {
            findings.push(Finding::warn(
                "uniform-job-id",
                file,
                &job_path,
                "use the shared canonical job vocabulary for this concern",
            ));
        }
        let Some(job) = job_value.as_mapping() else {
            continue;
        };
        let job_text = compact(job_value);
        if mapping_get(job, "timeout-minutes").is_none() && mapping_get(job, "uses").is_none() {
            findings.push(Finding::error(
                "timeout",
                file,
                format!("{job_path}.timeout-minutes"),
                "set a measured timeout-minutes budget",
            ));
        }
        if job_text.contains("playwright")
            && job_text.contains(" install")
            && !job_text.contains(".cache/ms-playwright")
        {
            findings.push(Finding::error(
                "playwright-cache",
                file,
                &job_path,
                "cache ~/.cache/ms-playwright with a lockfile-derived key before installing browsers",
            ));
        }
        if let Some(runs_on) = mapping_get(job, "runs-on") {
            let value = compact(runs_on);
            let native_apple_build = is_native_apple_job(job);
            if !native_apple_build
                && ["ubuntu-latest", "ubuntu-24.04", "macos-", "windows-"]
                    .iter()
                    .any(|forbidden| value.contains(forbidden))
            {
                findings.push(Finding::error(
                    "runner-os",
                    file,
                    format!("{job_path}.runs-on"),
                    "use the canonical matrix with ubuntu-26.04",
                ));
            }
        }
        let Some(steps) = mapping_get(job, "steps").and_then(Value::as_sequence) else {
            continue;
        };
        audit_steps(
            file, &job_id, &job_path, steps, text, offline, latest, findings,
        );
    }
}

fn audit_prebuilt_tool_surface(file: &str, text: &str, findings: &mut Vec<Finding>) {
    for (index, line) in text.lines().enumerate() {
        if line.contains("cargo:cargo-nextest") {
            findings.push(Finding::error(
                "prebuilt-tool",
                file,
                format!("line {}", index + 1),
                "install nextest from aqua:nextest-rs/nextest/cargo-nextest; CI tooling must not compile from source",
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn audit_steps(
    file: &str,
    job_id: &str,
    job_path: &str,
    steps: &[Value],
    raw: &str,
    offline: bool,
    latest: &mut BTreeMap<String, Option<String>>,
    findings: &mut Vec<Finding>,
) {
    let mut compile = false;
    let mut sccache = false;
    let mut swatinem = false;
    let mut target_cache = false;
    let mut target_cache_generation = false;
    let mut target_dir_override = false;
    let mut unstable_target_dir = false;
    let mut literal_target_cache = false;
    let mut first_compile_step = None;
    let mut first_target_cache_step = None;
    for (index, step) in steps.iter().enumerate() {
        let path = format!("{job_path}.steps[{index}]");
        let run = object_get(step, "run")
            .and_then(Value::as_str)
            .unwrap_or("");
        target_dir_override |= run.contains("CARGO_TARGET_DIR=");
        unstable_target_dir |= run.contains("CARGO_TARGET_DIR=")
            && (run.contains("GITHUB_RUN_ID") || run.contains("GITHUB_RUN_ATTEMPT"));
        let step_compiles = run.lines().any(|line| {
            let line = line.trim_start();
            [
                "cargo build",
                "cargo check",
                "cargo clippy",
                "cargo test",
                "cargo nextest",
                "cargo run",
                "cargo zigbuild",
                "cargo xtask",
                "rustc ",
            ]
            .iter()
            .any(|command| line.starts_with(command) || line.contains(&format!(" {command}")))
        });
        compile |= step_compiles;
        if step_compiles {
            first_compile_step.get_or_insert(index);
        }
        if run.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("cargo test") || line.contains(" cargo test")
        }) {
            findings.push(Finding::error(
                "test-runner",
                file,
                format!("{path}.run"),
                "use cargo nextest run; cargo test is forbidden by the estate test-runner contract",
            ));
        }
        for marker in ["::set-output", "::save-state", "node12", "node16"] {
            if run.contains(marker) {
                findings.push(Finding::error(
                    "deprecated",
                    file,
                    format!("{path}.run"),
                    format!("replace deprecated marker {marker}"),
                ));
            }
        }
        if run.contains("sccache --show-stats") || run.contains("kache --show-stats") {
            findings.push(Finding::error(
                "cache-reporting",
                file,
                format!("{path}.run"),
                "remove ad-hoc cache CLI reporting; the setup action/native adapter post step owns the report",
            ));
        }
        if has_unexplained_sudo(run) {
            findings.push(Finding::error(
                "privilege",
                file,
                format!("{path}.run"),
                "remove sudo; only a proven OS-package boundary may retain it with an immediately preceding # velnor-sudo-exception: reason",
            ));
        }
        let lane_identity_run = run
            .lines()
            .filter(|line| !line.trim_start().starts_with("Description:"))
            .collect::<Vec<_>>()
            .join("\n")
            .replace("--deny-self-hosted-runners", "");
        if lane_identity_run.contains("self-hosted")
            || lane_identity_run.contains("velnor-target-mvp")
            || lane_identity_run.contains("ubuntu-26.04")
        {
            findings.push(Finding::error(
                "lane-conditional",
                file,
                format!("{path}.run"),
                "remove hardcoded runner identity from workload scripts",
            ));
        }
        if let Some(condition) = object_get(step, "if").and_then(Value::as_str) {
            if condition.contains("matrix.config.lane") {
                findings.push(Finding::error(
                    "lane-conditional",
                    file,
                    format!("{path}.if"),
                    "step lane branching is forbidden; only matrix.config.writer is sanctioned",
                ));
            }
        }
        let Some(uses) = object_get(step, "uses").and_then(Value::as_str) else {
            continue;
        };
        let family = uses.split('@').next().unwrap_or(uses);
        if matches!(
            family,
            "dtolnay/rust-toolchain" | "EmbarkStudios/cargo-deny-action"
        ) {
            findings.push(Finding::error(
                "toolchain",
                file,
                format!("{path}.uses"),
                "install tools through pinned mise configuration",
            ));
        }
        if family == "actions/create-release" {
            findings.push(Finding::error(
                "deprecated",
                file,
                format!("{path}.uses"),
                "replace the deprecated create-release action",
            ));
        }
        sccache |= family == "mozilla-actions/sccache-action";
        swatinem |= family == "Swatinem/rust-cache";
        if family == "actions/cache" {
            if let Some(with) = object_get(step, "with") {
                let caches_target = object_get(with, "path").is_some_and(|value| {
                    compact(value).lines().any(|line| line.contains("target"))
                });
                target_cache |= caches_target;
                if caches_target {
                    first_target_cache_step.get_or_insert(index);
                }
                literal_target_cache |= object_get(with, "path").is_some_and(|value| {
                    compact(value).lines().any(|line| line.trim() == "target")
                });
                if caches_target {
                    let key = object_get(with, "key").map(compact).unwrap_or_default();
                    let restore = object_get(with, "restore-keys")
                        .map(compact)
                        .unwrap_or_default();
                    target_cache_generation |= key.contains("github.sha")
                        && !key.contains("github.ref")
                        && !restore.contains("github.sha")
                        && !restore.contains("github.ref");
                }
            }
        }
        audit_ref(file, &path, uses, raw, offline, latest, findings);
        if family == "mozilla-actions/sccache-action" {
            let gha = object_get(step, "env")
                .and_then(|env| object_get(env, "SCCACHE_GHA_ENABLED"))
                .or_else(|| {
                    object_get(step, "with")
                        .and_then(|with| object_get(with, "SCCACHE_GHA_ENABLED"))
                })
                .map(compact);
            // Workflow-level env is checked below from raw text because the
            // action may intentionally inherit the canonical value.
            if gha.as_deref() != Some("false") && !raw.contains("SCCACHE_GHA_ENABLED: \"false\"") {
                findings.push(Finding::error(
                    "sccache-local",
                    file,
                    path.clone(),
                    "set SCCACHE_GHA_ENABLED to false",
                ));
            }
        }
        if family == "actions/checkout"
            && object_get(step, "with")
                .and_then(|with| object_get(with, "fetch-depth"))
                .is_some_and(|value| compact(value) == "0")
            && !raw
                .lines()
                .any(|line| line.contains("fetch-depth: 0") && line.contains('#'))
        {
            findings.push(Finding::warn(
                "fetch-depth",
                file,
                path,
                "justify fetch-depth: 0 with a same-line consumer comment",
            ));
        }
    }
    if compile && !sccache && !matches!(job_id, "cache-off" | "cache-kache") {
        findings.push(Finding::error(
            "compile-cache",
            file,
            job_path,
            "compile job must set up the pinned sccache action",
        ));
    }
    if swatinem && sccache {
        findings.push(Finding::error(
            "double-cache",
            file,
            job_path,
            "remove Swatinem/rust-cache from the sccache job",
        ));
    }
    if compile && !target_cache && !matches!(job_id, "cache-off" | "cache-kache") {
        findings.push(Finding::error(
            "target-cache",
            file,
            job_path,
            "compile job must persist the Cargo target through actions/cache",
        ));
    }
    if target_cache && !target_cache_generation {
        findings.push(Finding::error(
            "target-cache-key",
            file,
            job_path,
            "target cache key must include github.sha while its restore prefix omits ref/SHA so main seeds PRs and each successful commit saves an updated generation",
        ));
    }
    if first_compile_step
        .zip(first_target_cache_step)
        .is_some_and(|(compile_step, cache_step)| cache_step > compile_step)
    {
        findings.push(Finding::error(
            "target-cache-order",
            file,
            job_path,
            "restore the Cargo target cache before the first compiling step",
        ));
    }
    if target_dir_override && literal_target_cache {
        findings.push(Finding::error(
            "target-cache-path",
            file,
            job_path,
            "cache the effective CARGO_TARGET_DIR, not literal target",
        ));
    }
    if unstable_target_dir {
        findings.push(Finding::error(
            "target-cache-path",
            file,
            job_path,
            "use a stable job-scoped CARGO_TARGET_DIR; run-specific paths invalidate restored Cargo fingerprints",
        ));
    }
}

fn audit_ref(
    file: &str,
    path: &str,
    uses: &str,
    raw: &str,
    offline: bool,
    latest: &mut BTreeMap<String, Option<String>>,
    findings: &mut Vec<Finding>,
) {
    if uses.starts_with("./") || uses.starts_with("docker://") {
        return;
    }
    let Some((family, reference)) = uses.split_once('@') else {
        findings.push(Finding::error(
            "action-pin",
            file,
            format!("{path}.uses"),
            "pin action to a 40-character commit SHA",
        ));
        return;
    };
    if reference.len() != SHA_LEN || !reference.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        findings.push(Finding::error(
            "action-pin",
            file,
            format!("{path}.uses"),
            "pin action to a 40-character commit SHA",
        ));
        return;
    }
    if offline || family.split('/').count() != 2 {
        return;
    }
    let tag = latest
        .entry(family.to_string())
        .or_insert_with(|| latest_release_tag(family))
        .clone();
    if let Some(tag) = tag {
        let major = tag.trim_start_matches('v').split('.').next().unwrap_or("");
        if !major.is_empty() && !uses_comment_major(raw, uses, major) {
            findings.push(Finding::warn(
                "action-major",
                file,
                format!("{path}.uses"),
                format!("verify pin is latest stable major v{major} ({tag})"),
            ));
        }
    }
}

// YAML values do not retain comments. Keep release lookup advisory and let
// Renovate plus the SHA comment enforce the exact pin in repository diffs.
fn uses_comment_major(raw: &str, uses: &str, major: &str) -> bool {
    raw.lines()
        .any(|line| line.contains(uses) && line.contains(&format!("# v{major}")))
}

fn latest_release_tag(family: &str) -> Option<String> {
    let output = Command::new("gh")
        .args([
            "api",
            &format!("repos/{family}/releases/latest"),
            "--jq",
            ".tag_name",
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn read_perf_log(value: &str, repo: Option<&str>) -> Result<String> {
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        let repo = repo.context("--repo is required when --perf-log is a run id")?;
        let output = Command::new("gh")
            .args(["run", "view", value, "--repo", repo, "--log"])
            .output()
            .context("fetch workflow log")?;
        if !output.status.success() {
            bail!(
                "gh run view failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        fs::read_to_string(value).with_context(|| format!("read perf log {value}"))
    }
}

fn audit_perf_log(text: &str, first_party: &BTreeSet<String>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let normalized = strip_log_prefix(line);
        let dependency_compile = normalized
            .split_once("Compiling ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|name| !first_party.contains(name) && rest_has_version(normalized));
        let marker = normalized.contains("Downloading crates")
            || normalized.contains("Updating crates.io index")
            || dependency_compile
            || normalized.to_ascii_lowercase().contains("mise")
                && (normalized.to_ascii_lowercase().contains("download")
                    || normalized.to_ascii_lowercase().contains("installing"))
            || normalized.to_ascii_lowercase().contains("cargo install ");
        if marker {
            findings.push(Finding::error(
                "perf-warm",
                "<perf-log>",
                format!("line {}", index + 1),
                format!("warm run performed download/compile/tool install: {normalized}"),
            ));
        }
    }
    findings
}

fn rest_has_version(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        word.strip_prefix('v')
            .is_some_and(|version| version.chars().next().is_some_and(|c| c.is_ascii_digit()))
    })
}

fn strip_log_prefix(line: &str) -> &str {
    line.find("Compiling ")
        .or_else(|| line.find("Downloading "))
        .or_else(|| line.find("Updating "))
        .map_or(line, |index| &line[index..])
}

fn cargo_package_names(root: &Path) -> BTreeSet<String> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output();
    let Ok(output) = output else {
        return BTreeSet::new();
    };
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .ok()
        .and_then(|value| value.get("packages").cloned())
        .and_then(|value| value.as_array().cloned())
        .into_iter()
        .flatten()
        .filter_map(|package| {
            package
                .get("name")
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .collect()
}

fn yaml_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let path = entry?.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "yml" | "yaml"))
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn has_trigger(yaml: &Value, name: &str) -> bool {
    object_get(yaml, "on")
        .and_then(Value::as_mapping)
        .is_some_and(|on| mapping_get(on, name).is_some())
}

fn has_lanes_input(yaml: &Value) -> bool {
    object_get(yaml, "on")
        .and_then(|on| object_get(on, "workflow_dispatch"))
        .and_then(|dispatch| object_get(dispatch, "inputs"))
        .and_then(|inputs| object_get(inputs, "lanes"))
        .is_some()
}

fn is_native_apple_workflow(yaml: &Value) -> bool {
    let Some(jobs) = object_get(yaml, "jobs").and_then(Value::as_mapping) else {
        return false;
    };
    !jobs.is_empty()
        && jobs
            .values()
            .all(|job| job.as_mapping().is_some_and(is_native_apple_job))
}

fn is_native_apple_job(job: &serde_yaml::Mapping) -> bool {
    mapping_get(job, "runs-on").is_some_and(|runs_on| compact(runs_on).starts_with("macos-"))
        && mapping_get(job, "steps")
            .and_then(Value::as_sequence)
            .is_some_and(|steps| {
                steps.iter().any(|step| {
                    object_get(step, "run")
                        .and_then(Value::as_str)
                        .is_some_and(|run| {
                            [
                                "./scripts/build-native-app.sh",
                                "./scripts/build-xcframework.sh",
                                "xcodebuild ",
                                "codesign ",
                                "xcrun notarytool ",
                                "swift test ",
                            ]
                            .iter()
                            .any(|marker| run.contains(marker))
                        })
                })
            })
}

fn object_get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value
        .as_mapping()
        .and_then(|mapping| mapping_get(mapping, key))
}

fn mapping_get<'a>(mapping: &'a serde_yaml::Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(key)
}

fn compact(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Sequence(values) => values.iter().map(compact).collect::<Vec<_>>().join("\n"),
        Value::Mapping(values) => values
            .iter()
            .map(|(key, value)| format!("{}={}", key, compact(value)))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Tagged(value) => compact(value.value()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audit(yaml: &str) -> Vec<Finding> {
        let value: Value = serde_yaml::from_str(yaml).unwrap();
        let mut findings = Vec::new();
        audit_workflow(
            ".github/workflows/ci.yml",
            yaml,
            &value,
            true,
            WorkflowAuditProfile {
                workload_override: None,
                legacy_uniform_warnings: true,
            },
            &mut BTreeMap::new(),
            &mut findings,
        );
        findings
    }

    fn has_rule(findings: &[Finding], rule: &str) -> bool {
        findings.iter().any(|finding| finding.rule == rule)
    }

    const BASE: &str = r#"
on:
  push:
  pull_request:
  workflow_dispatch:
    inputs:
      lanes: {type: choice, options: [velnor, github, both]}
concurrency:
  group: ci-${{ github.ref }}
jobs:
  rust:
    timeout-minutes: 20
    strategy:
      matrix:
        config: ${{ fromJSON(inputs.lanes == 'both' && '[]' || '[]') }}
    runs-on: ${{ matrix.config.runner }}
    steps:
      - uses: actions/checkout@0123456789012345678901234567890123456789
      - uses: mozilla-actions/sccache-action@0123456789012345678901234567890123456789
        env: {SCCACHE_GHA_ENABLED: "false"}
      - uses: actions/cache@0123456789012345678901234567890123456789
        with:
          path: target
          key: rust-build-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}-${{ github.sha }}
          restore-keys: rust-build-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}-
      - run: cargo nextest run --workspace --locked
"#;

    #[test]
    fn canonical_workflow_passes_static_rules() {
        assert!(audit(BASE).is_empty());
    }

    #[test]
    fn rejects_source_installed_nextest() {
        let yaml = BASE.replace(
            "      - run: cargo nextest run --workspace --locked",
            "      - uses: jdx/mise-action@0123456789012345678901234567890123456789\n        with:\n          install_args: rust cargo:cargo-nextest\n      - run: cargo nextest run --workspace --locked",
        );
        assert!(has_rule(&audit(&yaml), "prebuilt-tool"));
    }

    #[test]
    fn cross_compile_job_requires_target_cache() {
        let yaml = BASE
            .replace(
                "      - uses: actions/cache@0123456789012345678901234567890123456789\n        with:\n          path: target\n          key: rust-build-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}-${{ github.sha }}\n          restore-keys: rust-build-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}-\n",
                "",
            )
            .replace(
                "cargo nextest run --workspace --locked",
                "cargo zigbuild --target x86_64-unknown-linux-musl",
            );
        assert!(has_rule(&audit(&yaml), "target-cache"));
    }

    #[test]
    fn xtask_job_requires_target_cache() {
        let yaml = BASE
            .replace(
                "      - uses: actions/cache@0123456789012345678901234567890123456789\n        with:\n          path: target\n          key: rust-build-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}-${{ github.sha }}\n          restore-keys: rust-build-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}-\n",
                "",
            )
            .replace(
                "cargo nextest run --workspace --locked",
                "cargo xtask policy --output github",
            );
        assert!(has_rule(&audit(&yaml), "target-cache"));
    }

    #[test]
    fn target_override_rejects_literal_target_cache() {
        let yaml = BASE.replace(
            "      - run: cargo nextest run --workspace --locked",
            "      - run: echo 'CARGO_TARGET_DIR=/tmp/job-target' >> \"$GITHUB_ENV\"\n      - run: cargo nextest run --workspace --locked",
        );
        assert!(has_rule(&audit(&yaml), "target-cache-path"));
    }

    #[test]
    fn target_override_rejects_run_specific_path() {
        let yaml = BASE.replace(
            "      - run: cargo nextest run --workspace --locked",
            "      - run: echo 'CARGO_TARGET_DIR=/tmp/target-${GITHUB_RUN_ID}' >> \"$GITHUB_ENV\"\n      - run: cargo nextest run --workspace --locked",
        );
        assert!(has_rule(&audit(&yaml), "target-cache-path"));
    }

    #[test]
    fn target_cache_must_precede_compilation() {
        let cache = "      - uses: actions/cache@0123456789012345678901234567890123456789\n        with:\n          path: target\n          key: rust-build-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}-${{ github.sha }}\n          restore-keys: rust-build-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}-\n";
        let yaml = BASE.replace(cache, "").replace(
            "      - run: cargo nextest run --workspace --locked",
            &format!("      - run: cargo nextest run --workspace --locked\n{cache}"),
        );
        assert!(has_rule(&audit(&yaml), "target-cache-order"));
    }

    #[test]
    fn rejects_source_installed_nextest_in_mise_config() {
        let mut findings = Vec::new();
        audit_prebuilt_tool_surface(
            "mise.toml",
            "[tools]\n\"cargo:cargo-nextest\" = \"0.9.140\"\n",
            &mut findings,
        );
        assert!(has_rule(&findings, "prebuilt-tool"));
    }

    #[test]
    fn rejects_forbidden_runner_os() {
        assert!(has_rule(
            &audit(&BASE.replace("${{ matrix.config.runner }}", "ubuntu-latest")),
            "runner-os"
        ));
    }

    #[test]
    fn allows_native_apple_application_build_on_macos() {
        let yaml = BASE
            .replace("${{ matrix.config.runner }}", "macos-26")
            .replace(
                "      - run: cargo nextest run --workspace --locked",
                "      - run: ./scripts/build-native-app.sh",
            );
        assert!(!has_rule(&audit(&yaml), "runner-os"));
    }

    #[test]
    fn native_apple_workflow_does_not_require_fake_linux_lanes() {
        let yaml = r#"
on:
  push:
  workflow_dispatch:
concurrency:
  group: native-${{ github.ref }}
  cancel-in-progress: true
jobs:
  native:
    runs-on: macos-26
    timeout-minutes: 20
    steps:
      - run: ./scripts/build-native-app.sh
"#;
        let findings = audit(yaml);
        assert!(!has_rule(&findings, "lanes"), "{findings:?}");
        assert!(!has_rule(&findings, "runner-os"), "{findings:?}");
    }

    #[test]
    fn native_apple_release_does_not_require_a_linux_runner() {
        let yaml = r#"
on:
  workflow_dispatch:
concurrency:
  group: native-release-${{ github.ref }}
  cancel-in-progress: false
jobs:
  release:
    runs-on: macos-26
    timeout-minutes: 90
    steps:
      - run: |
          xcodebuild archive -project native/App.xcodeproj
          codesign --verify --deep --strict TableRock.app
          xcrun notarytool submit TableRock.zip --wait
"#;
        let findings = audit(yaml);
        assert!(!has_rule(&findings, "runner-os"), "{findings:?}");
    }

    #[test]
    fn requires_lanes_matrix() {
        assert!(has_rule(
            &audit(&BASE.replace(INLINE_MATRIX_MARKER, "inputs.lanes == 'github'")),
            "lanes"
        ));
    }

    #[test]
    fn rejects_legacy_toolchain_action() {
        assert!(has_rule(
            &audit(&BASE.replace("actions/checkout", "dtolnay/rust-toolchain")),
            "toolchain"
        ));
    }

    #[test]
    fn compile_job_requires_sccache() {
        let yaml = BASE
            .lines()
            .filter(|line| {
                !line.contains("mozilla-actions/sccache-action") && !line.contains("env: {SCCACHE")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(has_rule(&audit(&yaml), "compile-cache"));
    }

    #[test]
    fn compile_job_requires_updatable_ref_independent_target_cache() {
        let missing = BASE
            .lines()
            .filter(|line| {
                !line.contains("actions/cache@")
                    && !line.trim_start().starts_with("path: target")
                    && !line.trim_start().starts_with("key: rust-build-")
                    && !line.trim_start().starts_with("restore-keys: rust-build-")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(has_rule(&audit(&missing), "target-cache"));

        let ref_scoped = BASE.replace("${{ github.sha }}", "${{ github.ref }}");
        assert!(has_rule(&audit(&ref_scoped), "target-cache-key"));
    }

    #[test]
    fn playwright_install_requires_browser_cache() {
        let yaml = BASE.replace(
            "      - run: cargo nextest run --workspace --locked",
            "      - run: bunx playwright install --with-deps chromium",
        );
        assert!(has_rule(&audit(&yaml), "playwright-cache"));

        let cached = yaml.replace(
            "          path: target",
            "          path: |\n            target\n            ~/.cache/ms-playwright",
        );
        assert!(!has_rule(&audit(&cached), "playwright-cache"));
    }

    #[test]
    fn requires_concurrency_and_timeout() {
        let yaml = BASE
            .replace("concurrency:\n  group: ci-${{ github.ref }}\n", "")
            .replace("    timeout-minutes: 20\n", "");
        let findings = audit(&yaml);
        assert!(has_rule(&findings, "concurrency"));
        assert!(has_rule(&findings, "timeout"));
    }

    #[test]
    fn allows_non_cancellable_global_writer_serialization() {
        let yaml = BASE.replace(
            "  group: ci-${{ github.ref }}",
            "  group: release\n  cancel-in-progress: false",
        );
        assert!(!has_rule(&audit(&yaml), "uniform-concurrency"));
    }

    #[test]
    fn warns_for_cancellable_global_concurrency() {
        let yaml = BASE.replace(
            "  group: ci-${{ github.ref }}",
            "  group: release\n  cancel-in-progress: true",
        );
        assert!(has_rule(&audit(&yaml), "uniform-concurrency"));
    }

    #[test]
    fn fetch_depth_without_comment_warns() {
        let yaml = BASE.replace("- uses: actions/checkout@0123456789012345678901234567890123456789", "- uses: actions/checkout@0123456789012345678901234567890123456789\n        with: {fetch-depth: 0}");
        assert!(has_rule(&audit(&yaml), "fetch-depth"));
    }

    #[test]
    fn rejects_double_cache() {
        let yaml = BASE.replace("      - run: cargo nextest run --workspace --locked", "      - uses: Swatinem/rust-cache@0123456789012345678901234567890123456789\n      - run: cargo nextest run --workspace --locked");
        assert!(has_rule(&audit(&yaml), "double-cache"));
    }

    #[test]
    fn rejects_cargo_test_runner() {
        let yaml = BASE.replace(
            "cargo nextest run --workspace --locked",
            "cargo test --workspace --locked",
        );
        assert!(has_rule(&audit(&yaml), "test-runner"));
    }

    #[test]
    fn recognizes_live_cargo_test_instructions_without_flagging_prose() {
        for line in [
            "cargo test --workspace --locked",
            "rtk cargo test -p crate",
            "//! rtk cargo test -p crate",
            "FOO=bar cargo test --lib",
            "command = \"cargo test --workspace\"",
            "fmt && cargo test --workspace",
        ] {
            assert!(is_cargo_test_instruction(line), "missed {line:?}");
        }
        for line in [
            "Never use `cargo test`; use nextest.",
            "Historical cargo test failure caused the incident.",
        ] {
            assert!(!is_cargo_test_instruction(line), "false positive {line:?}");
        }
    }

    #[test]
    fn rejects_lane_condition_and_deprecated_command() {
        let yaml = BASE.replace(
            "      - run: cargo nextest run --workspace --locked",
            "      - if: matrix.config.lane == 'Velnor'\n        run: echo ::set-output name=x::y",
        );
        let findings = audit(&yaml);
        assert!(has_rule(&findings, "lane-conditional"));
        assert!(has_rule(&findings, "deprecated"));
    }

    #[test]
    fn permits_attestation_self_hosted_denial_policy() {
        let yaml = BASE.replace(
            "cargo nextest run --workspace --locked",
            "gh attestation verify artifact --deny-self-hosted-runners",
        );
        assert!(!has_rule(&audit(&yaml), "lane-conditional"));
    }

    #[test]
    fn permits_self_hosted_words_in_package_description() {
        let yaml = BASE.replace(
            "      - run: cargo nextest run --workspace --locked",
            "      - run: |\n          cat <<'EOF'\n          Description: apt repository for a self-hosted runner\n          EOF",
        );
        assert!(!has_rule(&audit(&yaml), "lane-conditional"));
    }

    #[test]
    fn rejects_unexplained_sudo_and_accepts_documented_exception() {
        assert!(has_unexplained_sudo("sudo chown -R user cache"));
        assert!(has_unexplained_sudo("mkdir cache && sudo chmod 777 cache"));
        assert!(!has_unexplained_sudo(
            "# velnor-sudo-exception: apt package has no user-space distribution\nsudo apt-get install reprepro"
        ));
        assert!(!has_unexplained_sudo("echo 'never use sudo here'"));
    }

    #[test]
    fn rejects_ad_hoc_compiler_cache_reporting() {
        let yaml = BASE.replace(
            "      - run: cargo nextest run --workspace --locked",
            "      - run: cargo nextest run --workspace --locked\n      - run: sccache --show-stats",
        );
        assert!(has_rule(&audit(&yaml), "cache-reporting"));
    }

    #[test]
    fn rejects_floating_action_ref() {
        assert!(has_rule(
            &audit(&BASE.replace("@0123456789012345678901234567890123456789", "@v6")),
            "action-pin"
        ));
    }

    #[test]
    fn cold_perf_markers_fail_but_first_party_compile_passes() {
        let first_party = BTreeSet::from(["my-crate".to_string()]);
        let findings = audit_perf_log(
            "Downloading crates ...\nCompiling serde v1.0.0\nCompiling my-crate v0.1.0",
            &first_party,
        );
        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn warm_perf_log_passes() {
        assert!(audit_perf_log("Finished test profile", &BTreeSet::new()).is_empty());
    }

    #[test]
    fn estate_manifest_parses_classified_repository() {
        let manifest: EstateManifest = serde_json::from_str(
            r#"{"version":1,"defaults":{},"repositories":[{"name":"one","path":"/one","concerns":{}}]}"#,
        )
        .unwrap();
        assert_eq!(manifest.repositories.len(), 1);
        assert_eq!(manifest.repositories[0].name, "one");
    }

    #[test]
    fn omitted_concern_is_not_treated_as_non_applicable() {
        let root = TestRepo::new();
        let repo = EstateRepository {
            name: "example/repo".to_string(),
            path: root.path.clone(),
            concerns: BTreeMap::new(),
        };
        let findings = audit_concern_contract(&repo, &BTreeMap::new(), &root.path).unwrap();
        assert!(has_rule(&findings, "missing-required"));
    }

    #[test]
    fn required_aggregator_cannot_be_omitted() {
        let root = TestRepo::new();
        let repo = EstateRepository {
            name: "example/repo".to_string(),
            path: root.path.clone(),
            concerns: BTreeMap::new(),
        };
        let mut defaults = required_concern_defaults();
        defaults.remove("required-aggregator");
        let findings = audit_concern_contract(&repo, &defaults, &root.path).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.rule == "missing-required"
                && finding.path.ends_with("concerns.required-aggregator")
        }));
    }

    #[test]
    fn repository_parameter_does_not_create_canonical_drift() {
        let root = TestRepo::new();
        fs::write(root.path.join(".github/workflows/ci.yml"), BASE).unwrap();
        let repo = EstateRepository {
            name: "example/repo".to_string(),
            path: root.path.clone(),
            concerns: BTreeMap::from([(
                "rust-ci".to_string(),
                ConcernContract {
                    classification: ConcernClassification::Required,
                    evidence: "Rust repository".to_string(),
                    implementations: vec![ConcernImplementation {
                        workflow: "ci.yml".to_string(),
                        job_ids: vec!["rust".to_string()],
                        canonical_markers: vec!["cargo nextest".to_string()],
                    }],
                },
            )]),
        };
        let defaults = required_concern_defaults();
        let findings = audit_concern_contract(&repo, &defaults, &root.path).unwrap();
        assert!(!has_rule(&findings, "canonical-drift"));
    }

    #[test]
    fn canonical_markers_must_keep_declared_order() {
        let root = TestRepo::new();
        fs::write(root.path.join(".github/workflows/ci.yml"), BASE).unwrap();
        let mut defaults = required_concern_defaults();
        defaults.insert(
            "rust-ci".to_string(),
            ConcernContract {
                classification: ConcernClassification::Required,
                evidence: "Rust repository".to_string(),
                implementations: vec![ConcernImplementation {
                    workflow: "ci.yml".to_string(),
                    job_ids: vec!["rust".to_string()],
                    canonical_markers: vec![
                        "cargo nextest".to_string(),
                        "actions/checkout@".to_string(),
                    ],
                }],
            },
        );
        let repo = EstateRepository {
            name: "example/repo".to_string(),
            path: root.path.clone(),
            concerns: BTreeMap::new(),
        };
        let findings = audit_concern_contract(&repo, &defaults, &root.path).unwrap();
        assert!(findings.iter().any(|finding| {
            finding.rule == "canonical-drift" && finding.message.contains("out of order")
        }));
    }

    #[test]
    fn estate_sweep_audits_two_repository_roots() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("velnor-audit-estate-{nonce}"));
        for name in ["one", "two"] {
            let root = base.join(name);
            fs::create_dir_all(root.join(".github/workflows")).unwrap();
            fs::write(root.join(".github/AGENTS.md"), "estate standard\n").unwrap();
            fs::write(root.join(".github/workflows/ci.yml"), BASE).unwrap();
            assert!(audit_repo(&root, true).unwrap().is_empty());
        }
        fs::remove_dir_all(base).unwrap();
    }

    struct TestRepo {
        path: PathBuf,
    }

    impl TestRepo {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("velnor-concern-test-{nonce}"));
            fs::create_dir_all(path.join(".github/workflows")).unwrap();
            Self { path }
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn required_concern_defaults() -> BTreeMap<String, ConcernContract> {
        REQUIRED_CONCERNS_FOR_TESTS
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ConcernContract {
                        classification: ConcernClassification::NonApplicable,
                        evidence: "test classification".to_string(),
                        implementations: Vec::new(),
                    },
                )
            })
            .collect()
    }

    const REQUIRED_CONCERNS_FOR_TESTS: [&str; 14] = [
        "lane-selection",
        "checkout",
        "tool-setup",
        "rust-ci",
        "integration-services",
        "cargo-cache",
        "docker-build",
        "artifacts",
        "docs-pages",
        "preview",
        "release",
        "renovate",
        "required-aggregator",
        "workflow-safety",
    ];
}

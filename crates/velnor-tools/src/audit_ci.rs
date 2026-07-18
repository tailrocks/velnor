//! Estate CI contract auditor (`VELNOR_PROJECTS_SETUP.md` §2.0–§2.12).

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::Serialize;
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
}

pub fn audit_ci(args: AuditCiArgs) -> Result<()> {
    let roots = if let Some(estate) = &args.estate {
        let text = fs::read_to_string(estate)
            .with_context(|| format!("read estate file {}", estate.display()))?;
        serde_json::from_str::<Vec<PathBuf>>(&text)
            .with_context(|| format!("parse estate JSON {}", estate.display()))?
    } else {
        vec![args.repo_path.clone()]
    };
    let mut all = BTreeMap::new();
    for root in roots {
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

fn audit_repo(root: &Path, offline: bool) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
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
        audit_workflow(&relative, &text, &yaml, offline, &mut latest, &mut findings);
    }
    findings.sort_by(|left, right| {
        (&left.file, &left.path, left.rule).cmp(&(&right.file, &right.path, right.rule))
    });
    Ok(findings)
}

fn audit_workflow(
    file: &str,
    text: &str,
    yaml: &Value,
    offline: bool,
    latest: &mut BTreeMap<String, Option<String>>,
    findings: &mut Vec<Finding>,
) {
    let workload = has_trigger(yaml, "push") || has_trigger(yaml, "pull_request");
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
    if !canonical_files.contains(&file_name) {
        findings.push(Finding::warn(
            "uniform-workflow-name",
            file,
            "$",
            "rename this executable concern to a canonical workflow filename",
        ));
    }
    if workload && file_name != "ci.yml" {
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
        if !group.contains("github.ref") {
            findings.push(Finding::warn(
                "uniform-concurrency",
                file,
                "$.concurrency.group",
                "include the workflow identity and github.ref",
            ));
        }
    }
    if workload && (!has_lanes_input(yaml) || !text.contains(INLINE_MATRIX_MARKER)) {
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
        if workload && !canonical_jobs.contains(&job_id.as_str()) {
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
        if mapping_get(job, "timeout-minutes").is_none() && mapping_get(job, "uses").is_none() {
            findings.push(Finding::error(
                "timeout",
                file,
                format!("{job_path}.timeout-minutes"),
                "set a measured timeout-minutes budget",
            ));
        }
        if let Some(runs_on) = mapping_get(job, "runs-on") {
            let value = compact(runs_on);
            if ["ubuntu-latest", "ubuntu-24.04", "macos-", "windows-"]
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
    for (index, step) in steps.iter().enumerate() {
        let path = format!("{job_path}.steps[{index}]");
        let run = object_get(step, "run")
            .and_then(Value::as_str)
            .unwrap_or("");
        compile |= run.lines().any(|line| {
            let line = line.trim_start();
            [
                "cargo build",
                "cargo check",
                "cargo clippy",
                "cargo test",
                "cargo nextest",
                "cargo run",
                "rustc ",
            ]
            .iter()
            .any(|command| line.starts_with(command) || line.contains(&format!(" {command}")))
        });
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
        if run.contains("self-hosted")
            || run.contains("velnor-target-mvp")
            || run.contains("ubuntu-26.04")
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
            target_cache |= object_get(step, "with")
                .and_then(|with| object_get(with, "path"))
                .is_some_and(|value| {
                    compact(value)
                        .lines()
                        .any(|line| line.trim() == "target" || line.contains("/target"))
                });
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
    if target_cache && sccache {
        findings.push(Finding::error(
            "double-cache",
            file,
            job_path,
            "remove actions/cache target caching from the sccache job",
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
      - run: cargo test
"#;

    #[test]
    fn canonical_workflow_passes_static_rules() {
        assert!(audit(BASE).is_empty());
    }

    #[test]
    fn rejects_forbidden_runner_os() {
        assert!(has_rule(
            &audit(&BASE.replace("${{ matrix.config.runner }}", "ubuntu-latest")),
            "runner-os"
        ));
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
    fn requires_concurrency_and_timeout() {
        let yaml = BASE
            .replace("concurrency:\n  group: ci-${{ github.ref }}\n", "")
            .replace("    timeout-minutes: 20\n", "");
        let findings = audit(&yaml);
        assert!(has_rule(&findings, "concurrency"));
        assert!(has_rule(&findings, "timeout"));
    }

    #[test]
    fn fetch_depth_without_comment_warns() {
        let yaml = BASE.replace("- uses: actions/checkout@0123456789012345678901234567890123456789", "- uses: actions/checkout@0123456789012345678901234567890123456789\n        with: {fetch-depth: 0}");
        assert!(has_rule(&audit(&yaml), "fetch-depth"));
    }

    #[test]
    fn rejects_double_cache() {
        let yaml = BASE.replace("      - run: cargo test", "      - uses: Swatinem/rust-cache@0123456789012345678901234567890123456789\n      - run: cargo test");
        assert!(has_rule(&audit(&yaml), "double-cache"));
    }

    #[test]
    fn rejects_lane_condition_and_deprecated_command() {
        let yaml = BASE.replace(
            "      - run: cargo test",
            "      - if: matrix.config.lane == 'Velnor'\n        run: echo ::set-output name=x::y",
        );
        let findings = audit(&yaml);
        assert!(has_rule(&findings, "lane-conditional"));
        assert!(has_rule(&findings, "deprecated"));
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
    fn estate_json_parses_path_list() {
        let paths: Vec<PathBuf> = serde_json::from_str(r#"["/one","/two"]"#).unwrap();
        assert_eq!(paths.len(), 2);
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
}

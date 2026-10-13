//! `lane-compare`: fetch the GitHub-hosted and Velnor lanes of one workflow
//! run via the GitHub API and diff their Checks-UI surface per step.
//!
//! Implements a repeatable comparison using the GitHub API. Strict mode
//! first requires a 1:1 GitHub↔Velnor bijection of comparison units (no
//! orphans, duplicates, skipped counterparts, or ambiguous names). The
//! step gate is **equal-or-better, never less informative**: any paired
//! step where the GitHub lane shows information the Velnor lane lacks (a
//! step missing entirely, an executed step that is not expandable, a
//! divergent display name or conclusion, or lane log content without
//! timestamps / groups / ANSI where GitHub has them) is a `WORSE` row.
//!
//! Data sources (V2 jobs have no v1 log archive — `runs/{id}/logs` contains
//! no Velnor per-step files and `jobs/{id}/logs` 404s, as recorded in the
//! evidence record):
//! - step metadata: `actions/runs/{id}/jobs` (numbers, names, conclusions,
//!   started/completed),
//! - per-step expandability: the job page HTML `<check-step …>` elements
//!   (`data-log-url` presence — exactly what the UI renders),
//! - lane log content: `jobs/{id}/logs` for the GitHub lane; the Velnor
//!   lane's `job-log` artifact(s) for the Velnor side.

use anyhow::{bail, Context, Result};
use clap::{ArgAction, Args, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Args)]
pub struct LaneCompareArgs {
    /// GitHub repository slug holding the dual-lane run.
    #[arg(long, default_value = super::DEFAULT_FIXTURE_REPO)]
    pub repo: String,
    /// Run id to compare; latest run of --workflow when omitted.
    #[arg(long)]
    pub run_id: Option<u64>,
    /// Workflow file used to resolve the latest run when --run-id is omitted.
    #[arg(long, default_value = "compat.yml")]
    pub workflow: String,
    /// Compare recent both-lane runs against a timing/parity baseline.
    #[arg(long)]
    pub watch: bool,
    /// Velnor-vs-GitHub slowdown percentage tolerated above the baseline.
    #[arg(long, default_value_t = 25.0)]
    pub regress_threshold: f64,
    /// Number of recent completed both-lane runs to inspect in --watch mode.
    #[arg(long, default_value_t = 5)]
    pub since: usize,
    /// Compare exactly this GitHub-lane job id (skips name-based pairing).
    #[arg(long, requires = "velnor_job")]
    pub github_job: Option<u64>,
    /// Compare exactly this Velnor-lane job id (skips name-based pairing).
    #[arg(long, requires = "github_job")]
    pub velnor_job: Option<u64>,
    /// Directory for the report and raw evidence.
    #[arg(long, default_value = ".velnor-compare")]
    pub output_dir: PathBuf,
    /// Exit nonzero when any paired step is less informative than GitHub.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub strict: bool,
    /// §2.11 workload class used for the Velnor wall-time budget.
    #[arg(long, value_enum)]
    pub class: Option<RunClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RunClass {
    A,
    B,
    C,
    D,
}

impl RunClass {
    // Initial warm/no-change budgets for each workload class.
    fn wall_budget_seconds(self) -> i64 {
        match self {
            Self::A => 150,
            Self::B | Self::C => 90,
            Self::D => 60,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BudgetVerdict {
    seconds: i64,
    budget: i64,
    pass: bool,
}

fn wall_budget_verdict(class: RunClass, seconds: i64) -> BudgetVerdict {
    let budget = class.wall_budget_seconds();
    BudgetVerdict {
        seconds,
        budget,
        pass: seconds <= budget,
    }
}

#[derive(Debug, Deserialize)]
struct JobsResponse {
    total_count: u64,
    jobs: Vec<Job>,
}

#[derive(Debug, Clone, Deserialize)]
struct Job {
    id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    html_url: Option<String>,
    steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize)]
struct Step {
    name: String,
    #[allow(dead_code)]
    status: String,
    conclusion: Option<String>,
    number: u64,
    started_at: Option<String>,
    completed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    GitHub,
    Velnor,
}

/// Per-step UI facts parsed from the job page's `<check-step>` elements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HtmlStep {
    number: u64,
    expandable: bool,
    external_id: String,
}

/// Lane-level log-content affordances (per-step blobs are not API-reachable
/// for V2 jobs, so content is judged per lane, structure per step).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LaneLogStats {
    lines: usize,
    timestamped_lines: usize,
    group_markers: usize,
    ansi: bool,
}

pub fn lane_compare(root: &Path, args: LaneCompareArgs) -> Result<()> {
    super::validate_repo_slug("--repo", &args.repo)?;
    if args.watch {
        return lane_compare_watch(root, args);
    }
    let run_id = match args.run_id {
        Some(id) => id,
        None => super::latest_fixture_run_id(&args.repo, &args.workflow)?,
    };

    let jobs = fetch_run_jobs(&args.repo, run_id)?;
    let census = match (args.github_job, args.velnor_job) {
        (Some(_), Some(_)) => None,
        _ => Some(pair_lane_census(&jobs)),
    };
    let pairs = match (args.github_job, args.velnor_job) {
        (Some(gh), Some(vl)) => {
            let github = jobs
                .iter()
                .find(|job| job.id == gh)
                .with_context(|| format!("job {gh} not found in run {run_id}"))?;
            let velnor = jobs
                .iter()
                .find(|job| job.id == vl)
                .with_context(|| format!("job {vl} not found in run {run_id}"))?;
            vec![(github.clone(), velnor.clone())]
        }
        _ => census
            .as_ref()
            .map(PairingCensus::matched_pairs)
            .unwrap_or_default(),
    };

    let out_dir = if args.output_dir.is_absolute() {
        args.output_dir.clone()
    } else {
        root.join(&args.output_dir)
    };
    let run_dir = out_dir.join(format!("lane-compare-run-{run_id}"));
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("create output directory {}", run_dir.display()))?;
    save_jobs_json(&run_dir, &jobs)?;

    let mut report = String::new();
    let mut worse_total = 0usize;
    let mut budget_failures = 0usize;
    writeln!(report, "# Lane compare — run {run_id} ({})", args.repo)?;
    writeln!(report)?;
    writeln!(
        report,
        "Gate: equal-or-better — zero rows where the GitHub lane shows \
         information the Velnor lane lacks. Strict mode also requires a 1:1 \
         GitHub↔Velnor bijection of comparison units."
    )?;
    writeln!(report)?;
    if let Some(census) = &census {
        report.push_str(&format_pairing_report(census)?);
    } else {
        writeln!(
            report,
            "Pairing: explicit `--github-job` / `--velnor-job` override; \
             full-run bijection skipped."
        )?;
    }

    let pairing_failures = census
        .as_ref()
        .is_some_and(PairingCensus::has_parity_failures);
    if args.strict && pairing_failures {
        writeln!(report, "\n## Result")?;
        writeln!(report)?;
        writeln!(
            report,
            "**FAIL** — pairing bijection failed (orphans, duplicates, \
             skipped counterpart, and/or ambiguous names)."
        )?;
        let report_path = run_dir.join("report.md");
        fs::write(&report_path, &report)
            .with_context(|| format!("write {}", report_path.display()))?;
        println!("{report}");
        println!("report: {}", report_path.display());
        bail!("lane-compare gate failed: pairing bijection; see report above");
    }

    for (github, velnor) in &pairs {
        for job in [github, velnor] {
            if job_is_skipped(job) {
                continue;
            }
            if job.status != "completed" {
                bail!(
                    "job {} ({}) is {}, not completed — compare a finished run",
                    job.name,
                    job.id,
                    job.status
                );
            }
        }
    }

    // Lane content: GitHub jobs expose a per-job log download; Velnor V2 jobs
    // do not (no v1 archive), so read the Velnor lane's job-log artifact(s).
    let velnor_content =
        fetch_velnor_job_log_artifacts(&args.repo, run_id).unwrap_or_else(|error| {
            eprintln!("warning: velnor job-log artifacts unavailable: {error:#}");
            String::new()
        });
    if let Some(class) = args.class {
        writeln!(report)?;
        writeln!(report, "## §2.11 Velnor budget ({class:?})")?;
        writeln!(report)?;
        writeln!(report, "| Job | Wall | Budget | Verdict |")?;
        writeln!(report, "|---|---:|---:|---|")?;
        for (_, velnor) in &pairs {
            let seconds = job_duration_seconds(velnor).unwrap_or(i64::MAX);
            let verdict = wall_budget_verdict(class, seconds);
            budget_failures += usize::from(!verdict.pass);
            writeln!(
                report,
                "| {} | {}s | {}s | {} |",
                velnor.name,
                verdict.seconds,
                verdict.budget,
                if verdict.pass { "PASS" } else { "FAIL" }
            )?;
        }
        writeln!(report)?;
        writeln!(
            report,
            "Pickup SLO is reported from Velnor's versioned `job-timing` forensic records; the GitHub jobs API does not expose an authoritative broker-message timestamp."
        )?;
    }

    for (github, velnor) in &pairs {
        let github_html = fetch_job_html_steps(github).unwrap_or_else(|error| {
            eprintln!("warning: {}: {error:#}", github.id);
            BTreeMap::new()
        });
        let velnor_html = fetch_job_html_steps(velnor).unwrap_or_else(|error| {
            eprintln!("warning: {}: {error:#}", velnor.id);
            BTreeMap::new()
        });
        let github_content = fetch_github_job_log(&args.repo, github.id)
            .map(|text| {
                let stats = analyze_lane_log(&text);
                let _ = fs::write(run_dir.join(format!("github-job-{}.log", github.id)), text);
                stats
            })
            .unwrap_or_else(|error| {
                eprintln!("warning: github job log {}: {error:#}", github.id);
                LaneLogStats::default()
            });
        let velnor_stats = analyze_lane_log(&velnor_content);
        let (section, worse) = compare_pair(
            github,
            velnor,
            &github_html,
            &velnor_html,
            github_content,
            velnor_stats,
        )?;
        worse_total += worse;
        report.push_str(&section);
    }
    if !velnor_content.is_empty() {
        fs::write(run_dir.join("velnor-job-log.log"), &velnor_content)
            .context("save velnor job-log artifact content")?;
    }

    writeln!(report, "\n## Result")?;
    writeln!(report)?;
    if worse_total == 0 && budget_failures == 0 {
        writeln!(
            report,
            "**PASS** — no paired step is less informative than the GitHub lane."
        )?;
        if pairing_failures {
            writeln!(
                report,
                "Pairing orphans/duplicates/skipped counterparts/ambiguous names \
                 are reported above; `--strict false` does not fail on them."
            )?;
        }
    } else {
        writeln!(
            report,
            "**FAIL** — {worse_total} parity row(s) and {budget_failures} §2.11 budget failure(s)."
        )?;
    }
    writeln!(report)?;
    writeln!(
        report,
        "Known documented divergence (not gated): V2 jobs have no v1 log \
         archive, so the per-job raw-log download 404s on the Velnor lane; \
         the `job-log` artifact is the workaround."
    )?;

    let report_path = run_dir.join("report.md");
    fs::write(&report_path, &report).with_context(|| format!("write {}", report_path.display()))?;
    println!("{report}");
    println!("report: {}", report_path.display());

    if args.strict && (worse_total > 0 || budget_failures > 0 || pairing_failures) {
        bail!("lane-compare gate failed: {worse_total} worse row(s), {budget_failures} budget failure(s); see report above");
    }
    Ok(())
}

fn lane_compare_watch(root: &Path, args: LaneCompareArgs) -> Result<()> {
    if args.github_job.is_some() || args.velnor_job.is_some() {
        bail!("--watch compares paired jobs discovered from runs; omit --github-job/--velnor-job");
    }
    if args.run_id.is_some() {
        bail!("--watch selects recent runs from --workflow; omit --run-id");
    }
    if args.since < 2 {
        bail!("--watch requires --since >= 2 so one current run can be checked against a baseline");
    }
    if args.regress_threshold < 0.0 {
        bail!("--regress-threshold must be non-negative");
    }

    let run_ids = recent_completed_run_ids(&args.repo, &args.workflow, args.since)?;
    if run_ids.len() < 2 {
        bail!(
            "need at least two completed both-lane runs for --watch; found {}",
            run_ids.len()
        );
    }

    let out_dir = if args.output_dir.is_absolute() {
        args.output_dir.clone()
    } else {
        root.join(&args.output_dir)
    };
    let watch_dir = out_dir.join("lane-compare-watch");
    fs::create_dir_all(&watch_dir)
        .with_context(|| format!("create output directory {}", watch_dir.display()))?;

    let mut samples = Vec::new();
    for run_id in run_ids {
        samples.push(lane_stats_for_run(&args.repo, run_id)?);
    }
    let current = samples
        .first()
        .cloned()
        .context("recent run sample unexpectedly empty")?;
    let baseline = baseline_from_samples(&samples[1..])
        .context("recent run sample did not contain a usable baseline")?;
    let verdict = is_regression(&baseline, &current, args.regress_threshold);

    let report = regression_report(&args.repo, &args.workflow, &baseline, &current, &verdict)?;
    let report_path = watch_dir.join("report.md");
    fs::write(&report_path, &report).with_context(|| format!("write {}", report_path.display()))?;
    let json_path = watch_dir.join("latest-stats.json");
    fs::write(&json_path, serde_json::to_vec_pretty(&current)?)
        .with_context(|| format!("write {}", json_path.display()))?;

    println!("{report}");
    println!("report: {}", report_path.display());
    println!("stats: {}", json_path.display());

    if verdict.regression {
        bail!(
            "lane-compare regression gate failed: {}",
            verdict.reasons.join("; ")
        );
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RunListItem {
    #[serde(rename = "databaseId")]
    database_id: u64,
    status: String,
}

fn recent_completed_run_ids(repo: &str, workflow: &str, limit: usize) -> Result<Vec<u64>> {
    let output = Command::new("gh")
        .args([
            "run",
            "list",
            "--repo",
            repo,
            "--workflow",
            workflow,
            "--limit",
            &limit.max(2).to_string(),
            "--json",
            "databaseId,status",
        ])
        .output()
        .with_context(|| format!("spawn gh run list for {repo} {workflow}"))?;
    if !output.status.success() {
        bail!(
            "gh run list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let runs: Vec<RunListItem> =
        serde_json::from_slice(&output.stdout).context("parse gh run list output")?;
    Ok(runs
        .into_iter()
        .filter(|run| run.status == "completed")
        .map(|run| run.database_id)
        .collect())
}

fn save_jobs_json(run_dir: &Path, jobs: &[Job]) -> Result<()> {
    fs::write(
        run_dir.join("jobs.json"),
        serde_json::to_vec_pretty(
            &jobs
                .iter()
                .map(|job| {
                    serde_json::json!({
                        "id": job.id,
                        "name": job.name,
                        "status": job.status,
                        "conclusion": job.conclusion,
                        "steps": job.steps.iter().map(|step| serde_json::json!({
                            "number": step.number,
                            "name": step.name,
                            "conclusion": step.conclusion,
                            "started_at": step.started_at,
                            "completed_at": step.completed_at,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect::<Vec<_>>(),
        )?,
    )
    .context("save jobs.json")
}

fn fetch_run_jobs(repo: &str, run_id: u64) -> Result<Vec<Job>> {
    let mut jobs = Vec::new();
    let mut page = 1u32;
    loop {
        let payload = gh_api_bytes(&format!(
            "repos/{repo}/actions/runs/{run_id}/jobs?per_page=100&page={page}"
        ))?;
        let response: JobsResponse =
            serde_json::from_slice(&payload).context("parse run jobs response")?;
        let fetched = response.jobs.len();
        jobs.extend(response.jobs);
        if fetched == 0 || jobs.len() as u64 >= response.total_count {
            break;
        }
        page += 1;
    }
    if jobs.is_empty() {
        bail!("run {run_id} has no jobs in {repo}");
    }
    Ok(jobs)
}

/// `gh api` subprocess: bypasses the reqwest TLS-fingerprint throttling GitHub
/// applies under runner load (see fetch_github_file) and reuses gh credentials.
fn gh_api_bytes(path: &str) -> Result<Vec<u8>> {
    let output = Command::new("gh")
        .args(["api", path])
        .output()
        .with_context(|| format!("spawn gh api {path}"))?;
    if !output.status.success() {
        bail!(
            "gh api {path} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn fetch_github_job_log(repo: &str, job_id: u64) -> Result<String> {
    let bytes = gh_api_bytes(&format!("repos/{repo}/actions/jobs/{job_id}/logs"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Job page HTML via curl (public page; `gh api` cannot fetch web routes —
/// curl also sidesteps the reqwest TLS-fingerprint throttle).
fn fetch_job_html_steps(job: &Job) -> Result<BTreeMap<u64, HtmlStep>> {
    let url = job
        .html_url
        .as_deref()
        .with_context(|| format!("job {} has no html_url", job.id))?;
    let output = Command::new("curl")
        .args(["-fsSL", url])
        .output()
        .with_context(|| format!("spawn curl {url}"))?;
    if !output.status.success() {
        bail!(
            "curl {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let html = String::from_utf8_lossy(&output.stdout);
    Ok(parse_check_steps(&html))
}

/// Extract `<check-step …>` elements: `data-number` plus whether
/// `data-log-url` is non-empty (that attribute is exactly what makes a step
/// expandable in the UI).
fn parse_check_steps(html: &str) -> BTreeMap<u64, HtmlStep> {
    let mut steps = BTreeMap::new();
    let mut rest = html;
    while let Some(start) = rest.find("<check-step") {
        let element = &rest[start..];
        let Some(end) = element.find('>') else {
            break;
        };
        let element = &element[..end];
        if let Some(number) =
            attr_value(element, "data-number").and_then(|value| value.parse::<u64>().ok())
        {
            steps.insert(
                number,
                HtmlStep {
                    number,
                    expandable: attr_value(element, "data-log-url")
                        .is_some_and(|value| !value.is_empty()),
                    external_id: attr_value(element, "data-external-id")
                        .unwrap_or_default()
                        .to_string(),
                },
            );
        }
        rest = &rest[start + end..];
    }
    steps
}

fn attr_value<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=\"");
    let start = element.find(&marker)? + marker.len();
    let rest = &element[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Download every per-job `job-log-*` artifact of the run and concatenate
/// their text content. Legacy runs may still contain the unsuffixed `job-log`
/// name, so the prefix also preserves backwards compatibility.
fn fetch_velnor_job_log_artifacts(repo: &str, run_id: u64) -> Result<String> {
    let payload = gh_api_bytes(&format!(
        "repos/{repo}/actions/runs/{run_id}/artifacts?per_page=100"
    ))?;
    let response: serde_json::Value =
        serde_json::from_slice(&payload).context("parse artifacts response")?;
    let artifacts = response
        .get("artifacts")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut content = String::new();
    for artifact in artifacts {
        let name = artifact.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if !name.starts_with("job-log") {
            continue;
        }
        let Some(id) = artifact.get("id").and_then(|v| v.as_u64()) else {
            continue;
        };
        let zip_bytes = gh_api_bytes(&format!("repos/{repo}/actions/artifacts/{id}/zip"))?;
        let cursor = std::io::Cursor::new(zip_bytes);
        let mut zip = zip::ZipArchive::new(cursor).context("open job-log artifact zip")?;
        for index in 0..zip.len() {
            let mut file = zip.by_index(index).context("read artifact entry")?;
            if file.is_dir() {
                continue;
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).context("read artifact file")?;
            content.push_str(&String::from_utf8_lossy(&bytes));
            content.push('\n');
        }
    }
    if content.is_empty() {
        bail!("no job-log artifacts found in run {run_id}");
    }
    Ok(content)
}

/// Test-only view of [`classify_job_name`]: the lane of a comparison job.
#[cfg(test)]
fn lane_of_job_name(name: &str) -> Option<Lane> {
    match classify_job_name(name) {
        JobRole::Comparison { lane, .. } => Some(lane),
        JobRole::Control | JobRole::Ambiguous => None,
    }
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut pos = 0;
    while let Some(found) = haystack[pos..].find(needle) {
        let abs = pos + found;
        let before_ok = haystack[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after_ok = haystack[abs + needle.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        pos = abs + 1;
    }
    false
}

/// Fixture-matrix pair key: the job name with the lane token and everything
/// after it removed, so `compat (app-a, github, "ubuntu-latest")` and
/// `compat (app-a, velnor, [...])` pair on `compat (app-a`. Used only when
/// the name does not match the generated lane-first pattern.
fn pair_key(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let cut = ["velnor", "github"]
        .iter()
        .filter_map(|token| lower.find(token))
        .min()
        .unwrap_or(lower.len());
    lower[..cut].trim_end_matches([' ', ',', '(']).to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobRole<'a> {
    Control,
    Ambiguous,
    Comparison { lane: Lane, key: &'a str },
}

/// Generated identity: `<Kind> · <unit> / GitHub|Velnor` (D1).
/// The lane is the trailing segment; `velnor` inside a unit label is not a lane token.
fn parse_unit_first(name: &str) -> Option<(Lane, &str)> {
    let (prefix, lane_token) = name.rsplit_once(" / ")?;
    let lane = match lane_token {
        "GitHub" => Lane::GitHub,
        "Velnor" => Lane::Velnor,
        _ => return None,
    };
    if prefix.is_empty() || prefix.starts_with("Control /") {
        return None;
    }
    Some((lane, prefix))
}

fn classify_job_name(name: &str) -> JobRole<'_> {
    if name.starts_with("Control /") {
        return JobRole::Control;
    }
    if let Some((lane, key)) = parse_unit_first(name) {
        return JobRole::Comparison { lane, key };
    }
    let lower = name.to_ascii_lowercase();
    let has_velnor = contains_word(&lower, "velnor");
    let has_github = contains_word(&lower, "github");
    match (has_velnor, has_github) {
        (true, false) => JobRole::Comparison {
            lane: Lane::Velnor,
            key: "",
        },
        (false, true) => JobRole::Comparison {
            lane: Lane::GitHub,
            key: "",
        },
        (true, true) => JobRole::Ambiguous,
        (false, false) => JobRole::Control,
    }
}

/// Fixture names need a computed pair_key; generated names already carry theirs.
fn comparison_key<'a>(name: &'a str, classified_key: &'a str) -> String {
    if classified_key.is_empty() {
        pair_key(name)
    } else {
        classified_key.to_string()
    }
}

fn job_is_skipped(job: &Job) -> bool {
    job.status.eq_ignore_ascii_case("skipped")
        || job
            .conclusion
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("skipped"))
}

fn is_skipped_counterpart(github: &Job, velnor: &Job) -> bool {
    job_is_skipped(github) != job_is_skipped(velnor)
}

#[derive(Debug, Clone, Default)]
struct PairingCensus {
    matched: Vec<(Job, Job, String)>,
    github_only: Vec<(String, Job)>,
    velnor_only: Vec<(String, Job)>,
    duplicate_github: Vec<(String, Vec<Job>)>,
    duplicate_velnor: Vec<(String, Vec<Job>)>,
    skipped_counterpart: Vec<(String, Job, Job)>,
    ambiguous: Vec<Job>,
    control: Vec<Job>,
}

impl PairingCensus {
    fn has_parity_failures(&self) -> bool {
        !self.github_only.is_empty()
            || !self.velnor_only.is_empty()
            || !self.duplicate_github.is_empty()
            || !self.duplicate_velnor.is_empty()
            || !self.skipped_counterpart.is_empty()
            || !self.ambiguous.is_empty()
    }

    fn matched_pairs(&self) -> Vec<(Job, Job)> {
        self.matched
            .iter()
            .map(|(github, velnor, _)| (github.clone(), velnor.clone()))
            .collect()
    }
}

fn pair_lane_census(jobs: &[Job]) -> PairingCensus {
    let mut github: BTreeMap<String, Vec<Job>> = BTreeMap::new();
    let mut velnor: BTreeMap<String, Vec<Job>> = BTreeMap::new();
    let mut census = PairingCensus::default();
    for job in jobs {
        match classify_job_name(&job.name) {
            JobRole::Control => census.control.push(job.clone()),
            JobRole::Ambiguous => census.ambiguous.push(job.clone()),
            JobRole::Comparison { lane, key } => {
                let key = comparison_key(&job.name, key);
                match lane {
                    Lane::GitHub => github.entry(key).or_default().push(job.clone()),
                    Lane::Velnor => velnor.entry(key).or_default().push(job.clone()),
                }
            }
        }
    }

    let mut keys = BTreeSet::new();
    keys.extend(github.keys().cloned());
    keys.extend(velnor.keys().cloned());
    for key in keys {
        let gh = github.get(&key).cloned().unwrap_or_default();
        let vl = velnor.get(&key).cloned().unwrap_or_default();
        if gh.len() > 1 {
            census.duplicate_github.push((key.clone(), gh.clone()));
        }
        if vl.len() > 1 {
            census.duplicate_velnor.push((key.clone(), vl.clone()));
        }
        match (gh.len(), vl.len()) {
            (1, 1) => {
                let github_job = gh[0].clone();
                let velnor_job = vl[0].clone();
                if is_skipped_counterpart(&github_job, &velnor_job) {
                    census.skipped_counterpart.push((
                        key.clone(),
                        github_job.clone(),
                        velnor_job.clone(),
                    ));
                }
                census.matched.push((github_job, velnor_job, key));
            }
            (n, 0) if n > 0 => {
                for job in gh {
                    census.github_only.push((key.clone(), job));
                }
            }
            (0, n) if n > 0 => {
                for job in vl {
                    census.velnor_only.push((key.clone(), job));
                }
            }
            _ => {}
        }
    }
    census
}

/// Test-only view of [`pair_lane_census`]: just the matched pairs.
#[cfg(test)]
fn pair_lane_jobs(jobs: &[Job]) -> Vec<(Job, Job)> {
    pair_lane_census(jobs).matched_pairs()
}

fn format_job_ref(job: &Job) -> String {
    format!("`{}` ({})", job.name, job.id)
}

fn format_pairing_report(census: &PairingCensus) -> Result<String> {
    let mut out = String::new();
    writeln!(out, "## Pairing")?;
    writeln!(out)?;
    writeln!(
        out,
        "Set-equality of comparison units. Control/helper jobs are ignored \
         and never fail parity."
    )?;
    writeln!(out)?;

    writeln!(out, "### Matched pairs")?;
    writeln!(out)?;
    if census.matched.is_empty() {
        writeln!(out, "- none")?;
    } else {
        for (github, velnor, key) in &census.matched {
            writeln!(
                out,
                "- `{key}`: {} ⇄ {}",
                format_job_ref(github),
                format_job_ref(velnor)
            )?;
        }
    }
    writeln!(out)?;

    writeln!(out, "### GitHub-only jobs")?;
    writeln!(out)?;
    write_keyed_jobs(&mut out, &census.github_only)?;
    writeln!(out)?;

    writeln!(out, "### Velnor-only jobs")?;
    writeln!(out)?;
    write_keyed_jobs(&mut out, &census.velnor_only)?;
    writeln!(out)?;

    writeln!(out, "### Duplicate GitHub executions")?;
    writeln!(out)?;
    write_duplicate_jobs(&mut out, &census.duplicate_github)?;
    writeln!(out)?;

    writeln!(out, "### Duplicate Velnor executions")?;
    writeln!(out)?;
    write_duplicate_jobs(&mut out, &census.duplicate_velnor)?;
    writeln!(out)?;

    writeln!(out, "### Skipped counterpart")?;
    writeln!(out)?;
    if census.skipped_counterpart.is_empty() {
        writeln!(out, "- none")?;
    } else {
        for (key, github, velnor) in &census.skipped_counterpart {
            writeln!(
                out,
                "- `{key}`: {} ⇄ {} (one side skipped, the other executed)",
                format_job_ref(github),
                format_job_ref(velnor)
            )?;
        }
    }
    writeln!(out)?;

    writeln!(out, "### Missing counterpart")?;
    writeln!(out)?;
    if census.github_only.is_empty() && census.velnor_only.is_empty() {
        writeln!(out, "- none")?;
    } else {
        for (key, job) in &census.github_only {
            writeln!(
                out,
                "- `{key}`: {} has no Velnor counterpart",
                format_job_ref(job)
            )?;
        }
        for (key, job) in &census.velnor_only {
            writeln!(
                out,
                "- `{key}`: {} has no GitHub counterpart",
                format_job_ref(job)
            )?;
        }
    }
    writeln!(out)?;

    writeln!(out, "### Ambiguous names")?;
    writeln!(out)?;
    if census.ambiguous.is_empty() {
        writeln!(out, "- none")?;
    } else {
        for job in &census.ambiguous {
            writeln!(out, "- {}", format_job_ref(job))?;
        }
    }
    writeln!(out)?;

    writeln!(out, "### Control jobs ignored")?;
    writeln!(out)?;
    if census.control.is_empty() {
        writeln!(out, "- none")?;
    } else {
        for job in &census.control {
            writeln!(
                out,
                "- {} (informational, not a failure)",
                format_job_ref(job)
            )?;
        }
    }
    writeln!(out)?;
    Ok(out)
}

fn write_keyed_jobs(out: &mut String, jobs: &[(String, Job)]) -> Result<()> {
    if jobs.is_empty() {
        writeln!(out, "- none")?;
    } else {
        for (key, job) in jobs {
            writeln!(out, "- `{key}`: {}", format_job_ref(job))?;
        }
    }
    Ok(())
}

fn write_duplicate_jobs(out: &mut String, groups: &[(String, Vec<Job>)]) -> Result<()> {
    if groups.is_empty() {
        writeln!(out, "- none")?;
    } else {
        for (key, jobs) in groups {
            let refs = jobs
                .iter()
                .map(format_job_ref)
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(out, "- `{key}` ({count}): {refs}", count = jobs.len())?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct LaneStats {
    pub run_id: u64,
    pub baseline_runs: usize,
    pub parity_worse_rows: usize,
    pub jobs: BTreeMap<String, JobClassStats>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct JobClassStats {
    pub github_seconds: f64,
    pub velnor_seconds: f64,
}

impl JobClassStats {
    fn velnor_ratio(self) -> Option<f64> {
        if self.github_seconds > 0.0 && self.velnor_seconds >= 0.0 {
            Some(self.velnor_seconds / self.github_seconds)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegressionVerdict {
    pub regression: bool,
    pub reasons: Vec<String>,
}

pub fn is_regression(
    baseline: &LaneStats,
    current: &LaneStats,
    threshold_pct: f64,
) -> RegressionVerdict {
    let mut reasons = Vec::new();
    if current.parity_worse_rows > 0 {
        reasons.push(format!(
            "parity diff: {} worse row(s)",
            current.parity_worse_rows
        ));
    }

    let multiplier = 1.0 + threshold_pct.max(0.0) / 100.0;
    for (job_class, current_stats) in &current.jobs {
        let Some(current_ratio) = current_stats.velnor_ratio() else {
            continue;
        };
        let Some(baseline_ratio) = baseline
            .jobs
            .get(job_class)
            .copied()
            .and_then(JobClassStats::velnor_ratio)
        else {
            continue;
        };
        if current_ratio > baseline_ratio * multiplier {
            reasons.push(format!(
                "{job_class}: velnor/github ratio {:.2} exceeds baseline {:.2} by >{threshold_pct:.1}%",
                current_ratio, baseline_ratio
            ));
        }
    }

    RegressionVerdict {
        regression: !reasons.is_empty(),
        reasons,
    }
}

fn lane_stats_for_run(repo: &str, run_id: u64) -> Result<LaneStats> {
    let jobs = fetch_run_jobs(repo, run_id)?;
    let census = pair_lane_census(&jobs);
    if census.matched.is_empty() {
        bail!("run {run_id} has no both-lane job pairs");
    }

    let mut stats = LaneStats {
        run_id,
        baseline_runs: 1,
        parity_worse_rows: 0,
        jobs: BTreeMap::new(),
    };
    for (github, velnor, key) in &census.matched {
        let github_html = fetch_job_html_steps(github).unwrap_or_default();
        let velnor_html = fetch_job_html_steps(velnor).unwrap_or_default();
        let (_section, worse) = compare_pair(
            github,
            velnor,
            &github_html,
            &velnor_html,
            LaneLogStats::default(),
            LaneLogStats::default(),
        )?;
        stats.parity_worse_rows += worse;
        let (Some(github_seconds), Some(velnor_seconds)) =
            (job_duration_seconds(github), job_duration_seconds(velnor))
        else {
            continue;
        };
        stats.jobs.insert(
            key.clone(),
            JobClassStats {
                github_seconds: github_seconds as f64,
                velnor_seconds: velnor_seconds as f64,
            },
        );
    }
    Ok(stats)
}

fn baseline_from_samples(samples: &[LaneStats]) -> Option<LaneStats> {
    if samples.is_empty() {
        return None;
    }
    let mut sums: BTreeMap<String, (f64, f64, usize)> = BTreeMap::new();
    for sample in samples {
        for (job_class, stats) in &sample.jobs {
            let entry = sums.entry(job_class.clone()).or_default();
            entry.0 += stats.github_seconds;
            entry.1 += stats.velnor_seconds;
            entry.2 += 1;
        }
    }
    if sums.is_empty() {
        // No baseline sample produced a complete timing pair. A baseline with
        // no job timings would silently skip every regression comparison and
        // false-green the gate, so refuse the baseline instead.
        return None;
    }
    let jobs = sums
        .into_iter()
        .filter_map(|(job_class, (github, velnor, count))| {
            (count > 0).then_some((
                job_class,
                JobClassStats {
                    github_seconds: github / count as f64,
                    velnor_seconds: velnor / count as f64,
                },
            ))
        })
        .collect();
    Some(LaneStats {
        run_id: 0,
        baseline_runs: samples.len(),
        parity_worse_rows: samples
            .iter()
            .map(|sample| sample.parity_worse_rows)
            .sum::<usize>()
            / samples.len(),
        jobs,
    })
}

fn regression_report(
    repo: &str,
    workflow: &str,
    baseline: &LaneStats,
    current: &LaneStats,
    verdict: &RegressionVerdict,
) -> Result<String> {
    let mut report = String::new();
    writeln!(report, "# Lane Compare Watch")?;
    writeln!(report)?;
    writeln!(report, "Repository: `{repo}`")?;
    writeln!(report, "Workflow: `{workflow}`")?;
    writeln!(report, "Current run: `{}`", current.run_id)?;
    writeln!(report, "Baseline sample size: `{}`", baseline.baseline_runs)?;
    writeln!(report)?;
    writeln!(
        report,
        "| job class | baseline gh | baseline velnor | current gh | current velnor | ratio delta |"
    )?;
    writeln!(
        report,
        "|-----------|-------------|-----------------|------------|----------------|-------------|"
    )?;
    for (job_class, current_stats) in &current.jobs {
        let baseline_stats = baseline.jobs.get(job_class).copied();
        let (baseline_github, baseline_velnor, baseline_ratio) = match baseline_stats {
            Some(stats) => (
                format!("{:.1}s", stats.github_seconds),
                format!("{:.1}s", stats.velnor_seconds),
                stats
                    .velnor_ratio()
                    .map_or_else(|| "—".to_string(), |ratio| format!("{ratio:.2}")),
            ),
            None => ("—".to_string(), "—".to_string(), "—".to_string()),
        };
        let current_ratio = current_stats
            .velnor_ratio()
            .map_or_else(|| "—".to_string(), |ratio| format!("{ratio:.2}"));
        writeln!(
            report,
            "| {job_class} | {baseline_github} | {baseline_velnor} | {:.1}s | {:.1}s | {baseline_ratio} -> {current_ratio} |",
            current_stats.github_seconds,
            current_stats.velnor_seconds,
        )?;
    }
    writeln!(report)?;
    writeln!(
        report,
        "Parity worse rows in current run: `{}`",
        current.parity_worse_rows
    )?;
    writeln!(report)?;
    if verdict.regression {
        writeln!(report, "## Result")?;
        writeln!(report)?;
        writeln!(report, "**FAIL**")?;
        for reason in &verdict.reasons {
            writeln!(report, "- {reason}")?;
        }
    } else {
        writeln!(report, "## Result")?;
        writeln!(report)?;
        writeln!(
            report,
            "**PASS** — no parity or timing regression detected."
        )?;
    }
    Ok(report)
}

fn job_duration_seconds(job: &Job) -> Option<i64> {
    let mut started = None;
    let mut completed = None;
    for step in &job.steps {
        let (Some(step_start), Some(step_end)) = (
            step.started_at.as_deref().and_then(parse_rfc3339),
            step.completed_at.as_deref().and_then(parse_rfc3339),
        ) else {
            continue;
        };
        started = Some(started.map_or(step_start, |current: time::OffsetDateTime| {
            current.min(step_start)
        }));
        completed = Some(completed.map_or(step_end, |current: time::OffsetDateTime| {
            current.max(step_end)
        }));
    }
    started
        .zip(completed)
        .map(|(started, completed)| (completed - started).whole_seconds())
}

enum AlignedRow<'a> {
    Pair(&'a Step, &'a Step),
    GitHubOnly(&'a Step),
    VelnorOnly(&'a Step),
}

/// Longest-common-subsequence alignment of the two lanes' step lists on
/// lane-normalized display names.
fn align_steps<'a>(github: &'a [Step], velnor: &'a [Step]) -> Vec<AlignedRow<'a>> {
    let gh_keys: Vec<String> = github
        .iter()
        .map(|step| normalized_step_name(&step.name))
        .collect();
    let vl_keys: Vec<String> = velnor
        .iter()
        .map(|step| normalized_step_name(&step.name))
        .collect();
    let (n, m) = (github.len(), velnor.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if gh_keys[i] == vl_keys[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut rows = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if gh_keys[i] == vl_keys[j] {
            rows.push(AlignedRow::Pair(&github[i], &velnor[j]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            rows.push(AlignedRow::GitHubOnly(&github[i]));
            i += 1;
        } else {
            rows.push(AlignedRow::VelnorOnly(&velnor[j]));
            j += 1;
        }
    }
    rows.extend(github[i..].iter().map(AlignedRow::GitHubOnly));
    rows.extend(velnor[j..].iter().map(AlignedRow::VelnorOnly));
    rows
}

/// Steps the GitHub runner generates around container actions; Velnor's
/// native adapters execute the same action without a build/pull phase.
fn runner_generated_step(name: &str) -> bool {
    name.starts_with("Build ") || name.starts_with("Pull ")
}

/// Lane-token-insensitive step-name comparison: step names legitimately embed
/// the matrix lane value (`write-result.py "app-a" "github" …`), so replace
/// lane tokens before comparing.
fn normalized_step_name(name: &str) -> String {
    let mut normalized = name.to_ascii_lowercase();
    for token in ["velnor", "github"] {
        normalized = normalized.replace(token, "{lane}");
    }
    normalized
}

fn analyze_lane_log(text: &str) -> LaneLogStats {
    let mut stats = LaneLogStats {
        ansi: text.contains('\u{1b}'),
        ..LaneLogStats::default()
    };
    for line in text.lines() {
        stats.lines += 1;
        let content = match strip_blob_timestamp(line) {
            Some(rest) => {
                stats.timestamped_lines += 1;
                rest
            }
            None => line,
        };
        if content.starts_with("##[group]") {
            stats.group_markers += 1;
        }
    }
    stats
}

/// Strip the `.NET "o"` blob prefix (`YYYY-MM-DDTHH:MM:SS.fffffffZ `) GitHub
/// stores on every downloaded log line — see docs/reference/interface.md.
fn strip_blob_timestamp(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    if bytes.len() < 28 {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if digits(0..4)
        && bytes[4] == b'-'
        && digits(5..7)
        && bytes[7] == b'-'
        && digits(8..10)
        && bytes[10] == b'T'
        && digits(11..13)
        && bytes[13] == b':'
        && digits(14..16)
        && bytes[16] == b':'
        && digits(17..19)
        && bytes[19] == b'.'
        && digits(20..27)
        && bytes[27] == b'Z'
    {
        match bytes.get(28) {
            Some(b' ') => Some(&line[29..]),
            _ => Some(""),
        }
    } else {
        None
    }
}

fn step_duration_seconds(step: &Step) -> Option<i64> {
    let started = parse_rfc3339(step.started_at.as_deref()?)?;
    let completed = parse_rfc3339(step.completed_at.as_deref()?)?;
    Some((completed - started).whole_seconds())
}

fn parse_rfc3339(value: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
}

fn executed(step: &Step) -> bool {
    step.conclusion.as_deref() != Some("skipped")
}

fn duration_cell(step: &Step) -> String {
    step_duration_seconds(step)
        .map(|secs| format!("{secs}s"))
        .unwrap_or_else(|| "—".to_string())
}

fn expandable_cell(html: &BTreeMap<u64, HtmlStep>, number: u64) -> &'static str {
    match html.get(&number) {
        Some(step) if step.expandable => "yes",
        Some(_) => "no",
        None => "?",
    }
}

/// Compare one paired step row; returns (verdict cell, is_worse).
fn step_verdict(
    github: &Step,
    velnor: &Step,
    github_html: &BTreeMap<u64, HtmlStep>,
    velnor_html: &BTreeMap<u64, HtmlStep>,
) -> (String, bool) {
    let mut worse = Vec::new();
    if normalized_step_name(&github.name) != normalized_step_name(&velnor.name) {
        worse.push(format!("name '{}' vs '{}'", github.name, velnor.name));
    }
    if github.conclusion != velnor.conclusion {
        worse.push(format!(
            "conclusion {} vs {}",
            github.conclusion.as_deref().unwrap_or("-"),
            velnor.conclusion.as_deref().unwrap_or("-")
        ));
    }
    let gh_expandable = github_html.get(&github.number).map(|step| step.expandable);
    let vl_expandable = velnor_html.get(&velnor.number).map(|step| step.expandable);
    if executed(github)
        && executed(velnor)
        && let (Some(true), Some(false)) = (gh_expandable, vl_expandable)
    {
        worse.push("not expandable".to_string());
    }
    if worse.is_empty() {
        ("ok".to_string(), false)
    } else {
        (format!("WORSE ({})", worse.join("; ")), true)
    }
}

#[allow(clippy::too_many_arguments)]
fn compare_pair(
    github: &Job,
    velnor: &Job,
    github_html: &BTreeMap<u64, HtmlStep>,
    velnor_html: &BTreeMap<u64, HtmlStep>,
    github_content: LaneLogStats,
    velnor_content: LaneLogStats,
) -> Result<(String, usize)> {
    let mut section = String::new();
    let mut worse_count = 0usize;

    writeln!(section, "\n## {} ⇄ {}", github.name, velnor.name)?;
    writeln!(section)?;
    writeln!(
        section,
        "Jobs: github `{}` ({}) ⇄ velnor `{}` ({})",
        github.id,
        github.conclusion.as_deref().unwrap_or("-"),
        velnor.id,
        velnor.conclusion.as_deref().unwrap_or("-"),
    )?;
    writeln!(section)?;
    writeln!(
        section,
        "| # | step (github) | step (velnor) | gh concl | vl concl | gh expand | vl expand | gh dur | vl dur | verdict |"
    )?;
    writeln!(
        section,
        "|---|---------------|---------------|----------|----------|-----------|-----------|--------|--------|---------|"
    )?;

    // Pair steps by SEQUENCE (longest common subsequence on lane-normalized
    // names), not by number: GitHub inserts runner-generated container-action
    // prep steps ("Build <action>", "Pull <image>") and leaves numbering gaps
    // for reserved pre/post slots, so positions drift between lanes even when
    // the user-visible step list matches.
    for row in align_steps(&github.steps, &velnor.steps) {
        match row {
            AlignedRow::Pair(gh_step, vl_step) => {
                let (verdict, is_worse) = step_verdict(gh_step, vl_step, github_html, velnor_html);
                if is_worse {
                    worse_count += 1;
                }
                writeln!(
                    section,
                    "| {}/{} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    gh_step.number,
                    vl_step.number,
                    gh_step.name,
                    vl_step.name,
                    gh_step.conclusion.as_deref().unwrap_or("-"),
                    vl_step.conclusion.as_deref().unwrap_or("-"),
                    expandable_cell(github_html, gh_step.number),
                    expandable_cell(velnor_html, vl_step.number),
                    duration_cell(gh_step),
                    duration_cell(vl_step),
                    verdict,
                )?;
            }
            AlignedRow::GitHubOnly(gh_step) => {
                let verdict = if runner_generated_step(&gh_step.name) {
                    "ok (runner-generated container prep; native adapters need no build step)"
                } else if executed(gh_step) {
                    worse_count += 1;
                    "WORSE (missing on velnor)"
                } else {
                    "ok (skipped, github-only)"
                };
                writeln!(
                    section,
                    "| {}/— | {} | — | {} | — | {} | — | {} | — | {} |",
                    gh_step.number,
                    gh_step.name,
                    gh_step.conclusion.as_deref().unwrap_or("-"),
                    expandable_cell(github_html, gh_step.number),
                    duration_cell(gh_step),
                    verdict,
                )?;
            }
            AlignedRow::VelnorOnly(vl_step) => {
                writeln!(
                    section,
                    "| —/{} | — | {} | — | {} | — | {} | — | {} | velnor-only |",
                    vl_step.number,
                    vl_step.name,
                    vl_step.conclusion.as_deref().unwrap_or("-"),
                    expandable_cell(velnor_html, vl_step.number),
                    duration_cell(vl_step),
                )?;
            }
        }
    }

    writeln!(section)?;
    writeln!(
        section,
        "Lane log content: github {} lines ({} timestamped, {} groups, ansi: {}) \
         ⇄ velnor {} lines ({} timestamped, {} groups, ansi: {})",
        github_content.lines,
        github_content.timestamped_lines,
        github_content.group_markers,
        github_content.ansi,
        velnor_content.lines,
        velnor_content.timestamped_lines,
        velnor_content.group_markers,
        velnor_content.ansi,
    )?;
    let mut content_worse = Vec::new();
    if velnor_content.lines > 0 {
        if github_content.timestamped_lines > 0 && velnor_content.timestamped_lines == 0 {
            content_worse.push("no timestamps");
        }
        if github_content.group_markers > 0 && velnor_content.group_markers == 0 {
            content_worse.push("no groups");
        }
        if github_content.ansi && !velnor_content.ansi {
            content_worse.push("no ANSI");
        }
    }
    if !content_worse.is_empty() {
        worse_count += content_worse.len();
        writeln!(
            section,
            "\n**WORSE (lane content):** {}",
            content_worse.join(", ")
        )?;
    }

    Ok((section, worse_count))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    fn step(number: u64, name: &str, conclusion: &str) -> Step {
        Step {
            name: name.to_string(),
            status: "completed".to_string(),
            conclusion: Some(conclusion.to_string()),
            number,
            started_at: Some("2026-06-11T07:00:00Z".to_string()),
            completed_at: Some("2026-06-11T07:00:05Z".to_string()),
        }
    }

    fn job(steps: Vec<Step>) -> Job {
        Job {
            id: 1,
            name: "compat (app-a, github)".to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
            html_url: None,
            steps,
        }
    }

    fn named_job(id: u64, name: &str) -> Job {
        Job {
            id,
            name: name.to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
            html_url: None,
            steps: Vec::new(),
        }
    }

    fn named_job_status(id: u64, name: &str, status: &str, conclusion: Option<&str>) -> Job {
        Job {
            id,
            name: name.to_string(),
            status: status.to_string(),
            conclusion: conclusion.map(str::to_string),
            html_url: None,
            steps: Vec::new(),
        }
    }

    #[test]
    fn lane_detection_and_pair_key_match_fixture_naming() {
        assert_eq!(
            lane_of_job_name(r#"compat (app-a, github, "ubuntu-latest")"#),
            Some(Lane::GitHub)
        );
        assert_eq!(
            lane_of_job_name(r#"compat (app-a, velnor, ["self-hosted","velnor-target-mvp"])"#),
            Some(Lane::Velnor)
        );
        assert_eq!(lane_of_job_name("lint"), None);
        // A name carrying both tokens is ambiguous, never mispaired.
        assert_eq!(lane_of_job_name("github-to-velnor sync"), None);

        assert_eq!(
            pair_key(r#"compat (app-a, github, "ubuntu-latest")"#),
            pair_key(r#"compat (app-a, velnor, ["self-hosted","velnor-target-mvp"])"#)
        );
        assert_ne!(
            pair_key(r#"compat (app-a, github, "ubuntu-latest")"#),
            pair_key(r#"compat (app-b, github, "ubuntu-latest")"#)
        );
    }

    #[test]
    fn pair_lane_jobs_pairs_by_key() {
        let jobs = vec![
            named_job(1, r#"compat (app-a, github, "ubuntu-latest")"#),
            named_job(
                2,
                r#"compat (app-a, velnor, ["self-hosted","velnor-target-mvp"])"#,
            ),
            named_job(3, "lint"),
        ];
        let pairs = pair_lane_jobs(&jobs);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.id, 1);
        assert_eq!(pairs[0].1.id, 2);
    }

    #[test]
    fn generated_matched_pair_passes_strict_bijection() {
        let jobs = vec![
            named_job(1, "Rust · rust-policy / GitHub"),
            named_job(2, "Rust · rust-policy / Velnor"),
        ];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.matched.len(), 1);
        assert_eq!(census.matched[0].2, "Rust · rust-policy");
        assert!(!census.has_parity_failures());
        let report = format_pairing_report(&census).unwrap();
        assert!(report.contains("### Matched pairs"));
        assert!(report.contains("Rust · rust-policy / GitHub"));
        assert!(report.contains("Rust · rust-policy / Velnor"));
    }

    #[test]
    fn github_only_unit_fails_strict() {
        let jobs = vec![named_job(1, "Rust · rust-policy / GitHub")];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.github_only.len(), 1);
        assert!(census.has_parity_failures());
        let report = format_pairing_report(&census).unwrap();
        assert!(report.contains("### GitHub-only jobs"));
        assert!(report.contains("### Missing counterpart"));
        assert!(report.contains("no Velnor counterpart"));
    }

    #[test]
    fn velnor_only_unit_fails_strict() {
        let jobs = vec![named_job(2, "Rust · rust-policy / Velnor")];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.velnor_only.len(), 1);
        assert!(census.has_parity_failures());
        let report = format_pairing_report(&census).unwrap();
        assert!(report.contains("### Velnor-only jobs"));
        assert!(report.contains("no GitHub counterpart"));
    }

    #[test]
    fn duplicate_pair_fails_strict() {
        let jobs = vec![
            named_job(1, "Rust · rust-policy / GitHub"),
            named_job(3, "Rust · rust-policy / GitHub"),
            named_job(2, "Rust · rust-policy / Velnor"),
        ];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.duplicate_github.len(), 1);
        assert!(census.matched.is_empty());
        assert!(census.has_parity_failures());
        let report = format_pairing_report(&census).unwrap();
        assert!(report.contains("### Duplicate GitHub executions"));
        assert!(report.contains("### Duplicate Velnor executions"));
    }

    #[test]
    fn skipped_counterpart_fails_strict() {
        let jobs = vec![
            named_job(1, "Rust · rust-policy / GitHub"),
            named_job_status(
                2,
                "Rust · rust-policy / Velnor",
                "completed",
                Some("skipped"),
            ),
        ];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.matched.len(), 1);
        assert_eq!(census.skipped_counterpart.len(), 1);
        assert!(census.has_parity_failures());

        let status_skipped = vec![
            named_job(1, "Rust · rust-policy / GitHub"),
            named_job_status(2, "Rust · rust-policy / Velnor", "skipped", None),
        ];
        let census = pair_lane_census(&status_skipped);
        assert_eq!(census.skipped_counterpart.len(), 1);
        assert!(census.has_parity_failures());
        let report = format_pairing_report(&census).unwrap();
        assert!(report.contains("### Skipped counterpart"));
        assert!(report.contains("one side skipped"));
    }

    #[test]
    fn control_jobs_do_not_create_false_parity_failure() {
        let jobs = vec![
            named_job(1, "Rust · rust-policy / GitHub"),
            named_job(2, "Rust · rust-policy / Velnor"),
            named_job(3, "Control / Planning"),
            named_job(4, "Control / Aggregate"),
            named_job(5, "Control / Prepare Cargo"),
            named_job(6, "lint"),
        ];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.matched.len(), 1);
        assert_eq!(census.control.len(), 4);
        assert!(!census.has_parity_failures());
        let report = format_pairing_report(&census).unwrap();
        assert!(report.contains("### Control jobs ignored"));
        assert!(report.contains("Control / Planning"));
        assert!(report.contains("lint"));
        assert!(report.contains("informational, not a failure"));
    }

    #[test]
    fn unit_id_containing_velnor_is_not_a_lane_token() {
        let jobs = vec![
            named_job(1, "Rust · rust-velnor-tools / GitHub"),
            named_job(2, "Rust · rust-velnor-tools / Velnor"),
            named_job(3, "Rust · rust-policy / GitHub"),
            named_job(4, "Rust · rust-policy / Velnor"),
        ];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.matched.len(), 2);
        let keys: Vec<&str> = census
            .matched
            .iter()
            .map(|(_, _, key)| key.as_str())
            .collect();
        assert!(keys.contains(&"Rust · rust-velnor-tools"));
        assert!(keys.contains(&"Rust · rust-policy"));
        assert!(census.ambiguous.is_empty());
        assert!(!census.has_parity_failures());
    }

    #[test]
    fn unambiguous_mapping_failure_fails_strict() {
        let jobs = vec![
            named_job(1, "Rust · rust-policy / GitHub"),
            named_job(2, "Rust · rust-policy / Velnor"),
            named_job(3, "github-to-velnor sync"),
        ];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.ambiguous.len(), 1);
        assert_eq!(census.ambiguous[0].name, "github-to-velnor sync");
        assert!(census.has_parity_failures());
        let report = format_pairing_report(&census).unwrap();
        assert!(report.contains("### Ambiguous names"));
        assert!(report.contains("github-to-velnor sync"));
    }

    #[test]
    fn fixture_matrix_naming_still_pairs() {
        let jobs = vec![
            named_job(1, r#"compat (app-a, github, "ubuntu-latest")"#),
            named_job(
                2,
                r#"compat (app-a, velnor, ["self-hosted","velnor-target-mvp"])"#,
            ),
        ];
        let census = pair_lane_census(&jobs);
        assert_eq!(census.matched.len(), 1);
        assert_eq!(census.matched[0].0.id, 1);
        assert_eq!(census.matched[0].1.id, 2);
        assert!(!census.has_parity_failures());
    }

    #[test]
    fn parse_check_steps_reads_number_and_expandability() {
        let html = r#"
<check-steps>
  <check-step
    data-name="Set up job"
    data-number="1"
    data-conclusion="success"
    data-external-id="e7cf94ab-32aa-4712-be84-b4521dcbad16"
    data-log-url="/o/r/commit/sha/checks/1/logs/1">
  </check-step>
  <check-step data-name="Skipped" data-number="3" data-conclusion="skipped" data-log-url="">
  </check-step>
</check-steps>"#;
        let steps = parse_check_steps(html);
        assert_eq!(steps.len(), 2);
        assert!(steps[&1].expandable);
        assert_eq!(
            steps[&1].external_id,
            "e7cf94ab-32aa-4712-be84-b4521dcbad16"
        );
        assert!(!steps[&3].expandable);
    }

    #[test]
    fn normalized_step_name_treats_lane_tokens_as_equal() {
        assert_eq!(
            normalized_step_name(
                r#"Run python3 .github/scripts/write-result.py "app-a" "github" "out/result.json""#
            ),
            normalized_step_name(
                r#"Run python3 .github/scripts/write-result.py "app-a" "velnor" "out/result.json""#
            ),
        );
        assert_ne!(
            normalized_step_name("Run just clippy \"app-a\""),
            normalized_step_name("Run just clippy \"app-b\""),
        );
    }

    #[test]
    fn analyze_lane_log_extracts_ui_affordances() {
        let text = "2026-06-11T07:34:33.1187693Z ##[group]Run actions/checkout@v6\n\
                    2026-06-11T07:34:33.1187693Z \u{1b}[36;1mecho hi\u{1b}[0m\n\
                    2026-06-11T07:34:33.1187693Z ##[endgroup]\n\
                    plain line without timestamp\n";
        let stats = analyze_lane_log(text);
        assert_eq!(stats.lines, 4);
        assert_eq!(stats.timestamped_lines, 3);
        assert_eq!(stats.group_markers, 1);
        assert!(stats.ansi);
    }

    #[test]
    fn strip_blob_timestamp_requires_seven_digit_form() {
        assert_eq!(
            strip_blob_timestamp("2026-06-11T07:34:33.1187693Z content"),
            Some("content")
        );
        assert_eq!(
            strip_blob_timestamp("2026-06-11T07:34:33.1187693Z"),
            Some("")
        );
        assert_eq!(strip_blob_timestamp("2026-06-11T07:34:33Z content"), None);
        assert_eq!(strip_blob_timestamp("plain"), None);
    }

    #[test]
    fn step_verdict_flags_information_loss() {
        let html_with = |number: u64, expandable: bool| {
            BTreeMap::from([(
                number,
                HtmlStep {
                    number,
                    expandable,
                    external_id: String::new(),
                },
            )])
        };
        // Same step, both expandable → ok.
        let (verdict, worse) = step_verdict(
            &step(2, "Run actions/checkout@v6", "success"),
            &step(2, "Run actions/checkout@v6", "success"),
            &html_with(2, true),
            &html_with(2, true),
        );
        assert_eq!(verdict, "ok");
        assert!(!worse);

        // Velnor not expandable while GitHub is.
        let (verdict, worse) = step_verdict(
            &step(2, "Run actions/checkout@v6", "success"),
            &step(2, "Run actions/checkout@v6", "success"),
            &html_with(2, true),
            &html_with(2, false),
        );
        assert!(worse, "{verdict}");
        assert!(verdict.contains("not expandable"));

        // Display-name divergence (e.g. unevaluated `${{ }}` or YAML id).
        let (verdict, worse) = step_verdict(
            &step(9, "Run set -euo pipefail", "success"),
            &step(9, "msrv", "success"),
            &html_with(9, true),
            &html_with(9, true),
        );
        assert!(worse, "{verdict}");
        assert!(verdict.contains("name"));

        // Lane-token differences are NOT divergences.
        let (_, worse) = step_verdict(
            &step(18, r#"Run write-result.py "github""#, "success"),
            &step(18, r#"Run write-result.py "velnor""#, "success"),
            &html_with(18, true),
            &html_with(18, true),
        );
        assert!(!worse);

        // Conclusion mismatch is information divergence.
        let (verdict, worse) = step_verdict(
            &step(2, "Run actions/checkout@v6", "success"),
            &step(2, "Run actions/checkout@v6", "failure"),
            &html_with(2, true),
            &html_with(2, true),
        );
        assert!(worse, "{verdict}");
        assert!(verdict.contains("conclusion"));

        // Skipped steps are not expandable on either lane — not a loss.
        let (_, worse) = step_verdict(
            &step(3, "Run echo skip", "skipped"),
            &step(3, "Run echo skip", "skipped"),
            &html_with(3, false),
            &html_with(3, false),
        );
        assert!(!worse);
    }

    #[test]
    fn align_steps_handles_runner_generated_prep_and_gaps() {
        let gh = vec![
            step(1, "Set up job", "success"),
            step(2, "Build hadolint/hadolint-action@sha", "success"),
            step(3, "Run actions/checkout@v6", "success"),
            step(4, "Login to Docker Hub", "success"),
            step(8, "Post Run actions/checkout@v6", "success"),
        ];
        let vl = vec![
            step(1, "Set up job", "success"),
            step(2, "Run actions/checkout@v6", "success"),
            step(3, "Login to Docker Hub", "skipped"),
            step(5, "Post Run actions/checkout@v6", "success"),
        ];
        let rows = align_steps(&gh, &vl);
        let summary: Vec<String> = rows
            .iter()
            .map(|row| match row {
                AlignedRow::Pair(g, v) => format!("pair {}={}", g.number, v.number),
                AlignedRow::GitHubOnly(g) => format!("gh {}", g.number),
                AlignedRow::VelnorOnly(v) => format!("vl {}", v.number),
            })
            .collect();
        assert_eq!(
            summary,
            vec!["pair 1=1", "gh 2", "pair 3=2", "pair 4=3", "pair 8=5"]
        );
        assert!(runner_generated_step("Build hadolint/hadolint-action@sha"));
        assert!(runner_generated_step("Pull node:20"));
        assert!(!runner_generated_step("Run actions/checkout@v6"));
    }

    #[test]
    fn step_duration_uses_rfc3339_fields() {
        let step = step(1, "Set up job", "success");
        assert_eq!(step_duration_seconds(&step), Some(5));
        let mut open_ended = step.clone();
        open_ended.completed_at = None;
        assert_eq!(step_duration_seconds(&open_ended), None);
    }

    #[test]
    fn job_duration_skips_incomplete_and_malformed_steps() {
        let mut incomplete = step(1, "incomplete", "success");
        incomplete.completed_at = None;
        let mut malformed = step(2, "malformed", "success");
        malformed.started_at = Some("not-rfc3339".to_string());
        let mut valid = step(3, "valid", "success");
        valid.started_at = Some("2026-06-11T07:00:10Z".to_string());
        valid.completed_at = Some("2026-06-11T07:00:17Z".to_string());

        assert_eq!(
            job_duration_seconds(&job(vec![incomplete, malformed, valid])),
            Some(7)
        );
    }

    #[test]
    fn job_duration_is_unavailable_without_a_complete_pair() {
        let mut incomplete = step(1, "incomplete", "success");
        incomplete.started_at = None;
        let mut malformed = step(2, "malformed", "success");
        malformed.completed_at = Some("not-rfc3339".to_string());

        assert_eq!(
            job_duration_seconds(&job(vec![incomplete, malformed])),
            None
        );
    }

    #[test]
    fn class_d_wall_budget_accepts_sixty_seconds() {
        assert_eq!(
            wall_budget_verdict(RunClass::D, 60),
            BudgetVerdict {
                seconds: 60,
                budget: 60,
                pass: true,
            }
        );
    }

    #[test]
    fn class_a_wall_budget_rejects_over_150_seconds() {
        assert!(!wall_budget_verdict(RunClass::A, 151).pass);
    }

    fn lane_stats(gh: f64, vl: f64, worse_rows: usize) -> LaneStats {
        LaneStats {
            run_id: 42,
            baseline_runs: 1,
            parity_worse_rows: worse_rows,
            jobs: BTreeMap::from([(
                "compat (app-a".to_string(),
                JobClassStats {
                    github_seconds: gh,
                    velnor_seconds: vl,
                },
            )]),
        }
    }

    #[test]
    fn is_regression_accepts_within_threshold() {
        let baseline = lane_stats(100.0, 110.0, 0);
        let current = lane_stats(100.0, 120.0, 0);

        let verdict = is_regression(&baseline, &current, 10.0);

        assert!(!verdict.regression, "{:?}", verdict.reasons);
    }

    #[test]
    fn is_regression_flags_velnor_slowdown_beyond_threshold() {
        let baseline = lane_stats(100.0, 100.0, 0);
        let current = lane_stats(100.0, 140.0, 0);

        let verdict = is_regression(&baseline, &current, 25.0);

        assert!(verdict.regression);
        assert!(verdict.reasons[0].contains("velnor/github ratio"));
    }

    #[test]
    fn is_regression_flags_parity_diff() {
        let baseline = lane_stats(100.0, 100.0, 0);
        let current = lane_stats(100.0, 100.0, 2);

        let verdict = is_regression(&baseline, &current, 25.0);

        assert!(verdict.regression);
        assert!(verdict.reasons[0].contains("parity diff"));
    }

    #[test]
    fn is_regression_accepts_velnor_faster() {
        let baseline = lane_stats(100.0, 120.0, 0);
        let current = lane_stats(100.0, 80.0, 0);

        let verdict = is_regression(&baseline, &current, 0.0);

        assert!(!verdict.regression, "{:?}", verdict.reasons);
    }

    #[test]
    fn is_regression_skips_job_without_baseline_timing() {
        let baseline = LaneStats::default();
        let current = lane_stats(100.0, 140.0, 0);

        let verdict = is_regression(&baseline, &current, 0.0);

        assert!(!verdict.regression, "{:?}", verdict.reasons);
    }

    #[test]
    fn baseline_from_samples_rejects_samples_without_usable_timing() {
        let samples = vec![LaneStats::default(), LaneStats::default()];

        assert!(baseline_from_samples(&samples).is_none());
    }

    #[test]
    fn baseline_from_samples_accepts_partial_timing_coverage() {
        let samples = vec![lane_stats(100.0, 110.0, 0), LaneStats::default()];

        let baseline = baseline_from_samples(&samples).expect("baseline should be usable");

        assert_eq!(baseline.baseline_runs, 2);
        assert_eq!(baseline.jobs.len(), 1);
    }

    #[test]
    fn regression_report_marks_missing_baseline_timing_unavailable() {
        let baseline = LaneStats::default();
        let current = lane_stats(100.0, 140.0, 0);

        let report = regression_report(
            "tailrocks/velnor",
            "compat.yml",
            &baseline,
            &current,
            &is_regression(&baseline, &current, 0.0),
        )
        .expect("report should render");

        assert!(report.contains("| compat (app-a | — | — | 100.0s | 140.0s | — -> 1.40 |"));
        assert!(!report.contains("| 0.0s |"));
    }
}

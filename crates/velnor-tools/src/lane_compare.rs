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
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    /// Compare exactly this GitHub-lane job id as a diagnostic subset.  The
    /// complete run census is still fetched and validated; selectors never
    /// define the expected workload or produce a full-run gate result.
    #[arg(long, requires = "velnor_job")]
    pub github_job: Option<u64>,
    /// Compare exactly this Velnor-lane job id as a diagnostic subset.  The
    /// complete run census is still fetched and validated; selectors never
    /// define the expected workload or produce a full-run gate result.
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
struct RunSummary {
    id: u64,
    status: String,
    conclusion: Option<String>,
    event: String,
    head_sha: String,
    #[serde(default)]
    run_attempt: Option<u64>,
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

#[derive(Debug, Clone)]
struct PairEvidence {
    github_html: BTreeMap<u64, HtmlStep>,
    velnor_html: BTreeMap<u64, HtmlStep>,
    github_log: String,
    github_content: LaneLogStats,
    velnor_content: LaneLogStats,
}

#[derive(Debug, Clone)]
struct ValidatedRun {
    summary: RunSummary,
    census: PairingCensus,
    velnor_content: String,
    pair_evidence: BTreeMap<(u64, u64), PairEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComparisonScope {
    FullRun,
    SubsetDiagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComparisonDecision {
    AuxiliaryPass,
    DiagnosticOnly,
    Fail,
}

fn comparison_decision(
    scope: ComparisonScope,
    strict: bool,
    worse_rows: usize,
    budget_failures: usize,
) -> ComparisonDecision {
    if worse_rows > 0 || budget_failures > 0 {
        return if strict {
            ComparisonDecision::Fail
        } else {
            ComparisonDecision::DiagnosticOnly
        };
    }
    if !strict || scope == ComparisonScope::SubsetDiagnostic {
        ComparisonDecision::DiagnosticOnly
    } else {
        ComparisonDecision::AuxiliaryPass
    }
}

fn select_pairs(
    census: &PairingCensus,
    selected_jobs: Option<(u64, u64)>,
    run_id: u64,
) -> Result<(Vec<(Job, Job)>, ComparisonScope)> {
    let Some((github_id, velnor_id)) = selected_jobs else {
        return Ok((census.matched_pairs(), ComparisonScope::FullRun));
    };
    let selected = census
        .matched
        .iter()
        .find(|(github, velnor, _)| github.id == github_id && velnor.id == velnor_id)
        .map(|(github, velnor, _)| (github.clone(), velnor.clone()));
    selected
        .map(|pair| (vec![pair], ComparisonScope::SubsetDiagnostic))
        .with_context(|| {
            format!(
                "selected jobs {github_id}/{velnor_id} are not a validated pair in run {run_id}; selectors cannot bypass the census"
            )
        })
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
    let census = pair_lane_census(&jobs);
    let selected_jobs = match (args.github_job, args.velnor_job) {
        (Some(github), Some(velnor)) => Some((github, velnor)),
        (None, None) => None,
        _ => bail!("--github-job and --velnor-job must be supplied together"),
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
        "Comparison: equal-or-better — zero rows where the GitHub lane shows \
         information the Velnor lane lacks. Expected comparison units come \
         from the complete Actions jobs census; selectors cannot redefine it."
    )?;
    writeln!(report)?;
    report.push_str(&format_pairing_report(&census)?);

    let validated = match validate_run_evidence(&args.repo, run_id, &jobs, census.clone()) {
        Ok(validated) => validated,
        Err(error) => {
            writeln!(report, "\n## Evidence")?;
            writeln!(report)?;
            writeln!(report, "**FAIL** — {error:#}")?;
            writeln!(report, "\n## Result")?;
            writeln!(report)?;
            writeln!(report, "**FAIL** — run evidence is incomplete or not successful; no comparison gate result.")?;
            let report_path = run_dir.join("report.md");
            fs::write(&report_path, &report)
                .with_context(|| format!("write {}", report_path.display()))?;
            println!("{report}");
            println!("report: {}", report_path.display());
            return Err(error.context("lane-compare run evidence validation failed"));
        }
    };
    writeln!(report, "## Independently fetched evidence")?;
    writeln!(report)?;
    writeln!(
        report,
        "Actions run `{}`: status `{}`, conclusion `{}`, event `{}`, head `{}`; attempt `{}`.",
        validated.summary.id,
        validated.summary.status,
        validated.summary.conclusion.as_deref().unwrap_or("-"),
        validated.summary.event,
        validated.summary.head_sha,
        validated
            .summary
            .run_attempt
            .map_or_else(|| "-".to_owned(), |attempt| attempt.to_string()),
    )?;
    writeln!(
        report,
        "Validated {} nonempty GitHub↔Velnor comparison unit(s) from the complete paginated jobs census and {} required Velnor job-log artifact(s).",
        validated.census.matched.len(),
        validated.pair_evidence.len(),
    )?;
    writeln!(
        report,
        "Limit: lane-compare is auxiliary. It does not independently prove expected source/ref, checkout SHA, required-check association, provider, runner, host, or trust identity; the independent checker must bind those facts."
    )?;
    writeln!(report)?;

    let (pairs, scope) = match select_pairs(&validated.census, selected_jobs, run_id) {
        Ok(selected) => selected,
        Err(error) => {
            writeln!(report, "## Result")?;
            writeln!(report)?;
            writeln!(report, "**FAIL** — {error:#}")?;
            let report_path = run_dir.join("report.md");
            fs::write(&report_path, &report)
                .with_context(|| format!("write {}", report_path.display()))?;
            println!("{report}");
            println!("report: {}", report_path.display());
            return Err(error);
        }
    };

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
        let evidence = validated
            .pair_evidence
            .get(&(github.id, velnor.id))
            .with_context(|| {
                format!(
                    "missing validated evidence for pair {}/{}",
                    github.id, velnor.id
                )
            })?;
        fs::write(
            run_dir.join(format!("github-job-{}.log", github.id)),
            &evidence.github_log,
        )?;
        let (section, worse) = compare_pair(
            github,
            velnor,
            &evidence.github_html,
            &evidence.velnor_html,
            evidence.github_content,
            evidence.velnor_content,
        )?;
        worse_total += worse;
        report.push_str(&section);
    }
    if !validated.velnor_content.is_empty() {
        fs::write(
            run_dir.join("velnor-job-log.log"),
            &validated.velnor_content,
        )
        .context("save velnor job-log artifact content")?;
    }

    writeln!(report, "\n## Result")?;
    writeln!(report)?;
    let decision = comparison_decision(scope, args.strict, worse_total, budget_failures);
    match decision {
        ComparisonDecision::AuxiliaryPass => writeln!(
            report,
            "**AUXILIARY PASS** — no paired step is less informative than the GitHub lane; this is not an authoritative goal/checker gate."
        )?,
        ComparisonDecision::DiagnosticOnly => writeln!(
            report,
            "**DIAGNOSTIC ONLY** — selected scope or non-strict mode cannot produce a full-run gate result ({} parity row(s), {} §2.11 budget failure(s)).",
            worse_total,
            budget_failures,
        )?,
        ComparisonDecision::Fail => writeln!(
            report,
            "**FAIL** — {worse_total} parity row(s) and {budget_failures} §2.11 budget failure(s)."
        )?,
    }
    writeln!(report)?;
    writeln!(
        report,
        "Required log and HTML evidence were fetched fail-closed. A missing, empty, truncated, or unavailable artifact/log is an evidence failure, not an empty comparison."
    )?;

    let report_path = run_dir.join("report.md");
    fs::write(&report_path, &report).with_context(|| format!("write {}", report_path.display()))?;
    println!("{report}");
    println!("report: {}", report_path.display());

    if decision == ComparisonDecision::Fail {
        bail!("lane-compare gate failed: {worse_total} worse row(s), {budget_failures} budget failure(s); see report above");
    }
    if decision == ComparisonDecision::DiagnosticOnly && scope == ComparisonScope::SubsetDiagnostic
    {
        bail!("lane-compare subset diagnostic is not a full-run gate; see report above");
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

    let run_items = recent_complete_both_lane_runs(&args.repo, &args.workflow, args.since)?;
    if run_items.len() < 2 {
        bail!(
            "need at least two completed both-lane runs for --watch; found {}",
            run_items.len()
        );
    }
    for run in &run_items {
        if !run.status.eq_ignore_ascii_case("completed")
            || !run
                .conclusion
                .as_deref()
                .is_some_and(|conclusion| conclusion.eq_ignore_ascii_case("success"))
        {
            bail!(
                "recent run {} is not a successful completed run (status `{}`, conclusion `{}`); watch sample is not proven",
                run.database_id,
                run.status,
                run.conclusion.as_deref().unwrap_or("missing")
            );
        }
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
    for run in &run_items {
        samples.push(lane_stats_for_run(&args.repo, run.database_id)?);
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

    if verdict.regression || verdict.not_proven {
        bail!(
            "lane-compare regression gate failed: {}",
            verdict.reasons.join("; ")
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct RunListItem {
    #[serde(rename = "databaseId")]
    database_id: u64,
    status: String,
    conclusion: Option<String>,
}

fn recent_run_args(repo: &str, workflow: &str, limit: usize) -> Vec<String> {
    vec![
        "run".to_owned(),
        "list".to_owned(),
        "--repo".to_owned(),
        repo.to_owned(),
        "--workflow".to_owned(),
        workflow.to_owned(),
        "--status".to_owned(),
        "success".to_owned(),
        "--limit".to_owned(),
        limit.max(2).to_string(),
        "--json".to_owned(),
        "databaseId,status,conclusion".to_owned(),
    ]
}

fn recent_run_items(repo: &str, workflow: &str, limit: usize) -> Result<Vec<RunListItem>> {
    let output = Command::new("gh")
        .args(recent_run_args(repo, workflow, limit))
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
    Ok(runs)
}

fn recent_complete_both_lane_runs(
    repo: &str,
    workflow: &str,
    limit: usize,
) -> Result<Vec<RunListItem>> {
    let target = limit.max(2);
    let mut fetch_limit = target;
    let mut inspected = BTreeSet::new();
    let mut selected = Vec::new();
    loop {
        let candidates = recent_run_items(repo, workflow, fetch_limit)?;
        let mut new_candidate = false;
        for run in candidates.iter() {
            if !inspected.insert(run.database_id) {
                continue;
            }
            new_candidate = true;
            if !run.status.eq_ignore_ascii_case("completed")
                || !run
                    .conclusion
                    .as_deref()
                    .is_some_and(|conclusion| conclusion.eq_ignore_ascii_case("success"))
            {
                continue;
            }
            let jobs = fetch_run_jobs(repo, run.database_id).with_context(|| {
                format!(
                    "inspect both-lane census for successful run {}",
                    run.database_id
                )
            })?;
            if has_complete_both_lane_census(&jobs) {
                selected.push(run.clone());
                if selected.len() == target {
                    return Ok(selected);
                }
            }
        }
        if candidates.len() < fetch_limit || !new_candidate {
            return Ok(selected);
        }
        let Some(next_limit) = fetch_limit.checked_mul(2) else {
            return Ok(selected);
        };
        fetch_limit = next_limit;
    }
}

fn has_complete_both_lane_census(jobs: &[Job]) -> bool {
    let census = pair_lane_census(jobs);
    !census.matched.is_empty() && !census.has_parity_failures()
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
    let mut seen_ids = BTreeSet::new();
    let mut page = 1u32;
    let mut total_count = None;
    loop {
        let payload = gh_api_bytes(&format!(
            "repos/{repo}/actions/runs/{run_id}/jobs?per_page=100&page={page}"
        ))?;
        let response: JobsResponse =
            serde_json::from_slice(&payload).context("parse run jobs response")?;
        total_count.get_or_insert(response.total_count);
        if total_count != Some(response.total_count) {
            bail!("run {run_id} jobs API changed total_count while paging");
        }
        let fetched = response.jobs.len();
        for job in response.jobs {
            if !seen_ids.insert(job.id) {
                bail!("run {run_id} jobs API repeated job id {}", job.id);
            }
            jobs.push(job);
        }
        let total = total_count.unwrap_or_default();
        if jobs.len() as u64 > total {
            bail!("run {run_id} jobs API returned more rows than total_count {total}");
        }
        if jobs.len() as u64 == total {
            break;
        }
        if fetched == 0 {
            bail!(
                "run {run_id} jobs API ended after {} of {total} jobs; census is truncated",
                jobs.len(),
            );
        }
        page += 1;
    }
    if jobs.is_empty() {
        bail!("run {run_id} has no jobs in {repo}");
    }
    Ok(jobs)
}

fn fetch_run_summary(repo: &str, run_id: u64) -> Result<RunSummary> {
    let payload = gh_api_bytes(&format!("repos/{repo}/actions/runs/{run_id}"))?;
    let summary: RunSummary = serde_json::from_slice(&payload)
        .with_context(|| format!("parse run {run_id} summary response"))?;
    if summary.id != run_id {
        bail!(
            "run summary identity mismatch: requested {run_id}, received {}",
            summary.id
        );
    }
    Ok(summary)
}

fn validate_run_summary(summary: &RunSummary, run_id: u64) -> Result<()> {
    if !summary.status.eq_ignore_ascii_case("completed") {
        bail!(
            "run {run_id} is {}, not completed; incomplete runs cannot produce comparison evidence",
            summary.status
        );
    }
    if summary.conclusion.as_deref() != Some("success") {
        bail!(
            "run {run_id} conclusion is {}, not success",
            summary.conclusion.as_deref().unwrap_or("missing")
        );
    }
    if summary.event.trim().is_empty() {
        bail!("run {run_id} has no event identity");
    }
    if summary.head_sha.len() != 40
        || !summary
            .head_sha
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        bail!("run {run_id} has no valid source head SHA");
    }
    Ok(())
}

fn validate_census(census: &PairingCensus, run_id: u64) -> Result<()> {
    if census.matched.is_empty() {
        bail!(
            "run {run_id} has an empty comparison census; at least one nonempty GitHub↔Velnor workload pair is required"
        );
    }
    if census.has_parity_failures() {
        bail!(
            "run {run_id} comparison census is not a complete 1:1 bijection; orphans, duplicates, skipped counterparts, or ambiguous names are present"
        );
    }
    Ok(())
}

fn validate_job_success(job: &Job, lane: Lane, run_id: u64) -> Result<()> {
    if !job.status.eq_ignore_ascii_case("completed") {
        bail!(
            "run {run_id} {} job {} ({}) is {}, not completed",
            lane_name(lane),
            job.id,
            job.name,
            job.status
        );
    }
    if job.conclusion.as_deref() != Some("success") {
        bail!(
            "run {run_id} {} job {} ({}) conclusion is {}, not success",
            lane_name(lane),
            job.id,
            job.name,
            job.conclusion.as_deref().unwrap_or("missing")
        );
    }
    Ok(())
}

fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::GitHub => "GitHub",
        Lane::Velnor => "Velnor",
    }
}

fn step_is_skipped(step: &Step) -> bool {
    step.status.eq_ignore_ascii_case("skipped")
        || step
            .conclusion
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("skipped"))
}

fn validate_html_step_coverage(
    job: &Job,
    lane: Lane,
    html: &BTreeMap<u64, HtmlStep>,
    run_id: u64,
) -> Result<()> {
    let job_numbers: BTreeSet<u64> = job.steps.iter().map(|step| step.number).collect();
    let missing: Vec<String> = job
        .steps
        .iter()
        .filter(|step| !step_is_skipped(step) && !html.contains_key(&step.number))
        .map(|step| format!("{} ({})", step.number, step.name))
        .collect();
    if !missing.is_empty() {
        bail!(
            "run {run_id} {} job {} HTML evidence is missing executed step(s): {}",
            lane_name(lane),
            job.id,
            missing.join(", ")
        );
    }
    let unexpected: Vec<String> = html
        .keys()
        .filter(|number| !job_numbers.contains(number))
        .map(u64::to_string)
        .collect();
    if !unexpected.is_empty() {
        bail!(
            "run {run_id} {} job {} HTML evidence has unexpected step number(s): {}",
            lane_name(lane),
            job.id,
            unexpected.join(", ")
        );
    }
    Ok(())
}

fn assess_run_evidence(
    run_id: u64,
    jobs: &[Job],
    census: PairingCensus,
    summary: RunSummary,
    velnor_content: String,
    pair_evidence: BTreeMap<(u64, u64), PairEvidence>,
) -> Result<ValidatedRun> {
    validate_run_summary(&summary, run_id)?;
    validate_census(&census, run_id)?;
    let job_ids: BTreeSet<u64> = jobs.iter().map(|job| job.id).collect();
    if job_ids.len() != jobs.len() {
        bail!("run {run_id} jobs census contains duplicate job identities");
    }
    for (github, velnor, _) in &census.matched {
        if !job_ids.contains(&github.id) || !job_ids.contains(&velnor.id) {
            bail!(
                "run {run_id} pairing census references job {} or {} outside the supplied jobs census",
                github.id,
                velnor.id
            );
        }
        validate_job_success(github, Lane::GitHub, run_id)?;
        validate_job_success(velnor, Lane::Velnor, run_id)?;
        let evidence = pair_evidence
            .get(&(github.id, velnor.id))
            .with_context(|| {
                format!(
                    "missing HTML/log evidence for validated pair {}/{} in run {run_id}",
                    github.id, velnor.id
                )
            })?;
        if evidence.github_html.is_empty() || evidence.velnor_html.is_empty() {
            bail!(
                "run {run_id} pair {}/{} has missing HTML check-step evidence",
                github.id,
                velnor.id
            );
        }
        validate_html_step_coverage(github, Lane::GitHub, &evidence.github_html, run_id)?;
        validate_html_step_coverage(velnor, Lane::Velnor, &evidence.velnor_html, run_id)?;
        require_nonempty_evidence(
            &format!("GitHub job {} log", github.id),
            &evidence.github_log,
        )?;
    }
    if pair_evidence.len() != census.matched.len() {
        bail!(
            "run {run_id} evidence census has {} pair payload(s) for {} expected pair(s)",
            pair_evidence.len(),
            census.matched.len()
        );
    }
    require_nonempty_evidence(
        &format!("Velnor job-log artifacts for run {run_id}"),
        &velnor_content,
    )?;
    Ok(ValidatedRun {
        summary,
        census,
        velnor_content,
        pair_evidence,
    })
}

fn validate_run_evidence(
    repo: &str,
    run_id: u64,
    jobs: &[Job],
    census: PairingCensus,
) -> Result<ValidatedRun> {
    let summary = fetch_run_summary(repo, run_id)?;
    validate_run_summary(&summary, run_id)?;
    validate_census(&census, run_id)?;
    for (github, velnor, _) in &census.matched {
        validate_job_success(github, Lane::GitHub, run_id)?;
        validate_job_success(velnor, Lane::Velnor, run_id)?;
    }
    let expected_velnor_job_ids: Vec<u64> = census
        .matched
        .iter()
        .map(|(_, velnor, _)| velnor.id)
        .collect();
    let velnor_logs = fetch_velnor_job_log_artifacts(repo, run_id, &expected_velnor_job_ids)?;
    let velnor_content = aggregate_velnor_job_logs(&velnor_logs, run_id)?;

    let mut pair_evidence = BTreeMap::new();
    for (github, velnor, _) in &census.matched {
        let github_html = fetch_job_html_steps(github).with_context(|| {
            format!(
                "fetch GitHub job {} HTML evidence for run {run_id}",
                github.id
            )
        })?;
        let velnor_html = fetch_job_html_steps(velnor).with_context(|| {
            format!(
                "fetch Velnor job {} HTML evidence for run {run_id}",
                velnor.id
            )
        })?;
        let github_log = fetch_github_job_log(repo, github.id).with_context(|| {
            format!(
                "fetch GitHub job {} log evidence for run {run_id}",
                github.id
            )
        })?;
        let github_content = analyze_lane_log(&github_log);
        if github_content.lines == 0 {
            bail!(
                "run {run_id} GitHub job {} log evidence was empty",
                github.id
            );
        }
        let velnor_content = velnor_job_log_stats(&velnor_logs, velnor.id, run_id)?;
        pair_evidence.insert(
            (github.id, velnor.id),
            PairEvidence {
                github_html,
                velnor_html,
                github_log,
                github_content,
                velnor_content,
            },
        );
    }
    assess_run_evidence(run_id, jobs, census, summary, velnor_content, pair_evidence)
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

fn require_nonempty_evidence(kind: &str, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        bail!("{kind} evidence is empty");
    }
    Ok(())
}

fn fetch_github_job_log(repo: &str, job_id: u64) -> Result<String> {
    let bytes = gh_api_bytes(&format!("repos/{repo}/actions/jobs/{job_id}/logs"))?;
    let log = String::from_utf8_lossy(&bytes).into_owned();
    require_nonempty_evidence(&format!("GitHub job {job_id} log"), &log)?;
    Ok(log)
}

fn github_auth_token() -> Result<String> {
    for variable in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(token) = std::env::var(variable) {
            let token = token.trim();
            if !token.is_empty() {
                return Ok(token.to_owned());
            }
        }
    }
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("read GitHub CLI authentication token")?;
    if !output.status.success() {
        bail!(
            "gh auth token failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if token.is_empty() {
        bail!("GitHub authentication token is empty");
    }
    Ok(token)
}

/// Job page HTML via authenticated curl (`gh api` cannot fetch web routes —
/// curl also sidesteps the reqwest TLS-fingerprint throttle).
fn fetch_job_html_steps(job: &Job) -> Result<BTreeMap<u64, HtmlStep>> {
    let url = job
        .html_url
        .as_deref()
        .with_context(|| format!("job {} has no html_url", job.id))?;
    let token = github_auth_token()?;
    fetch_job_html_steps_with_token(job.id, url, &token)
}

fn fetch_job_html_steps_with_token(
    job_id: u64,
    url: &str,
    token: &str,
) -> Result<BTreeMap<u64, HtmlStep>> {
    if token.trim().is_empty() {
        bail!("GitHub authentication token is empty");
    }
    let mut child = Command::new("curl")
        .args(["-fsSL", "-H", "@-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn curl {url}"))?;
    let mut stdin = child.stdin.take().context("open curl header input")?;
    writeln!(stdin, "Authorization: Bearer {}", token.trim())
        .context("write curl authorization header")?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .with_context(|| format!("wait for curl {url}"))?;
    if !output.status.success() {
        bail!(
            "curl {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let html = String::from_utf8_lossy(&output.stdout);
    let steps = parse_check_steps(&html)?;
    if steps.is_empty() {
        bail!("job {job_id} page contained no check-step evidence");
    }
    Ok(steps)
}

/// Extract `<check-step …>` elements: `data-number` plus whether
/// `data-log-url` is non-empty (that attribute is exactly what makes a step
/// expandable in the UI).
fn parse_check_steps(html: &str) -> Result<BTreeMap<u64, HtmlStep>> {
    let mut steps = BTreeMap::new();
    let mut rest = html;
    while let Some(start) = find_check_step_start(rest) {
        let element = &rest[start..];
        let Some(end) = element.find('>') else {
            bail!("malformed <check-step> element without closing `>`");
        };
        let element = &element[..end];
        let number_text =
            attr_value(element, "data-number").context("<check-step> is missing data-number")?;
        let number = number_text
            .parse::<u64>()
            .with_context(|| format!("invalid <check-step> data-number `{number_text}`"))?;
        if number == 0 {
            bail!("invalid <check-step> data-number `0`; step numbers start at 1");
        }
        if steps.contains_key(&number) {
            bail!("duplicate <check-step> data-number {number}");
        }
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
        rest = &rest[start + end..];
    }
    Ok(steps)
}

fn find_check_step_start(html: &str) -> Option<usize> {
    const MARKER: &str = "<check-step";
    let mut offset = 0;
    while let Some(found) = html[offset..].find(MARKER) {
        let start = offset + found;
        let after = html.as_bytes().get(start + MARKER.len()).copied();
        if matches!(
            after,
            Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            return Some(start);
        }
        offset = start + MARKER.len();
    }
    None
}

fn attr_value<'a>(element: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=\"");
    let start = element.find(&marker)? + marker.len();
    let rest = &element[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct ArtifactRef {
    id: u64,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ArtifactsResponse {
    total_count: u64,
    artifacts: Vec<ArtifactRef>,
}

/// Collect all artifact pages, rejecting duplicates and truncation.  The
/// expected job ids are supplied by the independently fetched jobs census;
/// artifact names cannot define the expected workload themselves.
fn collect_job_log_artifacts(
    pages: impl IntoIterator<Item = Result<ArtifactsResponse>>,
    expected_job_ids: &[u64],
) -> Result<BTreeMap<String, ArtifactRef>> {
    let expected: BTreeSet<String> = expected_job_ids
        .iter()
        .map(|id| format!("job-log-{id}"))
        .collect();
    if expected.is_empty() {
        bail!("cannot collect Velnor job-log artifacts without expected job ids");
    }
    if expected.len() != expected_job_ids.len() {
        bail!("expected Velnor job-log artifact census contains duplicate job ids");
    }
    let mut collected = BTreeMap::new();
    let mut observed = 0u64;
    let mut saw_page = false;
    let mut total_count = None;
    for page in pages {
        let page = page?;
        saw_page = true;
        observed = observed.saturating_add(page.artifacts.len() as u64);
        total_count.get_or_insert(page.total_count);
        if total_count != Some(page.total_count) {
            bail!("artifacts API changed total_count while paging");
        }
        for artifact in page.artifacts {
            if collected.insert(artifact.name.clone(), artifact).is_some() {
                bail!("artifacts API repeated artifact name");
            }
        }
        let total = page.total_count;
        if observed > total {
            bail!("artifacts API returned more rows than total_count {total}");
        }
    }
    if !saw_page {
        bail!("artifacts API returned no pages");
    }
    if observed != total_count.unwrap_or_default() {
        bail!(
            "artifacts API pages are truncated: observed {observed} of {} rows",
            total_count.unwrap_or_default()
        );
    }
    let missing: Vec<String> = expected
        .iter()
        .filter(|name| !collected.contains_key(*name))
        .cloned()
        .collect();
    if !missing.is_empty() {
        bail!(
            "missing required Velnor job-log artifact(s): {}",
            missing.join(", ")
        );
    }
    Ok(collected)
}

/// Download every expected per-job `job-log-<job-id>` artifact across all
/// paginated API pages. Missing, empty, duplicate, or truncated evidence is a
/// hard error; it is never converted into an empty log or warning. The result
/// remains keyed by job so one Velnor pair cannot inherit another pair's log
/// affordances.
fn fetch_velnor_job_log_artifacts(
    repo: &str,
    run_id: u64,
    expected_job_ids: &[u64],
) -> Result<BTreeMap<u64, String>> {
    let mut pages = Vec::new();
    let mut page_number = 1u32;
    let mut observed = 0u64;
    let mut total_count = None;
    loop {
        let payload = gh_api_bytes(&format!(
            "repos/{repo}/actions/runs/{run_id}/artifacts?per_page=100&page={page_number}"
        ))?;
        let page: ArtifactsResponse =
            serde_json::from_slice(&payload).context("parse artifacts response")?;
        let fetched = page.artifacts.len() as u64;
        observed = observed.saturating_add(fetched);
        total_count.get_or_insert(page.total_count);
        if total_count != Some(page.total_count) {
            bail!("artifacts API changed total_count while paging");
        }
        let done = observed >= page.total_count;
        pages.push(Ok(page));
        if done {
            break;
        }
        if fetched == 0 {
            bail!(
                "artifacts API ended after {observed} of {} rows; required log census is truncated",
                total_count.unwrap_or_default()
            );
        }
        page_number += 1;
    }
    let artifacts = collect_job_log_artifacts(pages, expected_job_ids)?;
    let mut content_by_job = BTreeMap::new();
    for job_id in expected_job_ids {
        let name = format!("job-log-{job_id}");
        let artifact = artifacts
            .get(&name)
            .with_context(|| format!("missing required artifact {name}"))?;
        let zip_bytes = gh_api_bytes(&format!(
            "repos/{repo}/actions/artifacts/{}/zip",
            artifact.id
        ))?;
        let cursor = std::io::Cursor::new(zip_bytes);
        let mut zip =
            zip::ZipArchive::new(cursor).with_context(|| format!("open {name} artifact zip"))?;
        let mut artifact_content = String::new();
        for index in 0..zip.len() {
            let mut file = zip
                .by_index(index)
                .with_context(|| format!("read {name} artifact entry"))?;
            if file.is_dir() {
                continue;
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .with_context(|| format!("read {name} artifact file"))?;
            artifact_content.push_str(&String::from_utf8_lossy(&bytes));
            artifact_content.push('\n');
        }
        require_nonempty_evidence(&format!("Velnor artifact {name}"), &artifact_content)?;
        if content_by_job.insert(*job_id, artifact_content).is_some() {
            bail!("duplicate Velnor job-log artifact target for job {job_id}");
        }
    }
    if content_by_job.is_empty() {
        bail!("Velnor job-log artifacts for run {run_id} were not requested");
    }
    Ok(content_by_job)
}

fn aggregate_velnor_job_logs(
    content_by_job: &BTreeMap<u64, String>,
    run_id: u64,
) -> Result<String> {
    let mut content = String::new();
    for artifact_content in content_by_job.values() {
        content.push_str(artifact_content);
    }
    require_nonempty_evidence(
        &format!("Velnor job-log artifacts for run {run_id}"),
        &content,
    )?;
    Ok(content)
}

fn velnor_job_log_stats(
    content_by_job: &BTreeMap<u64, String>,
    job_id: u64,
    run_id: u64,
) -> Result<LaneLogStats> {
    let content = content_by_job.get(&job_id).with_context(|| {
        format!("missing Velnor job-log content for job {job_id} in run {run_id}")
    })?;
    require_nonempty_evidence(&format!("Velnor job {job_id} log"), content)?;
    let stats = analyze_lane_log(content);
    if stats.lines == 0 {
        bail!("run {run_id} Velnor job {job_id} log evidence was empty");
    }
    Ok(stats)
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
    pub not_proven: bool,
    pub reasons: Vec<String>,
}

pub fn is_regression(
    baseline: &LaneStats,
    current: &LaneStats,
    threshold_pct: f64,
) -> RegressionVerdict {
    let mut reasons = Vec::new();
    let mut not_proven = false;
    let baseline_units: BTreeSet<&String> = baseline.jobs.keys().collect();
    let current_units: BTreeSet<&String> = current.jobs.keys().collect();
    if baseline_units != current_units {
        not_proven = true;
        reasons.push(format!(
            "workload set drift: baseline has {} unit(s), current has {} unit(s); no full-unit timing comparison is proven",
            baseline_units.len(),
            current_units.len(),
        ));
    }
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
        regression: !reasons.is_empty() && !not_proven,
        not_proven,
        reasons,
    }
}

fn lane_stats_for_run(repo: &str, run_id: u64) -> Result<LaneStats> {
    let jobs = fetch_run_jobs(repo, run_id)?;
    let census = pair_lane_census(&jobs);
    let validated = validate_run_evidence(repo, run_id, &jobs, census)?;

    let mut stats = LaneStats {
        run_id,
        baseline_runs: 1,
        parity_worse_rows: 0,
        jobs: BTreeMap::new(),
    };
    for (github, velnor, key) in &validated.census.matched {
        let evidence = validated
            .pair_evidence
            .get(&(github.id, velnor.id))
            .with_context(|| {
                format!(
                    "missing validated evidence for pair {}/{}",
                    github.id, velnor.id
                )
            })?;
        let (_section, worse) = compare_pair(
            github,
            velnor,
            &evidence.github_html,
            &evidence.velnor_html,
            evidence.github_content,
            evidence.velnor_content,
        )?;
        stats.parity_worse_rows += worse;
        let (Some(github_seconds), Some(velnor_seconds)) =
            (job_duration_seconds(github), job_duration_seconds(velnor))
        else {
            bail!(
                "run {run_id} pair {}/{} has incomplete timing evidence",
                github.id,
                velnor.id
            );
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
    let expected_units: BTreeSet<&String> = samples[0].jobs.keys().collect();
    if expected_units.is_empty()
        || samples
            .iter()
            .any(|sample| sample.jobs.keys().collect::<BTreeSet<_>>() != expected_units)
    {
        // A timing baseline with an empty or changing workload set would hide
        // missing units by averaging only whatever happened to be present.
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
    if verdict.not_proven {
        writeln!(report, "## Result")?;
        writeln!(report)?;
        writeln!(
            report,
            "**NOT PROVEN** — workload/evidence sets are not comparable."
        )?;
        for reason in &verdict.reasons {
            writeln!(report, "- {reason}")?;
        }
    } else if verdict.regression {
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
            "**AUXILIARY PASS** — no parity or timing regression detected; this watch report is not an authoritative goal/checker gate."
        )?;
    }
    writeln!(report)?;
    writeln!(
        report,
        "Limit: watch validates nonempty terminal-success comparison runs and complete evidence, but does not independently prove source/ref, checkout, required-check, provider, runner, host, or trust identity."
    )?;
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
    if executed(github) && executed(velnor) {
        if gh_expandable == Some(true) && vl_expandable == Some(false) {
            worse.push("not expandable".to_string());
        } else if gh_expandable == Some(true) && vl_expandable.is_none() {
            worse.push("missing HTML check-step evidence".to_string());
        }
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
                    "| —/{} | — | {} | — | {} | — | {} | — | {} | ok (Velnor-only informational step) |",
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

    fn successful_summary() -> RunSummary {
        RunSummary {
            id: 42,
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            event: "pull_request".to_owned(),
            head_sha: "a".repeat(40),
            run_attempt: Some(1),
        }
    }

    #[test]
    fn recent_run_query_limits_successful_runs() {
        let args = recent_run_args("tailrocks/velnor", "compat.yml", 1);
        assert_eq!(
            args,
            vec![
                "run".to_owned(),
                "list".to_owned(),
                "--repo".to_owned(),
                "tailrocks/velnor".to_owned(),
                "--workflow".to_owned(),
                "compat.yml".to_owned(),
                "--status".to_owned(),
                "success".to_owned(),
                "--limit".to_owned(),
                "2".to_owned(),
                "--json".to_owned(),
                "databaseId,status,conclusion".to_owned(),
            ]
        );
    }

    #[test]
    fn watch_keeps_only_complete_both_lane_censuses() {
        let (jobs, _, _) = valid_pair_fixture();
        assert!(has_complete_both_lane_census(&jobs));

        let github_only = jobs
            .iter()
            .filter(|job| job.id == 1)
            .cloned()
            .collect::<Vec<_>>();
        assert!(!has_complete_both_lane_census(&github_only));

        let mut skipped_counterpart = jobs;
        skipped_counterpart
            .iter_mut()
            .find(|job| job.id == 2)
            .unwrap()
            .status = "skipped".to_owned();
        assert!(!has_complete_both_lane_census(&skipped_counterpart));
    }

    #[test]
    fn private_job_html_fetch_sends_bearer_header() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request);
            assert!(
                request
                    .lines()
                    .any(|line| line == "Authorization: Bearer fixture-token"),
                "missing bearer header in request: {request}"
            );
            let body = b"<check-step data-number=\"1\" data-log-url=\"/log\"></check-step>";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });

        let steps =
            fetch_job_html_steps_with_token(42, &format!("http://{address}/job"), "fixture-token")
                .unwrap();
        server.join().unwrap();
        assert_eq!(steps.keys().copied().collect::<Vec<_>>(), vec![1]);
    }

    fn html_for_steps(numbers: &[u64]) -> BTreeMap<u64, HtmlStep> {
        numbers
            .iter()
            .map(|number| {
                (
                    *number,
                    HtmlStep {
                        number: *number,
                        expandable: true,
                        external_id: format!("external-{number}"),
                    },
                )
            })
            .collect()
    }

    fn pair_evidence(github: &Job, velnor: &Job) -> PairEvidence {
        PairEvidence {
            github_html: html_for_steps(
                &github
                    .steps
                    .iter()
                    .map(|step| step.number)
                    .collect::<Vec<_>>(),
            ),
            velnor_html: html_for_steps(
                &velnor
                    .steps
                    .iter()
                    .map(|step| step.number)
                    .collect::<Vec<_>>(),
            ),
            github_log: "github log\n".to_owned(),
            github_content: analyze_lane_log("github log\n"),
            velnor_content: analyze_lane_log("velnor log\n"),
        }
    }

    fn valid_pair_fixture() -> (Vec<Job>, PairingCensus, BTreeMap<(u64, u64), PairEvidence>) {
        let github = Job {
            id: 1,
            name: "Rust · rust-policy / GitHub".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            html_url: None,
            steps: vec![step(1, "Run check", "success")],
        };
        let velnor = Job {
            id: 2,
            name: "Rust · rust-policy / Velnor".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            html_url: None,
            steps: vec![step(1, "Run check", "success")],
        };
        let jobs = vec![github.clone(), velnor.clone()];
        let census = pair_lane_census(&jobs);
        let evidence = BTreeMap::from([((github.id, velnor.id), pair_evidence(&github, &velnor))]);
        (jobs, census, evidence)
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
    fn explicit_selector_still_requires_census_and_is_never_a_gate() {
        let (jobs, census, evidence) = valid_pair_fixture();
        let validated = assess_run_evidence(
            42,
            &jobs,
            census,
            successful_summary(),
            "velnor log\n".to_owned(),
            evidence,
        )
        .unwrap();
        let (pairs, scope) = select_pairs(&validated.census, Some((1, 2)), 42).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(scope, ComparisonScope::SubsetDiagnostic);
        assert_eq!(
            comparison_decision(scope, true, 0, 0),
            ComparisonDecision::DiagnosticOnly
        );
        assert!(select_pairs(&validated.census, Some((1, 999)), 42).is_err());
    }

    #[test]
    fn complete_validated_fixture_reaches_only_auxiliary_full_run_decision() {
        let (jobs, census, evidence) = valid_pair_fixture();
        let validated = assess_run_evidence(
            42,
            &jobs,
            census,
            successful_summary(),
            "velnor log\n".to_owned(),
            evidence,
        )
        .unwrap();
        let (pairs, scope) = select_pairs(&validated.census, None, 42).unwrap();
        let mut worse = 0;
        for (github, velnor) in pairs {
            let pair = validated
                .pair_evidence
                .get(&(github.id, velnor.id))
                .unwrap();
            worse += compare_pair(
                &github,
                &velnor,
                &pair.github_html,
                &pair.velnor_html,
                pair.github_content,
                pair.velnor_content,
            )
            .unwrap()
            .1;
        }
        assert_eq!(worse, 0);
        assert_eq!(
            comparison_decision(scope, true, worse, 0),
            ComparisonDecision::AuxiliaryPass
        );
        assert_eq!(
            comparison_decision(ComparisonScope::FullRun, false, 0, 0),
            ComparisonDecision::DiagnosticOnly
        );
    }

    #[test]
    fn empty_control_only_census_is_not_a_comparison() {
        let jobs = vec![named_job(1, "Control / Planning")];
        let census = pair_lane_census(&jobs);
        assert!(assess_run_evidence(
            42,
            &jobs,
            census,
            successful_summary(),
            "velnor log\n".to_owned(),
            BTreeMap::new(),
        )
        .is_err());
    }

    #[test]
    fn orphan_census_is_not_usable_for_watch() {
        let jobs = vec![named_job(1, "Rust · rust-policy / GitHub")];
        let census = pair_lane_census(&jobs);
        assert!(assess_run_evidence(
            42,
            &jobs,
            census,
            successful_summary(),
            "velnor log\n".to_owned(),
            BTreeMap::new(),
        )
        .is_err());
    }

    #[test]
    fn completed_failed_cancelled_and_timed_out_jobs_fail_evidence() {
        for (status, conclusion) in [
            ("completed", Some("failure")),
            ("completed", Some("cancelled")),
            ("completed", Some("timed_out")),
            ("completed", Some("skipped")),
            ("cancelled", None),
            ("skipped", None),
            ("in_progress", None),
        ] {
            let (mut jobs, _, evidence) = valid_pair_fixture();
            let velnor = jobs.iter_mut().find(|job| job.id == 2).unwrap();
            velnor.status = status.to_owned();
            velnor.conclusion = conclusion.map(str::to_owned);
            let census = pair_lane_census(&jobs);
            let error = assess_run_evidence(
                42,
                &jobs,
                census,
                successful_summary(),
                "velnor log\n".to_owned(),
                evidence,
            )
            .expect_err("non-success job cannot be accepted");
            assert!(!error.to_string().is_empty(), "{error:#}");
        }
    }

    #[test]
    fn completed_failed_run_fails_evidence() {
        let (jobs, census, evidence) = valid_pair_fixture();
        let mut summary = successful_summary();
        summary.conclusion = Some("failure".to_owned());
        let error = assess_run_evidence(
            42,
            &jobs,
            census,
            summary,
            "velnor log\n".to_owned(),
            evidence,
        )
        .expect_err("failed run cannot be accepted");
        assert!(error.to_string().contains("not success"), "{error:#}");
    }

    #[test]
    fn missing_html_or_log_payload_cannot_become_empty_green_evidence() {
        let (jobs, census, mut evidence) = valid_pair_fixture();
        evidence.get_mut(&(1, 2)).unwrap().github_log.clear();
        assert!(assess_run_evidence(
            42,
            &jobs,
            census.clone(),
            successful_summary(),
            "velnor log\n".to_owned(),
            evidence,
        )
        .is_err());

        let (jobs, census, mut evidence) = valid_pair_fixture();
        evidence.get_mut(&(1, 2)).unwrap().github_html.clear();
        assert!(assess_run_evidence(
            42,
            &jobs,
            census,
            successful_summary(),
            "velnor log\n".to_owned(),
            evidence,
        )
        .is_err());
    }

    #[test]
    fn partial_nonempty_html_cannot_pass_executed_step_coverage() {
        let (mut jobs, _, evidence) = valid_pair_fixture();
        for job in &mut jobs {
            job.steps.push(step(2, "Run security", "success"));
        }
        let census = pair_lane_census(&jobs);
        let error = assess_run_evidence(
            42,
            &jobs,
            census,
            successful_summary(),
            "velnor log\n".to_owned(),
            evidence,
        )
        .expect_err("nonempty but partial HTML must not pass");
        assert!(
            error.to_string().contains("missing executed step(s)"),
            "{error:#}"
        );
    }

    #[test]
    fn velnor_only_step_is_informational_for_equal_or_better_comparison() {
        let github = Job {
            id: 1,
            name: "Rust · rust-policy / GitHub".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            html_url: None,
            steps: vec![step(1, "Run check", "success")],
        };
        let velnor = Job {
            id: 2,
            name: "Rust · rust-policy / Velnor".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            html_url: None,
            steps: vec![
                step(1, "Run check", "success"),
                step(2, "Unexpected adapter step", "success"),
            ],
        };
        let html = BTreeMap::from([(
            1,
            HtmlStep {
                number: 1,
                expandable: true,
                external_id: String::new(),
            },
        )]);
        let velnor_html = BTreeMap::from([
            (
                1,
                HtmlStep {
                    number: 1,
                    expandable: true,
                    external_id: String::new(),
                },
            ),
            (
                2,
                HtmlStep {
                    number: 2,
                    expandable: true,
                    external_id: String::new(),
                },
            ),
        ]);
        let (section, worse) = compare_pair(
            &github,
            &velnor,
            &html,
            &velnor_html,
            analyze_lane_log("github log\n"),
            analyze_lane_log("velnor log\n"),
        )
        .unwrap();
        assert_eq!(worse, 0);
        assert!(section.contains("Velnor-only informational step"));
        assert_eq!(
            comparison_decision(ComparisonScope::FullRun, true, worse, 0),
            ComparisonDecision::AuxiliaryPass
        );
    }

    #[test]
    fn artifact_page_two_is_required_and_pagination_is_complete() {
        let first_page = ArtifactsResponse {
            total_count: 101,
            artifacts: (0..100)
                .map(|id| ArtifactRef {
                    id,
                    name: format!("unrelated-{id}"),
                })
                .collect(),
        };
        let second_page = ArtifactsResponse {
            total_count: 101,
            artifacts: vec![ArtifactRef {
                id: 101,
                name: "job-log-9001".to_owned(),
            }],
        };
        let collected =
            collect_job_log_artifacts(vec![Ok(first_page), Ok(second_page)], &[9001]).unwrap();
        assert_eq!(collected["job-log-9001"].id, 101);
    }

    #[test]
    fn artifact_census_rejects_truncation_duplicate_absence_and_api_error() {
        let truncated = ArtifactsResponse {
            total_count: 101,
            artifacts: (0..100)
                .map(|id| ArtifactRef {
                    id,
                    name: format!("unrelated-{id}"),
                })
                .collect(),
        };
        assert!(collect_job_log_artifacts(vec![Ok(truncated)], &[9001]).is_err());

        let duplicate = ArtifactsResponse {
            total_count: 2,
            artifacts: vec![
                ArtifactRef {
                    id: 1,
                    name: "job-log-9001".to_owned(),
                },
                ArtifactRef {
                    id: 2,
                    name: "job-log-9001".to_owned(),
                },
            ],
        };
        assert!(collect_job_log_artifacts(vec![Ok(duplicate)], &[9001]).is_err());

        let absent = ArtifactsResponse {
            total_count: 1,
            artifacts: vec![ArtifactRef {
                id: 1,
                name: "job-log-9002".to_owned(),
            }],
        };
        assert!(collect_job_log_artifacts(vec![Ok(absent)], &[9001]).is_err());
        let api_error: Vec<Result<ArtifactsResponse>> = vec![Err(anyhow::anyhow!("rate limit"))];
        assert!(collect_job_log_artifacts(api_error, &[9001]).is_err());
    }

    #[test]
    fn empty_log_evidence_is_not_a_green_comparison() {
        assert!(require_nonempty_evidence("fixture log", " \n").is_err());
        assert!(require_nonempty_evidence("fixture log", "actual\n").is_ok());
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
        let steps = parse_check_steps(html).unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps[&1].expandable);
        assert_eq!(
            steps[&1].external_id,
            "e7cf94ab-32aa-4712-be84-b4521dcbad16"
        );
        assert!(!steps[&3].expandable);
    }

    #[test]
    fn parse_check_steps_rejects_invalid_or_duplicate_numbers() {
        assert!(parse_check_steps(
            r#"<check-steps><check-step data-number="nope"></check-step></check-steps>"#
        )
        .is_err());
        assert!(parse_check_steps(
            r#"<check-steps><check-step data-number="0"></check-step></check-steps>"#
        )
        .is_err());
        assert!(parse_check_steps(
            r#"<check-steps><check-step data-number="1"></check-step><check-step data-number="1"></check-step></check-steps>"#
        )
        .is_err());
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
    fn velnor_log_stats_stay_scoped_to_the_paired_job() {
        let logs = BTreeMap::from([
            (
                101,
                "2026-06-11T07:34:33.1187693Z ##[group]job one\n".to_owned(),
            ),
            (202, "plain job two\n".to_owned()),
        ]);
        let job_one = velnor_job_log_stats(&logs, 101, 42).unwrap();
        let job_two = velnor_job_log_stats(&logs, 202, 42).unwrap();
        assert_eq!(job_one.lines, 1);
        assert_eq!(job_one.timestamped_lines, 1);
        assert_eq!(job_one.group_markers, 1);
        assert_eq!(job_two.lines, 1);
        assert_eq!(job_two.timestamped_lines, 0);
        assert_eq!(job_two.group_markers, 0);
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

        // Missing Velnor HTML is evidence loss, not an empty/green cell.
        let (verdict, worse) = step_verdict(
            &step(2, "Run actions/checkout@v6", "success"),
            &step(2, "Run actions/checkout@v6", "success"),
            &html_with(2, true),
            &BTreeMap::new(),
        );
        assert!(worse, "{verdict}");
        assert!(verdict.contains("missing HTML check-step evidence"));

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
        assert!(verdict.not_proven, "{:?}", verdict.reasons);
    }

    #[test]
    fn is_regression_rejects_current_baseline_workload_set_drift() {
        let mut baseline = lane_stats(100.0, 100.0, 0);
        baseline.jobs.insert(
            "compat (app-b".to_owned(),
            JobClassStats {
                github_seconds: 80.0,
                velnor_seconds: 80.0,
            },
        );
        let current = lane_stats(100.0, 100.0, 0);

        let verdict = is_regression(&baseline, &current, 0.0);

        assert!(!verdict.regression, "{:?}", verdict.reasons);
        assert!(verdict.not_proven, "{:?}", verdict.reasons);
        assert!(
            verdict.reasons[0].contains("workload set drift"),
            "{:?}",
            verdict.reasons
        );
    }

    #[test]
    fn baseline_from_samples_rejects_samples_without_usable_timing() {
        let samples = vec![LaneStats::default(), LaneStats::default()];

        assert!(baseline_from_samples(&samples).is_none());
    }

    #[test]
    fn baseline_from_samples_rejects_partial_timing_coverage() {
        let samples = vec![lane_stats(100.0, 110.0, 0), LaneStats::default()];

        assert!(baseline_from_samples(&samples).is_none());
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

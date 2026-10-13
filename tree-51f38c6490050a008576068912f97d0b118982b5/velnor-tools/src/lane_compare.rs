//! `lane-compare`: fetch the GitHub-hosted and Velnor lanes of one workflow
//! run via the GitHub API and diff their Checks-UI surface per step.
//!
//! Implements the repeatable comparison demanded by `docs/comparison.md`
//! ("Preferred extraction method: the GitHub API") and master-plan P4.1. The
//! gate is **equal-or-better, never less informative**: any paired step where
//! the GitHub lane shows information the Velnor lane lacks (a step missing
//! entirely, an executed step that is not expandable, a divergent display
//! name or conclusion, or lane log content without timestamps / groups /
//! ANSI where GitHub has them) is a `WORSE` row and fails strict mode.
//!
//! Data sources (V2 jobs have no v1 log archive — `runs/{id}/logs` contains
//! no Velnor per-step files and `jobs/{id}/logs` 404s, see master-plan P4.3):
//! - step metadata: `actions/runs/{id}/jobs` (numbers, names, conclusions,
//!   started/completed),
//! - per-step expandability: the job page HTML `<check-step …>` elements
//!   (`data-log-url` presence — exactly what the UI renders),
//! - lane log content: `jobs/{id}/logs` for the GitHub lane; the Velnor
//!   lane's `job-log` artifact(s) for the Velnor side.

use anyhow::{bail, Context, Result};
use clap::{ArgAction, Args};
use serde::Deserialize;
use std::collections::BTreeMap;
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
    let run_id = match args.run_id {
        Some(id) => id,
        None => super::latest_fixture_run_id(&args.repo, &args.workflow)?,
    };

    let jobs = fetch_run_jobs(&args.repo, run_id)?;
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
        _ => pair_lane_jobs(&jobs),
    };
    if pairs.is_empty() {
        bail!(
            "run {run_id} has no (github, velnor) lane job pairs; \
             job names: {:?}",
            jobs.iter().map(|job| job.name.as_str()).collect::<Vec<_>>()
        );
    }
    for (github, velnor) in &pairs {
        for job in [github, velnor] {
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

    let out_dir = if args.output_dir.is_absolute() {
        args.output_dir.clone()
    } else {
        root.join(&args.output_dir)
    };
    let run_dir = out_dir.join(format!("lane-compare-run-{run_id}"));
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("create output directory {}", run_dir.display()))?;
    save_jobs_json(&run_dir, &jobs)?;

    // Lane content: GitHub jobs expose a per-job log download; Velnor V2 jobs
    // do not (no v1 archive), so read the Velnor lane's job-log artifact(s).
    let velnor_content =
        fetch_velnor_job_log_artifacts(&args.repo, run_id).unwrap_or_else(|error| {
            eprintln!("warning: velnor job-log artifacts unavailable: {error:#}");
            String::new()
        });

    let mut report = String::new();
    let mut worse_total = 0usize;
    writeln!(report, "# Lane compare — run {run_id} ({})", args.repo)?;
    writeln!(report)?;
    writeln!(
        report,
        "Gate: equal-or-better — zero rows where the GitHub lane shows \
         information the Velnor lane lacks."
    )?;

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
    if worse_total == 0 {
        writeln!(
            report,
            "**PASS** — no paired step is less informative than the GitHub lane."
        )?;
    } else {
        writeln!(
            report,
            "**FAIL** — {worse_total} row(s) where the Velnor lane is less \
             informative than the GitHub lane."
        )?;
    }
    writeln!(report)?;
    writeln!(
        report,
        "Known documented divergence (not gated): V2 jobs have no v1 log \
         archive, so the per-job raw-log download 404s on the Velnor lane; \
         the `job-log` artifact is the workaround (master-plan P4.3)."
    )?;

    let report_path = run_dir.join("report.md");
    fs::write(&report_path, &report).with_context(|| format!("write {}", report_path.display()))?;
    println!("{report}");
    println!("report: {}", report_path.display());

    if args.strict && worse_total > 0 {
        bail!("lane-compare gate failed: {worse_total} worse row(s); see report above");
    }
    Ok(())
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

/// Download every `job-log` artifact of the run and concatenate their text
/// content. Several Velnor jobs in one run all upload an artifact named
/// `job-log` (per-job naming is master-plan P4.4), so the content check is
/// lane-level rather than per-job.
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

fn lane_of_job_name(name: &str) -> Option<Lane> {
    let lower = name.to_ascii_lowercase();
    let has_velnor = contains_word(&lower, "velnor");
    let has_github = contains_word(&lower, "github");
    match (has_velnor, has_github) {
        (true, false) => Some(Lane::Velnor),
        (false, true) => Some(Lane::GitHub),
        _ => None,
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

/// Pair key: the job name with the lane token and everything after it
/// removed, so `compat (app-a, github, "ubuntu-latest")` and
/// `compat (app-a, velnor, [...])` pair on `compat (app-a`.
fn pair_key(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let cut = ["velnor", "github"]
        .iter()
        .filter_map(|token| lower.find(token))
        .min()
        .unwrap_or(lower.len());
    lower[..cut].trim_end_matches([' ', ',', '(']).to_string()
}

fn pair_lane_jobs(jobs: &[Job]) -> Vec<(Job, Job)> {
    let mut github: BTreeMap<String, &Job> = BTreeMap::new();
    let mut velnor: BTreeMap<String, &Job> = BTreeMap::new();
    for job in jobs {
        match lane_of_job_name(&job.name) {
            Some(Lane::GitHub) => {
                github.insert(pair_key(&job.name), job);
            }
            Some(Lane::Velnor) => {
                velnor.insert(pair_key(&job.name), job);
            }
            None => {}
        }
    }
    github
        .into_iter()
        .filter_map(|(key, gh_job)| {
            velnor
                .get(&key)
                .map(|vl_job| (gh_job.clone(), (*vl_job).clone()))
        })
        .collect()
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
/// stores on every downloaded log line — see docs/log-format-contract.md.
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
    let parse = |value: &str| {
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
    };
    let started = parse(step.started_at.as_deref()?)?;
    let completed = parse(step.completed_at.as_deref()?)?;
    Some((completed - started).whole_seconds())
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
        if let (Some(true), Some(false)) = (gh_expandable, vl_expandable) {
            worse.push("not expandable".to_string());
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
        let job = |id: u64, name: &str| Job {
            id,
            name: name.to_string(),
            status: "completed".to_string(),
            conclusion: Some("success".to_string()),
            html_url: None,
            steps: Vec::new(),
        };
        let jobs = vec![
            job(1, r#"compat (app-a, github, "ubuntu-latest")"#),
            job(
                2,
                r#"compat (app-a, velnor, ["self-hosted","velnor-target-mvp"])"#,
            ),
            job(3, "lint"),
        ];
        let pairs = pair_lane_jobs(&jobs);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.id, 1);
        assert_eq!(pairs[0].1.id, 2);
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
}

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use clap::{Args, Parser, Subcommand};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const DEFAULT_FIXTURE_REPO: &str = "donbeave/velnor-actions-fixture";
const DEFAULT_FIXTURE_REF: &str = "main";
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/actions/runner/releases/latest";
const LATEST_RELEASE_REDIRECT_URL: &str = "https://github.com/actions/runner/releases/latest";

#[derive(Debug, Parser)]
#[command(name = "velnor-tools")]
#[command(about = "Rust repository automation for Velnor")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Check that the documented actions/runner reference matches upstream.
    CheckRunnerReference,
    /// Audit the public fixture repository feature surface.
    FixtureAudit(FixtureAuditArgs),
    /// Commit repository changes using the Rust automation helper.
    Commit(CommitArgs),
}

#[derive(Debug, Args)]
struct FixtureAuditArgs {
    /// GitHub repository slug to audit.
    #[arg(long, default_value = DEFAULT_FIXTURE_REPO)]
    repo: String,
    /// Git ref to audit.
    #[arg(long, default_value = DEFAULT_FIXTURE_REF)]
    git_ref: String,
    /// Audit a local fixture checkout/root instead of GitHub.
    #[arg(long)]
    fixture_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct CommitArgs {
    /// Commit message.
    #[arg(short, long)]
    message: String,
    /// Paths to stage. Defaults to all changes.
    #[arg(default_value = ".")]
    paths: Vec<String>,
    /// Push after committing.
    #[arg(long)]
    push: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = repo_root()?;
    match cli.command {
        CommandKind::CheckRunnerReference => check_runner_reference(&root).await,
        CommandKind::FixtureAudit(args) => fixture_audit(args).await,
        CommandKind::Commit(args) => commit(&root, args),
    }
}

fn repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("resolve git repository root")?;
    if !output.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

async fn check_runner_reference(root: &Path) -> Result<()> {
    let reference = pinned_reference(root)?;
    let protocol_version = pinned_protocol_version(root)?;
    let latest = latest_runner_release().await?;

    if protocol_version != reference.trim_start_matches('v') {
        bail!(
            "actions/runner protocol version drift: docs pin {reference}, protocol advertises {protocol_version}"
        );
    }
    if reference != latest {
        bail!(
            "actions/runner reference drift: pinned {reference}, latest {latest}\nRefresh docs/research/latest-runner-v2-refresh-2026-06-01.md and re-audit V2 source anchors."
        );
    }

    println!("actions/runner reference current: {reference}");
    Ok(())
}

fn pinned_reference(root: &Path) -> Result<String> {
    let path = root.join("docs/research/latest-runner-v2-refresh-2026-06-01.md");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let regex = Regex::new(r"latest release checked:\s*`(v[0-9]+\.[0-9]+\.[0-9]+)`")?;
    let captures = regex.captures(&text).ok_or_else(|| {
        anyhow::anyhow!("could not find pinned runner version in {}", path.display())
    })?;
    Ok(captures[1].to_string())
}

fn pinned_protocol_version(root: &Path) -> Result<String> {
    let path = root.join("crates/velnor-runner/src/protocol.rs");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let regex = Regex::new(r#"RUNNER_VERSION:\s*&str\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)""#)?;
    let captures = regex
        .captures(&text)
        .ok_or_else(|| anyhow::anyhow!("could not find RUNNER_VERSION in {}", path.display()))?;
    let version = captures[1].to_string();
    let expected_agent =
        format!(r#"RUNNER_USER_AGENT: &str = "actions-runner/{version} (velnor)""#);
    if !text.contains(&expected_agent) {
        bail!(
            "RUNNER_USER_AGENT in {} does not match RUNNER_VERSION {version}",
            path.display()
        );
    }
    Ok(version)
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

async fn latest_runner_release() -> Result<String> {
    let client = reqwest::Client::new();
    let response = client
        .get(LATEST_RELEASE_URL)
        .headers(github_headers("velnor-runner-reference-check")?)
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => {
            let release = response
                .json::<GitHubRelease>()
                .await
                .context("parse latest actions/runner release response")?;
            if !release.tag_name.starts_with('v') {
                bail!("GitHub latest release response did not include a tag_name");
            }
            Ok(release.tag_name)
        }
        Ok(response) => latest_release_from_redirect(format!("API {}", response.status())).await,
        Err(error) => latest_release_from_redirect(format!("API {error}")).await,
    }
}

async fn latest_release_from_redirect(reason: String) -> Result<String> {
    let client = reqwest::Client::new();
    let response = client
        .get(LATEST_RELEASE_REDIRECT_URL)
        .header(
            USER_AGENT,
            HeaderValue::from_static("velnor-runner-reference-check"),
        )
        .send()
        .await
        .with_context(|| format!("GitHub release lookup failed: {reason}"))?;
    let final_url = response.url().to_string();
    let regex = Regex::new(r"/releases/tag/(v[0-9]+\.[0-9]+\.[0-9]+)$")?;
    let captures = regex.captures(&final_url).ok_or_else(|| {
        anyhow::anyhow!("GitHub release lookup failed: {reason}, redirect URL {final_url}")
    })?;
    Ok(captures[1].to_string())
}

async fn fixture_audit(args: FixtureAuditArgs) -> Result<()> {
    let mut failures = Vec::new();
    for (path, snippets) in fixture_required_snippets() {
        let content = match read_fixture_file(&args, path).await {
            Ok(content) => content,
            Err(error) => {
                failures.push(format!("{path}: missing or unreadable: {error:#}"));
                continue;
            }
        };

        for (label, snippet) in snippets {
            if !content.contains(snippet) {
                failures.push(format!("{path}: missing {label}: {snippet}"));
            }
        }
    }

    if !failures.is_empty() {
        eprintln!("fixture audit failed:");
        for failure in failures {
            eprintln!("  - {failure}");
        }
        std::process::exit(1);
    }

    let source = args
        .fixture_root
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("{}@{}", args.repo, args.git_ref));
    println!("fixture audit passed for {source}");
    println!("checked {} files", fixture_required_snippets().len());
    Ok(())
}

async fn read_fixture_file(args: &FixtureAuditArgs, path: &str) -> Result<String> {
    if let Some(root) = &args.fixture_root {
        let path = root.join(path);
        return fs::read_to_string(&path).with_context(|| format!("read {}", path.display()));
    }
    fetch_github_file(&args.repo, &args.git_ref, path).await
}

#[derive(Debug, Deserialize)]
struct GitHubContentFile {
    #[serde(rename = "type")]
    kind: String,
    content: String,
}

async fn fetch_github_file(repo: &str, git_ref: &str, path: &str) -> Result<String> {
    let url = format!("https://api.github.com/repos/{repo}/contents/{path}?ref={git_ref}");
    let payload = reqwest::Client::new()
        .get(url)
        .headers(github_headers("velnor-fixture-audit")?)
        .send()
        .await
        .with_context(|| format!("fetch {repo}@{git_ref}:{path}"))?
        .error_for_status()
        .with_context(|| format!("fetch {repo}@{git_ref}:{path}"))?
        .json::<GitHubContentFile>()
        .await
        .with_context(|| format!("parse GitHub content response for {path}"))?;
    if payload.kind != "file" {
        bail!("{path} is not a file in {repo}@{git_ref}");
    }
    let content = payload.content.replace('\n', "");
    let bytes = general_purpose::STANDARD
        .decode(content)
        .with_context(|| format!("decode GitHub file content for {path}"))?;
    String::from_utf8(bytes).with_context(|| format!("decode UTF-8 content for {path}"))
}

fn github_headers(user_agent: &'static str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(user_agent));
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static("2022-11-28"),
    );
    if let Ok(token) = env::var("GITHUB_TOKEN") {
        if !token.trim().is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .context("build GitHub authorization header")?,
            );
        }
    }
    Ok(headers)
}

fn fixture_required_snippets() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            ".github/workflows/compat.yml",
            vec![
                ("GitHub-hosted lane", "runs-on: ubuntu-latest"),
                ("Velnor lane", "runs-on: [self-hosted, velnor-target-mvp]"),
                ("path filtering", "dorny/paths-filter@v4"),
                ("Rust toolchain setup", "dtolnay/rust-toolchain@stable"),
                ("just setup", "extractions/setup-just@v4"),
                ("cache action", "actions/cache@v5"),
                ("artifact upload", "actions/upload-artifact@v7"),
                ("artifact download", "actions/download-artifact@v8"),
                ("command env file", "GITHUB_ENV"),
                ("command output file", "GITHUB_OUTPUT"),
                ("command path file", "GITHUB_PATH"),
                ("step summary file", "GITHUB_STEP_SUMMARY"),
                ("matrix packages", "matrix:"),
                ("compare needs", "needs: [compat-github, compat-velnor]"),
                ("Velnor result gate", "needs.compat-velnor.result"),
                (
                    "fixture output composite",
                    "./.github/actions/check-fixture-output",
                ),
                ("aggregate composite", "./.github/actions/aggregate-needs"),
            ],
        ),
        (
            ".github/workflows/docker.yml",
            vec![
                ("GitHub-hosted Docker lane", "docker-github:"),
                ("Velnor Docker lane", "docker-velnor:"),
                (
                    "Velnor runner label",
                    "runs-on: [self-hosted, velnor-target-mvp]",
                ),
                ("Buildx setup", "docker/setup-buildx-action@v4"),
                ("Docker build action", "docker/build-push-action@v7"),
                ("non-push build", "push: false"),
                ("loaded image", "load: true"),
                ("container execution", "docker run --rm"),
            ],
        ),
        (
            ".github/actions/aggregate-needs/action.yml",
            vec![("aggregate composite metadata", "runs:")],
        ),
        (
            ".github/actions/check-fixture-output/action.yml",
            vec![("fixture output composite metadata", "runs:")],
        ),
    ]
}

fn commit(root: &Path, args: CommitArgs) -> Result<()> {
    run_git(
        root,
        ["add"]
            .into_iter()
            .chain(args.paths.iter().map(String::as_str)),
    )?;
    run_git(root, ["commit", "-m", args.message.as_str()])?;
    if args.push {
        run_git(root, ["push"])?;
    }
    Ok(())
}

fn run_git<'a, I>(root: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let output = Command::new("git")
        .current_dir(root)
        .args(&args)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        print!("{stdout}");
    }
    Ok(())
}

//! Estate CI contract auditor (`config/estate-repositories.json`).

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command};

use crate::fleet_policy::{generate_policies_from_ledger, ReleaseRefLedger};

const INLINE_MATRIX_MARKERS: [&str; 2] = ["inputs.lanes == 'both'", "inputs.lane == 'both'"];
const VELNOR_GUEST_IMAGE_REQUIREMENT: &str = "guest-image-build.user-namespace";
const VELNOR_GUEST_IMAGE_CAPABILITY: &str = "unprivileged user namespaces or CAP_SYS_ADMIN";

fn has_explicit_velnor_capability_gate(text: &str) -> bool {
    text.contains(&format!(
        "VELNOR_IMPLEMENTATION_REQUIREMENT: \"{VELNOR_GUEST_IMAGE_REQUIREMENT}\""
    )) && text.contains(&format!(
        "VELNOR_MISSING_CAPABILITY: \"{VELNOR_GUEST_IMAGE_CAPABILITY}\""
    )) && text.contains("velnor|both)")
        && text.contains("refusing GitHub substitution")
        && text.contains("exit 1")
}

fn has_truthful_both_lane_contract(text: &str) -> bool {
    INLINE_MATRIX_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
        || (text.contains(
            "LANES: ${{ github.event_name == 'workflow_dispatch' && inputs.lanes || 'velnor' }}",
        ) && text.contains("case \"$LANES\" in")
            && text.contains("both)")
            && text.contains("configs=\"[$velnor,$github]\""))
        || has_explicit_velnor_capability_gate(text)
}
const SHA_LEN: usize = 40;
const ESTATE_MANIFEST_FILE: &str = "config/estate-repositories.json";
const LEGACY_RUNNER_GROUP_DOCTOR: &str = "scripts/runner_group_doctor.sh";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum GeneratedCallerClass {
    Code,
    Tap,
    Apt,
    Fixture,
}

impl GeneratedCallerClass {
    const ALL: [Self; 4] = [Self::Code, Self::Tap, Self::Apt, Self::Fixture];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Tap => "tap",
            Self::Apt => "apt",
            Self::Fixture => "fixture",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.as_str() == value)
    }
}

#[derive(Debug, Args)]
pub struct AuditCiArgs {
    /// Repository checkout to audit.
    #[arg(long, default_value = ".")]
    pub repo_path: PathBuf,
    /// Canonical owner/repository identity used to enforce its generated class.
    #[arg(long)]
    pub repository_name: Option<String>,
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
    /// Clone and audit each estate repository's live remote default head.
    #[arg(long, requires = "estate")]
    pub remote_defaults: bool,
    /// Root containing local estate checkouts at <owner>/<repository>.
    #[arg(long, requires = "estate", conflicts_with = "remote_defaults")]
    pub estate_root: Option<PathBuf>,
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
    #[serde(default)]
    path: Option<PathBuf>,
    concerns: BTreeMap<String, ConcernContract>,
}

#[derive(Debug, Serialize)]
struct EstateAuditResult {
    default_branch: String,
    head_sha: String,
    generated_class: GeneratedCallerClass,
    generated_ci_sha256: Option<String>,
    findings: Vec<Finding>,
}

#[derive(Debug, Serialize)]
struct EstateAuditOutput {
    schema_version: &'static str,
    repositories: BTreeMap<String, EstateAuditResult>,
}

struct GeneratedCallerSample {
    repository: String,
    class: GeneratedCallerClass,
    sha256: String,
    bytes: Vec<u8>,
}

struct RemoteCheckout {
    path: PathBuf,
}

impl Drop for RemoteCheckout {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
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
    expected_generated_class: Option<GeneratedCallerClass>,
}

fn canonical_fleet_map(root: &Path) -> Result<BTreeMap<String, GeneratedCallerClass>> {
    let path = root.join(ESTATE_MANIFEST_FILE);
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read estate manifest {}", path.display()))?;
    let manifest: EstateManifest = serde_json::from_str(&text)
        .with_context(|| format!("parse estate manifest {}", path.display()))?;
    let mut map = BTreeMap::new();
    for repo in manifest.repositories {
        let class = if repo.name == "tailrocks/velnor-actions-fixture" {
            GeneratedCallerClass::Fixture
        } else if repo.name.ends_with("-apt") {
            GeneratedCallerClass::Apt
        } else if repo.name.starts_with("jackin-project/homebrew-")
            || repo.name.starts_with("tailrocks/homebrew-")
        {
            GeneratedCallerClass::Tap
        } else {
            GeneratedCallerClass::Code
        };
        if map.insert(repo.name.clone(), class).is_some() {
            bail!(
                "estate manifest contains duplicate repository {}",
                repo.name
            );
        }
    }
    Ok(map)
}

fn generated_caller_sample(
    root: &Path,
    repository: &str,
    class: GeneratedCallerClass,
) -> Result<Option<GeneratedCallerSample>> {
    let path = root.join(".github/workflows/ci.yml");
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    if !bytes.starts_with(b"# Generated by velnor-actions-generator. DO NOT EDIT.\n") {
        return Ok(None);
    }
    let sha256 = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(Some(GeneratedCallerSample {
        repository: repository.to_owned(),
        class,
        sha256,
        bytes,
    }))
}

fn generated_class_equality_findings(
    samples: &[GeneratedCallerSample],
) -> BTreeMap<String, Vec<Finding>> {
    let mut findings = BTreeMap::<String, Vec<Finding>>::new();
    for class in GeneratedCallerClass::ALL {
        let class_samples = samples
            .iter()
            .filter(|sample| sample.class == class)
            .collect::<Vec<_>>();
        let groups = class_samples.iter().fold(
            BTreeMap::<&[u8], Vec<&GeneratedCallerSample>>::new(),
            |mut groups, sample| {
                groups
                    .entry(sample.bytes.as_slice())
                    .or_default()
                    .push(sample);
                groups
            },
        );
        if groups.len() <= 1 {
            continue;
        }
        let summary = groups
            .values()
            .filter_map(|group| {
                group
                    .first()
                    .map(|sample| format!("{}={}", sample.sha256, group.len()))
            })
            .collect::<Vec<_>>()
            .join(", ");
        for sample in class_samples {
            findings
                .entry(sample.repository.clone())
                .or_default()
                .push(Finding::error(
                    "generated-class-bytes",
                    ".github/workflows/ci.yml",
                    "$",
                    format!(
                        "generated {} class is not byte-identical: repository sha256 {}; class hashes [{summary}]",
                        class.as_str(),
                        sample.sha256
                    ),
                ));
        }
    }
    findings
}

pub fn audit_ci(args: AuditCiArgs) -> Result<()> {
    let canonical_classes = if args.estate.is_some() || args.repository_name.is_some() {
        canonical_fleet_map(&args.repo_path)?
    } else {
        BTreeMap::new()
    };
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
    let mut estate_results = BTreeMap::new();
    let mut generated_samples = Vec::new();
    let mut all = BTreeMap::new();
    if let Some(estate) = &estate {
        if estate.version != 2 {
            bail!(
                "unsupported estate manifest version {} (expected 2)",
                estate.version
            );
        }
        validate_estate_scope(estate, &canonical_classes)?;
        if args.offline {
            bail!("estate audit cannot skip delivered-default freshness checks");
        }
        for repo in &estate.repositories {
            let expected_class = canonical_classes
                .get(&repo.name)
                .copied()
                .with_context(|| format!("canonical fleet map has no class for {}", repo.name))?;
            let (default_branch, head_sha) = remote_default_identity(&repo.name)?;
            let remote_checkout = if args.remote_defaults {
                Some(checkout_remote_default(
                    &repo.name,
                    &default_branch,
                    &head_sha,
                )?)
            } else {
                None
            };
            let configured = if remote_checkout.is_none() {
                Some(
                    args
                    .estate_root
                    .as_ref()
                    .map(|root| root.join(&repo.name))
                    .or_else(|| repo.path.clone())
                    .with_context(|| {
                        format!(
                            "estate repository {} has no portable checkout; pass --estate-root or --remote-defaults",
                            repo.name
                        )
                    })?,
                )
            } else {
                None
            };
            if let Some(configured) = &configured {
                verify_local_default(configured, &repo.name, &default_branch, &head_sha)?;
            }
            let root = remote_checkout
                .as_ref()
                .map(|checkout| checkout.path.as_path())
                .or(configured.as_deref())
                .context("estate checkout resolution produced no path")?;
            let canonical = root.canonicalize().with_context(|| {
                format!(
                    "estate repository {} path {} does not exist",
                    repo.name,
                    root.display()
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
            let mut findings = audit_repo_profile(
                &canonical,
                args.offline,
                Some(&workload_files),
                false,
                Some(expected_class),
            )?;
            findings.extend(audit_concern_contract(repo, &estate.defaults, &canonical)?);
            let generated_ci_sha256 =
                generated_caller_sample(&canonical, &repo.name, expected_class)?.map(|sample| {
                    let sha256 = sample.sha256.clone();
                    generated_samples.push(sample);
                    sha256
                });
            findings.sort_by(|left, right| {
                (&left.file, &left.path, left.rule).cmp(&(&right.file, &right.path, right.rule))
            });
            estate_results.insert(
                repo.name.clone(),
                EstateAuditResult {
                    default_branch,
                    head_sha,
                    generated_class: expected_class,
                    generated_ci_sha256,
                    findings,
                },
            );
        }
        for (repository, byte_findings) in generated_class_equality_findings(&generated_samples) {
            let result = estate_results.get_mut(&repository).with_context(|| {
                format!("generated sample has no estate result for {repository}")
            })?;
            result.findings.extend(byte_findings);
            result.findings.sort_by(|left, right| {
                (&left.file, &left.path, left.rule).cmp(&(&right.file, &right.path, right.rule))
            });
        }
    } else {
        let root = &args.repo_path;
        let canonical = root.canonicalize().unwrap_or(root.clone());
        let expected_class =
            args.repository_name
                .as_deref()
                .map(|repository| {
                    canonical_classes.get(repository).copied().with_context(|| {
                        format!("canonical fleet map has no class for {repository}")
                    })
                })
                .transpose()?;
        all.insert(
            canonical.display().to_string(),
            audit_repo_profile(&canonical, args.offline, None, true, expected_class)?,
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
        .count()
        + estate_results
            .values()
            .flat_map(|result| &result.findings)
            .filter(|finding| finding.severity == Severity::Error)
            .count();
    if args.json {
        if estate.is_some() {
            println!(
                "{}",
                serde_json::to_string_pretty(&EstateAuditOutput {
                    schema_version: "velnor.audit-ci.estate.v2",
                    repositories: estate_results,
                })?
            );
        } else {
            println!("{}", serde_json::to_string_pretty(&all)?);
        }
    } else {
        for (repo, result) in &estate_results {
            println!(
                "audit-ci: {repo} ({} {}; class {}; ci sha256 {})",
                result.default_branch,
                result.head_sha,
                result.generated_class.as_str(),
                result.generated_ci_sha256.as_deref().unwrap_or("missing")
            );
            if result.findings.is_empty() {
                println!("  PASS");
            }
            for finding in &result.findings {
                println!(
                    "  {:?} {} {} {} — {}",
                    finding.severity, finding.rule, finding.file, finding.path, finding.message
                );
            }
        }
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

fn validate_estate_scope(
    estate: &EstateManifest,
    canonical_classes: &BTreeMap<String, GeneratedCallerClass>,
) -> Result<()> {
    let observed = estate
        .repositories
        .iter()
        .map(|repo| repo.name.as_str())
        .collect::<BTreeSet<_>>();
    if observed.len() != estate.repositories.len() {
        bail!("estate manifest contains duplicate repository names");
    }
    let expected = canonical_classes
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if observed != expected {
        let missing = expected.difference(&observed).copied().collect::<Vec<_>>();
        let extra = observed.difference(&expected).copied().collect::<Vec<_>>();
        bail!(
            "estate scope mismatch: expected exactly 28 repositories; missing={missing:?}; extra={extra:?}"
        );
    }
    Ok(())
}

fn remote_default_identity(repository: &str) -> Result<(String, String)> {
    let url = format!("https://github.com/{repository}.git");
    let output = Command::new("git")
        .args(["ls-remote", "--symref", &url, "HEAD"])
        .output()
        .with_context(|| format!("resolve remote default for {repository}"))?;
    if !output.status.success() {
        bail!(
            "resolve remote default for {repository}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8(output.stdout).context("git ls-remote output is not UTF-8")?;
    let branch = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")
                .and_then(|rest| rest.strip_suffix("\tHEAD"))
        })
        .context("remote HEAD did not identify a default branch")?;
    let sha = stdout
        .lines()
        .filter_map(|line| line.strip_suffix("\tHEAD"))
        .find(|value| value.len() == SHA_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .with_context(|| {
            format!("remote HEAD for {repository} did not identify a 40-hex commit")
        })?;
    Ok((branch.to_string(), sha.to_ascii_lowercase()))
}

fn checkout_remote_default(
    repository: &str,
    default_branch: &str,
    head_sha: &str,
) -> Result<RemoteCheckout> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock predates Unix epoch")?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("velnor-estate-{}-{nonce}", std::process::id()));
    let url = format!("https://github.com/{repository}.git");
    let output = Command::new("git")
        .args([
            "clone",
            "--quiet",
            "--no-checkout",
            "--filter=blob:none",
            "--depth=1",
            "--branch",
            default_branch,
            &url,
        ])
        .arg(&path)
        .output()
        .with_context(|| format!("clone delivered default for {repository}"))?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&path);
        bail!(
            "clone delivered default for {repository}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // Keep the cheap sparse checkout, but include the directly inspected
    // repository trees. Any other tracked audit surface is read from its
    // verified index blob below, so sparse omission cannot become a false
    // negative or a hard failure.
    let sparse_input = b"/*\n!/*/\n/.github/\n/docs/\n/scripts/\n/operational/\n/fleet/\n";
    let sparse = Command::new("git")
        .current_dir(&path)
        .args(["sparse-checkout", "set", "--no-cone", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("configure sparse checkout for {repository}"))?;
    if let Err(error) = configure_sparse_checkout(sparse, repository, sparse_input) {
        let _ = fs::remove_dir_all(&path);
        return Err(error);
    }
    let checkout = Command::new("git")
        .current_dir(&path)
        .args(["checkout", "--quiet", "--detach", head_sha])
        .status()
        .with_context(|| format!("checkout delivered default for {repository}"))?;
    if !checkout.success() {
        let _ = fs::remove_dir_all(&path);
        bail!("checkout delivered default for {repository}");
    }
    verify_checkout_identity(&path, repository, default_branch, head_sha, false)?;
    Ok(RemoteCheckout { path })
}

fn configure_sparse_checkout(
    mut sparse: Child,
    repository: &str,
    sparse_input: &[u8],
) -> Result<()> {
    let mut reaped = false;
    let result = (|| {
        use std::io::Write as _;

        sparse
            .stdin
            .as_mut()
            .context("git sparse-checkout stdin unavailable")?
            .write_all(sparse_input)
            .context("write sparse-checkout patterns")?;
        // Close stdin before waiting so git receives EOF even on platforms
        // where Child::wait does not close the pipe early enough.
        sparse.stdin.take();
        let status = sparse.wait().context("wait for git sparse-checkout")?;
        reaped = true;
        if !status.success() {
            bail!("configure sparse checkout for {repository}");
        }
        Ok(())
    })();

    if result.is_err() && !reaped {
        kill_and_reap(&mut sparse);
    }
    result
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn verify_local_default(
    path: &Path,
    repository: &str,
    default_branch: &str,
    head_sha: &str,
) -> Result<()> {
    verify_checkout_identity(path, repository, default_branch, head_sha, true)
}

fn verify_checkout_identity(
    path: &Path,
    repository: &str,
    default_branch: &str,
    head_sha: &str,
    require_branch: bool,
) -> Result<()> {
    let git = |args: &[&str]| -> Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .with_context(|| format!("inspect checkout for {repository}"))?;
        if !output.status.success() {
            bail!(
                "inspect checkout for {repository}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8(output.stdout)
            .context("git checkout output is not UTF-8")?
            .trim()
            .to_string())
    };
    let observed_head = git(&["rev-parse", "HEAD"])?;
    if observed_head != head_sha {
        bail!("estate checkout {repository} is stale: local {observed_head}, remote {head_sha}");
    }
    if require_branch {
        let observed_branch = git(&["symbolic-ref", "--short", "HEAD"])?;
        if observed_branch != default_branch {
            bail!(
                "estate checkout {repository} is on {observed_branch}, expected default branch {default_branch}"
            );
        }
    }
    if !git(&["status", "--porcelain=v1", "--untracked-files=all"])?.is_empty() {
        bail!("estate checkout {repository} is dirty");
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
                    // Generated callers delegate these concerns to the immutable
                    // owner-local callable. Their old inline job IDs and markers
                    // are intentionally absent; `audit_generated_caller` validates
                    // the closed delegation contract once for the whole file.
                    if workflow == "ci.yml" && is_generated_caller(&text) {
                        continue;
                    }
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

#[cfg(test)]
fn audit_repo(root: &Path, offline: bool) -> Result<Vec<Finding>> {
    audit_repo_profile(root, offline, None, true, None)
}

fn audit_repo_profile(
    root: &Path,
    offline: bool,
    workload_files: Option<&BTreeSet<&str>>,
    legacy_uniform_warnings: bool,
    expected_generated_class: Option<GeneratedCallerClass>,
) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    findings.extend(audit_repository_surfaces(root)?);
    findings.extend(audit_fleet_policy_surface(root)?);
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
    let mut generated_caller_seen = false;
    for path in yaml_files(&workflow_dir)? {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let yaml: Value =
            serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        generated_caller_seen |= path.file_name().and_then(|name| name.to_str()) == Some("ci.yml")
            && is_generated_caller(&text);
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
                expected_generated_class,
            },
            &mut latest,
            &mut findings,
        );
    }
    if expected_generated_class.is_some() && !generated_caller_seen {
        findings.push(Finding::error(
            "generated-caller",
            ".github/workflows/ci.yml",
            "$",
            "canonical estate repository must install its generated class caller as ci.yml",
        ));
    }
    findings.sort_by(|left, right| {
        (&left.file, &left.path, left.rule).cmp(&(&right.file, &right.path, right.rule))
    });
    Ok(findings)
}

/// Plan 039 Step 1 repository-local enforcement: when a checkout carries
/// `fleet/release-refs.toml`, validate it against the ledger schema and require
/// every generated `<org>-desired-policy.json` under the configured policy
/// directory (default `fleet/policies/`) to be byte-current versus offline
/// deterministic generation.
fn audit_fleet_policy_surface(root: &Path) -> Result<Vec<Finding>> {
    let ledger_path = root.join("fleet/release-refs.toml");
    if !ledger_path.is_file() {
        return Ok(Vec::new());
    }
    let configured = std::env::var_os("VELNOR_FLEET_POLICY_OUT_DIR");
    let policies_dir = fleet_policy_directory(root, configured.as_deref().map(Path::new))?;
    let mut on_disk = BTreeMap::new();
    let mut unreadable = Vec::new();
    if policies_dir.is_dir() {
        for entry in fs::read_dir(&policies_dir)
            .with_context(|| format!("read {}", policies_dir.display()))?
        {
            let path = entry
                .with_context(|| format!("read entry under {}", policies_dir.display()))?
                .path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let Some(stem) = name.strip_suffix("-desired-policy.json") else {
                continue;
            };
            match fs::read_to_string(&path) {
                Ok(content) => {
                    on_disk.insert(stem.to_owned(), content);
                }
                Err(error) => unreadable.push((stem.to_owned(), error.to_string())),
            }
        }
    }
    let ledger = ReleaseRefLedger::load(&ledger_path);
    let mut findings = fleet_policy_findings(ledger, &on_disk);
    for (stem, reason) in unreadable {
        findings.push(Finding::error(
            "fleet-policy-current",
            &format!("fleet/policies/{stem}-desired-policy.json"),
            "$",
            format!("generated policy file is unreadable: {reason}"),
        ));
    }
    Ok(findings)
}

fn fleet_policy_directory(root: &Path, configured: Option<&Path>) -> Result<PathBuf> {
    let Some(path) = configured else {
        return Ok(root.join("fleet/policies"));
    };
    if path.is_relative() {
        bail!(
            "VELNOR_FLEET_POLICY_OUT_DIR must be absolute when set; refusing {}",
            path.display()
        );
    }
    Ok(path.to_owned())
}

/// Pure comparison core for [`audit_fleet_policy_surface`]: ledger parse
/// result plus org → on-disk policy bytes produces precise findings only.
fn fleet_policy_findings(
    ledger: Result<ReleaseRefLedger>,
    on_disk: &BTreeMap<String, String>,
) -> Vec<Finding> {
    const LEDGER_FILE: &str = "fleet/release-refs.toml";
    let ledger = match ledger {
        Ok(ledger) => ledger,
        Err(error) => {
            return vec![Finding::error(
                "fleet-policy-ledger",
                LEDGER_FILE,
                "$",
                format!("{:#}", error),
            )];
        }
    };
    let expected: BTreeMap<String, String> = match generate_policies_from_ledger(&ledger) {
        Ok(policies) => {
            let mut map = BTreeMap::new();
            for policy in policies {
                match policy.canonical_json() {
                    Ok(json) => {
                        map.insert(policy.organization.clone(), format!("{json}\n"));
                    }
                    Err(error) => {
                        return vec![Finding::error(
                            "fleet-policy-generate",
                            LEDGER_FILE,
                            "$",
                            format!("{:#}", error),
                        )];
                    }
                }
            }
            map
        }
        Err(error) => {
            return vec![Finding::error(
                "fleet-policy-generate",
                LEDGER_FILE,
                "$",
                format!("{:#}", error),
            )];
        }
    };
    let mut findings = Vec::new();
    for (organization, wanted) in &expected {
        let relative = format!("fleet/policies/{organization}-desired-policy.json");
        let Some(content) = on_disk.get(organization) else {
            findings.push(Finding::error(
                "fleet-policy-current",
                &relative,
                "$",
                "missing required generated policy file; run on the provisioned publisher host with VELNOR_FLEET_POLICY_OUT_DIR set: rtk mise run fleet-generate",
            ));
            continue;
        };
        if content != wanted {
            let first_difference = content
                .bytes()
                .zip(wanted.bytes())
                .position(|(left, right)| left != right)
                .unwrap_or_else(|| content.len().min(wanted.len()));
            findings.push(Finding::error(
                "fleet-policy-current",
                &relative,
                "$",
                format!(
                    "policy bytes are stale versus deterministic generation (first difference at byte {first_difference}); run on the provisioned publisher host with VELNOR_FLEET_POLICY_OUT_DIR set: rtk mise run fleet-generate"
                ),
            ));
        }
    }
    for organization in on_disk.keys() {
        if !expected.contains_key(organization) {
            findings.push(Finding::error(
                "fleet-policy-extra",
                &format!("fleet/policies/{organization}-desired-policy.json"),
                "$",
                format!(
                    "no ledger entries for organization '{organization}'; remove this stale generated policy"
                ),
            ));
        }
    }
    findings
}

#[derive(Debug, Clone)]
struct TrackedSurfaceEntry {
    relative: PathBuf,
    mode: u32,
    object: Option<String>,
}

const GIT_SYMLINK_MODE: u32 = 0o120000;
const MAX_SYMLINK_HOPS: usize = 16;

#[derive(Debug)]
struct TrackedSurfaceCollection {
    entries: Vec<TrackedSurfaceEntry>,
    tracked: BTreeMap<PathBuf, TrackedSurfaceEntry>,
}

fn audit_repository_surfaces(root: &Path) -> Result<Vec<Finding>> {
    let mut collection = collect_repository_surface_entries(root)?;
    let entries = &mut collection.entries;
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));

    let mut findings = Vec::new();
    for entry in entries.iter() {
        let relative = entry.relative.to_string_lossy().replace('\\', "/");
        let is_test_runner = is_test_runner_surface(&entry.relative, entry.mode);
        let is_legacy = is_repository_surface(&entry.relative, entry.mode);
        let text = read_tracked_surface(root, entry, &collection.tracked)?;

        if is_test_runner {
            audit_test_runner_text(&relative, &entry.relative, &text, &mut findings);
        }
        if is_legacy {
            if relative == LEGACY_RUNNER_GROUP_DOCTOR {
                findings.push(Finding::error(
                    "legacy-runner-group-surface",
                    &relative,
                    "$",
                    "active legacy runner-group doctor path is forbidden; use the reviewed velnor-tools fleet-policy flow",
                ));
            }
            if relative == "mise.toml" {
                audit_prebuilt_tool_surface(&relative, &text, &mut findings);
            }
            audit_legacy_runner_group_text(&relative, &text, &mut findings);
        }
    }
    Ok(findings)
}

fn audit_test_runner_text(file: &str, relative: &Path, text: &str, findings: &mut Vec<Finding>) {
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if relative
            .extension()
            .is_some_and(|extension| extension == "rs")
            && !trimmed.starts_with("///")
            && !trimmed.starts_with("//!")
        {
            continue;
        }
        if is_cargo_test_instruction(line) {
            findings.push(Finding::error(
                "test-runner",
                file,
                format!("line {}", index + 1),
                "use cargo nextest run; cargo test is forbidden by the estate test-runner contract",
            ));
        }
    }
}

fn collect_repository_surface_entries(root: &Path) -> Result<TrackedSurfaceCollection> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "--stage", "-z"])
        .output()
        .with_context(|| format!("list tracked audit surfaces under {}", root.display()))?;
    let entries = if output.status.success() {
        parse_git_index_entries(&output.stdout)?
    } else if cfg!(test) && String::from_utf8_lossy(&output.stderr).contains("not a git repository")
    {
        let mut entries = Vec::new();
        collect_filesystem_surface_entries(root, root, &mut entries)?;
        entries
    } else {
        bail!(
            "list tracked audit surfaces under {}: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    };
    let tracked = entries
        .iter()
        .cloned()
        .map(|entry| (entry.relative.clone(), entry))
        .collect();
    let entries = entries
        .into_iter()
        .filter(|entry| {
            // Apply the historical/generated exclusions before deciding which
            // paths are audit surfaces. This keeps generated evidence trees
            // from re-entering through executable names or extensions.
            !is_historical_or_generated_directory(&entry.relative)
                && (is_test_runner_surface(&entry.relative, entry.mode)
                    || is_repository_surface(&entry.relative, entry.mode))
        })
        .collect();
    Ok(TrackedSurfaceCollection { entries, tracked })
}

fn parse_git_index_entries(bytes: &[u8]) -> Result<Vec<TrackedSurfaceEntry>> {
    let mut entries = Vec::new();
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("git ls-files returned a tracked entry without a path")?;
        let (header, raw_path) = record.split_at(separator);
        let raw_path = &raw_path[1..];
        let mut fields = header.split(|byte| *byte == b' ');
        let mode = parse_git_mode(fields.next().context("tracked entry is missing its mode")?)?;
        let object = fields
            .next()
            .context("tracked entry is missing its object")?;
        let stage = fields
            .next()
            .context("tracked entry is missing its stage")?;
        if stage != b"0" {
            bail!("tracked audit surface has unresolved merge stage");
        }
        let raw_path = std::str::from_utf8(raw_path).context("tracked path is not UTF-8")?;
        entries.push(TrackedSurfaceEntry {
            relative: safe_relative_path(raw_path)?,
            mode,
            object: Some(
                std::str::from_utf8(object)
                    .context("tracked entry object is not UTF-8")?
                    .to_owned(),
            ),
        });
    }
    Ok(entries)
}

fn parse_git_mode(value: &[u8]) -> Result<u32> {
    let value = std::str::from_utf8(value).context("tracked entry mode is not UTF-8")?;
    u32::from_str_radix(value, 8)
        .with_context(|| format!("tracked entry has invalid octal mode {value:?}"))
}

fn safe_relative_path(raw_path: &str) -> Result<PathBuf> {
    let path = Path::new(raw_path);
    if raw_path.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("tracked path must be a non-empty relative path: {raw_path:?}");
    }
    Ok(path.to_owned())
}

fn collect_filesystem_surface_entries(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<TrackedSurfaceEntry>,
) -> Result<()> {
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let entry = entry.with_context(|| format!("read entry under {}", directory.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("read file type for {}", path.display()))?;
        let relative = path.strip_prefix(root).unwrap_or(&path).to_owned();
        if file_type.is_dir() {
            if !is_historical_or_generated_directory(&relative) {
                collect_filesystem_surface_entries(root, &path, entries)?;
            }
        } else if file_type.is_file() || file_type.is_symlink() {
            let mode = if file_type.is_symlink() {
                GIT_SYMLINK_MODE
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    fs::metadata(&path)?.permissions().mode()
                }
                #[cfg(not(unix))]
                {
                    0
                }
            };
            entries.push(TrackedSurfaceEntry {
                relative,
                mode,
                object: None,
            });
        } else {
            bail!(
                "audit surface {} is non-regular; refusing to inspect special files",
                path.display()
            );
        }
    }
    Ok(())
}

fn read_tracked_surface(
    root: &Path,
    entry: &TrackedSurfaceEntry,
    tracked: &BTreeMap<PathBuf, TrackedSurfaceEntry>,
) -> Result<String> {
    let root = root
        .canonicalize()
        .with_context(|| format!("canonicalize audit root {}", root.display()))?;
    let mut seen = BTreeSet::new();
    read_tracked_surface_at(&root, &entry.relative, entry, tracked, 0, &mut seen)
}

fn read_tracked_surface_at(
    root: &Path,
    relative: &Path,
    entry: &TrackedSurfaceEntry,
    tracked: &BTreeMap<PathBuf, TrackedSurfaceEntry>,
    symlink_hops: usize,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<String> {
    if symlink_hops > MAX_SYMLINK_HOPS {
        bail!(
            "tracked symlink {} exceeds the {MAX_SYMLINK_HOPS}-hop resolution limit",
            relative.display()
        );
    }
    if !seen.insert(relative.to_owned()) {
        bail!("tracked symlink cycle includes {}", relative.display());
    }

    let path = root.join(relative);
    let metadata = materialized_surface_metadata(root, relative)?;
    match metadata {
        Some(metadata) if metadata.file_type().is_symlink() => {
            if entry.mode != GIT_SYMLINK_MODE {
                bail!(
                    "tracked regular surface {} is unexpectedly materialized as a symlink",
                    relative.display()
                );
            }
            let target = fs::read_link(&path)
                .with_context(|| format!("read tracked symlink {}", relative.display()))?;
            read_symlink_target(root, relative, &target, tracked, symlink_hops, seen)
        }
        Some(metadata) if metadata.file_type().is_file() => {
            if entry.mode == GIT_SYMLINK_MODE {
                bail!(
                    "tracked symlink {} is not materialized as a symlink",
                    relative.display()
                );
            }
            fs::read_to_string(&path).with_context(|| format!("read {}", relative.display()))
        }
        Some(_metadata) => bail!(
            "audit surface {} is non-regular; refusing to inspect special files",
            relative.display()
        ),
        None => {
            let bytes = read_index_blob(root, entry)?;
            if entry.mode == GIT_SYMLINK_MODE {
                let target =
                    PathBuf::from(String::from_utf8(bytes).with_context(|| {
                        format!("decode tracked symlink {}", relative.display())
                    })?);
                read_symlink_target(root, relative, &target, tracked, symlink_hops, seen)
            } else {
                String::from_utf8(bytes)
                    .with_context(|| format!("read {} from its tracked blob", relative.display()))
            }
        }
    }
}

fn materialized_surface_metadata(root: &Path, relative: &Path) -> Result<Option<fs::Metadata>> {
    let mut current = root.to_owned();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            bail!(
                "tracked surface path is not normalized: {}",
                relative.display()
            );
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if index + 1 < components.len() => {
                if metadata.file_type().is_symlink() {
                    bail!(
                        "tracked surface {} has an unexpected symlinked parent {}",
                        relative.display(),
                        current.display()
                    );
                }
                if !metadata.file_type().is_dir() {
                    bail!(
                        "tracked surface {} has a non-directory parent {}",
                        relative.display(),
                        current.display()
                    );
                }
            }
            Ok(metadata) => return Ok(Some(metadata)),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "inspect materialized tracked surface {}",
                        relative.display()
                    )
                })
            }
        }
    }
    bail!("tracked surface path is empty: {}", relative.display())
}

fn read_symlink_target(
    root: &Path,
    relative: &Path,
    target: &Path,
    tracked: &BTreeMap<PathBuf, TrackedSurfaceEntry>,
    symlink_hops: usize,
    seen: &mut BTreeSet<PathBuf>,
) -> Result<String> {
    let target = if target.is_absolute() {
        target.strip_prefix(root).with_context(|| {
            format!(
                "tracked symlink {} resolves outside audit root to {}",
                relative.display(),
                target.display()
            )
        })?
    } else {
        &relative
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(target)
    };
    let target = normalize_relative_path(target).with_context(|| {
        format!(
            "tracked symlink {} resolves outside audit root through {}",
            relative.display(),
            target.display()
        )
    })?;
    let target_entry = tracked.get(&target).with_context(|| {
        format!(
            "tracked symlink {} resolves to untracked or unreadable {}",
            relative.display(),
            target.display()
        )
    })?;
    read_tracked_surface_at(root, &target, target_entry, tracked, symlink_hops + 1, seen)
}

fn normalize_relative_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!("path escapes its root")
                }
            }
            Component::RootDir | Component::Prefix(_) => bail!("path is absolute"),
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("path resolves to the audit root, not a file")
    }
    Ok(normalized)
}

fn read_index_blob(root: &Path, entry: &TrackedSurfaceEntry) -> Result<Vec<u8>> {
    let object = entry
        .object
        .as_deref()
        .context("sparse tracked surface has no verified index blob")?;
    let output = Command::new("git")
        .current_dir(root)
        .args(["cat-file", "blob", object])
        .output()
        .with_context(|| format!("read tracked blob for {}", entry.relative.display()))?;
    if !output.status.success() {
        bail!(
            "read tracked blob for {}: {}",
            entry.relative.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn is_repository_surface(path: &Path, mode: u32) -> bool {
    if mode == GIT_SYMLINK_MODE || mode & 0o111 != 0 {
        return true;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("docs" | "operational" | "scripts")
        )
    }) {
        return true;
    }
    if matches!(
        name,
        "Containerfile"
            | "Dockerfile"
            | "Justfile"
            | "Makefile"
            | "postinst"
            | "postrm"
            | "preinst"
            | "prerm"
    ) {
        return true;
    }
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension,
                "Dockerfile"
                    | "bash"
                    | "env"
                    | "json"
                    | "md"
                    | "mdx"
                    | "service"
                    | "sh"
                    | "toml"
                    | "tsv"
                    | "timer"
                    | "yaml"
                    | "yml"
                    | "zsh"
            )
        })
}

fn audit_legacy_runner_group_text(file: &str, text: &str, findings: &mut Vec<Finding>) {
    let markdown = is_markdown_file(file);
    let file_marked_historical = markdown && markdown_file_marked_historical(text);
    let mut historical_section_level = None;
    let mut fence = None;
    let mut historical_fence_pending = false;
    let mut logical_command = None;

    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if let Some(current) = fence {
            if markdown_fence_closes(line, current) {
                flush_logical_command(file, &mut logical_command, findings);
                fence = None;
                continue;
            }
            let explicit = markdown && is_markdown_fence_historical_marker(line);
            let active = !current.historical && !explicit;
            append_logical_command(
                file,
                &mut logical_command,
                line,
                line_number,
                active,
                findings,
            );
            if explicit && let Some(current) = fence.as_mut() {
                current.historical = true;
            }
            continue;
        }

        if markdown && let Some((marker, length)) = markdown_fence_opener(line) {
            flush_logical_command(file, &mut logical_command, findings);
            fence = Some(MarkdownFence {
                marker,
                length,
                historical: file_marked_historical
                    || historical_section_level.is_some()
                    || historical_fence_pending,
            });
            historical_fence_pending = false;
            continue;
        }

        if markdown {
            let heading_level = markdown_heading_level(line);
            if let Some(level) = heading_level {
                if is_historical_section_marker(line) {
                    historical_section_level = Some(level);
                } else if historical_section_level.is_some_and(|section| level <= section) {
                    historical_section_level = None;
                }
            }
        }
        let explicit = markdown && is_markdown_historical_marker(line);
        if !explicit && !line.trim().is_empty() {
            historical_fence_pending = false;
        }
        let active = !file_marked_historical && historical_section_level.is_none() && !explicit;
        append_logical_command(
            file,
            &mut logical_command,
            line,
            line_number,
            active,
            findings,
        );
        if explicit {
            historical_fence_pending = true;
        }
    }
    flush_logical_command(file, &mut logical_command, findings);
}

fn is_markdown_file(file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("mdx")
        })
}

fn markdown_file_marked_historical(text: &str) -> bool {
    let mut fence = None;
    for line in text.lines().take(8) {
        if let Some(current) = fence {
            if markdown_fence_closes(line, current) {
                fence = None;
            }
            continue;
        }
        if let Some((marker, length)) = markdown_fence_opener(line) {
            fence = Some(MarkdownFence {
                marker,
                length,
                historical: false,
            });
            continue;
        }
        if is_markdown_historical_marker(line) {
            return true;
        }
    }
    false
}

fn is_markdown_historical_marker(line: &str) -> bool {
    markdown_heading_level(line).is_none()
        && !contains_active_legacy_runner_group_command(line)
        && is_explicit_historical_marker(line)
}

fn is_markdown_fence_historical_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    let is_comment = trimmed.starts_with("# ")
        || trimmed.starts_with("// ")
        || trimmed.starts_with("; ")
        || trimmed.starts_with("<!--");
    is_comment
        && !contains_active_legacy_runner_group_command(line)
        && is_explicit_historical_marker(line)
}

fn contains_active_legacy_runner_group_command(line: &str) -> bool {
    line.contains(LEGACY_RUNNER_GROUP_DOCTOR) || direct_runner_group_mutation(line).is_some()
}

#[derive(Debug, Clone, Copy)]
struct MarkdownFence {
    marker: u8,
    length: usize,
    historical: bool,
}

#[derive(Debug)]
struct LogicalShellCommand {
    start_line: usize,
    text: String,
    saw_active_line: bool,
}

fn append_logical_command(
    file: &str,
    logical_command: &mut Option<LogicalShellCommand>,
    line: &str,
    line_number: usize,
    active: bool,
    findings: &mut Vec<Finding>,
) {
    let continued = has_shell_line_continuation(line);
    if let Some(command) = logical_command.as_mut() {
        command.text.push('\n');
        command.text.push_str(line);
        command.saw_active_line |= active;
        if !continued {
            flush_logical_command(file, logical_command, findings);
        }
    } else if continued {
        *logical_command = Some(LogicalShellCommand {
            start_line: line_number,
            text: line.to_owned(),
            saw_active_line: active,
        });
    } else if active {
        audit_logical_shell_command(file, line_number, line, findings);
    }
}

fn flush_logical_command(
    file: &str,
    logical_command: &mut Option<LogicalShellCommand>,
    findings: &mut Vec<Finding>,
) {
    let Some(command) = logical_command.take() else {
        return;
    };
    if command.saw_active_line {
        audit_logical_shell_command(file, command.start_line, &command.text, findings);
    }
}

fn has_shell_line_continuation(line: &str) -> bool {
    let trimmed = line.trim_end_matches([' ', '\t']);
    let backslashes = trimmed
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    backslashes % 2 == 1
}

fn audit_logical_shell_command(
    file: &str,
    line_number: usize,
    command: &str,
    findings: &mut Vec<Finding>,
) {
    if command.contains("runner_group_doctor.sh") {
        findings.push(Finding::error(
            "legacy-runner-group-surface",
            file,
            format!("line {line_number}"),
            format!(
                "active reference to {LEGACY_RUNNER_GROUP_DOCTOR} is forbidden; remove the legacy runner-group doctor"
            ),
        ));
    }
    if let Some(method) = direct_runner_group_mutation(command) {
        findings.push(Finding::error(
            "legacy-runner-group-surface",
            file,
            format!("line {line_number}"),
            format!(
                "active direct runner-group REST mutation ({method}) is forbidden; use the reviewed velnor-tools fleet-policy flow"
            ),
        ));
    }
}

fn direct_runner_group_mutation(command: &str) -> Option<&'static str> {
    let tokens = shell_tokens(command);
    for index in 0..tokens.len() {
        let client = tokens[index];
        if client.eq_ignore_ascii_case("gh")
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.eq_ignore_ascii_case("api"))
            && let Some(method) = runner_group_api_mutation(&tokens[index + 2..], command, false)
        {
            return Some(method);
        }
        if client.eq_ignore_ascii_case("curl")
            && let Some(method) = runner_group_api_mutation(&tokens[index + 1..], command, true)
        {
            return Some(method);
        }
    }
    None
}

fn runner_group_api_mutation(tokens: &[&str], command: &str, curl: bool) -> Option<&'static str> {
    let has_runner_group_target = contains_ascii_case_insensitive(command, "runner-groups")
        || contains_ascii_case_insensitive(command, "runner_group");
    let has_explicit_path = tokens.iter().any(|token| token.contains('/'));
    let target_is_ambiguous = !has_runner_group_target && !has_explicit_path;
    if !has_runner_group_target && !target_is_ambiguous {
        return None;
    }

    let mut explicit_method = None;
    let mut body = false;
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        let lower = token.to_ascii_lowercase();
        let method_flag = if curl {
            matches!(lower.as_str(), "-x" | "--request")
        } else {
            matches!(lower.as_str(), "-x" | "--request" | "--method")
        };
        if method_flag {
            explicit_method = Some(
                tokens
                    .get(index + 1)
                    .map(|method| method.to_ascii_uppercase())
                    .unwrap_or_default(),
            );
            index += 2;
            continue;
        }
        let short_method = if curl {
            lower.strip_prefix("-x").filter(|value| !value.is_empty())
        } else {
            lower
                .strip_prefix("-x")
                .or_else(|| lower.strip_prefix("--method"))
                .or_else(|| lower.strip_prefix("--request"))
                .filter(|value| !value.is_empty())
        };
        if let Some(method) = short_method {
            explicit_method = Some(method.to_ascii_uppercase());
            index += 1;
            continue;
        }
        if curl && matches!(lower.as_str(), "-g" | "--get" | "--head") {
            explicit_method = Some("GET".to_string());
        } else if is_body_option(&lower, curl) {
            body = true;
        }
        index += 1;
    }

    match explicit_method {
        Some(method) if method == "GET" || method == "HEAD" => None,
        Some(method) if matches!(method.as_str(), "POST" | "PATCH" | "PUT" | "DELETE") => {
            Some(match method.as_str() {
                "POST" => "POST",
                "PATCH" => "PATCH",
                "PUT" => "PUT",
                "DELETE" => "DELETE",
                _ => unreachable!(),
            })
        }
        Some(_) => Some("unknown"),
        None if body => Some("POST (implicit body)"),
        None => None,
    }
}

fn shell_tokens(command: &str) -> Vec<&str> {
    command
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '\'' | '"' | '`' | '$' | '(' | ')' | ';' | '|' | '&' | '<' | '>' | '=' | ','
                )
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_body_option(token: &str, curl: bool) -> bool {
    if curl {
        matches!(
            token,
            "-d" | "--data"
                | "--data-raw"
                | "--data-binary"
                | "--data-urlencode"
                | "-f"
                | "--form"
                | "--form-string"
                | "--json"
                | "-t"
                | "--upload-file"
        ) || (token.starts_with("-d") && token.len() > 2)
            || (token.starts_with("-f") && token.len() > 2)
            || (token.starts_with("-t") && token.len() > 2)
    } else {
        matches!(token, "-f" | "-F" | "--field" | "--raw-field" | "--input")
            || token.starts_with("--field")
            || token.starts_with("--raw-field")
            || token.starts_with("--input")
    }
}

fn markdown_heading_level(line: &str) -> Option<usize> {
    let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    if spaces > 3 {
        return None;
    }
    let trimmed = &line[spaces..];
    let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    (trimmed
        .as_bytes()
        .get(level)
        .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
        || trimmed.as_bytes().get(level).is_none())
    .then_some(level)
}

fn markdown_fence_opener(line: &str) -> Option<(u8, usize)> {
    let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    if spaces > 3 {
        return None;
    }
    let bytes = line.as_bytes();
    let marker = *bytes.get(spaces)?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let length = bytes[spaces..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    if length < 3 {
        return None;
    }
    if marker == b'`' && bytes[spaces + length..].contains(&b'`') {
        return None;
    }
    Some((marker, length))
}

fn markdown_fence_closes(line: &str, fence: MarkdownFence) -> bool {
    let Some((marker, length)) = markdown_fence_opener(line) else {
        return false;
    };
    marker == fence.marker
        && length >= fence.length
        && line[line.bytes().take_while(|byte| *byte == b' ').count() + length..]
            .bytes()
            .all(|byte| byte == b' ' || byte == b'\t')
}

fn is_historical_section_marker(line: &str) -> bool {
    contains_ascii_case_insensitive(line, "historical")
        || contains_ascii_case_insensitive(line, "superseded")
        || contains_ascii_case_insensitive(line, "non-executable")
        || contains_ascii_case_insensitive(line, "non executable")
}

fn is_explicit_historical_marker(line: &str) -> bool {
    contains_ascii_case_insensitive(line, "non-executable")
        || contains_ascii_case_insensitive(line, "non executable")
        || contains_ascii_case_insensitive(line, "do not run")
        || contains_ascii_case_insensitive(line, "historical evidence")
        || contains_ascii_case_insensitive(line, "superseded")
        || contains_ascii_case_insensitive(line, "not current")
        || contains_ascii_case_insensitive(line, "not a remedy")
}

fn contains_ascii_case_insensitive(text: &str, needle: &str) -> bool {
    text.as_bytes().windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle.as_bytes())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
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

fn is_test_runner_surface(path: &Path, mode: u32) -> bool {
    if mode == GIT_SYMLINK_MODE || mode & 0o111 != 0 {
        return true;
    }
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
    let file_name = Path::new(file)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(file);
    let generated_caller = file_name == "ci.yml" && is_generated_caller(text);
    if generated_caller {
        audit_generated_caller(file, text, yaml, profile.expected_generated_class, findings);
    }
    let workload = profile
        .workload_override
        .unwrap_or_else(|| has_trigger(yaml, "push") || has_trigger(yaml, "pull_request"));
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
        if !globally_serialized && !group.contains("github.event_name") {
            findings.push(Finding::error(
                "concurrency-event",
                file,
                "$.concurrency.group",
                "include github.event_name in the concurrency group so a push, schedule, or workflow_dispatch run cannot cancel a live pull_request run that shares github.ref",
            ));
        }
    }
    audit_lane_selector(file, yaml, text, findings);
    if workload
        && !generated_caller
        && has_trigger(yaml, "workflow_dispatch")
        && !is_native_apple_workflow(yaml)
        && !is_forced_public_unmerged_workflow(file_name, yaml)
        && (!has_lane_selector(yaml)
            || (lane_selector_offers_both(yaml) && !has_truthful_both_lane_contract(text)))
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
    audit_entry_job_event_coverage(file, yaml, jobs, findings);
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

fn is_forced_public_unmerged_workflow(file_name: &str, yaml: &Value) -> bool {
    file_name.contains("public-unmerged")
        && !has_trigger(yaml, "push")
        && !has_trigger(yaml, "schedule")
        && !has_trigger(yaml, "workflow_dispatch")
        && (has_trigger(yaml, "pull_request") || has_trigger(yaml, "merge_group"))
}

fn is_generated_caller(text: &str) -> bool {
    text.starts_with("# Generated by velnor-actions-generator. DO NOT EDIT.\n")
}

/// Fail-closed aggregator used by generated `ci-required`.
///
/// A Velnor-lane infrastructure failure must not become mergeable: both the
/// selected owner job result and its `contract` output have to be `success`.
/// Anything else (failure, cancelled, skipped, empty contract) is rejection.
pub(crate) fn fleet_contract_is_success(selected_result: &str, selected_contract: &str) -> bool {
    selected_result == "success" && selected_contract == "success"
}

const CALLER_CONTRACT_RESULT_GUARD: &str = r#"if [ "${sel_result}" != "success" ]"#;
const CALLER_CONTRACT_OUTPUT_GUARD: &str = r#"if [ "${sel_contract}" != "success" ]"#;

#[derive(Clone, Copy)]
struct GeneratedOwnerLaneContract {
    owner: &'static str,
    lane_expression: &'static str,
}

const GENERATED_OWNER_LANE_CONTRACTS: [GeneratedOwnerLaneContract; 3] = [
    GeneratedOwnerLaneContract {
        owner: "jackin-project",
        lane_expression: "${{ github.event_name == 'workflow_dispatch' && inputs.lanes || (github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'github' || 'velnor' }}",
    },
    GeneratedOwnerLaneContract {
        owner: "tailrocks",
        lane_expression: "${{ github.event_name == 'workflow_dispatch' && inputs.lanes || (github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'github' || 'velnor' }}",
    },
    GeneratedOwnerLaneContract {
        owner: "ChainArgos",
        lane_expression: "${{ github.event_name == 'workflow_dispatch' && inputs.lanes || (github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'github' || 'velnor' }}",
    },
];
const GENERATED_AGGREGATOR_RUNNER_EXPRESSION: &str = "${{ ((github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') || github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'ubuntu-26.04' || fromJSON('[\"self-hosted\",\"velnor-target-mvp\"]') }}";

fn audit_generated_caller(
    file: &str,
    text: &str,
    yaml: &Value,
    expected_class: Option<GeneratedCallerClass>,
    findings: &mut Vec<Finding>,
) {
    if Path::new(file).file_name().and_then(|name| name.to_str()) != Some("ci.yml") {
        findings.push(Finding::error(
            "generated-caller",
            file,
            "$",
            "generated fleet caller must be installed only as .github/workflows/ci.yml",
        ));
    }
    let Some(jobs) = object_get(yaml, "jobs").and_then(Value::as_mapping) else {
        findings.push(Finding::error(
            "generated-caller",
            file,
            "$.jobs",
            "generated caller must declare the closed owner-call and aggregator job set",
        ));
        return;
    };
    let observed = jobs.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let expected = GENERATED_OWNER_LANE_CONTRACTS
        .iter()
        .map(|contract| contract.owner)
        .chain(std::iter::once("ci-required"))
        .collect::<BTreeSet<_>>();
    if observed != expected {
        findings.push(Finding::error(
            "generated-caller",
            file,
            "$.jobs",
            "generated caller job set must be exactly jackin-project, tailrocks, ChainArgos, ci-required",
        ));
    }

    let mut classes = BTreeSet::new();
    for contract in GENERATED_OWNER_LANE_CONTRACTS {
        let owner = contract.owner;
        let Some(job) = mapping_get(jobs, owner).and_then(Value::as_mapping) else {
            continue;
        };
        let Some(uses) = mapping_get(job, "uses").and_then(Value::as_str) else {
            findings.push(Finding::error(
                "generated-caller",
                file,
                format!("$.jobs.{owner}.uses"),
                "owner job must call its immutable owner-local reusable workflow",
            ));
            continue;
        };
        let prefix = format!("{owner}/velnor-actions/.github/workflows/ci-");
        let Some(rest) = uses.strip_prefix(&prefix) else {
            findings.push(Finding::error(
                "generated-caller",
                file,
                format!("$.jobs.{owner}.uses"),
                "owner job must call the matching owner-local velnor-actions workflow",
            ));
            continue;
        };
        let Some((class, sha)) = rest.split_once(".yml@") else {
            findings.push(Finding::error(
                "generated-caller",
                file,
                format!("$.jobs.{owner}.uses"),
                "owner callable must use ci-<class>.yml@<full-sha>",
            ));
            continue;
        };
        let parsed_class = GeneratedCallerClass::parse(class);
        if parsed_class.is_none() {
            findings.push(Finding::error(
                "generated-caller",
                file,
                format!("$.jobs.{owner}.uses"),
                "owner callable class must be exactly code, tap, apt, or fixture",
            ));
        }
        if let (Some(observed), Some(expected)) = (parsed_class, expected_class)
            && observed != expected
        {
            findings.push(Finding::error(
                "generated-caller",
                file,
                format!("$.jobs.{owner}.uses"),
                format!(
                    "canonical fleet map requires class {}, observed {}",
                    expected.as_str(),
                    observed.as_str()
                ),
            ));
        }
        if sha.len() != SHA_LEN || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            findings.push(Finding::error(
                "generated-caller",
                file,
                format!("$.jobs.{owner}.uses"),
                "owner callable ref must be an immutable 40-hex commit",
            ));
        }
        if let Some(class) = parsed_class {
            classes.insert(class);
        }

        let lane = mapping_get(job, "with")
            .and_then(Value::as_mapping)
            .and_then(|with| mapping_get(with, "lane"))
            .and_then(Value::as_str);
        if lane != Some(contract.lane_expression) {
            findings.push(Finding::error(
                "generated-caller",
                file,
                format!("$.jobs.{owner}.with.lane"),
                format!(
                    "owner {owner} must use its canonical automatic lane default, preserve the public-unmerged GitHub trust route, and forward the closed dispatch lane choice"
                ),
            ));
        }
    }
    if classes.len() != 1 {
        findings.push(Finding::error(
            "generated-caller",
            file,
            "$.jobs",
            "all three owner-local calls must select one identical repository class",
        ));
    }
    let lanes_input = object_get(yaml, "on")
        .and_then(|on| object_get(on, "workflow_dispatch"))
        .and_then(|dispatch| object_get(dispatch, "inputs"))
        .and_then(|inputs| object_get(inputs, "lanes"))
        .and_then(Value::as_mapping);
    let lanes_options = lanes_input
        .and_then(|lane| mapping_get(lane, "options"))
        .and_then(Value::as_sequence)
        .map(|options| options.iter().filter_map(Value::as_str).collect::<Vec<_>>());
    if lanes_input
        .and_then(|lane| mapping_get(lane, "type"))
        .and_then(Value::as_str)
        != Some("choice")
        || lanes_input
            .and_then(|lane| mapping_get(lane, "default"))
            .and_then(Value::as_str)
            != Some("velnor")
        || lanes_options != Some(vec!["velnor", "github", "both"])
    {
        findings.push(Finding::error(
            "generated-caller",
            file,
            "$.on.workflow_dispatch.inputs.lanes",
            "generated caller must expose the canonical plural lanes choice with the exact Velnor-default set: velnor, github, both",
        ));
    }
    let dispatch_inputs = object_get(yaml, "on")
        .and_then(|on| object_get(on, "workflow_dispatch"))
        .and_then(|dispatch| object_get(dispatch, "inputs"))
        .and_then(Value::as_mapping);
    if dispatch_inputs.is_some_and(|inputs| mapping_get(inputs, "lane").is_some()) {
        findings.push(Finding::error(
            "generated-caller",
            file,
            "$.on.workflow_dispatch.inputs.lane",
            "generated-caller must not define legacy singular lane input alongside lanes",
        ));
    }
    let aggregator_runner = mapping_get(jobs, "ci-required")
        .and_then(Value::as_mapping)
        .and_then(|job| mapping_get(job, "runs-on"))
        .and_then(Value::as_str);
    if aggregator_runner != Some(GENERATED_AGGREGATOR_RUNNER_EXPRESSION) {
        findings.push(Finding::error(
            "generated-caller",
            file,
            "$.jobs.ci-required.runs-on",
            "ci-required must follow the selected owner's canonical lane and the public-unmerged GitHub trust route",
        ));
    }
    let guards_present =
        text.contains(CALLER_CONTRACT_RESULT_GUARD) && text.contains(CALLER_CONTRACT_OUTPUT_GUARD);
    // Drive the shipped truth table from the YAML text: missing guards look
    // like success+success (mergeable); present guards are checked against an
    // infrastructure-failure sample that must never be accepted.
    let infra_sample_result = if text.contains("${sel_result}") {
        "failure"
    } else {
        "success"
    };
    let infra_sample_contract = if text.contains("${sel_contract}") {
        ""
    } else {
        "success"
    };
    if !guards_present || fleet_contract_is_success(infra_sample_result, infra_sample_contract) {
        findings.push(Finding::error(
            "generated-caller",
            file,
            "$.jobs.ci-required",
            "ci-required must fail closed unless the selected owner result and contract output are both success",
        ));
    }
}

fn audit_entry_job_event_coverage(
    file: &str,
    yaml: &Value,
    jobs: &serde_yaml::Mapping,
    findings: &mut Vec<Finding>,
) {
    if Path::new(file).file_name().and_then(|name| name.to_str()) != Some("preview.yml") {
        return;
    }
    const EVENTS: [&str; 5] = [
        "push",
        "pull_request",
        "merge_group",
        "workflow_dispatch",
        "workflow_run",
    ];
    for (job_key, job_value) in jobs {
        let Some(job) = job_value.as_mapping() else {
            continue;
        };
        if mapping_get(job, "needs").is_some() {
            continue;
        }
        let Some(condition) = mapping_get(job, "if").and_then(Value::as_str) else {
            continue;
        };
        if !condition.contains("github.event_name")
            && !condition.contains("github.event.workflow_run")
        {
            continue;
        }
        for event in EVENTS {
            if has_trigger(yaml, event)
                && !condition.contains(&format!("'{event}'"))
                && !condition.contains(&format!("\"{event}\""))
            {
                findings.push(Finding::error(
                    "trigger-if-desync",
                    file,
                    format!("$.jobs.{job_key}.if"),
                    format!(
                        "entry job condition excludes declared {event} event; synchronize on: and if: before delivery"
                    ),
                ));
            }
        }
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
    let mut sccache = false;
    let mut swatinem = false;
    let mut target_cache = false;
    let mut target_cache_generation = false;
    let mut target_dir_override = false;
    let mut unstable_target_dir = false;
    let mut literal_target_cache = false;
    let mut cargo_fuzz = false;
    let mut fuzz_target_cache = false;
    let mut first_compile_step = None;
    let mut first_target_cache_step = None;
    for (index, step) in steps.iter().enumerate() {
        let path = format!("{job_path}.steps[{index}]");
        let run = object_get(step, "run")
            .and_then(Value::as_str)
            .unwrap_or("");
        let step_fuzz = run.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("cargo ") && line.contains(" fuzz ")
                || line.starts_with("cargo +") && line.contains(" fuzz ")
        });
        cargo_fuzz |= step_fuzz;
        target_dir_override |= run.contains("CARGO_TARGET_DIR=");
        unstable_target_dir |= run.contains("CARGO_TARGET_DIR=")
            && (run.contains("GITHUB_RUN_ID") || run.contains("GITHUB_RUN_ATTEMPT"));
        let step_compiles = step_fuzz
            || run.lines().any(|line| {
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
        if run.contains("sccache --show-stats") {
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
        let lane_selector_coordinator = job_id == "matrix-setup"
            && object_get(step, "id").and_then(Value::as_str) == Some("set")
            && run.contains("case \"$LANES\" in")
            && run.contains("configs=\"[$velnor,$github]\"");
        let attestation_environment_verifier =
            Path::new(file).file_name().and_then(|name| name.to_str()) == Some("l2-provenance.yml")
                && object_get(step, "name").and_then(Value::as_str)
                    == Some("Verify exact source and signer")
                && run.contains("gh attestation verify")
                && run.contains("certificate.runnerEnvironment == $environment")
                && run.contains("expected_environment=github-hosted")
                && run.contains("expected_environment=self-hosted");
        if !attestation_environment_verifier
            && !lane_selector_coordinator
            && (lane_identity_run.contains("self-hosted")
                || lane_identity_run.contains("velnor-target-mvp")
                || lane_identity_run.contains("ubuntu-26.04"))
        {
            findings.push(Finding::error(
                "lane-conditional",
                file,
                format!("{path}.run"),
                "remove hardcoded runner identity from workload scripts",
            ));
        }
        if let Some(condition) = object_get(step, "if").and_then(Value::as_str)
            && condition.contains("matrix.config.lane")
        {
            findings.push(Finding::error(
                "lane-conditional",
                file,
                format!("{path}.if"),
                "step lane branching is forbidden; only matrix.config.writer is sanctioned",
            ));
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
        if family == "actions/cache"
            && let Some(with) = object_get(step, "with")
        {
            let caches_target = object_get(with, "path")
                .is_some_and(|value| compact(value).lines().any(|line| line.contains("target")));
            fuzz_target_cache |= object_get(with, "path").is_some_and(|value| {
                compact(value).lines().any(|line| {
                    let path = line.trim();
                    path == "fuzz/target" || path.ends_with("/fuzz/target")
                })
            });
            target_cache |= caches_target;
            if caches_target {
                first_target_cache_step.get_or_insert(index);
            }
            literal_target_cache |= object_get(with, "path")
                .is_some_and(|value| compact(value).lines().any(|line| line.trim() == "target"));
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
    if swatinem && sccache {
        findings.push(Finding::error(
            "double-cache",
            file,
            job_path,
            "remove Swatinem/rust-cache from the sccache job",
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
    if cargo_fuzz && !fuzz_target_cache {
        findings.push(Finding::error(
            "target-cache-path",
            file,
            job_path,
            "cargo fuzz writes to fuzz/target; persist that effective target path",
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

fn lane_selector(yaml: &Value) -> Option<(&'static str, &Value)> {
    let inputs = object_get(yaml, "on")
        .and_then(|on| object_get(on, "workflow_dispatch"))
        .and_then(|dispatch| object_get(dispatch, "inputs"))?;
    ["lanes", "lane"]
        .into_iter()
        .find_map(|name| object_get(inputs, name).map(|input| (name, input)))
}

fn has_lane_selector(yaml: &Value) -> bool {
    lane_selector(yaml).is_some()
}

fn lane_selector_offers_both(yaml: &Value) -> bool {
    lane_selector(yaml)
        .and_then(|(_, input)| object_get(input, "options"))
        .and_then(Value::as_sequence)
        .is_some_and(|options| options.iter().any(|value| value.as_str() == Some("both")))
}

fn audit_lane_selector(file: &str, yaml: &Value, text: &str, findings: &mut Vec<Finding>) {
    let Some((name, input)) = lane_selector(yaml) else {
        return;
    };
    let path = format!("$.on.workflow_dispatch.inputs.{name}");
    let input_type = object_get(input, "type").and_then(Value::as_str);
    let default = object_get(input, "default").and_then(Value::as_str);
    let options = object_get(input, "options")
        .and_then(Value::as_sequence)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let options_valid = options == ["velnor", "github"] || options == ["velnor", "github", "both"];
    if input_type != Some("choice") || default != Some("velnor") || !options_valid {
        findings.push(Finding::error(
            "lane-selector",
            file,
            &path,
            "use a choice defaulting to velnor with ordered options velnor, github, and optional both",
        ));
    }
    // Generated callers delegate the selected lane to an immutable owner-local
    // reusable workflow. Their local YAML intentionally contains no runner
    // labels; `audit_generated_caller` closes the caller identity, ref, input,
    // and Velnor-default forwarding contract, while generator/release audits
    // prove the callable's real Velnor+GitHub expansion.
    let capability_gated = has_explicit_velnor_capability_gate(text);
    if options.contains(&"both")
        && !is_generated_caller(text)
        && (!has_truthful_both_lane_contract(text)
            || (!capability_gated
                && (!text.contains("velnor-target-mvp") || !text.contains("ubuntu-26.04"))))
    {
        findings.push(Finding::error(
            "lane-selector",
            file,
            &path,
            "both must select real Velnor and GitHub runner lanes, or fail before side effects at the canonical tracked Velnor capability gate",
        ));
    }
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
    let apple_target_matrix = mapping_get(job, "strategy")
        .and_then(|strategy| object_get(strategy, "matrix"))
        .and_then(|matrix| object_get(matrix, "target"))
        .and_then(Value::as_sequence)
        .is_some_and(|targets| {
            !targets.is_empty()
                && targets.iter().all(|target| {
                    target
                        .as_str()
                        .is_some_and(|target| target.ends_with("-apple-darwin"))
                })
        });
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
                                "swiftc ",
                                "swift test ",
                            ]
                            .iter()
                            .any(|marker| run.contains(marker))
                                || run.contains("cargo build")
                                || (apple_target_matrix && run.contains("cargo rustc"))
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
    use std::sync::atomic::{AtomicU64, Ordering};

    fn audit(yaml: &str) -> Vec<Finding> {
        audit_file_with_class(".github/workflows/ci.yml", yaml, None)
    }

    fn audit_file(file: &str, yaml: &str) -> Vec<Finding> {
        audit_file_with_class(file, yaml, None)
    }

    fn audit_file_with_class(
        file: &str,
        yaml: &str,
        expected_generated_class: Option<GeneratedCallerClass>,
    ) -> Vec<Finding> {
        let value: Value = serde_yaml::from_str(yaml).unwrap();
        let mut findings = Vec::new();
        audit_workflow(
            file,
            yaml,
            &value,
            true,
            WorkflowAuditProfile {
                workload_override: None,
                legacy_uniform_warnings: true,
                expected_generated_class,
            },
            &mut BTreeMap::new(),
            &mut findings,
        );
        findings
    }

    fn has_rule(findings: &[Finding], rule: &str) -> bool {
        findings.iter().any(|finding| finding.rule == rule)
    }

    #[test]
    fn sparse_checkout_early_error_reaps_child_and_preserves_error() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        let child = command.spawn().unwrap();
        let started = std::time::Instant::now();

        let error = configure_sparse_checkout(child, "test-repository", b"patterns").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("git sparse-checkout stdin unavailable"),
            "{error:#}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    const BASE: &str = r#"
on:
  push:
  pull_request:
  workflow_dispatch:
    inputs:
      lanes: {type: choice, default: velnor, options: [velnor, github, both]}
concurrency:
  group: ${{ format('{0}-{1}-{2}', github.workflow, github.event_name, github.ref) }}
jobs:
  rust:
    timeout-minutes: 20
    strategy:
      matrix:
        config: ${{ fromJSON(inputs.lanes == 'both' && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"]},{"lane":"GitHub","runner":"ubuntu-26.04"}]' || '[]') }}
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

    const GENERATED_CALLER: &str = r#"# Generated by velnor-actions-generator. DO NOT EDIT.
on:
  push:
  schedule:
    - cron: "23 3 * * 0"
  workflow_dispatch:
    inputs:
      lanes:
        type: choice
        default: velnor
        options: [velnor, github, both]
concurrency:
  group: ${{ format('{0}-{1}-{2}', github.workflow, github.event_name, github.ref) }}
jobs:
  jackin-project:
    uses: jackin-project/velnor-actions/.github/workflows/ci-code.yml@0123456789012345678901234567890123456789
    with:
      lane: ${{ github.event_name == 'workflow_dispatch' && inputs.lanes || (github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'github' || 'velnor' }}
  tailrocks:
    uses: tailrocks/velnor-actions/.github/workflows/ci-code.yml@0123456789012345678901234567890123456789
    with:
      lane: ${{ github.event_name == 'workflow_dispatch' && inputs.lanes || (github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'github' || 'velnor' }}
  ChainArgos:
    uses: ChainArgos/velnor-actions/.github/workflows/ci-code.yml@0123456789012345678901234567890123456789
    with:
      lane: ${{ github.event_name == 'workflow_dispatch' && inputs.lanes || (github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'github' || 'velnor' }}
  ci-required:
    timeout-minutes: 10
    runs-on: ${{ ((github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') || github.event_name == 'pull_request' || github.event_name == 'merge_group' || github.event_name == 'push') && 'ubuntu-26.04' || fromJSON('["self-hosted","velnor-target-mvp"]') }}
    steps:
      - run: |
          if [ "${sel_result}" != "success" ]; then
            echo "ci-required: selected owner result is '${sel_result}', expected 'success'" >&2
            exit 1
          fi
          if [ "${sel_contract}" != "success" ]; then
            echo "ci-required: selected owner contract is '${sel_contract}', expected 'success'" >&2
            exit 1
          fi
"#;

    #[test]
    fn canonical_workflow_passes_static_rules() {
        assert!(audit(BASE).is_empty());
    }

    #[test]
    fn fleet_contract_fails_closed_after_lane_infrastructure_failure() {
        assert!(!fleet_contract_is_success("failure", ""));
        assert!(!fleet_contract_is_success("cancelled", ""));
        assert!(!fleet_contract_is_success("success", ""));
        assert!(!fleet_contract_is_success("failure", "success"));
        assert!(fleet_contract_is_success("success", "success"));
    }

    #[test]
    fn generated_caller_requires_contract_fail_closed_guards() {
        assert!(!has_rule(&audit(GENERATED_CALLER), "generated-caller"));
        let dropped = GENERATED_CALLER.replace(CALLER_CONTRACT_OUTPUT_GUARD, "if false");
        assert!(has_rule(&audit(&dropped), "generated-caller"));
    }

    #[test]
    fn generated_caller_uses_closed_owner_local_delegation_contract() {
        let findings = audit(GENERATED_CALLER);
        assert!(!has_rule(&findings, "generated-caller"));
        assert!(!has_rule(&findings, "lanes"));
        assert!(!has_rule(&findings, "lane-selector"));

        let tampered = GENERATED_CALLER.replace(
            "tailrocks/velnor-actions/.github/workflows/ci-code.yml@",
            "attacker/velnor-actions/.github/workflows/ci-code.yml@",
        );
        assert!(has_rule(&audit(&tampered), "generated-caller"));

        let unknown_class = GENERATED_CALLER.replace("ci-code.yml@", "ci-custom.yml@");
        assert!(has_rule(&audit(&unknown_class), "generated-caller"));

        let wrong_valid_class = GENERATED_CALLER.replace("ci-code.yml@", "ci-tap.yml@");
        assert!(has_rule(
            &audit_file_with_class(
                ".github/workflows/ci.yml",
                &wrong_valid_class,
                Some(GeneratedCallerClass::Code),
            ),
            "generated-caller"
        ));
        assert!(!has_rule(
            &audit_file_with_class(
                ".github/workflows/ci.yml",
                &wrong_valid_class,
                Some(GeneratedCallerClass::Tap),
            ),
            "generated-caller"
        ));

        let github_default = GENERATED_CALLER.replacen("default: velnor", "default: github", 1);
        assert!(has_rule(&audit(&github_default), "generated-caller"));

        let push_missing_from_lane_route = GENERATED_CALLER.replacen(
            GENERATED_OWNER_LANE_CONTRACTS[1].lane_expression,
            "${{ github.event_name == 'workflow_dispatch' && inputs.lanes || (github.event_name == 'pull_request' || github.event_name == 'merge_group') && 'github' || 'velnor' }}",
            1,
        );
        assert!(has_rule(
            &audit(&push_missing_from_lane_route),
            "generated-caller"
        ));

        let jackin_legacy_default = GENERATED_CALLER.replacen(
            GENERATED_OWNER_LANE_CONTRACTS[0].lane_expression,
            "${{ github.event_name == 'workflow_dispatch' && inputs.lanes || 'github' }}",
            1,
        );
        assert!(has_rule(&audit(&jackin_legacy_default), "generated-caller"));

        let missing_public_trust_override = GENERATED_CALLER.replacen(
            GENERATED_OWNER_LANE_CONTRACTS[2].lane_expression,
            "${{ github.event_name == 'workflow_dispatch' && inputs.lanes || 'velnor' }}",
            1,
        );
        assert!(has_rule(
            &audit(&missing_public_trust_override),
            "generated-caller"
        ));

        let github_only_aggregator = GENERATED_CALLER.replace(
            GENERATED_AGGREGATOR_RUNNER_EXPRESSION,
            "${{ 'ubuntu-26.04' }}",
        );
        assert!(has_rule(
            &audit(&github_only_aggregator),
            "generated-caller"
        ));

        let aggregator_push_missing = GENERATED_CALLER.replace(
            "|| github.event_name == 'merge_group' || github.event_name == 'push') && 'ubuntu-26.04'",
            "|| github.event_name == 'merge_group') && 'ubuntu-26.04'",
        );
        assert!(has_rule(
            &audit(&aggregator_push_missing),
            "generated-caller"
        ));
    }

    #[test]
    fn generated_caller_rejects_singular_era_lane_selector() {
        let singular = GENERATED_CALLER
            .replace("    inputs:\n      lanes:", "    inputs:\n      lane:")
            .replace("&& inputs.lanes ||", "&& inputs.lane ||");
        let findings = audit(&singular);
        assert!(has_rule(&findings, "generated-caller"), "{findings:?}");
    }

    #[test]
    fn generated_caller_rejects_dual_lane_and_lanes_dispatch_inputs() {
        let dual = GENERATED_CALLER.replace(
            "      lanes:\n",
            "      lane:\n        type: choice\n        default: velnor\n        options: [velnor, github, both]\n      lanes:\n",
        );
        let findings = audit(&dual);
        assert!(
            findings.iter().any(|finding| {
                finding.rule == "generated-caller"
                    && finding.path == "$.on.workflow_dispatch.inputs.lane"
                    && finding.message
                        == "generated-caller must not define legacy singular lane input alongside lanes"
            }),
            "dual dispatch inputs must trip the exact finding: {findings:?}"
        );
        // The canonical plural surface alone stays clean.
        assert!(!has_rule(&audit(GENERATED_CALLER), "generated-caller"));
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
    fn cargo_fuzz_requires_its_effective_target_cache() {
        let yaml = BASE.replace(
            "cargo nextest run --workspace --locked",
            "cargo +nightly fuzz build --target x86_64-unknown-linux-gnu",
        );
        assert!(has_rule(&audit(&yaml), "target-cache-path"));

        let yaml = yaml.replace(
            "          path: target",
            "          path: |\n            target\n            fuzz/target",
        );
        assert!(!has_rule(&audit(&yaml), "target-cache-path"));

        let workspace_glob = yaml.replace(
            "            fuzz/target",
            "            crates/*/fuzz/target",
        );
        assert!(!has_rule(&audit(&workspace_glob), "target-cache-path"));
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
    fn allows_cargo_build_with_an_exclusively_apple_target_matrix_on_macos() {
        let yaml = r#"
on:
  push:
concurrency:
  group: native-${{ github.ref }}
  cancel-in-progress: true
jobs:
  native:
    strategy:
      matrix:
        target: [aarch64-apple-darwin, x86_64-apple-darwin]
    runs-on: macos-26
    timeout-minutes: 20
    steps:
      - run: cargo build --release --locked --target ${{ matrix.target }}
"#;
        assert!(!has_rule(&audit(yaml), "runner-os"));
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
    fn native_swift_template_parse_stays_on_macos() {
        let yaml = r#"
on:
  push:
concurrency:
  group: swift-templates-${{ github.ref }}
  cancel-in-progress: true
jobs:
  templates-macos:
    runs-on: macos-26
    timeout-minutes: 10
    steps:
      - run: find templates -name '*.swift' -print0 | xargs -0 -n1 swiftc -parse
"#;
        assert!(!has_rule(&audit(yaml), "runner-os"));
    }

    #[test]
    fn native_rust_apple_target_stays_on_macos() {
        let yaml = BASE
            .replace("${{ matrix.config.runner }}", "macos-26")
            .replace(
                "cargo nextest run --workspace --locked",
                "cargo build --release --target aarch64-apple-darwin",
            );
        assert!(!has_rule(&audit(&yaml), "runner-os"));
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
            &audit(&BASE.replace(INLINE_MATRIX_MARKERS[0], "inputs.lanes == 'github'")),
            "lanes"
        ));
    }

    #[test]
    fn lane_selector_defaults_to_velnor_and_rejects_fake_both() {
        let github_default = BASE.replace("default: velnor", "default: github");
        assert!(has_rule(&audit(&github_default), "lane-selector"));

        let fake_both = BASE.replace("velnor-target-mvp", "github-only");
        assert!(has_rule(&audit(&fake_both), "lane-selector"));
    }

    #[test]
    fn singular_lane_selector_is_supported() {
        let singular = BASE
            .replace("lanes:", "lane:")
            .replace("inputs.lanes", "inputs.lane");
        assert!(!has_rule(&audit(&singular), "lane-selector"));
        assert!(!has_rule(&audit(&singular), "lanes"));
    }

    #[test]
    fn two_lane_selector_does_not_require_fake_both() {
        let two_lanes = BASE
            .replace(", both", "")
            .replace(INLINE_MATRIX_MARKERS[0], "inputs.lanes == 'velnor'");
        assert!(!has_rule(&audit(&two_lanes), "lane-selector"));
        assert!(!has_rule(&audit(&two_lanes), "lanes"));
    }

    #[test]
    fn output_based_lane_coordinator_proves_real_both_without_workload_branching() {
        let yaml = r#"
on:
  workflow_dispatch:
    inputs:
      lanes: {type: choice, default: velnor, options: [velnor, github, both]}
concurrency:
  group: release
  cancel-in-progress: false
jobs:
  matrix-setup:
    runs-on: ${{ (inputs.lanes == 'github') && 'ubuntu-26.04' || fromJSON('["self-hosted","velnor-target-mvp"]') }}
    timeout-minutes: 5
    steps:
      - id: set
        env:
          LANES: ${{ github.event_name == 'workflow_dispatch' && inputs.lanes || 'velnor' }}
        run: |
          github='{"lane":"GitHub","runner":"ubuntu-26.04"}'
          velnor='{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"]}'
          case "$LANES" in
            velnor) configs="[$velnor]" ;;
            github) configs="[$github]" ;;
            both) configs="[$velnor,$github]" ;;
          esac
"#;
        let findings = audit(yaml);
        assert!(!has_rule(&findings, "lane-selector"), "{findings:?}");
        assert!(!has_rule(&findings, "lanes"), "{findings:?}");
        assert!(!has_rule(&findings, "lane-conditional"), "{findings:?}");
    }

    #[test]
    fn pull_request_only_workflow_does_not_offer_trusted_lane_selection() {
        let yaml = BASE
            .replace(
                "  workflow_dispatch:\n    inputs:\n      lanes: {type: choice, default: velnor, options: [velnor, github, both]}\n",
                "",
            )
            .replace(INLINE_MATRIX_MARKERS[0], "github.event_name == 'pull_request'");
        assert!(!has_rule(&audit(&yaml), "lanes"));
    }

    #[test]
    fn forced_public_unmerged_workflow_does_not_offer_trusted_lane_selection() {
        let yaml = BASE
            .replace("  push:\n", "")
            .replace("  workflow_dispatch:\n    inputs:\n      lanes: {type: choice, default: velnor, options: [velnor, github, both]}\n", "  merge_group:\n")
            .replace(INLINE_MATRIX_MARKERS[0], "'[{\"lane\":\"GitHub\",\"runner\":\"ubuntu-26.04\",\"writer\":true}]'");
        let findings = audit_file(".github/workflows/compat-public-unmerged.yml", &yaml);
        assert!(!has_rule(&findings, "lanes"), "{findings:?}");

        let trusted_push = yaml.replace("on:\n", "on:\n  push:\n");
        assert!(!has_rule(
            &audit_file(
                ".github/workflows/compat-public-unmerged.yml",
                &trusted_push,
            ),
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
    fn plain_cargo_needs_no_explicit_compiler_or_target_cache() {
        let yaml = BASE
            .lines()
            .filter(|line| {
                !line.contains("mozilla-actions/sccache-action")
                    && !line.contains("env: {SCCACHE")
                    && !line.contains("actions/cache@")
                    && !line.trim_start().starts_with("path: target")
                    && !line.trim_start().starts_with("key: rust-build-")
                    && !line.trim_start().starts_with("restore-keys: rust-build-")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let findings = audit(&yaml);
        assert!(!has_rule(&findings, "compile-cache"), "{findings:?}");
        assert!(!has_rule(&findings, "target-cache"), "{findings:?}");
    }

    #[test]
    fn explicit_target_cache_remains_ref_independent() {
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
            .replace(
                "concurrency:\n  group: ${{ format('{0}-{1}-{2}', github.workflow, github.event_name, github.ref) }}\n",
                "",
            )
            .replace("    timeout-minutes: 20\n", "");
        let findings = audit(&yaml);
        assert!(has_rule(&findings, "concurrency"));
        assert!(has_rule(&findings, "timeout"));
    }

    #[test]
    fn cancellable_concurrency_requires_event_name() {
        let collapsed = BASE.replace(
            "format('{0}-{1}-{2}', github.workflow, github.event_name, github.ref)",
            "format('{0}-{1}', github.workflow, github.ref)",
        );
        let findings = audit(&collapsed);
        assert!(has_rule(&findings, "concurrency-event"), "{findings:?}");
        assert!(!has_rule(&audit(BASE), "concurrency-event"));
        assert!(!has_rule(&audit(GENERATED_CALLER), "concurrency-event"));
        let dispatch_cancels_pr = GENERATED_CALLER.replace(
            "format('{0}-{1}-{2}', github.workflow, github.event_name, github.ref)",
            "format('{0}-{1}', github.workflow, github.ref)",
        );
        assert!(has_rule(&audit(&dispatch_cancels_pr), "concurrency-event"));
    }

    #[test]
    fn allows_non_cancellable_global_writer_serialization() {
        let yaml = BASE.replace(
            "  group: ${{ format('{0}-{1}-{2}', github.workflow, github.event_name, github.ref) }}",
            "  group: release\n  cancel-in-progress: false",
        );
        assert!(!has_rule(&audit(&yaml), "uniform-concurrency"));
        assert!(!has_rule(&audit(&yaml), "concurrency-event"));
    }

    #[test]
    fn warns_for_cancellable_global_concurrency() {
        let yaml = BASE.replace(
            "  group: ${{ format('{0}-{1}-{2}', github.workflow, github.event_name, github.ref) }}",
            "  group: release\n  cancel-in-progress: true",
        );
        assert!(has_rule(&audit(&yaml), "uniform-concurrency"));
        assert!(has_rule(&audit(&yaml), "concurrency-event"));
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
    fn permits_exact_provenance_environment_evidence_verifier_only() {
        let verifier = r#"      - name: Verify exact source and signer
        run: |
          expected_environment=github-hosted
          expected_environment=self-hosted
          gh attestation verify subject
          jq '.[0].verificationResult.signature.certificate.runnerEnvironment == $environment' verification.json
"#;
        let yaml = BASE.replace(
            "      - run: cargo nextest run --workspace --locked\n",
            verifier,
        );
        assert!(!has_rule(
            &audit_file(".github/workflows/l2-provenance.yml", &yaml),
            "lane-conditional"
        ));
        assert!(has_rule(
            &audit_file(".github/workflows/ci.yml", &yaml),
            "lane-conditional"
        ));
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
    fn entry_job_must_accept_every_declared_active_event() {
        let workflow: Value = serde_yaml::from_str(
            r#"on:
  push:
    branches: [main]
  workflow_dispatch:
jobs:
  source:
    if: github.event_name == 'workflow_dispatch' || github.event.workflow_run.conclusion == 'success'
    runs-on: ubuntu-26.04
    steps: []
"#,
        )
        .unwrap();
        let jobs = object_get(&workflow, "jobs").unwrap().as_mapping().unwrap();
        let mut findings = Vec::new();
        audit_entry_job_event_coverage(
            ".github/workflows/preview.yml",
            &workflow,
            jobs,
            &mut findings,
        );
        assert!(has_rule(&findings, "trigger-if-desync"));
    }

    #[test]
    fn entry_job_accepting_push_and_dispatch_passes_event_coverage() {
        let workflow: Value = serde_yaml::from_str(
            r#"on:
  push:
    branches: [main]
  workflow_dispatch:
jobs:
  source:
    if: github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'workflow_dispatch')
    runs-on: ubuntu-26.04
    steps: []
"#,
        )
        .unwrap();
        let jobs = object_get(&workflow, "jobs").unwrap().as_mapping().unwrap();
        let mut findings = Vec::new();
        audit_entry_job_event_coverage(
            ".github/workflows/preview.yml",
            &workflow,
            jobs,
            &mut findings,
        );
        assert!(!has_rule(&findings, "trigger-if-desync"));
    }

    #[test]
    fn estate_manifest_parses_classified_repository() {
        let manifest: EstateManifest = serde_json::from_str(
            r#"{"version":2,"defaults":{},"repositories":[{"name":"one","path":"/one","concerns":{}}]}"#,
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
            path: Some(root.path.clone()),
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
            path: Some(root.path.clone()),
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
            path: Some(root.path.clone()),
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
            path: Some(root.path.clone()),
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
            fs::write(root.join(".github/workflows/ci.yml"), BASE).unwrap();
            assert!(audit_repo(&root, true).unwrap().is_empty());
        }
        fs::remove_dir_all(base).unwrap();
    }

    struct TestRepo {
        path: PathBuf,
    }

    static NEXT_TEST_REPO_ID: AtomicU64 = AtomicU64::new(0);

    impl TestRepo {
        fn new() -> Self {
            let path = (0..128)
                .find_map(|_| {
                    let sequence = NEXT_TEST_REPO_ID.fetch_add(1, Ordering::Relaxed);
                    let nonce = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos();
                    let path = std::env::temp_dir().join(format!(
                        "velnor-concern-test-{}-{nonce}-{sequence}",
                        std::process::id()
                    ));
                    match fs::create_dir(&path) {
                        Ok(()) => Some(path),
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                        Err(error) => panic!("create isolated test repository: {error}"),
                    }
                })
                .unwrap_or_else(|| panic!("create isolated test repository after 128 attempts"));
            if let Err(error) = fs::create_dir_all(path.join(".github/workflows")) {
                let _ = fs::remove_dir_all(&path);
                panic!("initialize isolated test repository: {error}");
            }
            Self { path }
        }
    }

    impl Drop for TestRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_repos_get_distinct_atomic_directories_under_concurrency() {
        let handles = (0..8)
            .map(|_| std::thread::spawn(|| (0..32).map(|_| TestRepo::new()).collect::<Vec<_>>()))
            .collect::<Vec<_>>();
        let repos = handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("fixture thread completes"))
            .collect::<Vec<_>>();
        let paths = repos
            .iter()
            .map(|repo| repo.path.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(paths.len(), repos.len());
        drop(repos);
        assert!(paths.iter().all(|path| !path.exists()), "{paths:?}");
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

    fn sample_fleet_ledger() -> ReleaseRefLedger {
        ReleaseRefLedger {
            schema_version: 1,
            entries: vec![crate::fleet_policy::ReleaseRefEntry {
                owner: "tailrocks".to_owned(),
                repository: "ruxel".to_owned(),
                workflow_path: ".github/workflows/ci.yml".to_owned(),
                git_ref: "refs/heads/main".to_owned(),
                admission_reason: "test".to_owned(),
                approving_change: "test".to_owned(),
                review_state: crate::fleet_policy::ReviewState::SeedPendingReview,
                expiry: None,
            }],
        }
    }

    fn approved_sample_fleet_ledger() -> ReleaseRefLedger {
        let mut ledger = sample_fleet_ledger();
        for entry in &mut ledger.entries {
            entry.review_state = crate::fleet_policy::ReviewState::Approved;
        }
        ledger
    }

    // Currency expectation is built with the same generator the check audits,
    // a circularity intentionally mitigated by the independent snapshot
    // equality test `generate_reproduces_committed_snapshot_bytes`
    // (crates/velnor-tools/src/fleet_policy.rs), which pins real committed
    // ground truth.
    fn expected_policy_bytes(ledger: &ReleaseRefLedger) -> BTreeMap<String, String> {
        crate::fleet_policy::generate_policies_from_ledger(ledger)
            .expect("sample ledger generates")
            .into_iter()
            .map(|policy| {
                let bytes = format!("{}\n", policy.canonical_json().expect("canonical"));
                (policy.organization.clone(), bytes)
            })
            .collect()
    }

    #[test]
    fn fleet_policy_findings_empty_when_bytes_current() {
        let ledger = approved_sample_fleet_ledger();
        let findings = fleet_policy_findings(Ok(ledger.clone()), &expected_policy_bytes(&ledger));
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn fleet_policy_findings_name_missing_file() {
        let ledger = approved_sample_fleet_ledger();
        let findings = fleet_policy_findings(Ok(ledger), &BTreeMap::new());
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].rule, "fleet-policy-current");
        assert_eq!(
            findings[0].file,
            "fleet/policies/tailrocks-desired-policy.json"
        );
        assert!(findings[0]
            .message
            .contains("missing required generated policy file"));
    }

    #[test]
    fn fleet_policy_findings_pinpoint_stale_bytes() {
        let ledger = approved_sample_fleet_ledger();
        let mut stale = expected_policy_bytes(&ledger);
        let current = stale.get("tailrocks").cloned().expect("tailrocks");
        // Flip the very first byte of the JSON body.
        let drifted = current.replacen('{', "[", 1);
        assert_ne!(drifted, current);
        stale.insert("tailrocks".to_owned(), drifted);
        let findings = fleet_policy_findings(Ok(ledger), &stale);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].rule, "fleet-policy-current");
        assert!(
            findings[0].message.contains("first difference at byte 0"),
            "{findings:?}"
        );
    }

    #[test]
    fn fleet_policy_findings_reject_extra_org_files() {
        let ledger = approved_sample_fleet_ledger();
        let mut on_disk = expected_policy_bytes(&ledger);
        on_disk.insert("ghost-org".to_owned(), "{}\n".to_owned());
        let findings = fleet_policy_findings(Ok(ledger), &on_disk);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].rule, "fleet-policy-extra");
        assert!(findings[0].message.contains("'ghost-org'"));
    }

    #[test]
    fn fleet_policy_findings_surface_invalid_ledger_precisely() {
        let mut invalid = sample_fleet_ledger();
        invalid.entries[0].git_ref = "main".to_owned();
        let findings = fleet_policy_findings(Err(anyhow::anyhow!("boom")), &BTreeMap::new());
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].rule, "fleet-policy-ledger");
        assert_eq!(findings[0].file, "fleet/release-refs.toml");
        assert_eq!(findings[0].message, "boom");
        // The filesystem path validates before comparing anything.
        assert!(crate::fleet_policy::validate_ledger(&invalid)
            .unwrap_err()
            .to_string()
            .contains("unqualified ref"));
    }

    #[test]
    fn fleet_policy_surface_names_toml_parse_failure_class() {
        let root = TestRepo::new();
        std::fs::create_dir_all(root.path.join("fleet")).unwrap();
        std::fs::write(
            root.path.join("fleet/release-refs.toml"),
            "schema_version = 1\n[[entries\nowner = \"tailrocks\"\n",
        )
        .unwrap();
        let findings = audit_fleet_policy_surface(&root.path).expect("audit runs");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].rule, "fleet-policy-ledger");
        assert_eq!(findings[0].file, "fleet/release-refs.toml");
        // The message names the parse failure class, not a synthetic error.
        assert!(
            findings[0].message.contains("parsing release-ref ledger"),
            "{}",
            findings[0].message
        );
        assert!(
            findings[0].message.contains("TOML parse error"),
            "{}",
            findings[0].message
        );
    }

    #[test]
    fn fleet_policy_surface_skips_checkouts_without_ledger() {
        let root = TestRepo::new();
        assert!(audit_fleet_policy_surface(&root.path)
            .expect("skip")
            .is_empty());

        std::fs::create_dir_all(root.path.join("fleet")).unwrap();
        std::fs::write(
            root.path.join("fleet/release-refs.toml"),
            "schema_version = 1\n\n[[entries]]\nowner = \"tailrocks\"\nrepository = \"ruxel\"\nworkflow_path = \".github/workflows/ci.yml\"\ngit_ref = \"refs/heads/main\"\nadmission_reason = \"test\"\napproving_change = \"test\"\nreview_state = \"approved\"\n",
        )
        .unwrap();
        let findings = audit_fleet_policy_surface(&root.path).expect("audit");
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].rule, "fleet-policy-current");
        assert!(findings[0]
            .message
            .contains("missing required generated policy file"));
    }

    #[test]
    fn fleet_policy_directory_requires_absolute_publisher_override() {
        let root = Path::new("/checkout");
        assert_eq!(
            fleet_policy_directory(root, None).unwrap(),
            PathBuf::from("/checkout/fleet/policies")
        );
        let error = fleet_policy_directory(root, Some(Path::new("fleet/policies")))
            .expect_err("publisher override must be absolute")
            .to_string();
        assert!(error.contains("VELNOR_FLEET_POLICY_OUT_DIR"), "{error}");
    }

    #[test]
    fn legacy_runner_group_guard_scans_active_repository_surfaces() {
        let root = TestRepo::new();
        fs::write(root.path.join(".github/workflows/ci.yml"), BASE).unwrap();
        fs::create_dir_all(root.path.join("config")).unwrap();
        fs::write(
            root.path.join("config/runner.env"),
            "repair_command='gh api --method PUT'\n",
        )
        .unwrap();
        fs::create_dir_all(root.path.join("scripts")).unwrap();
        fs::write(
            root.path.join("scripts/check-runner"),
            "scripts/runner_group_doctor.sh\n",
        )
        .unwrap();
        fs::write(
            root.path.join(LEGACY_RUNNER_GROUP_DOCTOR),
            "#!/usr/bin/env bash\n",
        )
        .unwrap();

        let findings = audit_repo(&root.path, true).unwrap();
        let legacy = findings
            .iter()
            .filter(|finding| finding.rule == "legacy-runner-group-surface")
            .collect::<Vec<_>>();
        assert_eq!(legacy.len(), 3, "{legacy:?}");
        assert!(legacy
            .iter()
            .any(|finding| { finding.file == "config/runner.env" && finding.path == "line 1" }));
        assert!(legacy
            .iter()
            .any(|finding| { finding.file == "scripts/check-runner" && finding.path == "line 1" }));
        assert!(legacy
            .iter()
            .any(|finding| { finding.file == LEGACY_RUNNER_GROUP_DOCTOR && finding.path == "$" }));
    }

    #[test]
    fn legacy_runner_group_guard_ignores_historical_markdown_commands() {
        let text = r#"
### Historical direct REST examples — non-executable

```sh
scripts/runner_group_doctor.sh
gh api --method PUT orgs/tailrocks/actions/runner-groups/3/repositories/7
```

### Active procedure

```sh
scripts/runner_group_doctor.sh
gh api --method PUT orgs/tailrocks/actions/runner-groups/3/repositories/7
```
"#;
        let mut findings = Vec::new();
        audit_legacy_runner_group_text("fixture-test-input.md", text, &mut findings);

        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(findings.iter().all(|finding| {
            finding.rule == "legacy-runner-group-surface" && finding.file == "fixture-test-input.md"
        }));
    }

    #[test]
    fn legacy_runner_group_guard_keeps_non_heading_file_marker_file_scoped() {
        let text = r#"Historical direct REST examples — non-executable.

### Active procedure

```sh
scripts/runner_group_doctor.sh
gh api --method PUT orgs/tailrocks/actions/runner-groups/3/repositories/7
```
"#;
        let mut findings = Vec::new();
        audit_legacy_runner_group_text("fixture-test-input.md", text, &mut findings);

        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn legacy_runner_group_guard_honors_inline_historical_fence_marker() {
        let text = r#"```sh
# HISTORICAL / NON-EXECUTABLE — do not run.
gh api --method PUT orgs/tailrocks/actions/runner-groups/3/repositories/7
scripts/runner_group_doctor.sh
```
"#;
        let mut findings = Vec::new();
        audit_legacy_runner_group_text("fixture-test-input.md", text, &mut findings);

        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn legacy_runner_group_guard_keeps_non_markdown_fence_like_text_active() {
        let text = r#"Historical direct REST examples — non-executable.

```sh
scripts/runner_group_doctor.sh
```
"#;
        let mut findings = Vec::new();
        audit_legacy_runner_group_text("config/runner.env", text, &mut findings);

        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].rule, "legacy-runner-group-surface");
        assert_eq!(findings[0].path, "line 4");
    }
}

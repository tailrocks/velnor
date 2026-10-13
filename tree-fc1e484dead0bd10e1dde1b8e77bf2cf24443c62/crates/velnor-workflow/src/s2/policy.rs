//! `velnor-workflow policy`: the workflow trust validator.
//!
//! The base branch's `ci-policy.yml` runs this subcommand under
//! `pull_request_target` with the binary the base branch pins, against the
//! pull request's tree. The validator therefore never compares that tree
//! against its own rendering or against pin literals compiled into itself —
//! a generator change would then be unable to pass the check it must change.
//! Instead it separates two questions:
//!
//! * **Is the tree generated?** The tree declares the generator that rendered
//!   it (`[generator] revision` in `.github-gen/velnor-workflow.toml`, the
//!   D19 pin). The validator proves the pin is reachable from the audited
//!   head and descends from the validator the base branch runs, obtains the
//!   generator built at that pin, regenerates the tree into a scratch
//!   directory, and requires every generated file to be byte-identical.
//! * **Is the tree safe?** The validator's own semantic rules run on the
//!   tree's YAML: no `pull_request_target` outside the policy entrypoint,
//!   every self-hosted job behind a trusted-event gate, every action pinned to
//!   a full SHA, the entrypoint restricted to `contents: read` with no
//!   secrets, and the ruleset's required contexts emitted by `ci-pr.yml`.
//!
//! Every rule reports `PASS` or `FAIL` with a one-line reason; the report is
//! what a reviewer reads in the job log.
//!
//! Policy never compiles a generator as a hidden fallback: the pinned binary
//! must already be provisioned (the running executable, an explicit pointer,
//! `PATH`, or a product the entrypoint job acquired), and an unprovisioned
//! pin fails closed. Only an explicit `--pin-build` — a local-development and
//! bootstrap escape hatch, never emitted into generated CI — permits building
//! the pin from the audited tree.
//!
//! The candidate exception binds the env-slot candidate binary by manifest
//! before executing it: the manifest's closure must equal the audited tree's
//! candidate closure (computed locally from git history) and the binary's
//! digest must match the manifest first, because a `--closure` echo is an
//! assertion by untrusted bytes, not proof. The manifest arrives via
//! `--candidate-manifest` or `VELNOR_WORKFLOW_CANDIDATE_MANIFEST`; without
//! either, env-slot binaries are skipped, never executed.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_yaml::{Mapping, Value};

use super::{
    closure as closure_identity, config, runtime, GeneratorError, ProjectConfig, SOURCE_CLOSURE,
    SOURCE_REVISION,
};

/// The generation config the audited tree declares itself with.
pub(crate) const GENERATION_CONFIG: &str = ".github-gen/velnor-workflow.toml";
/// The runtime contract, kept beside the generation config for the Velnor
/// lane fields the advisory audit needs.
const RUNTIME_CONFIG: &str = ".github/ci/project.toml";
/// The base-owned policy entrypoint: the only workflow allowed to run on
/// `pull_request_target`.
const POLICY_ENTRYPOINT: &str = ".github/workflows/ci-policy.yml";
/// The pull-request aggregate whose job display names are the ruleset's
/// status-check contexts.
const PULL_REQUEST_AGGREGATE: &str = ".github/workflows/ci-pr.yml";
/// Names the manifest binding the env-slot candidate binary.
pub use super::VELNOR_WORKFLOW_CANDIDATE_MANIFEST_ENV;
/// Names a `velnor-workflow` binary built at the pinned revision.
pub use super::VELNOR_WORKFLOW_PINNED_BINARY_ENV;
/// The revision of the validator the base branch runs.
const BASE_REVISION_ENV: &str = super::VELNOR_POLICY_REVISION_ENV;
/// Environment the pinned-generator build must never inherit: cache wrappers
/// and caller flags would let the audited tree's build reach a shared store.
const PIN_BUILD_ENV_REMOVED: &[&str] = &[
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "SCCACHE_GHA_ENABLED",
    "CARGO_INCREMENTAL",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
];

/// Inputs of one `velnor-workflow policy` evaluation.
#[derive(Clone, Debug)]
pub(crate) struct PolicyOptions {
    /// The audited tree (a full checkout: the regeneration scans it).
    pub(crate) root: PathBuf,
    /// The commit the tree is checked out at; `git rev-parse HEAD` when absent.
    pub(crate) head_sha: Option<String>,
    /// The base branch commit the pull request targets; the head when absent
    /// (push, schedule, dispatch, and local audits).
    pub(crate) base_sha: Option<String>,
    /// The validator revision the base branch runs (`ci-policy.yml`'s pin).
    pub(crate) base_revision: String,
    /// The repository ruleset's required status-check contexts, when the
    /// caller resolved them live.
    pub(crate) ruleset_contexts: Option<Vec<String>>,
    /// Whether a declared pin that is not already provisioned may be built
    /// from the audited tree. Only an explicit `--pin-build` (local
    /// development and bootstrap) sets this; generated CI never does.
    pub(crate) build_pin: bool,
    /// Manifest binding the env-slot candidate binary; `None` disables it.
    pub(crate) candidate_manifest: Option<PathBuf>,
}

/// One rule's verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuleReport {
    pub(crate) name: &'static str,
    pub(crate) passed: bool,
    pub(crate) reason: String,
    pub(crate) details: Vec<String>,
}

impl RuleReport {
    fn pass(name: &'static str, reason: impl Into<String>) -> Self {
        Self {
            name,
            passed: true,
            reason: reason.into(),
            details: Vec::new(),
        }
    }

    fn fail(name: &'static str, reason: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            name,
            passed: false,
            reason: reason.into(),
            details,
        }
    }

    fn from_findings(name: &'static str, pass_reason: &str, findings: Vec<String>) -> Self {
        if findings.is_empty() {
            Self::pass(name, pass_reason)
        } else {
            let count = findings.len();
            Self::fail(
                name,
                format!("{count} finding{}", if count == 1 { "" } else { "s" }),
                findings,
            )
        }
    }
}

/// The complete verdict of one evaluation.
#[derive(Clone, Debug, Default)]
pub(crate) struct PolicyReport {
    pub(crate) rules: Vec<RuleReport>,
}

impl PolicyReport {
    pub(crate) fn failed(&self) -> Vec<&RuleReport> {
        self.rules.iter().filter(|rule| !rule.passed).collect()
    }

    pub(crate) fn passed(&self) -> bool {
        self.rules.iter().all(|rule| rule.passed)
    }

    #[cfg(test)]
    pub(crate) fn rule(&self, name: &str) -> Option<&RuleReport> {
        self.rules.iter().find(|rule| rule.name == name)
    }

    /// The reviewer-facing rendering: one line per rule, failing details
    /// indented beneath their rule, and a one-line summary.
    pub(crate) fn render(&self) -> String {
        let width = self
            .rules
            .iter()
            .map(|rule| rule.name.len())
            .max()
            .unwrap_or(0);
        let mut output = String::new();
        for rule in &self.rules {
            let verdict = if rule.passed { "PASS" } else { "FAIL" };
            let _ = writeln!(
                output,
                "{verdict} {name:<width$}  {reason}",
                name = rule.name,
                reason = rule.reason
            );
            for detail in &rule.details {
                let _ = writeln!(output, "       - {detail}");
            }
        }
        let failed = self.failed().len();
        let _ = write!(
            output,
            "policy: {} rule{}, {failed} failed",
            self.rules.len(),
            if self.rules.len() == 1 { "" } else { "s" }
        );
        output
    }
}

/// `velnor-workflow policy [--workflow-root PATH] [--head-sha SHA] [--base-sha SHA]
/// [--base-revision SHA] [--ruleset-contexts a,b] [--pin-build]
/// [--candidate-manifest PATH]`.
///
/// `--pin-build` is a local-development and bootstrap escape hatch: without
/// it an unprovisioned pin fails closed instead of compiling from source.
/// `--candidate-manifest` binds the env-slot candidate binary to a manifest
/// (falling back to [`VELNOR_WORKFLOW_CANDIDATE_MANIFEST_ENV`]); without
/// either the candidate exception skips env-slot binaries.
///
/// # Errors
/// Usage errors, unreadable inputs, and a failed evaluation (the rendered
/// report is printed first; the error names the failing rules).
pub(crate) fn run_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let mut options = BTreeMap::new();
    let mut build_pin = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_str().ok_or_else(|| {
            GeneratorError::usage("policy arguments must be valid UTF-8".to_owned())
        })?;
        if argument == "--pin-build" {
            build_pin = true;
            index += 1;
            continue;
        }
        let Some(name) = argument.strip_prefix("--") else {
            return Err(GeneratorError::usage(format!(
                "unexpected policy argument: {argument}"
            )));
        };
        let (name, value) = if let Some((name, value)) = name.split_once('=') {
            (name.to_owned(), value.to_owned())
        } else {
            index += 1;
            let value = arguments
                .get(index)
                .and_then(|value| value.to_str())
                .ok_or_else(|| GeneratorError::usage(format!("--{name} requires a value")))?;
            (name.to_owned(), value.to_owned())
        };
        if !matches!(
            name.as_str(),
            "workflow-root"
                | "head-sha"
                | "base-sha"
                | "base-revision"
                | "ruleset-contexts"
                | "candidate-manifest"
        ) {
            return Err(GeneratorError::usage(format!(
                "unsupported policy option: --{name}"
            )));
        }
        if options.insert(name.clone(), value).is_some() {
            return Err(GeneratorError::usage(format!("--{name} given twice")));
        }
        index += 1;
    }
    let root = options
        .get("workflow-root")
        .map(PathBuf::from)
        .or_else(|| env::var_os("WORKFLOW_ROOT").map(PathBuf::from))
        .or_else(|| env::var_os("GITHUB_WORKSPACE").map(PathBuf::from))
        .or_else(|| env::current_dir().ok())
        .ok_or_else(|| GeneratorError::usage("resolve workflow root"))?;
    let base_revision = options
        .get("base-revision")
        .cloned()
        .or_else(|| env::var(BASE_REVISION_ENV).ok())
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "--base-revision or {BASE_REVISION_ENV} is required: the validator revision the base branch runs"
            ))
        })?;
    let ruleset_contexts = options.get("ruleset-contexts").map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    let candidate_manifest =
        candidate_manifest_source(options.get("candidate-manifest").map(String::as_str));
    let report = evaluate(&PolicyOptions {
        root,
        head_sha: options.get("head-sha").cloned(),
        base_sha: options.get("base-sha").cloned(),
        base_revision,
        ruleset_contexts,
        build_pin,
        candidate_manifest,
    })?;
    println!("{}", report.render());
    if report.passed() {
        return Ok(());
    }
    Err(GeneratorError::usage(format!(
        "workflow policy failed: {}",
        report
            .failed()
            .iter()
            .map(|rule| rule.name)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Resolve the candidate manifest the policy run binds env-slot candidates
/// to: the `--candidate-manifest` flag wins (an empty value disables the
/// binding), else [`VELNOR_WORKFLOW_CANDIDATE_MANIFEST_ENV`], else none.
fn candidate_manifest_source(cli: Option<&str>) -> Option<PathBuf> {
    if let Some(flag) = cli {
        if flag.is_empty() {
            return None;
        }
        return Some(PathBuf::from(flag));
    }
    env::var_os(VELNOR_WORKFLOW_CANDIDATE_MANIFEST_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Evaluate every rule against `options.root`.
///
/// # Errors
/// Only when an input cannot be read or a tool cannot run; policy violations
/// are `FAIL` rules in the returned report.
pub(crate) fn evaluate(options: &PolicyOptions) -> Result<PolicyReport, GeneratorError> {
    let root = options
        .root
        .canonicalize()
        .map_err(|error| GeneratorError::io("canonicalize workflow root", &options.root, &error))?;
    let declared = DeclaredTree::read(&root)?;
    let mut report = PolicyReport::default();
    let pin = declared_pin_rule(&declared, &mut report);
    pin_rules(&root, &declared, pin.as_deref(), options, &mut report);
    semantic_rules(&root, &declared, options, &mut report)?;
    Ok(report)
}

/// `pin-declared`: the generator commit the tree names, from the generation
/// config or, failing that, the entrypoint literal.
fn declared_pin_rule(declared: &DeclaredTree, report: &mut PolicyReport) -> Option<String> {
    match &declared.pin {
        Some(DeclaredPin::Config(pin)) => {
            report.rules.push(RuleReport::pass(
                "pin-declared",
                format!("{GENERATION_CONFIG} [generator] revision = {pin}"),
            ));
            Some(pin.clone())
        }
        Some(DeclaredPin::Entrypoint(pin)) => {
            report.rules.push(RuleReport::pass(
                "pin-declared",
                format!(
                    "{POLICY_ENTRYPOINT} {BASE_REVISION_ENV}: {pin} ({GENERATION_CONFIG} declares no [generator] revision, so the generator rendered its own)"
                ),
            ));
            Some(pin.clone())
        }
        None => {
            report.rules.push(RuleReport::fail(
                "pin-declared",
                format!(
                    "neither {GENERATION_CONFIG} `[generator] revision` nor {POLICY_ENTRYPOINT} `{BASE_REVISION_ENV}:` names the velnor-workflow commit that rendered this tree"
                ),
                Vec::new(),
            ));
            None
        }
    }
}

/// `pin-reachable`, `pin-monotonic`, `entrypoint-pin`, `generated-tree`: is
/// the tree what the generator it names renders, and is that generator one
/// the base branch may adopt?
fn pin_rules(
    root: &Path,
    declared: &DeclaredTree,
    pin: Option<&str>,
    options: &PolicyOptions,
    report: &mut PolicyReport,
) {
    let Some(pin) = pin else {
        for rule in [
            "pin-reachable",
            "pin-monotonic",
            "entrypoint-pin",
            "generated-tree",
        ] {
            report
                .rules
                .push(RuleReport::fail(rule, "no declared pin", Vec::new()));
        }
        return;
    };
    let source = declared.pin_source(root);
    let head = resolve_head(root, options.head_sha.as_deref());
    let base = options.base_sha.clone().or_else(|| head.clone().ok());
    let foreign = |rule: &'static str| {
        RuleReport::pass(
            rule,
            format!(
                "not applicable: {} consumes the generator from {}; the pin is a commit of that repository, proven when the pinned generator is obtained",
                declared.repository.as_deref().unwrap_or("this repository"),
                super::workflow_setup_action_repository()
            ),
        )
    };
    report.rules.push(match (&head, &source) {
        (_, PinSource::Remote(_)) => foreign("pin-reachable"),
        (Ok(head), PinSource::Checkout(_)) => pin_reachable(root, pin, head, base.as_deref()),
        (Err(reason), PinSource::Checkout(_)) => {
            RuleReport::fail("pin-reachable", reason.clone(), Vec::new())
        }
    });
    report.rules.push(match (&head, &source) {
        (_, PinSource::Remote(_)) => foreign("pin-monotonic"),
        (Ok(head), PinSource::Checkout(_)) => {
            pin_monotonic(root, pin, &options.base_revision, head, base.as_deref())
        }
        (Err(reason), PinSource::Checkout(_)) => {
            RuleReport::fail("pin-monotonic", reason.clone(), Vec::new())
        }
    });
    report.rules.push(entrypoint_pin(root, pin));
    let lookup =
        PinnedBinaryLookup::from_env(pin, options.build_pin, options.candidate_manifest.clone());
    let mainline = matches!((&head, &base), (Ok(head), Some(base)) if head == base);
    let comparison = regenerate_and_compare(
        root,
        root,
        pin,
        &declared.default_branch,
        &declared.excludes,
        &lookup,
        &source,
    );
    report
        .rules
        .push(generated_tree_report(pin, comparison, mainline));
}

/// The same exact-pin contract applies before and after integration.
/// A candidate-only match diagnoses an unpinned renderer; it never authorizes merge.
fn generated_tree_report(
    pin: &str,
    comparison: Result<TreeComparison, GeneratorError>,
    mainline: bool,
) -> RuleReport {
    match comparison {
        Ok(TreeComparison::Pin) => RuleReport::pass(
            "generated-tree",
            format!(
                "every generated file is byte-identical to the render of velnor-workflow at {pin}"
            ),
        ),
        Ok(TreeComparison::Candidate(closure)) => RuleReport::fail(
            "generated-tree",
            format!(
                "the declared pin {pin} does not render the tree: only candidate {closure} matches; commit the renderer source, pin that commit, and regenerate before merge (mainline={mainline})"
            ),
            Vec::new(),
        ),
        Ok(TreeComparison::Differences(differences)) => RuleReport::fail(
            "generated-tree",
            format!("the tree differs from the render of velnor-workflow at {pin}"),
            differences,
        ),
        Err(error) => RuleReport::fail(
            "generated-tree",
            format!("could not regenerate the tree with velnor-workflow at {pin}"),
            vec![error.to_string()],
        ),
    }
}

/// The validator's own rules over the tree's YAML, independent of any pin.
fn semantic_rules(
    root: &Path,
    declared: &DeclaredTree,
    options: &PolicyOptions,
    report: &mut PolicyReport,
) -> Result<(), GeneratorError> {
    let audit = audit_workflows(root)?;
    let entrypoint = audit_policy_entrypoint(root, &declared.velnor_policy)?;
    report.rules.push(RuleReport::from_findings(
        "pull-request-target",
        &format!(
            "only {POLICY_ENTRYPOINT} runs on pull_request_target, with the reviewed trigger set"
        ),
        [audit.pull_request_target, entrypoint.trigger].concat(),
    ));
    report.rules.push(RuleReport::from_findings(
        "entrypoint-privileges",
        &format!(
            "{POLICY_ENTRYPOINT} holds contents: read only, references no secrets, and persists no credentials"
        ),
        entrypoint.privileges,
    ));
    report.rules.push(RuleReport::from_findings(
        "trusted-runners",
        "every self-hosted job is gated on a trusted event and an approved runner",
        audit.runners,
    ));
    report.rules.push(RuleReport::from_findings(
        "action-pins",
        "every action reference is a full-SHA pin or a reviewed local path",
        audit.actions,
    ));
    report.rules.push(RuleReport::from_findings(
        "workflow-structure",
        "every workflow parses as GitHub would run it",
        audit.structure,
    ));
    report.rules.push(required_checks(
        root,
        declared,
        options.ruleset_contexts.as_deref(),
    ));
    Ok(())
}

/// D19 guard for `--check`: the generator the tree declares must render it
/// byte-identically. `--check` proved the running binary agrees with the
/// tree; this proves the declared pin does too, which is what the base
/// branch's policy validator regenerates the tree with. Without `--pin-build`
/// an unprovisioned pin fails closed; see `resolve_pinned_binary`.
///
/// # Errors
/// When the pinned generator cannot be obtained or renders any generated file
/// differently.
pub(crate) fn verify_declared_pin_renders_tree(
    output_root: &Path,
    checkout: &Path,
    config: &ProjectConfig,
    build_pin: bool,
) -> Result<(), GeneratorError> {
    let pin = &config.workflow_revision;
    if !super::is_full_revision(pin) {
        return Err(GeneratorError::usage(format!(
            "declared workflow revision must be a full 40-character SHA, got {pin:?}"
        )));
    }
    let generation = config::discover(checkout)?;
    let excludes = generation
        .as_ref()
        .map(config::RepoGenerationConfig::effective_policy_exclude_workflows)
        .unwrap_or_default();
    let source = pin_source(
        checkout,
        generation
            .as_ref()
            .and_then(config::RepoGenerationConfig::repository),
    );
    let lookup = PinnedBinaryLookup::from_env(pin, build_pin, None);
    match regenerate_and_compare(
        checkout,
        output_root,
        pin,
        &config.default_branch,
        &excludes,
        &lookup,
        &source,
    )? {
        TreeComparison::Pin => Ok(()),
        TreeComparison::Candidate(closure) => Err(GeneratorError::usage(format!(
            "only candidate {closure} renders the tree; pin the committed renderer source and regenerate before merge (declared pin {pin})"
        ))),
        TreeComparison::Differences(differences) => Err(GeneratorError::usage(format!(
            "the declared generator pin {pin} renders the tree differently; set `[generator] revision` in {GENERATION_CONFIG} to the last generator commit and regenerate:\n{}",
            differences.join("\n")
        ))),
    }
}

// ---------------------------------------------------------------------------
// The declared tree
// ---------------------------------------------------------------------------

/// The generator commit the audited tree names, and where it named it.
enum DeclaredPin {
    /// `[generator] revision` in the generation config: authoritative, and
    /// required in the generator's own repository so every binary renders
    /// the same pins.
    Config(String),
    /// The `VELNOR_WORKFLOW_POLICY_REVISION:` literal in the entrypoint: what
    /// `pull_request_target` runs after merge. A tree without a configured
    /// revision was rendered by a generator naming its own commit, which is
    /// exactly this literal.
    Entrypoint(String),
}

/// What the audited tree says about itself.
struct DeclaredTree {
    pin: Option<DeclaredPin>,
    /// `[generator] repository`, the slug the tree says it belongs to.
    repository: Option<String>,
    default_branch: String,
    excludes: BTreeSet<String>,
    velnor_policy: VelnorPolicyContract,
    /// Ruleset contexts `ci-pr.yml` must emit as job display names.
    required_checks: Vec<String>,
    /// Ruleset contexts reported by GitHub Apps rather than workflows.
    external_checks: Vec<String>,
}

impl DeclaredTree {
    fn read(root: &Path) -> Result<Self, GeneratorError> {
        let generation = config::discover(root)?;
        let pin = generation
            .as_ref()
            .and_then(|generation| generation.revision())
            .filter(|revision| super::is_full_revision(revision))
            .map(|revision| DeclaredPin::Config(revision.to_owned()))
            .or_else(|| entrypoint_policy_revision(root).map(DeclaredPin::Entrypoint));
        let excludes = generation
            .as_ref()
            .map(config::RepoGenerationConfig::effective_policy_exclude_workflows)
            .unwrap_or_default();
        let repository = generation
            .as_ref()
            .and_then(config::RepoGenerationConfig::repository)
            .map(str::to_owned);
        let velnor_policy = configured_velnor_policy(root)?;
        let required_checks = generation
            .as_ref()
            .map(|generation| generation.ruleset_required_status_checks().to_vec())
            .filter(|contexts| !contexts.is_empty())
            .unwrap_or_else(|| {
                if generation
                    .as_ref()
                    .and_then(config::RepoGenerationConfig::ci_required)
                    .unwrap_or(true)
                {
                    vec!["ci-required".to_owned()]
                } else {
                    Vec::new()
                }
            });
        let external_checks = generation
            .as_ref()
            .map(|generation| generation.ruleset_external_status_checks().to_vec())
            .unwrap_or_default();
        Ok(Self {
            pin,
            repository,
            default_branch: velnor_policy.default_branch.clone(),
            excludes,
            velnor_policy,
            required_checks,
            external_checks,
        })
    }

    fn pin_source(&self, root: &Path) -> PinSource {
        pin_source(root, self.repository.as_deref())
    }
}

/// Where the generator at the declared pin comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PinSource {
    /// The generator's own repository: the pin is a commit of the audited
    /// checkout, so ancestry is decidable and the build clones locally.
    Checkout(PathBuf),
    /// A repository that consumes the generator: the pin is a commit of the
    /// generator's repository, obtained with `cargo install --git`.
    Remote(String),
}

fn pin_source(root: &Path, repository: Option<&str>) -> PinSource {
    if repository == Some(super::workflow_setup_action_repository()) {
        PinSource::Checkout(root.to_path_buf())
    } else {
        PinSource::Remote(super::VELNOR_WORKFLOW_INSTALL_GIT_URL.to_owned())
    }
}

/// The `VELNOR_WORKFLOW_POLICY_REVISION:` literal the entrypoint exports,
/// when it is a full SHA.
fn entrypoint_policy_revision(root: &Path) -> Option<String> {
    let content = fs::read_to_string(root.join(POLICY_ENTRYPOINT)).ok()?;
    let marker = format!("{BASE_REVISION_ENV}: ");
    content
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix(marker.as_str()))
        .map(|value| {
            value
                .trim()
                .trim_matches(|character| character == '"' || character == '\'')
        })
        .find(|value| super::is_full_revision(value))
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Git facts
// ---------------------------------------------------------------------------

fn git(root: &Path, arguments: &[&str]) -> Result<Option<String>, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .map_err(|error| {
            GeneratorError::usage(format!("run git {}: {error}", arguments.join(" ")))
        })?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    } else {
        Ok(None)
    }
}

fn resolve_head(root: &Path, head: Option<&str>) -> Result<String, String> {
    let head = match head {
        Some(head) => head.to_owned(),
        None => match git(root, &["rev-parse", "HEAD"]) {
            Ok(Some(head)) => head,
            Ok(None) => {
                return Err("workflow root is not a git checkout; pass --head-sha".to_owned())
            }
            Err(error) => return Err(error.to_string()),
        },
    };
    if !super::is_full_revision(&head) {
        return Err(format!(
            "head must be a full 40-character SHA, got {head:?}"
        ));
    }
    Ok(head)
}

fn commit_exists(root: &Path, revision: &str) -> bool {
    git(root, &["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .is_ok_and(|result| result.is_some())
}

/// `Some(true)` when `ancestor` is reachable from `descendant`; `None` when
/// either commit is absent from the checkout.
fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> Option<bool> {
    if ancestor == descendant {
        return commit_exists(root, ancestor).then_some(true);
    }
    if !commit_exists(root, ancestor) || !commit_exists(root, descendant) {
        return None;
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn pin_reachable(root: &Path, pin: &str, head: &str, base: Option<&str>) -> RuleReport {
    match is_ancestor(root, pin, head) {
        Some(true) => {
            let provenance = match base.and_then(|base| is_ancestor(root, pin, base)) {
                Some(true) => "inherited from the base branch".to_owned(),
                Some(false) => "introduced by this change".to_owned(),
                None => "base commit not in this checkout; provenance unknown".to_owned(),
            };
            RuleReport::pass(
                "pin-reachable",
                format!("{pin} is an ancestor of head {head} ({provenance})"),
            )
        }
        Some(false) => RuleReport::fail(
            "pin-reachable",
            format!("{pin} is not an ancestor of head {head}; the tree must be rendered by a commit it contains"),
            Vec::new(),
        ),
        None => RuleReport::fail(
            "pin-reachable",
            missing_commit_reason(root, pin, head),
            Vec::new(),
        ),
    }
}

/// Whether the checkout is shallow (`git clone --depth`, `actions/checkout`
/// with the default `fetch-depth: 1`): history stops at a grafted boundary,
/// so an older commit is absent without having been removed.
fn is_shallow_checkout(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|output| output.is_some_and(|value| value.trim() == "true"))
}

/// Why `pin-reachable` could not decide: names the commits the checkout
/// lacks and separates the two causes — a shallow checkout that cut the
/// history the rule walks (the job must check out with `fetch-depth: 0`)
/// from a full checkout that genuinely lacks the commit (the pin or head is
/// not in this repository's history).
fn missing_commit_reason(root: &Path, pin: &str, head: &str) -> String {
    let mut missing = Vec::new();
    if !commit_exists(root, pin) {
        missing.push(format!("pin {pin}"));
    }
    if !commit_exists(root, head) {
        missing.push(format!("head {head}"));
    }
    let missing = if missing.is_empty() {
        format!("pin {pin} or head {head}")
    } else {
        missing.join(" and ")
    };
    if is_shallow_checkout(root) {
        format!(
            "{missing} is not a commit in this shallow checkout; the rule walks history from the audited head to the declared pin, so the job that runs the validator must check out full history (actions/checkout `fetch-depth: 0`)"
        )
    } else {
        format!(
            "{missing} is not a commit in this full-history checkout; the tree must be rendered by a commit its own history contains, so fetch the missing commit or re-pin to one the head descends from"
        )
    }
}

/// The pin the tree at the merge base of `head` and `base` declared, when
/// both commits are present and the merge base carries a generation config.
fn merge_base_pin(root: &Path, head: &str, base: &str) -> Option<String> {
    let merge_base = git(root, &["merge-base", base, head]).ok().flatten()?;
    let content = git(
        root,
        &["show", &format!("{merge_base}:{GENERATION_CONFIG}")],
    )
    .ok()
    .flatten()?;
    let generation = config::parse(Path::new(GENERATION_CONFIG), content.as_bytes()).ok()?;
    generation
        .revision()
        .filter(|revision| super::is_full_revision(revision))
        .map(str::to_owned)
}

/// The validator chain never regresses: after merge, `ci-policy.yml` pins
/// whatever the merged tree declares, so a declared pin that is older than
/// the validator the base branch runs today would downgrade the gate. The
/// pin therefore must be the base validator or descend from it — unless the
/// change left the pin exactly as the merge base declared it, in which case
/// the merge keeps the base branch's own (newer) pin and nothing regresses.
fn pin_monotonic(
    root: &Path,
    pin: &str,
    base_revision: &str,
    head: &str,
    base: Option<&str>,
) -> RuleReport {
    if !super::is_full_revision(base_revision) {
        return RuleReport::fail(
            "pin-monotonic",
            format!(
                "base validator revision must be a full 40-character SHA, got {base_revision:?}"
            ),
            Vec::new(),
        );
    }
    if pin == base_revision {
        return RuleReport::pass(
            "pin-monotonic",
            format!("the declared pin is the base validator {base_revision}"),
        );
    }
    match is_ancestor(root, base_revision, pin) {
        Some(true) => RuleReport::pass(
            "pin-monotonic",
            format!("the declared pin descends from the base validator {base_revision}"),
        ),
        Some(false)
            if base.is_some_and(|base| {
                merge_base_pin(root, head, base).as_deref() == Some(pin)
            }) =>
        {
            RuleReport::pass(
                "pin-monotonic",
                format!(
                    "the declared pin predates the base validator {base_revision} but is unchanged since the merge base; the merge keeps the base branch's pin"
                ),
            )
        }
        Some(false) => RuleReport::fail(
            "pin-monotonic",
            format!(
                "the declared pin does not descend from the base validator {base_revision}; rebase onto the base branch and re-pin so the policy chain never regresses"
            ),
            Vec::new(),
        ),
        None => RuleReport::fail(
            "pin-monotonic",
            format!(
                "the base validator {base_revision} is not a commit in this checkout; rebase onto the base branch and re-pin"
            ),
            Vec::new(),
        ),
    }
}

// ---------------------------------------------------------------------------
// The declared pin in the entrypoint
// ---------------------------------------------------------------------------

/// Every 40-hex literal the entrypoint installs, acquires, or exports as the
/// policy revision must be the declared pin: the generator renders them from
/// one value, and after merge this file is what `pull_request_target` runs.
/// The shapes are the setup action's `rev:` input, the consumer action pin
/// (`setup-velnor-workflow@<sha>`), the exported revision env, and legacy
/// `--rev <sha>`. Tokens after a marker that are not full revisions (the
/// owner job's `closure --rev="$VAR"` variable references, the consumer's
/// `rev: ${{ steps.pin.outputs.value }}`) are not pin literals. Rendered
/// templates use the `--rev=` form (never `--rev ` with a space) so a base
/// validator from before the product re-architecture, which scans for the
/// `--rev ` marker, keeps accepting the tree during the transition.
fn entrypoint_pin(root: &Path, pin: &str) -> RuleReport {
    let path = root.join(POLICY_ENTRYPOINT);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            return RuleReport::fail(
                "entrypoint-pin",
                format!("{POLICY_ENTRYPOINT}: {error}"),
                Vec::new(),
            );
        }
    };
    let mut literals = Vec::new();
    for line in content.lines() {
        for marker in [
            "rev: ".to_owned(),
            "setup-velnor-workflow@".to_owned(),
            "--rev ".to_owned(),
            "--rev=".to_owned(),
            format!("{BASE_REVISION_ENV}: "),
        ] {
            if let Some(index) = line.find(marker.as_str()) {
                let value = line[index + marker.len()..]
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_matches(|character| character == '"' || character == '\'');
                if super::is_full_revision(value) {
                    literals.push((marker.trim_end().to_owned(), value.to_owned()));
                }
            }
        }
    }
    if literals.is_empty() {
        return RuleReport::fail(
            "entrypoint-pin",
            format!(
                "{POLICY_ENTRYPOINT} acquires or exports no pinned policy revision (`rev: <sha>`, `setup-velnor-workflow@<sha>`, `--rev=<sha>`, or `{BASE_REVISION_ENV}: <sha>`)"
            ),
            Vec::new(),
        );
    }
    let drift = literals
        .iter()
        .filter(|(_, value)| value != pin)
        .map(|(marker, value)| {
            format!("{POLICY_ENTRYPOINT}: {marker} {value} is not the declared pin")
        })
        .collect::<Vec<_>>();
    if drift.is_empty() {
        RuleReport::pass(
            "entrypoint-pin",
            format!(
                "{POLICY_ENTRYPOINT} acquires and exports the declared pin ({} literal{})",
                literals.len(),
                if literals.len() == 1 { "" } else { "s" }
            ),
        )
    } else {
        RuleReport::fail(
            "entrypoint-pin",
            format!("{POLICY_ENTRYPOINT} pins a revision other than the declared one"),
            drift,
        )
    }
}

// ---------------------------------------------------------------------------
// Regeneration with the declared pin
// ---------------------------------------------------------------------------

fn policy_install_root(revision: &str) -> PathBuf {
    env::var_os("RUNNER_TEMP")
        .or_else(|| env::var_os("TMPDIR"))
        .map_or_else(env::temp_dir, PathBuf::from)
        .join(format!("velnor-workflow-policy-{revision}"))
}

/// The revision a `velnor-workflow` binary reports for itself.
fn binary_revision(binary: &Path) -> Result<String, String> {
    binary_report(binary, "--revision")
}

/// The source-closure digest a `velnor-workflow` binary reports for itself.
fn binary_closure(binary: &Path) -> Result<String, String> {
    binary_report(binary, "--closure")
}

fn binary_report(binary: &Path, flag: &str) -> Result<String, String> {
    let output = Command::new(binary)
        .arg(flag)
        .output()
        .map_err(|error| format!("{}: cannot run `{flag}`: {error}", binary.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{}: `{flag}` failed ({}): {}",
            binary.display(),
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Closures a renderer for `pin` (a commit of `repo`) must report: the lean
/// release and debug CI products, plus the default-feature debug candidate.
/// The candidate's TUI is TTY-gated and policy always renders through pipes,
/// so the reached code is identical in all three.
pub(crate) fn expected_closures(repo: &Path, pin: &str) -> Result<Vec<String>, GeneratorError> {
    Ok(vec![
        closure_identity::closure_of_tree(
            repo,
            pin,
            closure_identity::CI_FEATURES,
            closure_identity::PROFILE_RELEASE,
        )?,
        closure_identity::closure_of_tree(
            repo,
            pin,
            closure_identity::CI_FEATURES,
            closure_identity::PROFILE_DEBUG,
        )?,
        closure_identity::candidate_closure_of_tree(repo, pin)?,
    ])
}

/// The process environment the pinned-binary lookup reads, captured once so
/// tests can drive the resolver without mutating global state.
pub(crate) struct PinnedBinaryLookup {
    /// [`VELNOR_WORKFLOW_PINNED_BINARY_ENV`].
    pinned_binary: Option<PathBuf>,
    /// `PATH`.
    search_path: Option<OsString>,
    /// Where an earlier resolution built the pin.
    install_root: PathBuf,
    /// Never build without an explicit `--pin-build`, or under
    /// `CARGO_NET_OFFLINE=true`.
    build_forbidden: bool,
    /// Manifest binding the env-slot candidate binary. `None` disables the
    /// env-slot candidate; the running binary stays manifest-exempt.
    candidate_manifest: Option<PathBuf>,
}

impl PinnedBinaryLookup {
    pub(crate) fn from_env(
        revision: &str,
        build_pin: bool,
        candidate_manifest: Option<PathBuf>,
    ) -> Self {
        Self {
            pinned_binary: env::var_os(VELNOR_WORKFLOW_PINNED_BINARY_ENV).map(PathBuf::from),
            search_path: env::var_os("PATH"),
            install_root: policy_install_root(revision),
            build_forbidden: !build_pin
                || env::var("CARGO_NET_OFFLINE").is_ok_and(|value| value == "true"),
            candidate_manifest: candidate_manifest.or_else(|| {
                env::var_os(VELNOR_WORKFLOW_CANDIDATE_MANIFEST_ENV)
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
            }),
        }
    }

    /// Every `velnor-workflow` on the search path, in search order.
    fn path_binaries(&self) -> Vec<PathBuf> {
        let name = format!("velnor-workflow{}", env::consts::EXE_SUFFIX);
        self.search_path
            .as_ref()
            .map(|path| {
                env::split_paths(path)
                    .filter(|directory| !directory.as_os_str().is_empty())
                    .map(|directory| directory.join(&name))
                    .filter(|candidate| candidate.is_file())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Locate a `velnor-workflow` renderer for the declared pin `revision`,
/// proven by the source closure it reports (`--closure`), never by where it
/// lives:
///
/// 1. the running binary, when its closure is one of `expected`;
/// 2. [`VELNOR_WORKFLOW_PINNED_BINARY_ENV`] — an explicit pointer; a binary
///    there that is not the pin is a configuration error, not a fallback;
/// 3. a `velnor-workflow` on `PATH` (hosted jobs install the pinned runtime
///    there; release/preview/maintenance jobs install the pin itself);
/// 4. a binary this resolver built earlier under [`policy_install_root`];
/// 5. a build at `revision` from `source` — a local clone of the audited
///    checkout (which must contain the commit) for the generator's own
///    repository, `cargo install --git` for a consumer; only with an
///    explicit `--pin-build`, and refused under `CARGO_NET_OFFLINE=true`.
///
/// `expected` holds the pin's closures ([`expected_closures`]) whenever the
/// pin is a commit of the audited history. `None` (a consumer tree without
/// generator history) falls back to the pin's revision plus a wellformed
/// closure report — exactly today's trust level, never weaker — while CI,
/// which always has the history, takes the strict path.
///
/// Fails closed with every attempt listed when none of these yields the pin.
pub(crate) fn resolve_pinned_binary(
    revision: &str,
    expected: Option<&[String]>,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
) -> Result<PathBuf, GeneratorError> {
    match expected {
        Some(set) => {
            if set.contains(&SOURCE_CLOSURE.to_owned())
                && let Ok(current) = env::current_exe()
            {
                return Ok(current);
            }
        }
        None => {
            if SOURCE_REVISION == revision
                && closure_identity::is_full_closure(SOURCE_CLOSURE)
                && let Ok(current) = env::current_exe()
            {
                return Ok(current);
            }
        }
    }
    let mut attempts = Vec::new();
    if let Some(binary) = lookup.pinned_binary.clone() {
        return match prove_candidate(&binary, revision, expected) {
            Ok(()) => Ok(binary),
            Err(detail) => Err(GeneratorError::usage(format!(
                "{VELNOR_WORKFLOW_PINNED_BINARY_ENV}={} is not the declared pin: {detail}",
                binary.display()
            ))),
        };
    }
    let install_root = &lookup.install_root;
    let installed = install_root.join("bin").join("velnor-workflow");
    let mut candidates = lookup.path_binaries();
    if installed.is_file() {
        candidates.push(installed.clone());
    }
    for candidate in candidates {
        match prove_candidate(&candidate, revision, expected) {
            Ok(()) => return Ok(candidate),
            Err(detail) => attempts.push(format!("{}: {detail}", candidate.display())),
        }
    }
    if lookup.build_forbidden {
        let tried = if attempts.is_empty() {
            "no velnor-workflow on PATH".to_owned()
        } else {
            attempts.join("; ")
        };
        return Err(GeneratorError::usage(format!(
            "no velnor-workflow renderer for the declared pin {revision} is provisioned and building one is forbidden here ({tried}); export {VELNOR_WORKFLOW_PINNED_BINARY_ENV}=<binary for {revision}>, put one on PATH, or rerun with --pin-build to compile the pin from source (local development only; never emitted into CI)"
        )));
    }
    build_pinned_binary(revision, expected, source, install_root, &installed)
}

/// The candidate manifest the publisher wrote beside the candidate binary
/// (`profile`, `platform`, `repository`, `run_id`, `revision`, `closure`,
/// `binary_sha256`, plus `build_revision`). The consume-side binding uses
/// only the closure and the digest: `revision` names the PR head the
/// publisher built for, which a legit older pin may still equal on closure
/// paths, so closure equality is the content binding.
#[derive(serde::Deserialize)]
struct CandidateManifest {
    revision: String,
    closure: String,
    binary_sha256: String,
}

/// Read and shape-validate the manifest at `path`: valid JSON whose
/// revision, closure, and digest fields are full hex digests of the right
/// length. Anything else fails closed — a manifest the validator cannot
/// parse proves nothing.
fn load_candidate_manifest(path: &Path) -> Result<CandidateManifest, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
    let manifest: CandidateManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: not a candidate manifest: {error}", path.display()))?;
    if !super::is_full_revision(&manifest.revision) {
        return Err(format!(
            "{}: revision {:?} is not a full commit SHA",
            path.display(),
            manifest.revision
        ));
    }
    if !closure_identity::is_full_closure(&manifest.closure) {
        return Err(format!(
            "{}: closure {:?} is not a source-closure digest",
            path.display(),
            manifest.closure
        ));
    }
    if manifest.binary_sha256.len() != 64
        || !manifest
            .binary_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!(
            "{}: binary_sha256 {:?} is not a SHA-256 digest",
            path.display(),
            manifest.binary_sha256
        ));
    }
    Ok(manifest)
}

/// The SHA-256 hex digest of the bytes at `path`, computed before any
/// execution: `binary_closure` itself runs the binary, so the digest gate
/// must come first.
fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::Digest as _;
    let bytes =
        fs::read(path).map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
    let mut digest = String::with_capacity(64);
    for byte in sha2::Sha256::digest(&bytes) {
        let _ = write!(digest, "{byte:02x}");
    }
    Ok(digest)
}

/// Whether `binary` is a renderer for `revision`: its reported closure is
/// one of `expected`, or — when the pin's tree is unavailable — its reported
/// revision is the pin and its closure report is wellformed.
fn prove_candidate(
    binary: &Path,
    revision: &str,
    expected: Option<&[String]>,
) -> Result<(), String> {
    let reported = binary_closure(binary)?;
    if let Some(set) = expected {
        if set.contains(&reported) {
            return Ok(());
        }
        return Err(format!(
            "reports closure {reported}, which is not the pin's closure"
        ));
    }
    if !closure_identity::is_full_closure(&reported) {
        return Err(format!(
            "reports {reported:?} instead of a source-closure digest; install a current binary"
        ));
    }
    match binary_revision(binary)? {
        claimed if claimed == revision => Ok(()),
        claimed => Err(format!(
            "reports revision {claimed}, but the declared pin is {revision}"
        )),
    }
}

/// Build the lean `velnor-workflow` product at `revision` with
/// `cargo install --locked --no-default-features` and every cache wrapper
/// and caller flag removed, so the build never touches a shared store. The
/// generator's own repository builds from a local clone of the audited
/// checkout (hardlinked objects) checked out at the pin; a consumer installs
/// from the generator's git URL. The result must prove the pin exactly like
/// a provisioned candidate ([`prove_candidate`]).
fn build_pinned_binary(
    revision: &str,
    expected: Option<&[String]>,
    source: &PinSource,
    install_root: &Path,
    installed: &Path,
) -> Result<PathBuf, GeneratorError> {
    fs::create_dir_all(install_root).map_err(|error| {
        GeneratorError::usage(format!(
            "prepare pinned generator root at {}: {error}",
            install_root.display()
        ))
    })?;
    let mut command = Command::new("cargo");
    command.args(["install", "--locked", "--no-default-features", "--force"]);
    match source {
        PinSource::Checkout(checkout) => {
            let clone = install_root.join("source");
            if clone.exists() {
                fs::remove_dir_all(&clone).map_err(|error| {
                    GeneratorError::io("remove stale pinned source clone", &clone, &error)
                })?;
            }
            let status = Command::new("git")
                .arg("clone")
                .arg("--quiet")
                .arg("--no-checkout")
                .arg(checkout)
                .arg(&clone)
                .status()
                .map_err(|error| {
                    GeneratorError::usage(format!("clone the audited checkout: {error}"))
                })?;
            if !status.success() {
                return Err(GeneratorError::usage(format!(
                    "clone the audited checkout {} for the pinned build failed",
                    checkout.display()
                )));
            }
            let status = Command::new("git")
                .arg("-C")
                .arg(&clone)
                .args(["checkout", "--quiet", "--detach", revision])
                .status()
                .map_err(|error| {
                    GeneratorError::usage(format!("check out pin {revision}: {error}"))
                })?;
            if !status.success() {
                return Err(GeneratorError::usage(format!(
                    "pin {revision} is not a commit of the audited checkout"
                )));
            }
            command
                .arg("--path")
                .arg(clone.join("crates").join("velnor-workflow"));
        }
        PinSource::Remote(url) => {
            command.args(["--git", url, "--rev", revision, "velnor-workflow"]);
        }
    }
    command
        .arg("--root")
        .arg(install_root)
        .args(["--bin", "velnor-workflow"])
        .env("CARGO_TARGET_DIR", install_root.join("target"));
    for name in PIN_BUILD_ENV_REMOVED {
        command.env_remove(name);
    }
    for (name, _) in env::vars_os() {
        if name.to_string_lossy().starts_with("MBX_") {
            command.env_remove(name);
        }
    }
    let status = command.status().map_err(|error| {
        GeneratorError::usage(format!("build velnor-workflow at pin {revision}: {error}"))
    })?;
    if !status.success() {
        return Err(GeneratorError::usage(format!(
            "build velnor-workflow at pin {revision} failed"
        )));
    }
    match prove_candidate(installed, revision, expected) {
        Ok(()) => Ok(installed.to_path_buf()),
        Err(detail) => Err(GeneratorError::usage(format!(
            "velnor-workflow built for pin {revision} at {} is unusable: {detail}",
            installed.display()
        ))),
    }
}

fn scratch_directory(label: &str) -> Result<PathBuf, GeneratorError> {
    let base = env::var_os("RUNNER_TEMP")
        .or_else(|| env::var_os("TMPDIR"))
        .map_or_else(env::temp_dir, PathBuf::from);
    let path = base.join(format!(
        "velnor-workflow-{label}-{}",
        crate::unique_suffix()
    ));
    fs::create_dir_all(&path)
        .map_err(|error| GeneratorError::io("create scratch directory", &path, &error))?;
    Ok(path)
}

/// Scan `checkout` with the generator built at `pin`, render into a scratch
/// directory, and return every path under `tree` that differs, in sorted
/// order. Every rendered file must match the tree byte for byte, and every
/// workflow in the tree must be generator-owned unless the tree's policy
/// excludes it. `checkout` and `tree` are the same directory except under
/// `--check --output`, where the rendered tree lives apart from its source.
///
/// # Errors
/// When the pinned generator cannot be obtained or its render fails.
/// What `regenerate_and_compare` proved about the tree.
pub(crate) enum TreeComparison {
    /// The tree is byte-identical to the declared pin's render.
    Pin,
    /// The tree differs from the pin's render but is byte-identical to the
    /// render of the audited tree's own candidate: a generator change in
    /// flight. Carries the candidate closure that proved it.
    Candidate(String),
    /// Neither renderer reproduces the tree.
    Differences(Vec<String>),
}

pub(crate) fn regenerate_and_compare(
    checkout: &Path,
    tree: &Path,
    pin: &str,
    default_branch: &str,
    excludes: &BTreeSet<String>,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
) -> Result<TreeComparison, GeneratorError> {
    // The generator's own repository audits a full history, so the pin's
    // closures are always computable there; a consumer tree without generator
    // history resolves through the revision fallback instead.
    let expected = match source {
        PinSource::Checkout(_) => Some(expected_closures(checkout, pin)?),
        PinSource::Remote(_) => expected_closures(checkout, pin).ok(),
    };
    let binary = resolve_pinned_binary(pin, expected.as_deref(), lookup, source)?;
    let scratch = scratch_directory("policy-render")?;
    let verdict = render_and_compare(&binary, checkout, tree, &scratch, default_branch, excludes)
        .and_then(|differences| {
            if differences.is_empty() {
                return Ok(TreeComparison::Pin);
            }
            match render_with_candidate(checkout, tree, &scratch, default_branch, excludes, lookup)?
            {
                Some(closure) => Ok(TreeComparison::Candidate(closure)),
                None => Ok(TreeComparison::Differences(differences)),
            }
        });
    let _ = fs::remove_dir_all(&scratch);
    verdict
}

/// The candidate exception: when the tree differs from the declared pin's
/// render, it may still be legitimate — a generator change in flight renders
/// with the audited tree's own candidate, not with the pin.
///
/// Acceptance requires the manifest binding, not `--closure` alone: the
/// manifest's closure must equal the audited checkout's own candidate
/// closure (computed locally from git history), the env-slot binary's digest
/// must match the manifest before any execution, and only then does the
/// `--closure` self-report stay as a final tripwire. A `--closure` echo is an
/// assertion by untrusted bytes, not proof. Returns the proving closure, or
/// `None` when no bound candidate reproduces the tree.
fn render_with_candidate(
    checkout: &Path,
    tree: &Path,
    scratch: &Path,
    default_branch: &str,
    excludes: &BTreeSet<String>,
    lookup: &PinnedBinaryLookup,
) -> Result<Option<String>, GeneratorError> {
    let Ok(Some(head)) = git(checkout, &["rev-parse", "HEAD"]) else {
        return Ok(None);
    };
    if !super::is_full_revision(&head) {
        return Ok(None);
    }
    let Ok(wanted) = closure_identity::candidate_closure_of_tree(checkout, &head) else {
        return Ok(None);
    };
    let current_exe = env::current_exe().ok();
    // The manifest gate fails closed loudly: a manifest path that cannot be
    // loaded, or names another tree, is a configuration error, not a skip.
    let manifest = match &lookup.candidate_manifest {
        None => None,
        Some(path) => {
            let manifest = load_candidate_manifest(path).map_err(GeneratorError::usage)?;
            if manifest.closure != wanted {
                return Err(GeneratorError::usage(format!(
                    "candidate manifest {} names closure {}, but the audited tree's candidate closure is {wanted}",
                    path.display(),
                    manifest.closure
                )));
            }
            Some(manifest)
        }
    };
    let mut binaries = Vec::new();
    if let Some(pinned) = &lookup.pinned_binary {
        binaries.push(pinned.clone());
    }
    if let Some(current) = &current_exe
        && !binaries.contains(current)
    {
        binaries.push(current.clone());
    }
    for binary in binaries {
        if !binary.is_file() {
            continue;
        }
        // The running binary stays manifest-exempt: in CI it is the base
        // product itself, and locally the operator trusts their own binary —
        // which preserves the `--check`/bootstrap self-recognition path.
        let is_self = current_exe.as_deref() == Some(binary.as_path());
        if !is_self {
            let Some(bound) = &manifest else {
                continue;
            };
            // Digest before any exec: `binary_closure` runs the binary.
            let Ok(digest) = sha256_file(&binary) else {
                continue;
            };
            if digest != bound.binary_sha256 {
                continue;
            }
        }
        let Ok(reported) = binary_closure(&binary) else {
            continue;
        };
        if reported != wanted {
            continue;
        }
        let differences =
            render_and_compare(&binary, checkout, tree, scratch, default_branch, excludes)?;
        if differences.is_empty() {
            return Ok(Some(reported));
        }
    }
    Ok(None)
}

fn render_and_compare(
    binary: &Path,
    checkout: &Path,
    root: &Path,
    scratch: &Path,
    default_branch: &str,
    excludes: &BTreeSet<String>,
) -> Result<Vec<String>, GeneratorError> {
    let output = Command::new(binary)
        .arg(checkout)
        .arg("--output")
        .arg(scratch)
        .args(["--plain", "--force", "--default-branch", default_branch])
        .current_dir(checkout)
        .output()
        .map_err(|error| {
            GeneratorError::usage(format!(
                "run {} to regenerate the tree: {error}",
                binary.display()
            ))
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = if detail.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout).into_owned()
        } else {
            detail.into_owned()
        };
        return Err(GeneratorError::usage(format!(
            "regeneration with {} failed: {}",
            binary.display(),
            detail.trim()
        )));
    }
    let mut rendered = BTreeMap::new();
    collect_files(scratch, scratch, &mut rendered)?;
    let mut differences = Vec::new();
    for (relative, content) in &rendered {
        let actual = root.join(relative);
        match fs::read(&actual) {
            Ok(bytes) if &bytes == content => {}
            Ok(_) => differences.push(format!(
                "{}: differs from the pinned render",
                relative.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                differences.push(format!("{}: missing from the tree", relative.display()));
            }
            Err(error) => return Err(GeneratorError::io("read tree file", &actual, &error)),
        }
    }
    let workflows = root.join(".github/workflows");
    if let Ok(entries) = fs::read_dir(&workflows) {
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
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if excludes.contains(name) {
                continue;
            }
            let relative = PathBuf::from(".github/workflows").join(name);
            if !rendered.contains_key(&relative) {
                differences.push(format!(
                    "{}: not generator-owned (hand-written workflows are refused; list it under [policy] exclude_workflows only while migrating)",
                    relative.display()
                ));
            }
        }
    }
    differences.sort();
    Ok(differences)
}

fn collect_files(
    base: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), GeneratorError> {
    for entry in fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read rendered directory", directory, &error))?
    {
        let path = entry
            .map_err(|error| GeneratorError::usage(format!("read rendered entry: {error}")))?
            .path();
        if path.is_dir() {
            collect_files(base, &path, files)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();
            let content = fs::read(&path)
                .map_err(|error| GeneratorError::io("read rendered file", &path, &error))?;
            files.insert(relative, content);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Required status-check contexts
// ---------------------------------------------------------------------------

fn required_checks(root: &Path, declared: &DeclaredTree, live: Option<&[String]>) -> RuleReport {
    let mut findings = Vec::new();
    let aggregate = root.join(PULL_REQUEST_AGGREGATE);
    let emitted = match fs::read_to_string(&aggregate) {
        Ok(yaml) => match super::workflow_job_display_names(&yaml) {
            Ok(names) => Some(names),
            Err(error) => {
                findings.push(format!("{PULL_REQUEST_AGGREGATE}: {error}"));
                None
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            findings.push(format!("{PULL_REQUEST_AGGREGATE}: {error}"));
            None
        }
    };
    match &emitted {
        Some(names) => {
            for context in &declared.required_checks {
                if !names.contains(context) {
                    findings.push(format!(
                        "{PULL_REQUEST_AGGREGATE} emits no job named `{context}` (required by the ruleset)"
                    ));
                }
            }
        }
        None if !declared.required_checks.is_empty() => {
            findings.push(format!(
                "{PULL_REQUEST_AGGREGATE} is absent but the ruleset requires [{}]",
                declared.required_checks.join(", ")
            ));
        }
        None => {}
    }
    let mut summary = format!(
        "{PULL_REQUEST_AGGREGATE} emits [{}]",
        declared.required_checks.join(", ")
    );
    if let Some(live) = live {
        let live: BTreeSet<&str> = live.iter().map(String::as_str).collect();
        // The entrypoint's own job is the gate this validator runs behind; a
        // ruleset that does not require it makes every rule here advisory.
        let entrypoint_contexts = fs::read_to_string(root.join(POLICY_ENTRYPOINT))
            .ok()
            .and_then(|yaml| super::workflow_job_display_names(&yaml).ok())
            .unwrap_or_default();
        for context in &entrypoint_contexts {
            if !live.contains(context.as_str()) {
                findings.push(format!(
                    "the live ruleset does not require the policy entrypoint context `{context}`; the gate is advisory until the ruleset requires it"
                ));
            }
        }
        let declared_all: BTreeSet<&str> = declared
            .required_checks
            .iter()
            .chain(&declared.external_checks)
            .chain(&entrypoint_contexts)
            .map(String::as_str)
            .collect();
        for context in live.difference(&declared_all) {
            findings.push(format!(
                "ruleset requires `{context}` but the tree declares it neither in [policy] ruleset_required_status_checks nor ruleset_external_status_checks"
            ));
        }
        for context in declared_all.difference(&live) {
            // The entrypoint's own contexts were already reported above.
            if entrypoint_contexts.iter().any(|name| name == context) {
                continue;
            }
            findings.push(format!(
                "the tree declares ruleset context `{context}` but the live ruleset does not require it"
            ));
        }
        let _ = write!(
            summary,
            "; live ruleset requires [{}]",
            live.iter().copied().collect::<Vec<_>>().join(", ")
        );
    } else {
        summary.push_str("; no live ruleset supplied (--ruleset-contexts)");
    }
    RuleReport::from_findings("required-checks", &summary, findings)
}

// ---------------------------------------------------------------------------
// The policy entrypoint
// ---------------------------------------------------------------------------

/// Findings about `ci-policy.yml` itself, split by the rule they feed.
#[derive(Default)]
struct EntrypointAudit {
    trigger: Vec<String>,
    privileges: Vec<String>,
}

fn audit_policy_entrypoint(
    root: &Path,
    velnor_policy: &VelnorPolicyContract,
) -> Result<EntrypointAudit, GeneratorError> {
    let path = root.join(POLICY_ENTRYPOINT);
    let mut audit = EntrypointAudit::default();
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            audit.trigger.push(format!(
                "{POLICY_ENTRYPOINT}: the base-owned policy entrypoint is missing"
            ));
            return Ok(audit);
        }
        Err(error) => return Err(GeneratorError::io("read policy entrypoint", &path, &error)),
    };
    let parser = serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
    let document: Value = match serde_yaml::from_str_with_config(&content, &parser) {
        Ok(document) => document,
        Err(error) => {
            audit
                .trigger
                .push(format!("{POLICY_ENTRYPOINT}: parse: {error}"));
            return Ok(audit);
        }
    };
    let Some(workflow) = document.as_mapping() else {
        audit.trigger.push(format!(
            "{POLICY_ENTRYPOINT}: workflow document must be a YAML mapping"
        ));
        return Ok(audit);
    };
    audit_entrypoint_triggers(workflow, &mut audit);
    audit_entrypoint_privileges(workflow, &content, velnor_policy, &mut audit);
    Ok(audit)
}

/// Trigger set: `pull_request_target` with the reviewed activity types, plus
/// `workflow_dispatch` so the validator can be proven on the base branch.
fn audit_entrypoint_triggers(workflow: &Mapping, audit: &mut EntrypointAudit) {
    let finding = |message: &str| format!("{POLICY_ENTRYPOINT}: {message}");
    match mapping_value(workflow, "on").and_then(Value::as_mapping) {
        Some(on) => {
            for key in on.keys() {
                if !matches!(key.as_str(), "pull_request_target" | "workflow_dispatch") {
                    audit.trigger.push(finding(&format!(
                        "trigger `{key}` is not admitted; the entrypoint runs only on pull_request_target and workflow_dispatch"
                    )));
                }
            }
            match mapping_value(on, "pull_request_target").and_then(Value::as_mapping) {
                Some(event) => {
                    let expected = ["opened", "synchronize", "reopened"];
                    let types = mapping_value(event, "types")
                        .and_then(Value::as_sequence)
                        .map(|types| types.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                        .unwrap_or_default();
                    if types != expected {
                        audit.trigger.push(finding(&format!(
                            "pull_request_target types must be [{}], got [{}]",
                            expected.join(", "),
                            types.join(", ")
                        )));
                    }
                }
                None => audit.trigger.push(finding(
                    "must run on pull_request_target with explicit activity types",
                )),
            }
        }
        None => audit
            .trigger
            .push(finding("`on` must be a mapping of triggers")),
    }
}

/// Privileges: `contents: read` at both levels, no secrets, no persisted
/// credentials, one job on a hosted or trust-gated approved runner.
fn audit_entrypoint_privileges(
    workflow: &Mapping,
    content: &str,
    velnor_policy: &VelnorPolicyContract,
    audit: &mut EntrypointAudit,
) {
    let finding = |message: &str| format!("{POLICY_ENTRYPOINT}: {message}");
    if !is_contents_read_only(mapping_value(workflow, "permissions")) {
        audit.privileges.push(finding(
            "workflow permissions must be exactly `contents: read`",
        ));
    }
    let references_secrets = content.lines().any(|line| line.contains("secrets."));
    if references_secrets {
        audit
            .privileges
            .push(finding("must not reference `secrets.`"));
    }
    match mapping_value(workflow, "jobs").and_then(Value::as_mapping) {
        Some(jobs) if jobs.len() == 1 => {
            let Some((job_id, job)) = jobs.iter().next() else {
                return;
            };
            let Some(job) = job.as_mapping() else {
                audit
                    .privileges
                    .push(finding(&format!("job {job_id} must be a YAML mapping")));
                return;
            };
            if !is_contents_read_only(mapping_value(job, "permissions")) {
                audit.privileges.push(finding(&format!(
                    "job {job_id} permissions must be exactly `contents: read`"
                )));
            }
            if mapping_value(job, "environment").is_some() {
                audit.privileges.push(finding(&format!(
                    "job {job_id} must not bind a deployment environment"
                )));
            }
            let steps = mapping_value(job, "steps")
                .and_then(Value::as_sequence)
                .cloned()
                .unwrap_or_default();
            for step in &steps {
                let Some(step) = step.as_mapping() else {
                    continue;
                };
                let uses = mapping_value(step, "uses")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if uses.starts_with("actions/checkout@") {
                    let persists = mapping_value(step, "with")
                        .and_then(Value::as_mapping)
                        .and_then(|with| mapping_value(with, "persist-credentials"))
                        .and_then(Value::as_bool);
                    if persists != Some(false) {
                        audit.privileges.push(finding(&format!(
                            "job {job_id} checkout must set `persist-credentials: false`"
                        )));
                    }
                }
            }
            match mapping_value(job, "runs-on") {
                Some(runs_on) => {
                    let mut resolving = BTreeSet::new();
                    let analysis = analyze_runner(runs_on, None, &mut resolving, velnor_policy);
                    if analysis.dynamic || analysis.invalid {
                        audit.privileges.push(finding(&format!(
                            "job {job_id} runs-on must be static labels"
                        )));
                    } else if analysis.local_provider {
                        let gated = mapping_value(job, "if")
                            .and_then(Value::as_str)
                            .is_some_and(|condition| {
                                has_safe_runner_gate(condition, job, velnor_policy)
                            });
                        if !gated {
                            audit.privileges.push(finding(&format!(
                                "job {job_id} runs on a local provider without a trusted-event gate"
                            )));
                        }
                    } else if analysis.foreign {
                        audit.privileges.push(finding(&format!(
                            "job {job_id} runs-on does not match any declared provider selector"
                        )));
                    }
                }
                None => audit
                    .privileges
                    .push(finding(&format!("job {job_id} declares no runs-on"))),
            }
        }
        Some(jobs) => audit.privileges.push(finding(&format!(
            "must declare exactly one job, found {}",
            jobs.len()
        ))),
        None => audit.privileges.push(finding("`jobs` must be a mapping")),
    }
}

fn is_contents_read_only(permissions: Option<&Value>) -> bool {
    permissions
        .and_then(Value::as_mapping)
        .is_some_and(|permissions| {
            permissions.len() == 1
                && mapping_value(permissions, "contents").and_then(Value::as_str) == Some("read")
        })
}

// ---------------------------------------------------------------------------
// Semantic workflow audit
// ---------------------------------------------------------------------------

/// The semantic findings over every workflow in a tree, bucketed by rule.
#[derive(Debug, Default)]
pub(crate) struct WorkflowAudit {
    pub(crate) pull_request_target: Vec<String>,
    pub(crate) runners: Vec<String>,
    pub(crate) actions: Vec<String>,
    pub(crate) structure: Vec<String>,
}

#[derive(Clone, Copy)]
enum Rule {
    PullRequestTarget,
    TrustedRunners,
    ActionPins,
    Structure,
}

/// Findings recorded while auditing one tree.
#[derive(Default)]
struct PolicyFindings {
    audit: WorkflowAudit,
    /// The tree root: findings name workflows relative to it.
    root: PathBuf,
    /// The job under inspection, so a finding names where it was made.
    job: Option<String>,
}

impl PolicyFindings {
    fn record(&mut self, rule: Rule, path: &Path, message: &str) {
        let path = path.strip_prefix(&self.root).unwrap_or(path).display();
        let line = match &self.job {
            Some(job) => format!("{path}: job {job}: {message}"),
            None => format!("{path}: {message}"),
        };
        match rule {
            Rule::PullRequestTarget => self.audit.pull_request_target.push(line),
            Rule::TrustedRunners => self.audit.runners.push(line),
            Rule::ActionPins => self.audit.actions.push(line),
            Rule::Structure => self.audit.structure.push(line),
        }
    }
}

/// Audit every workflow under `root/.github/workflows` with the validator's
/// semantic rules. The policy entrypoint is exempt from the
/// `pull_request_target` rule here; [`audit_policy_entrypoint`] judges it.
///
/// # Errors
/// When the workflow directory or a workflow file cannot be read.
pub(crate) fn audit_workflows(root: &Path) -> Result<WorkflowAudit, GeneratorError> {
    let workflows = root.join(".github/workflows");
    let entries = fs::read_dir(&workflows)
        .map_err(|error| GeneratorError::io("read workflow directory", &workflows, &error))?;
    let policy_entrypoint = workflows.join("ci-policy.yml");
    let policy_excludes = configured_policy_excludes(root);
    let velnor_policy = configured_velnor_policy(root)?;
    let mut findings = PolicyFindings {
        root: root.to_path_buf(),
        ..PolicyFindings::default()
    };
    // GitHub rejects a workflow whose YAML carries duplicate keys, so the
    // auditor must fail closed on exactly the inputs GitHub refuses instead of
    // silently auditing the last-key-wins rewrite of them.
    let parser = serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| GeneratorError::usage(format!("read workflow entry: {error}")))?
            .path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("yml" | "yaml")
            )
        {
            paths.push(path);
        }
    }
    paths.sort();
    for path in paths {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| policy_excludes.contains(name))
        {
            continue;
        }
        let content = fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read workflow", &path, &error))?;
        let document: Value = match serde_yaml::from_str_with_config(&content, &parser) {
            Ok(document) => document,
            Err(error) => {
                findings.record(Rule::Structure, &path, &format!("parse workflow: {error}"));
                continue;
            }
        };
        let Some(workflow) = document.as_mapping() else {
            findings.record(
                Rule::Structure,
                &path,
                "workflow document must be a YAML mapping",
            );
            continue;
        };
        inspect_workflow(
            workflow,
            &path,
            path == policy_entrypoint,
            &velnor_policy,
            &mut findings,
        );
    }
    Ok(findings.audit)
}

fn inspect_workflow(
    workflow: &Mapping,
    path: &Path,
    is_policy_entrypoint: bool,
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    for (key, value) in workflow {
        let key = key.as_str();
        match key {
            "on" => {
                if contains_exact_yaml_value(value, "pull_request_target") && !is_policy_entrypoint
                {
                    failures.record(
                        Rule::PullRequestTarget,
                        path,
                        "pull_request_target is forbidden outside the policy entrypoint",
                    );
                }
            }
            "jobs" => inspect_jobs(value, path, velnor_policy, failures),
            _ => inspect_yaml_value(value, path, None, false, velnor_policy, failures),
        }
    }
}

/// The local provider a static `runs-on` resolves to, if any. Labels are
/// compared as sets against the declared selectors; anything that matches
/// no selector is not a local job (it is a finding elsewhere).
fn static_local_provider(job: &Mapping, velnor_policy: &VelnorPolicyContract) -> Option<String> {
    let runs_on = mapping_value(job, "runs-on")?;
    let labels = match runs_on {
        Value::String(label) => vec![label.as_str()],
        Value::Sequence(sequence) => sequence
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?,
        Value::Mapping(mapping) => mapping_value(mapping, "labels")?
            .as_sequence()?
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    if labels.iter().any(|label| label.contains("${{")) {
        return None;
    }
    let provider = velnor_policy.provider_for_labels(&labels)?;
    VelnorPolicyContract::is_local_provider(provider).then(|| provider.to_owned())
}

/// A GitHub-owned execution label: inherently hosted, never a trust fact.
/// Selectors for local capacity are caller-managed and never carry these
/// prefixes.
fn is_github_owned_label(label: &str) -> bool {
    label.starts_with("ubuntu-") || label.starts_with("macos-") || label.starts_with("windows-")
}

fn has_safe_runner_gate(
    condition: &str,
    job: &Mapping,
    velnor_policy: &VelnorPolicyContract,
) -> bool {
    if has_trusted_runner_gate(condition) {
        return true;
    }
    // Generated provider jobs admit a provider-selecting dispatch on any ref
    // (dispatch needs write access on a ref of this repository) plus the
    // automatic events, with the trusted-event conjunct for local providers
    // and trusted-only units. That generated shape is trusted even when the
    // advisory checkout lacks `.github/ci/project.toml` and only carries
    // `.github-gen`.
    let Some(provider) = static_local_provider(job, velnor_policy) else {
        return false;
    };
    is_generated_provider_gate(condition, &provider)
}

fn generation_workflow(root: &Path) -> Result<Option<toml::Value>, GeneratorError> {
    let path = root.join(GENERATION_CONFIG);
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

fn configured_policy_excludes(root: &Path) -> BTreeSet<String> {
    config::discover(root)
        .ok()
        .flatten()
        .map(|config| config.effective_policy_exclude_workflows())
        .unwrap_or_default()
}

fn toml_string_array(
    value: Option<&toml::Value>,
    field: &str,
) -> Result<Vec<String>, GeneratorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| GeneratorError::usage(format!("{field} must be an array")))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| GeneratorError::usage(format!("{field} must contain only strings")))
        })
        .collect()
}

/// The provider facts the audit needs from the tree's configuration: the
/// provider universe, the automatic set, and the per-provider selectors.
/// Labels carry no authority here; they only name which declared selector a
/// job's `runs-on` resolves to.
#[derive(Clone, Debug, Default)]
struct VelnorPolicyContract {
    providers: Vec<String>,
    automatic_providers: Vec<String>,
    selectors: BTreeMap<String, Vec<String>>,
    default_branch: String,
}

impl VelnorPolicyContract {
    /// The provider whose declared selector `labels` equals, if any. Labels
    /// are compared as sets: order is not a routing fact.
    fn provider_for_labels(&self, labels: &[&str]) -> Option<&str> {
        let mut sorted = labels.to_vec();
        sorted.sort_unstable();
        self.selectors.iter().find_map(|(provider, selector)| {
            let mut expected = selector.iter().map(String::as_str).collect::<Vec<_>>();
            expected.sort_unstable();
            (expected == sorted).then_some(provider.as_str())
        })
    }

    fn is_local_provider(provider: &str) -> bool {
        matches!(provider, "github-self-hosted" | "velnor")
    }
}

fn configured_velnor_policy(root: &Path) -> Result<VelnorPolicyContract, GeneratorError> {
    let path = root.join(RUNTIME_CONFIG);
    // Advisory sparse-checkout omits `.github/ci`. Do not default the
    // contract on a missing project.toml: the generation config still
    // carries providers and selectors.
    let runtime = match fs::read_to_string(&path) {
        Ok(content) => Some(toml::from_str::<toml::Value>(&content).map_err(|error| {
            GeneratorError::usage(format!("parse workflow config {}: {error}", path.display()))
        })?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(GeneratorError::io("read workflow config", &path, &error)),
    };
    let generation = generation_workflow(root)?;
    if runtime.is_none() && generation.is_none() {
        return Ok(VelnorPolicyContract {
            default_branch: "main".to_owned(),
            ..VelnorPolicyContract::default()
        });
    }
    let generation_workflow = generation.as_ref().and_then(toml::Value::as_table);
    let mut providers = runtime
        .as_ref()
        .and_then(|value| value.get("providers"))
        .map(|value| toml_string_array(Some(value), "providers"))
        .transpose()?
        .unwrap_or_default();
    if providers.is_empty() {
        providers = toml_string_array(
            generation_workflow.and_then(|workflow| workflow.get("providers")),
            "[workflow] providers",
        )?;
    }
    let automatic_providers = runtime
        .as_ref()
        .and_then(|value| value.get("automatic_providers"))
        .map(|value| toml_string_array(Some(value), "automatic_providers"))
        .transpose()?
        .unwrap_or_default();
    // Selectors are generation-time only: the runtime contract never carries
    // them. Generation overlays declared selectors on the scan defaults, so
    // the audit starts from the same defaults; otherwise a default-routed
    // job reads as foreign.
    let mut selectors: BTreeMap<String, Vec<String>> = super::scan::default_selectors()
        .into_iter()
        .map(|(provider, selector)| (provider.as_str().to_owned(), selector.runs_on))
        .collect();
    if let Some(tables) = generation_workflow
        .and_then(|workflow| workflow.get("selectors"))
        .and_then(toml::Value::as_table)
    {
        for (provider, table) in tables {
            let runs_on = toml_string_array(
                table.get("runs_on"),
                &format!("[workflow.selectors.{provider}] runs_on"),
            )?;
            if !runs_on.is_empty() {
                selectors.insert(provider.clone(), runs_on);
            }
        }
    }
    let default_branch = runtime
        .as_ref()
        .and_then(|value| value.get("default_branch"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            generation_workflow
                .and_then(|workflow| workflow.get("default_branch"))
                .and_then(toml::Value::as_str)
        })
        .unwrap_or("main")
        .to_owned();
    let policy = VelnorPolicyContract {
        providers,
        automatic_providers,
        selectors,
        default_branch,
    };
    policy.validate()?;
    Ok(policy)
}

impl VelnorPolicyContract {
    /// The audit trusts the configuration's provider facts, so it validates
    /// them first: canonical ids only, the automatic set inside the universe,
    /// and a selector for every provider in the universe.
    fn validate(&self) -> Result<(), GeneratorError> {
        for provider in self.providers.iter().chain(&self.automatic_providers) {
            if !matches!(
                provider.as_str(),
                "github-hosted" | "github-self-hosted" | "velnor"
            ) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy found unknown provider `{provider}` in the configured provider sets"
                )));
            }
        }
        for provider in &self.automatic_providers {
            if !self.providers.iter().any(|known| known == provider) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy found automatic provider `{provider}` outside the configured provider universe"
                )));
            }
        }
        for (provider, selector) in &self.selectors {
            if selector.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "workflow policy found provider `{provider}` with an empty selector"
                )));
            }
        }
        Ok(())
    }
}

fn normalize_gate_expression(value: &str) -> String {
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

/// The normalized trusted-event conjunct every local-provider admission
/// carries: fork and bot pull requests are untrusted, everything else is
/// trusted.
fn trusted_event_conjunct() -> &'static str {
    "!(github.event_name=='pull_request'&&(github.event.pull_request.head.repo.fork||github.event.pull_request.user.type=='Bot'))"
}

/// Whether `value` is the generated admission for `provider`: a
/// provider-selecting dispatch (`inputs.providers` comma-boundary match) or
/// the automatic events, with the trusted-event conjunct. The reusable
/// provider/unit selector prefix is stripped first; the remaining
/// expression must be exactly the admission the generator renders.
fn is_generated_provider_gate(value: &str, provider: &str) -> bool {
    let normalized = normalize_gate_expression(value);
    let value = strip_reusable_unit_selector(&normalized).unwrap_or(&normalized);
    let dispatch = format!(
        "github.event_name=='workflow_dispatch'&&contains(format(',{{0}},',github.event.inputs.providers),',{provider},')"
    );
    let trusted = trusted_event_conjunct();
    // The generator renders `((dispatch) || (automatic)) && (trusted)` for
    // local providers: the automatic side is the event predicate when the
    // provider is in the automatic set, `false` otherwise.
    let automatic_shapes = ["github.event_name!='workflow_dispatch'", "false"];
    automatic_shapes.into_iter().any(|automatic| {
        let event = format!("({dispatch})||({automatic})");
        value == format!("({event})&&({trusted})") || value == format!("{event}&&({trusted})")
    })
}

fn inspect_jobs(
    value: &Value,
    path: &Path,
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    let Some(jobs) = value.as_mapping() else {
        failures.record(Rule::Structure, path, "jobs must be a YAML mapping");
        return;
    };
    for (job_id, job) in jobs {
        let Some(job) = job.as_mapping() else {
            let name = job_id.as_str();
            failures.record(
                Rule::Structure,
                path,
                &format!("job {name} must be a YAML mapping"),
            );
            continue;
        };
        let trusted_gate = mapping_value(job, "if")
            .and_then(Value::as_str)
            .is_some_and(|condition| has_safe_runner_gate(condition, job, velnor_policy));
        let matrix = mapping_value(job, "strategy")
            .and_then(Value::as_mapping)
            .and_then(|strategy| mapping_value(strategy, "matrix"))
            .and_then(Value::as_mapping);
        failures.job = Some(job_id.clone());
        audit_job_level_env(job, path, failures);
        inspect_mapping(job, path, matrix, trusted_gate, velnor_policy, failures);
        failures.job = None;
    }
}

/// GitHub expression contexts forbidden in job-level `env:`.
///
/// Per the context-availability table
/// (<https://docs.github.com/en/actions/reference/workflows-and-actions/contexts#context-availability>),
/// `jobs.<job_id>.env` allows only `github, needs, strategy, matrix, vars,
/// secrets, inputs`, and "The listed contexts are only available for the given
/// workflow key, and may not be used anywhere else." `runner` is additionally
/// proven by a live rejection: a workflow with job-level `CARGO_HOME: ${{
/// runner.temp }}/...` concluded `failure` with 0 jobs and `Unrecognized
/// named-value: 'runner'`. `steps` is additionally proven by its section ("You
/// can access this context from any step in a job",
/// <https://docs.github.com/en/actions/reference/workflows-and-actions/contexts#steps-context>).
/// `matrix` and `strategy` are proven allowed (listed in the table and "from
/// any job or step" in their sections), so they are not flagged. `job`, `env`,
/// and `jobs` are also absent from the table but omitted from enforcement as
/// outside this rule's named scope.
const JOB_ENV_FORBIDDEN_CONTEXTS: &[&str] = &["runner", "steps"];

/// Reject forbidden contexts in a job's own `env:` mapping. Step-level `env:`
/// allows every context (including `runner` and `steps`), so only the job
/// mapping's direct `env` child is inspected; steps are left to
/// [`inspect_mapping`].
fn audit_job_level_env(job: &Mapping, path: &Path, failures: &mut PolicyFindings) {
    let Some(env) = mapping_value(job, "env").and_then(Value::as_mapping) else {
        return;
    };
    for (key, value) in env {
        let Some(text) = value.as_str() else {
            continue;
        };
        for expression in github_expressions(text) {
            if let Some(context) = expression_root_context(&expression)
                && JOB_ENV_FORBIDDEN_CONTEXTS.contains(&context.as_str())
            {
                failures.record(
                    Rule::Structure,
                    path,
                    &format!(
                        "job-level env `{}` uses forbidden `{context}` context: `${{{{ {expression} }}}}`",
                        key.as_str(),
                        expression = expression.trim(),
                    ),
                );
            }
        }
    }
}

/// Every `${{ ... }}` inner expression in `text`, in order.
fn github_expressions(text: &str) -> Vec<String> {
    let mut expressions = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${{") {
        let after_start = &rest[start + 3..];
        let Some(end) = after_start.find("}}") else {
            break;
        };
        expressions.push(after_start[..end].to_owned());
        rest = &after_start[end + 2..];
    }
    expressions
}

/// The root context of a `${{ ... }}` inner expression: the leading identifier
/// (`runner` in `runner.temp`, `steps` in `steps.prove.outputs.asset`,
/// `github` in `github.event.inputs.providers`). Function names (`toJson`), string
/// literals, and non-identifiers yield their leading token or `None`; callers
/// match against the forbidden list, so only a forbidden root flags.
fn expression_root_context(expression: &str) -> Option<String> {
    let mut chars = expression.trim().chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let mut root = String::from(first);
    for character in chars {
        if character.is_ascii_alphanumeric() || character == '_' {
            root.push(character);
        } else {
            break;
        }
    }
    Some(root)
}

fn inspect_yaml_value(
    value: &Value,
    path: &Path,
    matrix: Option<&Mapping>,
    trusted_gate: bool,
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    match value {
        Value::Mapping(mapping) => {
            inspect_mapping(mapping, path, matrix, trusted_gate, velnor_policy, failures);
        }
        Value::Sequence(sequence) => {
            for item in sequence {
                inspect_yaml_value(item, path, matrix, trusted_gate, velnor_policy, failures);
            }
        }
        Value::Tagged(tagged) => {
            inspect_yaml_value(
                tagged.value(),
                path,
                matrix,
                trusted_gate,
                velnor_policy,
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
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    for (key, value) in mapping {
        let key = key.as_str();
        match key {
            "pull_request_target" => {
                failures.record(
                    Rule::PullRequestTarget,
                    path,
                    "pull_request_target is forbidden outside the policy entrypoint",
                );
            }
            "uses" => inspect_uses(value, path, failures),
            "runs-on" => inspect_runner(value, path, matrix, trusted_gate, velnor_policy, failures),
            _ => inspect_yaml_value(value, path, matrix, trusted_gate, velnor_policy, failures),
        }
    }
}

fn inspect_uses(value: &Value, path: &Path, failures: &mut PolicyFindings) {
    let Some(action) = value.as_str() else {
        failures.record(Rule::ActionPins, path, "uses must be a scalar reference");
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
                Rule::ActionPins,
                path,
                &format!(
                    "same-repository reusable workflow call at a pinned revision wedges push/schedule scheduling; inline the approved steps instead: {action}"
                ),
            );
        } else {
            failures.record(
                Rule::ActionPins,
                path,
                &format!(
                    "reusable workflow must be an approved local generated workflow: {action}"
                ),
            );
        }
    } else if !is_full_sha_reference(action) {
        failures.record(
            Rule::ActionPins,
            path,
            &format!("action is not a full SHA pin: {action}"),
        );
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
        .is_some_and(|(action, reference)| !action.is_empty() && super::is_full_revision(reference))
}

/// A repository-local composite action, pinned by the audited tree itself:
/// the policy audits the pull-request head tree, so a tampered local action
/// is reviewed code exactly like an inline `run:` step, and a tampered
/// reference outside `.github/actions/` stays rejected below. The owner
/// policy job's setup composite resolves out of its sibling checkout
/// instead of the root (a root checkout would wipe `policy-checkout/`),
/// so that exact reference is reviewed too. The owner package publisher has
/// a separate, exact `source/` checkout for repository verification tasks;
/// arbitrary checkout-root action paths remain rejected.
fn is_approved_local_action(value: &str) -> bool {
    if matches!(
        value,
        super::VELNOR_WORKFLOW_POLICY_SETUP_ACTION | super::VELNOR_WORKFLOW_SOURCE_SETUP_ACTION
    ) {
        return true;
    }
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
    if !super::is_full_revision(reference) {
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
        && super::estate::FLEET_VELNOR_ACTION_OWNERS.contains(&owner)
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

#[derive(Clone, Copy, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "d2a shape: one flag per analysis outcome"
)]
struct RunnerAnalysis {
    /// Resolves to a local provider's declared selector.
    local_provider: bool,
    /// Static labels that match no declared selector and no GitHub-owned
    /// image: the job routes nowhere the config declares.
    foreign: bool,
    dynamic: bool,
    invalid: bool,
}

impl RunnerAnalysis {
    fn merge(&mut self, other: Self) {
        self.local_provider |= other.local_provider;
        self.foreign |= other.foreign;
        self.dynamic |= other.dynamic;
        self.invalid |= other.invalid;
    }
}

fn inspect_runner(
    value: &Value,
    path: &Path,
    matrix: Option<&Mapping>,
    trusted_gate: bool,
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    let mut resolving = BTreeSet::new();
    let analysis = analyze_runner(value, matrix, &mut resolving, velnor_policy);
    if analysis.invalid {
        failures.record(
            Rule::TrustedRunners,
            path,
            "runs-on must contain only static labels or a static runner-group mapping",
        );
    }
    if analysis.dynamic {
        failures.record(
            Rule::TrustedRunners,
            path,
            "runs-on contains an unresolved or dynamic runner label",
        );
    }
    if analysis.foreign {
        failures.record(
            Rule::TrustedRunners,
            path,
            "runs-on does not match any declared provider selector",
        );
    }
    if analysis.local_provider && !trusted_gate {
        failures.record(
            Rule::TrustedRunners,
            path,
            "local-provider jobs require a trusted-event gate",
        );
    }
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
    super::estate::APPROVED_DYNAMIC_RUNNERS
        .iter()
        .any(|shape| normalized == **shape)
}

/// Classify one static label: GitHub-owned images are hosted; a label that
/// equals a declared selector's whole set is that provider; anything else
/// static is foreign. Label substrings are never consulted.
fn classify_static_label(label: &str, velnor_policy: &VelnorPolicyContract) -> RunnerAnalysis {
    if is_github_owned_label(label) {
        return RunnerAnalysis::default();
    }
    match velnor_policy.provider_for_labels(&[label]) {
        Some(provider) if VelnorPolicyContract::is_local_provider(provider) => RunnerAnalysis {
            local_provider: true,
            ..RunnerAnalysis::default()
        },
        Some(_) => RunnerAnalysis::default(),
        None => RunnerAnalysis {
            foreign: true,
            ..RunnerAnalysis::default()
        },
    }
}

/// Classify one static label set: it must equal a declared selector as a
/// set, or be a single GitHub-owned image.
fn classify_static_labels(labels: &[&str], velnor_policy: &VelnorPolicyContract) -> RunnerAnalysis {
    if labels.len() == 1 {
        return classify_static_label(labels[0], velnor_policy);
    }
    match velnor_policy.provider_for_labels(labels) {
        Some(provider) if VelnorPolicyContract::is_local_provider(provider) => RunnerAnalysis {
            local_provider: true,
            ..RunnerAnalysis::default()
        },
        Some(_) => RunnerAnalysis::default(),
        None => RunnerAnalysis {
            foreign: true,
            ..RunnerAnalysis::default()
        },
    }
}

fn analyze_runner(
    value: &Value,
    matrix: Option<&Mapping>,
    resolving: &mut BTreeSet<String>,
    velnor_policy: &VelnorPolicyContract,
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
                    result.merge(analyze_runner(value, matrix, resolving, velnor_policy));
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
            } else {
                classify_static_label(label, velnor_policy)
            }
        }
        Value::Sequence(sequence) => {
            let Some(labels) = sequence
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()
            else {
                // Non-string sequence entries (nested mappings, numbers)
                // cannot route anywhere.
                return RunnerAnalysis {
                    invalid: true,
                    ..RunnerAnalysis::default()
                };
            };
            if labels.iter().any(|label| label.contains("${{")) {
                let mut result = RunnerAnalysis::default();
                for value in sequence {
                    result.merge(analyze_runner(value, matrix, resolving, velnor_policy));
                }
                return result;
            }
            classify_static_labels(&labels, velnor_policy)
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
                    "labels" => {
                        result.merge(analyze_runner(value, matrix, resolving, velnor_policy));
                    }
                    _ => result.invalid = true,
                }
            }
            result
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => RunnerAnalysis {
            invalid: true,
            ..RunnerAnalysis::default()
        },
        Value::Tagged(tagged) => analyze_runner(tagged.value(), matrix, resolving, velnor_policy),
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

fn has_trusted_runner_gate(value: &str) -> bool {
    let value = value.trim();
    let value = value
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map_or(value, str::trim)
        .split_whitespace()
        .collect::<String>();
    if value.ends_with("&&false") {
        return true;
    }
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
    if !runtime::valid_branch(branch) {
        return false;
    }
    let ci_gate = format!(
        "github.ref=='refs/heads/{branch}'&&(github.event_name=='push'||github.event_name=='schedule'||github.event_name=='workflow_dispatch')"
    );
    let release_gate = format!(
        "(github.event_name=='push'&&(github.ref_type=='tag'||github.ref=='refs/heads/{branch}'))||github.event_name=='schedule'||(github.event_name=='workflow_dispatch'&&github.ref=='refs/heads/{branch}')"
    );
    // The self-hosted writer gate (`trusted_renovate_gate`): scheduled runs
    // plus default-branch dispatches, and nothing else. The writer's `on:`
    // carries exactly these two triggers, so the gate admits every event the
    // workflow can start from and no event it cannot — push and pull_request
    // can never reach the job.
    let writer_gate = format!(
        "github.event_name=='schedule'||(github.event_name=='workflow_dispatch'&&github.ref=='refs/heads/{branch}')"
    );
    value == ci_gate
        || value == format!("always()&&{ci_gate}")
        || value == writer_gate
        || value == format!("always()&&{writer_gate}")
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
    let without_provider = strip_inputs_provider_selector(value);
    let value = without_provider.unwrap_or(value);
    strip_inputs_unit_membership_selector(value)
        .or_else(|| strip_selected_units_selector(value))
        .or_else(|| strip_combined_selected_units_selector(value))
        .or(without_provider)
}

/// Peel `inputs.provider == '<id>'|'control' &&` from a generated
/// reusable-job gate. The conjunct only restricts which caller intends the
/// job; the remaining expression must still be a trusted event gate.
fn strip_inputs_provider_selector(value: &str) -> Option<&str> {
    let rest = value.strip_prefix("inputs.provider=='")?;
    let separator = rest.find("'&&")?;
    let provider = &rest[..separator];
    if !matches!(
        provider,
        "github-hosted" | "github-self-hosted" | "velnor" | "control"
    ) {
        return None;
    }
    Some(&rest[separator + "'&&".len()..])
}

/// Peel the collapsed kind-reusable membership selector: the job runs for
/// the caller's `inputs.unit` when the plan selected it. The selector names
/// no unit id, so the callee stays O(1) in the kind's units; the remaining
/// expression must still be a trusted event gate.
fn strip_inputs_unit_membership_selector(value: &str) -> Option<&str> {
    const SELECTOR: &str =
        "contains(inputs.selected_units,format('\"unit_id\":\"{0}\"',inputs.unit))&&(";
    value.strip_prefix(SELECTOR)?.strip_suffix(')')
}

fn strip_selected_units_selector(value: &str) -> Option<&str> {
    // contains(inputs.selected_units,'"unit_id":"unit,"')&&(gate)
    const PREFIX: &str = "contains(inputs.selected_units,'\"unit_id\":\"";
    let rest = value.strip_prefix(PREFIX)?;
    let separator = rest.find("\"')&&(")?;
    let unit = &rest[..separator];
    if !runtime::is_unit_id(unit) {
        return None;
    }
    rest[separator + "\"')&&(".len()..].strip_suffix(')')
}

fn is_selected_units_selector(value: &str) -> bool {
    const PREFIX: &str = "contains(inputs.selected_units,'\"unit_id\":\"";
    let Some(rest) = value.strip_prefix(PREFIX) else {
        return false;
    };
    let Some(separator) = rest.find("\"')") else {
        return false;
    };
    runtime::is_unit_id(&rest[..separator])
}

fn strip_combined_selected_units_selector(value: &str) -> Option<&str> {
    // (contains(...'"unit_id":"a"')||contains(...'"unit_id":"b"'))&&(gate)
    // The FIRST boundary ends the selectors: unit ids cannot hold `)&&(`,
    // but the gate behind it (a provider admission) can.
    let value = value.strip_prefix('(')?;
    let split = value.find(")&&(")?;
    let selectors = &value[..split];
    let gate = value[split + 4..].strip_suffix(')')?;
    if selectors.is_empty() || gate.is_empty() {
        return None;
    }
    for selector in selectors.split("||") {
        if !is_selected_units_selector(selector) {
            return None;
        }
    }
    Some(gate)
}

fn is_safe_trusted_gate_conjunction(value: &str) -> bool {
    !value.is_empty() && !value.contains("||") && !value.contains("github.ref")
}

#[cfg(test)]
mod tests;

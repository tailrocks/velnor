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
//! Candidate source arrives as raw Git objects acquired by base-controlled
//! code. A separate permissions-free job builds and renders that source in a
//! disposable sandbox. This process treats its output as untrusted data: it
//! validates the source head and closure, hashes relative names and bytes,
//! then compares the output with the audited tree. It never executes
//! candidate code.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_yaml::{Mapping, Value};
use unicode_normalization::UnicodeNormalization as _;

use super::provider::normalize_runner_label;
use super::{
    closure as closure_identity, config, runtime, GeneratorError, ProjectConfig, SOURCE_CLOSURE,
    SOURCE_REVISION,
};

/// The generation config the audited tree declares itself with.
pub(crate) const GENERATION_CONFIG: &str = ".github-gen/velnor-workflow.toml";
/// The base-owned policy entrypoint: the only workflow allowed to run on
/// `pull_request_target`.
const POLICY_ENTRYPOINT: &str = ".github/workflows/ci-policy.yml";
/// The pull-request aggregate whose job display names are the ruleset's
/// status-check contexts.
const PULL_REQUEST_AGGREGATE: &str = ".github/workflows/ci-pr.yml";
/// Closure of the pinned policy binary, verified by the runtime provisioner.
pub use super::VELNOR_WORKFLOW_PINNED_BINARY_CLOSURE_ENV;
/// Names a `velnor-workflow` binary built at the pinned revision.
pub use super::VELNOR_WORKFLOW_PINNED_BINARY_ENV;
/// Source revision of the pinned policy binary, read from its verified manifest.
pub use super::VELNOR_WORKFLOW_PINNED_BINARY_REVISION_ENV;
/// Digest of the pinned policy binary, verified by the runtime provisioner.
pub use super::VELNOR_WORKFLOW_PINNED_BINARY_SHA256_ENV;
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
    /// The audited tree, materialized from the trusted source-object artifact.
    pub(crate) root: PathBuf,
    /// A separate full-history checkout used only for Git ancestry and pin
    /// lookup. Candidate worktree bytes are never read from this checkout.
    pub(crate) git_root: Option<PathBuf>,
    /// Trusted base-branch checkout used to anchor provider policy.
    pub(crate) trusted_root: Option<PathBuf>,
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
    /// Artifact directory produced by the isolated candidate-render job.
    pub(crate) candidate_render: Option<PathBuf>,
    /// Source-object artifact produced by the trusted acquisition job.
    pub(crate) candidate_source: Option<PathBuf>,
    /// The workflow repository hosting the run that produced the artifact.
    pub(crate) candidate_repository: Option<String>,
    /// Repository containing the immutable source commit that was rendered.
    pub(crate) candidate_source_repository: Option<String>,
    /// The source workflow run whose candidate artifact is being checked.
    pub(crate) candidate_run_id: Option<String>,
    /// The platform string from the candidate producer manifest.
    pub(crate) candidate_platform: Option<String>,
    /// Digest-pinned builder image used for the isolated candidate render.
    pub(crate) candidate_builder_image: Option<String>,
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
/// [--candidate-render PATH] [--candidate-repository SLUG]
/// [--candidate-run-id ID] [--candidate-platform PLATFORM]`.
///
/// `--pin-build` is a local-development and bootstrap escape hatch: without
/// it an unprovisioned pin fails closed instead of compiling from source.
/// `--candidate-render` supplies the output artifact from the isolated
/// candidate-render job. Policy validates its identity and digest as data;
/// it never executes the candidate binary.
///
/// # Errors
/// Usage errors, unreadable inputs, and a failed evaluation (the rendered
/// report is printed first; the error names the failing rules).
pub(crate) fn run_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("snapshot-source") => return snapshot_source_cli(&arguments[1..]),
        Some("materialize-source") => return materialize_source_cli(&arguments[1..]),
        Some("validate-cargo-sources") => return validate_cargo_sources_cli(&arguments[1..]),
        Some("seal-source-vendor") => return seal_source_vendor_cli(&arguments[1..]),
        Some("seal-candidate-render") => return seal_candidate_render_cli(&arguments[1..]),
        _ => {}
    }
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
                | "candidate-render"
                | "candidate-source"
                | "candidate-repository"
                | "candidate-source-repository"
                | "candidate-run-id"
                | "candidate-platform"
                | "candidate-builder-image"
                | "git-root"
                | "trusted-root"
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
    let candidate_render = options.get("candidate-render").map(PathBuf::from);
    let report = evaluate(&PolicyOptions {
        root,
        git_root: options.get("git-root").map(PathBuf::from),
        trusted_root: options.get("trusted-root").map(PathBuf::from),
        head_sha: options.get("head-sha").cloned(),
        base_sha: options.get("base-sha").cloned(),
        base_revision,
        ruleset_contexts,
        build_pin,
        candidate_render,
        candidate_source: options.get("candidate-source").map(PathBuf::from),
        candidate_repository: options.get("candidate-repository").cloned(),
        candidate_source_repository: options
            .get("candidate-source-repository")
            .cloned(),
        candidate_run_id: options.get("candidate-run-id").cloned(),
        candidate_platform: options.get("candidate-platform").cloned(),
        candidate_builder_image: options.get("candidate-builder-image").cloned(),
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

/// Evaluate every rule against `options.root`.
///
/// # Errors
/// Only when an input cannot be read or a tool cannot run; policy violations
/// are `FAIL` rules in the returned report.
pub(crate) fn evaluate(options: &PolicyOptions) -> Result<PolicyReport, GeneratorError> {
    validate_candidate_policy_options(options)?;
    let root = options
        .root
        .canonicalize()
        .map_err(|error| GeneratorError::io("canonicalize workflow root", &options.root, &error))?;
    let trusted_root = options
        .trusted_root
        .as_deref()
        .unwrap_or(&root)
        .canonicalize()
        .map_err(|error| {
            GeneratorError::io(
                "canonicalize trusted policy root",
                options.trusted_root.as_deref().unwrap_or(&root),
                &error,
            )
        })?;
    // Bind the candidate tree to immutable Git objects before reading any
    // candidate-controlled config or workflow file. In particular, symlink
    // containment must be proven before TOML/YAML readers follow a link.
    let candidate_source = if let Some(source_artifact) = options.candidate_source.as_deref() {
        let git_root = options.git_root.as_deref().ok_or_else(|| {
            GeneratorError::usage("--git-root is required with --candidate-source")
        })?;
        let head = options.head_sha.as_deref().ok_or_else(|| {
            GeneratorError::usage("--head-sha is required with --candidate-source")
        })?;
        let base = options.base_sha.as_deref().ok_or_else(|| {
            GeneratorError::usage("--base-sha is required with --candidate-source")
        })?;
        let repository = options.candidate_repository.as_deref().ok_or_else(|| {
            GeneratorError::usage("--candidate-repository is required with --candidate-source")
        })?;
        let source_repository = options
            .candidate_source_repository
            .as_deref()
            .ok_or_else(|| {
                GeneratorError::usage(
                    "--candidate-source-repository is required with --candidate-source",
                )
            })?;
        let run_id = options.candidate_run_id.as_deref().ok_or_else(|| {
            GeneratorError::usage("--candidate-run-id is required with --candidate-source")
        })?;
        let closure = verify_candidate_source_artifact(
            source_artifact,
            git_root,
            &root,
            head,
            base,
            repository,
            source_repository,
            run_id,
        )?;
        Some((head.to_owned(), closure))
    } else {
        None
    };
    let declared = DeclaredTree::read(&root, &trusted_root)?;
    let mut report = PolicyReport::default();
    if let Some((head, closure)) = candidate_source {
        report.rules.push(RuleReport::pass(
            "candidate-source",
            format!("base-acquired raw Git objects bind {head} to closure {closure}"),
        ));
    }
    let pin = declared_pin_rule(&declared, &mut report);
    if let Some(expected_repository) = options.candidate_repository.as_deref() {
        report.rules.push(match declared.repository.as_deref() {
            Some(repository) if repository == expected_repository => RuleReport::pass(
                "repository-identity",
                format!("generation config targets {expected_repository}"),
            ),
            Some(repository) => RuleReport::fail(
                "repository-identity",
                format!("generation config targets {repository}, run is {expected_repository}"),
                Vec::new(),
            ),
            None => RuleReport::fail(
                "repository-identity",
                format!("generation config omits its repository identity; run is {expected_repository}"),
                Vec::new(),
            ),
        });
    }
    pin_rules(&root, &trusted_root, &declared, pin.as_deref(), options, &mut report);
    semantic_rules(&root, &declared, options, &mut report)?;
    Ok(report)
}

fn validate_candidate_policy_options(options: &PolicyOptions) -> Result<(), GeneratorError> {
    let candidate_identity_fields = [
        options.candidate_repository.is_some(),
        options.candidate_source_repository.is_some(),
        options.candidate_run_id.is_some(),
    ];
    let candidate_mode = options.candidate_source.is_some() || options.candidate_render.is_some();
    if !candidate_mode {
        if candidate_identity_fields.iter().any(|present| *present)
            || options.candidate_platform.is_some()
            || options.candidate_builder_image.is_some()
        {
            return Err(GeneratorError::usage(
                "candidate identity options require --candidate-source",
            ));
        }
        return Ok(());
    }
    if options.candidate_source.is_none() {
        return Err(GeneratorError::usage(
            "candidate verification requires --candidate-source",
        ));
    }
    if options.git_root.is_none() || options.trusted_root.is_none() {
        return Err(GeneratorError::usage(
            "candidate verification requires separate --git-root and --trusted-root checkouts",
        ));
    }
    let repository = options.candidate_repository.as_deref().ok_or_else(|| {
        GeneratorError::usage("--candidate-repository is required with --candidate-source")
    })?;
    let source_repository = options
        .candidate_source_repository
        .as_deref()
        .ok_or_else(|| {
            GeneratorError::usage(
                "--candidate-source-repository is required with --candidate-source",
            )
        })?;
    let run_id = options
        .candidate_run_id
        .as_deref()
        .ok_or_else(|| GeneratorError::usage("--candidate-run-id is required with --candidate-source"))?;
    let head = options.head_sha.as_deref().ok_or_else(|| {
        GeneratorError::usage("--head-sha is required with --candidate-source")
    })?;
    let base = options.base_sha.as_deref().ok_or_else(|| {
        GeneratorError::usage("--base-sha is required with --candidate-source")
    })?;
    validate_source_identity(repository, source_repository, run_id, head, base)?;
    match (
        options.candidate_render.is_some(),
        options.candidate_platform.as_deref(),
        options.candidate_builder_image.as_deref(),
    ) {
        (false, None, None) => {}
        (false, _, _) => {
            return Err(GeneratorError::usage(
                "candidate platform and builder are valid only with --candidate-render",
            ));
        }
        (true, Some("Linux-X64"), Some(builder)) if is_digest_pinned_image(builder) => {}
        (true, platform, Some(builder)) if !is_digest_pinned_image(builder) => {
            return Err(GeneratorError::usage(format!(
                "candidate builder image must be pinned by sha256 digest, got {builder:?}"
            )));
        }
        (true, platform, _) => {
            return Err(GeneratorError::usage(format!(
                "candidate render platform must be Linux-X64 and builder must be digest-pinned, got {platform:?}"
            )));
        }
    }
    Ok(())
}

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
    trusted_root: &Path,
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
    let git_root = options.git_root.as_deref().unwrap_or(trusted_root);
    // The audited config is untrusted input. In CI, the event repository is
    // the authority for deciding whether the pin belongs to this checkout;
    // only local policy runs fall back to the declared repository.
    let source = declared.pin_source(
        git_root,
        options
            .candidate_repository
            .as_deref()
            .or(declared.repository.as_deref()),
    );
    let head = resolve_head(git_root, options.head_sha.as_deref());
    let base = options.base_sha.clone().or_else(|| head.clone().ok());
    let history = match &source {
        PinSource::Checkout(checkout) => {
            let mut revisions = vec![pin, options.base_revision.as_str()];
            if let Ok(head) = &head {
                revisions.push(head);
            }
            if let Some(base) = base.as_deref() {
                revisions.push(base);
            }
            ensure_pin_history(checkout, &revisions)
        }
        PinSource::Remote(_) => Ok(()),
    };
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
    report.rules.push(match (&head, &source, &history) {
        (_, PinSource::Checkout(_), Err(error)) => {
            RuleReport::fail("pin-reachable", error.to_string(), Vec::new())
        }
        (_, PinSource::Remote(_), _) => foreign("pin-reachable"),
        (Ok(head), PinSource::Checkout(_), Ok(())) => {
            pin_reachable(git_root, pin, head, base.as_deref())
        }
        (Err(reason), PinSource::Checkout(_), Ok(())) => {
            RuleReport::fail("pin-reachable", reason.clone(), Vec::new())
        }
    });
    report.rules.push(match (&head, &source, &history) {
        (_, PinSource::Checkout(_), Err(error)) => {
            RuleReport::fail("pin-monotonic", error.to_string(), Vec::new())
        }
        (_, PinSource::Remote(_), _) => foreign("pin-monotonic"),
        (Ok(head), PinSource::Checkout(_), Ok(())) => {
            pin_monotonic(git_root, pin, &options.base_revision, head, base.as_deref())
        }
        (Err(reason), PinSource::Checkout(_), Ok(())) => {
            RuleReport::fail("pin-monotonic", reason.clone(), Vec::new())
        }
    });
    report.rules.push(entrypoint_pin(root, pin));
    let lookup = PinnedBinaryLookup::from_env(pin, options.build_pin);
    let mainline = matches!((&head, &base), (Ok(head), Some(base)) if head == base);
    let comparison = match (&history, &head) {
        (Err(error), _) => Err(GeneratorError::usage(error.to_string())),
        (Ok(()), Err(error)) => Err(GeneratorError::usage(error.clone())),
        (Ok(()), Ok(head)) => regenerate_and_compare(
            git_root,
            root,
            head,
            base.as_deref().unwrap_or(head),
            pin,
            &declared.default_branch,
            &declared.excludes,
            &lookup,
            &source,
            options.candidate_source.as_deref(),
            options.candidate_render.as_deref(),
            options.candidate_repository.as_deref(),
            options.candidate_source_repository.as_deref(),
            options.candidate_run_id.as_deref(),
            options.candidate_platform.as_deref(),
            options.candidate_builder_image.as_deref(),
        ),
    };
    report
        .rules
        .push(generated_tree_report(pin, comparison, mainline));
}

/// Report the `generated-tree` verdict. The candidate exception passes on
/// pull requests (a generator change in flight) but fails on mainline: once
/// merged, the pin must advance so the tight invariant holds again.
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
        Ok(TreeComparison::Candidate(closure)) if mainline => RuleReport::fail(
            "generated-tree",
            format!(
                "the pin is stale on mainline: the tree matches the candidate render ({closure}), not the render of velnor-workflow at {pin}; bump [generator] revision to HEAD and regenerate"
            ),
            Vec::new(),
        ),
        Ok(TreeComparison::Candidate(closure)) => RuleReport::pass(
            "generated-tree",
            format!(
                "the tree matches the candidate render ({closure}), not the render of velnor-workflow at {pin}: a generator change in flight; bump [generator] revision after merge"
            ),
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
    // Provider labels, runner groups, default branch, and exclusions come
    // from the base checkout. The candidate tree is data and cannot redefine
    // which execution target the policy trusts.
    let audit = audit_workflows_with_contract(
        root,
        &declared.velnor_policy,
        &declared.excludes,
    )?;
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
    let lookup = PinnedBinaryLookup::from_env(pin, build_pin);
    let head = resolve_head(checkout, None).map_err(GeneratorError::usage)?;
    match regenerate_and_compare(
        checkout,
        output_root,
        &head,
        &head,
        pin,
        &config.default_branch,
        &excludes,
        &lookup,
        &source,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )? {
        TreeComparison::Pin => Ok(()),
        TreeComparison::Candidate(closure) => {
            eprintln!(
                "notice: the tree matches the candidate render ({closure}), not the render of the declared pin {pin}; bump `[generator] revision` in {GENERATION_CONFIG} after merge"
            );
            Ok(())
        }
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
    fn read(root: &Path, trusted_root: &Path) -> Result<Self, GeneratorError> {
        let generation = config::discover(root)?;
        let pin = generation
            .as_ref()
            .and_then(|generation| generation.revision())
            .filter(|revision| super::is_full_revision(revision))
            .map(|revision| DeclaredPin::Config(revision.to_owned()))
            .or_else(|| entrypoint_policy_revision(root).map(DeclaredPin::Entrypoint));
        let trusted_generation = config::discover(trusted_root)?;
        let excludes = trusted_generation
            .as_ref()
            .map(config::RepoGenerationConfig::effective_policy_exclude_workflows)
            .unwrap_or_default();
        let repository = generation
            .as_ref()
            .and_then(config::RepoGenerationConfig::repository)
            .map(str::to_owned);
        let velnor_policy = configured_velnor_policy(trusted_root)?;
        let required_checks = trusted_generation
            .as_ref()
            .map(|generation| generation.ruleset_required_status_checks().to_vec())
            .filter(|contexts| !contexts.is_empty())
            .unwrap_or_else(|| {
                if trusted_generation
                    .as_ref()
                    .and_then(config::RepoGenerationConfig::ci_required)
                    .unwrap_or(true)
                {
                    vec!["ci-required".to_owned()]
                } else {
                    Vec::new()
                }
            });
        let external_checks = trusted_generation
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

    fn pin_source(&self, root: &Path, repository: Option<&str>) -> PinSource {
        pin_source(root, repository)
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
    let output = git_output(root, arguments)?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    } else {
        Ok(None)
    }
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<std::process::Output, GeneratorError> {
    closure_identity::sanitized_git_command()
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .map_err(|error| {
            GeneratorError::usage(format!("run git {}: {error}", arguments.join(" ")))
        })
}

fn validate_clean_head(checkout: &Path, expected_head: &str) -> Result<(), GeneratorError> {
    if !super::is_full_revision(expected_head) {
        return Err(GeneratorError::usage(format!(
            "audited head must be a full 40-character SHA, got {expected_head:?}"
        )));
    }
    let head = git(checkout, &["rev-parse", "--verify", "HEAD^{commit}"])?
        .ok_or_else(|| GeneratorError::usage("workflow root is not a Git checkout"))?;
    if head != expected_head {
        return Err(GeneratorError::usage(format!(
            "workflow checkout HEAD {head} does not match the explicit audited head {expected_head}"
        )));
    }
    let status = git_output(
        checkout,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !status.status.success() {
        return Err(GeneratorError::usage(format!(
            "cannot prove workflow checkout cleanliness: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        )));
    }
    if !status.stdout.is_empty() {
        let entries = status.stdout.split(|byte| *byte == 0).count().saturating_sub(1);
        return Err(GeneratorError::usage(format!(
            "workflow checkout at {expected_head} is dirty ({entries} staged, modified, or untracked entries); clean it before policy evaluation"
        )));
    }
    Ok(())
}

fn validate_history_checkout(
    checkout: &Path,
    expected_head: &str,
    expected_base: &str,
) -> Result<(), GeneratorError> {
    validate_clean_head(checkout, expected_base)?;
    require_commit(checkout, expected_head)
}

#[derive(Clone, Debug)]
struct RawTreeEntry {
    mode: String,
    kind: String,
    object: String,
    path: String,
}

fn parse_raw_tree_entries(output: &[u8]) -> Result<Vec<RawTreeEntry>, GeneratorError> {
    let mut entries = Vec::new();
    for record in output.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
        let tab = record.iter().position(|byte| *byte == b'\t').ok_or_else(|| {
            GeneratorError::usage("git ls-tree returned a record without a path separator")
        })?;
        let header = std::str::from_utf8(&record[..tab]).map_err(|error| {
            GeneratorError::usage(format!("git ls-tree returned invalid header bytes: {error}"))
        })?;
        let mut fields = header.split_ascii_whitespace();
        let mode = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let object = fields.next().unwrap_or_default();
        if mode.is_empty()
            || kind.is_empty()
            || object.len() != 40
            || !object.bytes().all(|byte| byte.is_ascii_hexdigit())
            || fields.next().is_some()
        {
            return Err(GeneratorError::usage(format!(
                "git ls-tree returned an invalid entry header: {header:?}"
            )));
        }
        let path = std::str::from_utf8(&record[tab + 1..]).map_err(|error| {
            GeneratorError::usage(format!("Git tree path is not UTF-8: {error}"))
        })?;
        validate_snapshot_path(path)?;
        entries.push(RawTreeEntry {
            mode: mode.to_owned(),
            kind: kind.to_owned(),
            object: object.to_owned(),
            path: path.to_owned(),
        });
    }
    validate_snapshot_path_collisions(&entries)?;
    Ok(entries)
}

fn validate_snapshot_path(path: &str) -> Result<(), GeneratorError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.chars().any(|character| matches!(character, '\\' | ':'))
    {
        return Err(GeneratorError::usage(format!(
            "Git tree path is not portable: {path:?}"
        )));
    }
    if path
        .split('/')
        .any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.eq_ignore_ascii_case(".git")
                || component.chars().any(char::is_control)
        })
    {
        return Err(GeneratorError::usage(format!(
            "Git tree path contains an unsafe component: {path:?}"
        )));
    }
    Ok(())
}

fn validate_snapshot_path_collisions(entries: &[RawTreeEntry]) -> Result<(), GeneratorError> {
    let mut names = BTreeMap::<String, String>::new();
    let mut files = BTreeSet::<String>::new();
    let mut directories = BTreeSet::<String>::new();
    for entry in entries {
        if entry.mode == "160000" && entry.kind == "commit" {
            return Err(GeneratorError::usage(format!(
                "candidate tree contains unsupported Git submodule at {:?}",
                entry.path
            )));
        }
        let is_directory = matches!((entry.mode.as_str(), entry.kind.as_str()), ("040000" | "40000", "tree"));
        let components = entry.path.split('/').collect::<Vec<_>>();
        let mut original_prefix = String::new();
        let mut folded_prefix = String::new();
        for (index, component) in components.iter().enumerate() {
            if index != 0 {
                original_prefix.push('/');
                folded_prefix.push('/');
            }
            original_prefix.push_str(component);
            folded_prefix.push_str(&casefold_path_component(component));
            if let Some(previous) = names.get(&folded_prefix)
                && previous != &original_prefix
            {
                return Err(GeneratorError::usage(format!(
                    "Git tree contains case-colliding paths {previous:?} and {original_prefix:?}; the candidate snapshot would be ambiguous on a case-insensitive filesystem"
                )));
            }
            names.insert(folded_prefix.clone(), original_prefix.clone());
            if index + 1 < components.len() || (is_directory && index + 1 == components.len()) {
                if files.contains(&folded_prefix) {
                    return Err(GeneratorError::usage(format!(
                        "Git tree path {original_prefix:?} is both a file and a directory"
                    )));
                }
                directories.insert(folded_prefix.clone());
            }
        }
        if !is_directory && directories.contains(&folded_prefix) {
            return Err(GeneratorError::usage(format!(
                "Git tree path {:?} collides with a directory on a case-insensitive filesystem",
                entry.path
            )));
        }
        if is_directory {
            if files.contains(&folded_prefix) {
                return Err(GeneratorError::usage(format!(
                    "Git tree path {:?} is both a file and a directory",
                    entry.path
                )));
            }
            directories.insert(folded_prefix);
        } else {
            if !files.insert(folded_prefix.clone()) {
                return Err(GeneratorError::usage(format!(
                    "Git tree contains duplicate path {:?}",
                    entry.path
                )));
            }
        }
    }
    Ok(())
}

/// Validate symlink resolution against the complete candidate tree before
/// materializing any link. Checking each target's `..` depth in isolation is
/// insufficient: an earlier symlink can move resolution upward before later
/// `..` components are processed.
fn validate_snapshot_symlink_targets(
    checkout: &Path,
    entries: &[RawTreeEntry],
) -> Result<(), GeneratorError> {
    let mut links = BTreeMap::<String, String>::new();
    for entry in entries.iter().filter(|entry| entry.mode == "120000") {
        let size = git_output(checkout, &["cat-file", "-s", &entry.object])?;
        if !size.status.success() {
            return Err(GeneratorError::usage(format!(
                "read candidate symlink size at {:?}: {}",
                entry.path,
                String::from_utf8_lossy(&size.stderr).trim()
            )));
        }
        let size = String::from_utf8_lossy(&size.stdout)
            .trim()
            .parse::<usize>()
            .map_err(|error| {
                GeneratorError::usage(format!("invalid candidate symlink size: {error}"))
            })?;
        if size == 0 || size > 4095 {
            return Err(GeneratorError::usage(format!(
                "candidate symlink {:?} has an invalid target size of {size} bytes",
                entry.path
            )));
        }
        let target = git_output(checkout, &["cat-file", "blob", &entry.object])?;
        if !target.status.success() || target.stdout.len() != size {
            return Err(GeneratorError::usage(format!(
                "read candidate symlink target at {:?}: {}",
                entry.path,
                String::from_utf8_lossy(&target.stderr).trim()
            )));
        }
        let target = std::str::from_utf8(&target.stdout).map_err(|error| {
            GeneratorError::usage(format!(
                "candidate symlink {:?} has a non-UTF-8 target: {error}",
                entry.path
            ))
        })?;
        validate_symlink_target(&entry.path, target.as_bytes())?;
        links.insert(entry.path.clone(), target.to_owned());
    }

    for (path, target) in &links {
        resolve_snapshot_symlink(path, target, &links)?;
    }
    Ok(())
}

fn resolve_snapshot_symlink(
    path: &str,
    target: &str,
    links: &BTreeMap<String, String>,
) -> Result<(), GeneratorError> {
    let mut resolved = path
        .split('/')
        .take(path.split('/').count().saturating_sub(1))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut pending = target.split('/').map(str::to_owned).collect::<Vec<_>>();
    let mut index = 0;
    let mut expansions = 0;
    while index < pending.len() {
        match pending[index].as_str() {
            "" | "." => index += 1,
            ".." => {
                if resolved.pop().is_none() {
                    return Err(GeneratorError::usage(format!(
                        "symlink {path:?} escapes the candidate source snapshot through a symlink chain"
                    )));
                }
                index += 1;
            }
            component => {
                resolved.push(component.to_owned());
                let resolved_path = resolved.join("/");
                if let Some(next_target) = links.get(&resolved_path) {
                    expansions += 1;
                    if expansions > 40 {
                        return Err(GeneratorError::usage(format!(
                            "symlink {path:?} exceeds the 40-link resolution limit"
                        )));
                    }
                    resolved.pop();
                    let mut expanded = next_target
                        .split('/')
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    expanded.extend_from_slice(&pending[index + 1..]);
                    pending = expanded;
                    index = 0;
                } else {
                    index += 1;
                }
            }
        }
    }
    Ok(())
}

fn casefold_path_component(component: &str) -> String {
    component.nfkc().flat_map(char::to_lowercase).collect()
}

fn validate_symlink_target(path: &str, target: &[u8]) -> Result<(), GeneratorError> {
    let target = std::str::from_utf8(target).map_err(|error| {
        GeneratorError::usage(format!("symlink {path:?} has a non-UTF-8 target: {error}"))
    })?;
    if target.is_empty()
        || target.starts_with('/')
        || target.chars().any(|character| matches!(character, '\\' | ':'))
    {
        return Err(GeneratorError::usage(format!(
            "symlink {path:?} has a non-portable target {target:?}"
        )));
    }
    let mut depth = path.split('/').count().saturating_sub(1) as i64;
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return Err(GeneratorError::usage(format!(
                        "symlink {path:?} escapes the candidate source snapshot"
                    )));
                }
            }
            _ => depth += 1,
        }
    }
    Ok(())
}

fn materialize_git_snapshot(
    checkout: &Path,
    expected_head: &str,
    destination: &Path,
) -> Result<(), GeneratorError> {
    require_commit(checkout, expected_head)?;
    let parent = destination.parent().ok_or_else(|| {
        GeneratorError::usage(format!(
            "candidate snapshot destination {} has no parent",
            destination.display()
        ))
    })?;
    let file_name = destination.file_name().filter(|name| !name.is_empty()).ok_or_else(|| {
        GeneratorError::usage(format!(
            "candidate snapshot destination {} has no final component",
            destination.display()
        ))
    })?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| GeneratorError::io("resolve candidate snapshot parent", parent, &error))?;
    let destination = canonical_parent.join(file_name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(GeneratorError::usage(format!(
                "candidate snapshot destination {} already exists; refusing to follow or replace it",
                destination.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(GeneratorError::io(
                "inspect candidate snapshot destination",
                &destination,
                &error,
            ));
        }
    }
    fs::create_dir(&destination)
        .map_err(|error| GeneratorError::io("create candidate snapshot", &destination, &error))?;
    set_private_directory(&destination)?;
    let result = materialize_snapshot_contents(checkout, expected_head, &destination);
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let cleanup = fs::remove_dir_all(&destination);
            if let Err(cleanup) = cleanup {
                return Err(GeneratorError::usage(format!(
                    "{error}; also failed to remove partial candidate snapshot {}: {cleanup}",
                    destination.display()
                )));
            }
            Err(error)
        }
    }
}

fn materialize_snapshot_contents(
    checkout: &Path,
    expected_head: &str,
    destination: &Path,
) -> Result<(), GeneratorError> {
    let tree = git_output(
        checkout,
        &["ls-tree", "-r", "-z", "--full-tree", expected_head],
    )?;
    if !tree.status.success() {
        return Err(GeneratorError::usage(format!(
            "read raw Git tree for {expected_head}: {}",
            String::from_utf8_lossy(&tree.stderr).trim()
        )));
    }
    let entries = parse_raw_tree_entries(&tree.stdout)?;
    validate_snapshot_symlink_targets(checkout, &entries)?;
    const MAX_SOURCE_ENTRIES: usize = 100_000;
    const MAX_SOURCE_TREE_BYTES: u64 = 1024 * 1024 * 1024;
    if entries.len() > MAX_SOURCE_ENTRIES {
        return Err(GeneratorError::usage(format!(
            "candidate tree has {} entries, above the {MAX_SOURCE_ENTRIES}-entry limit",
            entries.len()
        )));
    }

    // Create every real directory before placing file and symlink entries.
    // Git trees cannot contain a file as another file's parent, and the
    // case-fold validation above also rejects that ambiguity on APFS.
    let mut directories = BTreeSet::new();
    for entry in &entries {
        let mut parent = PathBuf::new();
        let components = entry.path.split('/').collect::<Vec<_>>();
        for component in components.iter().take(components.len().saturating_sub(1)) {
            parent.push(component);
            directories.insert(parent.clone());
        }
        if entry.mode == "160000" {
            return Err(GeneratorError::usage(format!(
                "candidate tree contains unsupported Git submodule at {:?}",
                entry.path
            )));
        }
    }
    let mut directories = directories.into_iter().collect::<Vec<_>>();
    directories.sort_by_key(|path| path.components().count());
    for relative in &directories {
        let path = destination.join(relative);
        fs::create_dir(&path)
            .map_err(|error| GeneratorError::io("create candidate source directory", &path, &error))?;
    }

    let mut total_blob_bytes = 0_u64;
    for entry in &entries {
        match (entry.mode.as_str(), entry.kind.as_str()) {
            ("100644" | "100755" | "120000", "blob") => {}
            _ => {
                return Err(GeneratorError::usage(format!(
                    "unsupported Git tree entry {} {} at {:?}",
                    entry.mode, entry.kind, entry.path
                )));
            }
        }
        let size = git_output(checkout, &["cat-file", "-s", &entry.object])?;
        if !size.status.success() {
            return Err(GeneratorError::usage(format!(
                "read Git blob size for {:?}: {}",
                entry.path,
                String::from_utf8_lossy(&size.stderr).trim()
            )));
        }
        let size = String::from_utf8_lossy(&size.stdout)
            .trim()
            .parse::<u64>()
            .map_err(|error| GeneratorError::usage(format!("invalid Git blob size: {error}")))?;
        const MAX_SNAPSHOT_BLOB_BYTES: u64 = 512 * 1024 * 1024;
        if size > MAX_SNAPSHOT_BLOB_BYTES {
            return Err(GeneratorError::usage(format!(
                "Git blob at {:?} is {size} bytes, above the 512 MiB snapshot limit",
                entry.path
            )));
        }
        total_blob_bytes = total_blob_bytes.checked_add(size).ok_or_else(|| {
            GeneratorError::usage("candidate source tree byte count overflow")
        })?;
        if total_blob_bytes > MAX_SOURCE_TREE_BYTES {
            return Err(GeneratorError::usage(
                "candidate source tree exceeds the 1 GiB raw-blob limit",
            ));
        }
        let blob = git_output(checkout, &["cat-file", "blob", &entry.object])?;
        if !blob.status.success() {
            return Err(GeneratorError::usage(format!(
                "read Git blob for {:?}: {}",
                entry.path,
                String::from_utf8_lossy(&blob.stderr).trim()
            )));
        }
        if blob.stdout.len() as u64 != size {
            return Err(GeneratorError::usage(format!(
                "Git blob at {:?} changed size during materialization",
                entry.path
            )));
        }
        let path = destination.join(&entry.path);
        if entry.mode == "120000" {
            validate_symlink_target(&entry.path, &blob.stdout)?;
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt as _;
                use std::os::unix::fs::symlink;
                symlink(std::ffi::OsStr::from_bytes(&blob.stdout), &path).map_err(|error| {
                    GeneratorError::io("create candidate source symlink", &path, &error)
                })?;
            }
            #[cfg(not(unix))]
            return Err(GeneratorError::usage(
                "candidate source symlinks are not supported on this platform",
            ));
            continue;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| GeneratorError::io("create candidate source file", &path, &error))?;
        file.write_all(&blob.stdout)
            .map_err(|error| GeneratorError::io("write candidate source file", &path, &error))?;
        drop(file);
        set_snapshot_file_mode(&path, entry.mode == "100755")?;
    }
    for relative in directories.into_iter().rev() {
        set_snapshot_directory_mode(&destination.join(relative))?;
    }
    set_snapshot_directory_mode(destination)?;
    Ok(())
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
    let status = closure_identity::sanitized_git_command()
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

/// Resolve D19 and ancestry commits in an owner checkout before policy walks
/// history or computes the pinned tree closure. A shallow checkout must be
/// unshallowed because fetching only D19 leaves its parents and the path to
/// the audited head unavailable to `merge-base`.
fn ensure_pin_history(checkout: &Path, revisions: &[&str]) -> Result<(), GeneratorError> {
    if revisions
        .iter()
        .any(|revision| !super::is_full_revision(revision))
    {
        return Err(GeneratorError::usage(
            "pin history requires full 40-character commit revisions",
        ));
    }
    let shallow = is_shallow_checkout(checkout);
    let missing = revisions
        .iter()
        .copied()
        .filter(|revision| !commit_exists(checkout, revision))
        .collect::<Vec<_>>();
    if !shallow && missing.is_empty() {
        return Ok(());
    }

    let mut diagnostics = Vec::new();
    if shallow {
        match closure_identity::sanitized_git_command()
            .arg("-C")
            .arg(checkout)
            .args(["fetch", "--no-tags", "--unshallow", "origin"])
            .output()
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                diagnostics.push(String::from_utf8_lossy(&output.stderr).trim().to_owned())
            }
            Err(error) => diagnostics.push(error.to_string()),
        }
    }
    for revision in revisions.iter().copied() {
        if commit_exists(checkout, revision) {
            continue;
        }
        match closure_identity::sanitized_git_command()
            .arg("-C")
            .arg(checkout)
            .args(["fetch", "--no-tags", "origin", revision])
            .output()
        {
            Ok(output) if output.status.success() && commit_exists(checkout, revision) => {}
            Ok(output) => {
                diagnostics.push(String::from_utf8_lossy(&output.stderr).trim().to_owned())
            }
            Err(error) => diagnostics.push(error.to_string()),
        }
    }
    let missing = revisions
        .iter()
        .copied()
        .filter(|revision| !commit_exists(checkout, revision))
        .collect::<Vec<_>>();
    if !missing.is_empty() || is_shallow_checkout(checkout) {
        let detail = diagnostics
            .into_iter()
            .filter(|detail| !detail.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        let remediation = if is_shallow_checkout(checkout) {
            "; configure the validator checkout with actions/checkout `fetch-depth: 0` and verify `origin` can fetch the D19 pin and its ancestry"
        } else {
            "; verify `origin` can fetch these objects or re-pin to a commit reachable from the audited head"
        };
        return Err(GeneratorError::usage(format!(
            "cannot prove D19 pin and ancestry in {}: missing [{}]{}{}{}",
            checkout.display(),
            missing.join(", "),
            if is_shallow_checkout(checkout) {
                "; history remains shallow after fetching from origin"
            } else {
                ""
            },
            remediation,
            if detail.is_empty() {
                String::new()
            } else {
                format!("; git fetch: {detail}")
            }
        )));
    }
    Ok(())
}

/// Ensure D19 is available before its pinned tree closure is read. Direct
/// generator checks also use the same history-fetch contract as policy.
fn ensure_pin_present(checkout: &Path, pin: &str) -> Result<(), GeneratorError> {
    ensure_pin_history(checkout, &[pin])
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
    /// Holds the sealed job-private product bytes open while the renderer is
    /// executed through its inherited descriptor. On Linux, the executable
    /// path is `/proc/self/fd/N`, so replacing a cache or temp pathname cannot
    /// redirect the later `exec`.
    _pinned_binary_guard: Option<fs::File>,
    /// Digest bound by the release manifest the provisioner verified.
    pinned_binary_sha256: Option<String>,
    /// Source revision bound by that same manifest.
    pinned_binary_revision: Option<String>,
    /// Source closure bound by that same manifest.
    pinned_binary_closure: Option<String>,
    /// `PATH`.
    search_path: Option<OsString>,
    /// Where an earlier resolution built the pin.
    install_root: PathBuf,
    /// Never build without an explicit `--pin-build`, or under
    /// `CARGO_NET_OFFLINE=true`.
    build_forbidden: bool,
}

impl PinnedBinaryLookup {
    pub(crate) fn from_env(
        revision: &str,
        build_pin: bool,
    ) -> Self {
        let pointer = env::var_os(VELNOR_WORKFLOW_PINNED_BINARY_ENV).map(PathBuf::from);
        let pinned_binary_sha256 = env::var(VELNOR_WORKFLOW_PINNED_BINARY_SHA256_ENV).ok();
        let pinned_binary_revision = env::var(VELNOR_WORKFLOW_PINNED_BINARY_REVISION_ENV).ok();
        let pinned_binary_closure = env::var(VELNOR_WORKFLOW_PINNED_BINARY_CLOSURE_ENV).ok();
        Self {
            pinned_binary: pointer,
            _pinned_binary_guard: None,
            pinned_binary_sha256,
            pinned_binary_revision,
            pinned_binary_closure,
            search_path: env::var_os("PATH"),
            install_root: policy_install_root(revision),
            build_forbidden: !build_pin
                || env::var("CARGO_NET_OFFLINE").is_ok_and(|value| value == "true"),
        }
    }

    /// Copy the explicitly provisioned product into the policy invocation's
    /// private scratch tree before any command executes it. Reading once,
    /// hashing those bytes, and writing the same bytes into a content-addressed
    /// job copy prevents later replacement of the persistent cache slot from
    /// changing the renderer that this invocation runs.
    fn snapshot_pinned_binary(&self, scratch: &Path) -> Result<Self, GeneratorError> {
        let Some(source) = &self.pinned_binary else {
            if self.pinned_binary_sha256.is_some()
                || self.pinned_binary_revision.is_some()
                || self.pinned_binary_closure.is_some()
            {
                return Err(GeneratorError::usage(format!(
                    "pinned policy manifest fields require {VELNOR_WORKFLOW_PINNED_BINARY_ENV}"
                )));
            }
            return Ok(Self {
                pinned_binary: None,
                _pinned_binary_guard: None,
                pinned_binary_sha256: None,
                pinned_binary_revision: None,
                pinned_binary_closure: None,
                search_path: self.search_path.clone(),
                install_root: self.install_root.clone(),
                build_forbidden: self.build_forbidden,
            });
        };
        let expected = self.pinned_binary_sha256.as_deref().ok_or_else(|| {
            GeneratorError::usage(format!(
                "{VELNOR_WORKFLOW_PINNED_BINARY_ENV} requires {VELNOR_WORKFLOW_PINNED_BINARY_SHA256_ENV} from the verified product manifest"
            ))
        })?;
        let expected_revision = self.pinned_binary_revision.as_deref().ok_or_else(|| {
            GeneratorError::usage(format!(
                "{VELNOR_WORKFLOW_PINNED_BINARY_ENV} requires {VELNOR_WORKFLOW_PINNED_BINARY_REVISION_ENV} from the verified product manifest"
            ))
        })?;
        if !super::is_full_revision(expected_revision) {
            return Err(GeneratorError::usage(format!(
                "{VELNOR_WORKFLOW_PINNED_BINARY_REVISION_ENV} is not a full source revision"
            )));
        }
        let expected_closure = self.pinned_binary_closure.as_deref().ok_or_else(|| {
            GeneratorError::usage(format!(
                "{VELNOR_WORKFLOW_PINNED_BINARY_ENV} requires {VELNOR_WORKFLOW_PINNED_BINARY_CLOSURE_ENV} from the verified product manifest"
            ))
        })?;
        if !closure_identity::is_full_closure(expected_closure) {
            return Err(GeneratorError::usage(format!(
                "{VELNOR_WORKFLOW_PINNED_BINARY_CLOSURE_ENV} is not a full source-closure digest"
            )));
        }
        let snapshot = snapshot_verified_binary(source, expected, scratch, "pinned-policy")?;
        Ok(Self {
            pinned_binary: Some(snapshot.path),
            _pinned_binary_guard: snapshot._guard,
            pinned_binary_sha256: Some(expected.to_owned()),
            pinned_binary_revision: Some(expected_revision.to_owned()),
            pinned_binary_closure: Some(expected_closure.to_owned()),
            search_path: self.search_path.clone(),
            install_root: self.install_root.clone(),
            build_forbidden: self.build_forbidden,
        })
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
/// generator history) relies on the verified product manifest: its digest,
/// source revision, and closure must match the binary. The product revision
/// may differ from the requested pin when both names refer to the same source
/// closure. Unmanifested PATH candidates still need to report the exact pin.
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
        let (Some(digest), Some(manifest_revision), Some(manifest_closure)) = (
            lookup.pinned_binary_sha256.as_deref(),
            lookup.pinned_binary_revision.as_deref(),
            lookup.pinned_binary_closure.as_deref(),
        ) else {
            return Err(GeneratorError::usage(format!(
                "{VELNOR_WORKFLOW_PINNED_BINARY_ENV} is not bound to a verified product manifest"
            )));
        };
        return match prove_manifest_bound_binary(
            &binary,
            digest,
            manifest_revision,
            manifest_closure,
            expected,
        ) {
            Ok(()) => Ok(binary),
            Err(detail) => Err(GeneratorError::usage(format!(
                "{VELNOR_WORKFLOW_PINNED_BINARY_ENV}={} is not the verified pin product: {detail}",
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

/// Validate the product manifest's identity before any of its self-reports
/// are used. A product revision may differ from the consumer's pin when
/// both revisions have the same source closure; the binary must match the
/// manifest revision and closure, while local history (when available) must
/// also agree with the closure set for the consumer pin.
fn prove_manifest_bound_binary(
    binary: &Path,
    expected_digest: &str,
    manifest_revision: &str,
    manifest_closure: &str,
    expected_pin_closures: Option<&[String]>,
) -> Result<(), String> {
    let digest = sha256_file(binary)?;
    if digest != expected_digest {
        return Err(format!(
            "reports digest {digest}, but the verified manifest names {expected_digest}"
        ));
    }
    if !super::is_full_revision(manifest_revision) {
        return Err(format!(
            "manifest revision {manifest_revision:?} is not a full source revision"
        ));
    }
    if !closure_identity::is_full_closure(manifest_closure) {
        return Err(format!(
            "manifest closure {manifest_closure:?} is not a full source closure"
        ));
    }
    let reported_revision = binary_revision(binary)?;
    if reported_revision != manifest_revision {
        return Err(format!(
            "reports revision {reported_revision}, but the verified manifest names {manifest_revision}"
        ));
    }
    let reported_closure = binary_closure(binary)?;
    if reported_closure != manifest_closure {
        return Err(format!(
            "reports closure {reported_closure}, but the verified manifest names {manifest_closure}"
        ));
    }
    if let Some(expected) = expected_pin_closures
        && !expected.contains(&reported_closure)
    {
        return Err(format!(
            "reports closure {reported_closure}, which is not the declared pin's closure"
        ));
    }
    Ok(())
}

/// Identity for a raw Git object pack and the trusted base policy tool that
/// created it. This artifact contains source data only; no PR-built executable
/// crosses the workflow boundary.
#[derive(serde::Deserialize, serde::Serialize)]
struct CandidateSourceManifest {
    schema: String,
    repository: String,
    source_repository: String,
    run_id: String,
    revision: String,
    base_revision: String,
    tree: String,
    closure: String,
    pack_sha256: String,
    vendor_sha256: Option<String>,
    policy_tool_revision: String,
    policy_tool_closure: String,
    policy_tool_sha256: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct CandidateRenderManifest {
    schema: String,
    repository: String,
    source_repository: String,
    run_id: String,
    platform: String,
    revision: String,
    base_revision: String,
    tree: String,
    closure: String,
    source_pack_sha256: String,
    vendor_sha256: String,
    builder_image: String,
    render_sha256: String,
}

fn copy_current_policy_tool(path: &Path) -> Result<(String, String, String), GeneratorError> {
    if !super::is_full_revision(SOURCE_REVISION)
        || !closure_identity::is_full_closure(SOURCE_CLOSURE)
    {
        return Err(GeneratorError::usage(
            "the trusted policy tool has no full source revision and closure",
        ));
    }
    let current = env::current_exe()
        .map_err(|error| GeneratorError::usage(format!("resolve trusted policy executable: {error}")))?;
    let expected = sha256_file(&current).map_err(GeneratorError::usage)?;
    let mut input = fs::File::open(&current)
        .map_err(|error| GeneratorError::io("open trusted policy executable", &current, &error))?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| GeneratorError::io("create trusted policy tool copy", path, &error))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|error| GeneratorError::io("copy trusted policy executable", path, &error))?;
    output
        .sync_all()
        .map_err(|error| GeneratorError::io("sync trusted policy tool copy", path, &error))?;
    drop(output);
    let actual = sha256_file(path).map_err(GeneratorError::usage)?;
    if actual != expected {
        return Err(GeneratorError::usage(format!(
            "trusted policy tool changed while copying: expected {expected}, got {actual}"
        )));
    }
    set_readonly_executable(path)?;
    Ok((
        SOURCE_REVISION.to_owned(),
        SOURCE_CLOSURE.to_owned(),
        actual,
    ))
}

fn is_digest_pinned_image(image: &str) -> bool {
    let Some((name, digest)) = image.rsplit_once('@') else {
        return false;
    };
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':')
        })
        && digest
            .strip_prefix("sha256:")
            .is_some_and(is_sha256)
}

fn write_exclusive(path: &Path, content: &[u8]) -> Result<(), GeneratorError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| GeneratorError::io("create exclusive policy file", path, &error))?;
    file.write_all(content)
        .and_then(|()| file.sync_all())
        .map_err(|error| GeneratorError::io("write exclusive policy file", path, &error))
}

fn read_regular_file(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("{}: cannot inspect: {error}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "{}: expected a regular file, not a symlink or special file",
            path.display()
        ));
    }
    if metadata.len() > maximum {
        return Err(format!(
            "{}: file is {} bytes, above the {maximum}-byte limit",
            path.display(),
            metadata.len()
        ));
    }
    let bytes = fs::read(path).map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
    if bytes.len() as u64 != metadata.len() {
        return Err(format!(
            "{}: file changed size while being read",
            path.display()
        ));
    }
    Ok(bytes)
}

fn candidate_render_options(
    arguments: &[OsString],
    allowed: &[&str],
) -> Result<BTreeMap<String, String>, GeneratorError> {
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index]
            .to_str()
            .ok_or_else(|| GeneratorError::usage("candidate render options must be UTF-8"))?;
        let Some(name) = argument.strip_prefix("--") else {
            return Err(GeneratorError::usage(format!(
                "unexpected candidate render argument: {argument}"
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
        if !allowed.contains(&name.as_str()) {
            return Err(GeneratorError::usage(format!(
                "unsupported candidate render option: --{name}"
            )));
        }
        if options.insert(name.clone(), value).is_some() {
            return Err(GeneratorError::usage(format!("--{name} given twice")));
        }
        index += 1;
    }
    Ok(options)
}

fn required_option<'a>(
    options: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, GeneratorError> {
    options
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| GeneratorError::usage(format!("--{name} is required")))
}

fn snapshot_source_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = candidate_render_options(
        arguments,
        &[
            "workflow-root",
            "head-sha",
            "base-sha",
            "repository",
            "source-repository",
            "run-id",
            "output",
        ],
    )?;
    let checkout = PathBuf::from(required_option(&options, "workflow-root")?);
    let head = required_option(&options, "head-sha")?;
    let base = required_option(&options, "base-sha")?;
    let repository = required_option(&options, "repository")?;
    let source_repository = required_option(&options, "source-repository")?;
    let run_id = required_option(&options, "run-id")?;
    let output = PathBuf::from(required_option(&options, "output")?);
    validate_source_identity(repository, source_repository, run_id, head, base)?;
    validate_clean_head(&checkout, base)?;
    require_commit(&checkout, head)?;
    require_commit(&checkout, base)?;
    let tree = git(&checkout, &["rev-parse", "--verify", &format!("{head}^{{tree}}")])?
        .ok_or_else(|| GeneratorError::usage(format!("candidate {head} has no root tree")))?;
    if !super::is_full_revision(&tree) {
        return Err(GeneratorError::usage(format!(
            "candidate {head} has malformed root tree {tree:?}"
        )));
    }
    let closure = closure_identity::candidate_closure_of_tree(&checkout, head)?;
    let trusted_tool_closure = closure_identity::closure_of_tree(
        &checkout,
        base,
        closure_identity::CI_FEATURES,
        closure_identity::PROFILE_RELEASE,
    )?;
    if SOURCE_REVISION != base || SOURCE_CLOSURE != trusted_tool_closure {
        return Err(GeneratorError::usage(format!(
            "source acquisition tool identity differs from the clean base checkout: tool revision {SOURCE_REVISION}, base {base}, tool closure {SOURCE_CLOSURE}, expected {trusted_tool_closure}"
        )));
    }
    let parent = output.parent().ok_or_else(|| {
        GeneratorError::usage(format!("source artifact {} has no parent", output.display()))
    })?;
    let file_name = output.file_name().filter(|name| !name.is_empty()).ok_or_else(|| {
        GeneratorError::usage(format!("source artifact {} has no final component", output.display()))
    })?;
    let parent = fs::canonicalize(parent)
        .map_err(|error| GeneratorError::io("resolve source artifact parent", parent, &error))?;
    let output = parent.join(file_name);
    fs::create_dir(&output)
        .map_err(|error| GeneratorError::io("create source artifact", &output, &error))?;
    set_private_directory(&output)?;
    let pack_path = output.join("candidate-source.pack");
    let result = (|| {
        create_candidate_source_pack(&checkout, head, &pack_path)?;
        let (tool_revision, tool_closure, tool_sha256) =
            copy_current_policy_tool(&output.join("policy-tool"))?;
        let manifest = CandidateSourceManifest {
            schema: "velnor.candidate-source/v2".to_owned(),
            repository: repository.to_owned(),
            source_repository: source_repository.to_owned(),
            run_id: run_id.to_owned(),
            revision: head.to_owned(),
            base_revision: base.to_owned(),
            tree,
            closure,
            pack_sha256: sha256_file(&pack_path).map_err(GeneratorError::usage)?,
            vendor_sha256: None,
            policy_tool_revision: tool_revision,
            policy_tool_closure: tool_closure,
            policy_tool_sha256: tool_sha256,
        };
        write_json_create_new(&output.join("candidate-source-manifest.json"), &manifest)?;
        Ok(())
    })();
    if let Err(error) = result {
        let cleanup = fs::remove_dir_all(&output);
        if let Err(cleanup) = cleanup {
            return Err(GeneratorError::usage(format!(
                "{error}; also failed to remove partial source artifact {}: {cleanup}",
                output.display()
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn materialize_source_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = candidate_render_options(
        arguments,
        &[
            "source-pack",
            "source-manifest",
            "repository",
            "source-repository",
            "run-id",
            "head-sha",
            "base-sha",
            "sealed-vendor",
            "output",
        ],
    )?;
    let pack = PathBuf::from(required_option(&options, "source-pack")?);
    let manifest_path = PathBuf::from(required_option(&options, "source-manifest")?);
    let repository = required_option(&options, "repository")?;
    let source_repository = required_option(&options, "source-repository")?;
    let run_id = required_option(&options, "run-id")?;
    let head = required_option(&options, "head-sha")?;
    let base = required_option(&options, "base-sha")?;
    let sealed_vendor = match required_option(&options, "sealed-vendor")? {
        "true" => true,
        "false" => false,
        value => {
            return Err(GeneratorError::usage(format!(
                "--sealed-vendor must be true or false, got {value:?}"
            )));
        }
    };
    let output = PathBuf::from(required_option(&options, "output")?);
    validate_source_identity(repository, source_repository, run_id, head, base)?;
    let manifest_bytes = read_regular_file(&manifest_path, 1024 * 1024).map_err(GeneratorError::usage)?;
    let manifest: CandidateSourceManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| GeneratorError::usage(format!("invalid candidate source manifest: {error}")))?;
    validate_source_manifest(
        &manifest,
        repository,
        source_repository,
        run_id,
        head,
        base,
    )?;
    let artifact_root = manifest_path
        .parent()
        .ok_or_else(|| GeneratorError::usage("candidate source manifest has no parent"))?;
    if sealed_vendor {
        verify_source_vendor(artifact_root, &manifest)?;
        require_exact_artifact_entries(
            artifact_root,
            &[
                "candidate-source.pack",
                "candidate-source-manifest.json",
                "policy-tool",
                "vendor",
            ],
            "sealed candidate source",
        )?;
    } else {
        if manifest.vendor_sha256.is_some() {
            return Err(GeneratorError::usage(
                "unsealed source materialization received an already sealed vendor tree",
            ));
        }
        require_exact_artifact_entries(
            artifact_root,
            &[
                "candidate-source.pack",
                "candidate-source-manifest.json",
                "policy-tool",
            ],
            "unsealed candidate source",
        )?;
    }
    let pack_metadata = fs::symlink_metadata(&pack)
        .map_err(|error| GeneratorError::io("inspect candidate source pack", &pack, &error))?;
    if !pack_metadata.file_type().is_file() || pack_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "candidate source pack {} is not a regular file",
            pack.display()
        )));
    }
    const MAX_SOURCE_PACK_BYTES: u64 = 1024 * 1024 * 1024;
    if pack_metadata.len() > MAX_SOURCE_PACK_BYTES {
        return Err(GeneratorError::usage(format!(
            "candidate source pack is {} bytes, above the 1 GiB limit",
            pack_metadata.len()
        )));
    }
    let pack_digest = sha256_file(&pack).map_err(GeneratorError::usage)?;
    if pack_digest != manifest.pack_sha256 {
        return Err(GeneratorError::usage(format!(
            "candidate source pack digest mismatch: manifest {}, actual {pack_digest}",
            manifest.pack_sha256
        )));
    }
    let policy_tool = manifest_path
        .parent()
        .ok_or_else(|| GeneratorError::usage("candidate source manifest has no parent"))?
        .join("policy-tool");
    let policy_tool_digest = sha256_bytes(
        &read_regular_file(&policy_tool, 512 * 1024 * 1024)
            .map_err(GeneratorError::usage)?,
    );
    if policy_tool_digest != manifest.policy_tool_sha256 {
        return Err(GeneratorError::usage(format!(
            "trusted policy tool digest mismatch: manifest {}, actual {policy_tool_digest}",
            manifest.policy_tool_sha256
        )));
    }
    create_source_checkout(&pack, head, &manifest.tree, &output)
}

fn validate_cargo_sources_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = candidate_render_options(arguments, &["candidate-root", "trusted-root"])?;
    let candidate_root = PathBuf::from(required_option(&options, "candidate-root")?);
    let trusted_root = PathBuf::from(required_option(&options, "trusted-root")?);
    validate_candidate_cargo_sources(&candidate_root, &trusted_root)
}

fn validate_candidate_cargo_sources(
    candidate_root: &Path,
    trusted_root: &Path,
) -> Result<(), GeneratorError> {
    let candidate_root = fs::canonicalize(candidate_root).map_err(|error| {
        GeneratorError::io("resolve candidate Cargo root", candidate_root, &error)
    })?;
    let trusted_root = fs::canonicalize(trusted_root).map_err(|error| {
        GeneratorError::io("resolve trusted Cargo root", trusted_root, &error)
    })?;
    let candidate_lock = candidate_root.join("Cargo.lock");
    let trusted_lock = trusted_root.join("Cargo.lock");
    let candidate_packages = cargo_lock_packages(&candidate_lock, "candidate")?;
    let trusted_packages = cargo_lock_packages(&trusted_lock, "trusted base")?;
    let untrusted = candidate_packages
        .iter()
        .filter(|package| package.source.is_some() && !trusted_packages.contains(*package))
        .map(|package| format!("{} {} from {}", package.name, package.version, package.source.as_deref().unwrap_or_default()))
        .collect::<Vec<_>>();
    if let Some(package) = untrusted.first() {
        return Err(GeneratorError::usage(format!(
            "candidate Cargo.lock adds or changes external package {package:?} that is absent from the trusted base lock; update the trusted base lock before fetching it"
        )));
    }

    let mut manifests = Vec::new();
    collect_cargo_manifests(&candidate_root, &candidate_root, &mut manifests)?;
    if manifests.is_empty() || !manifests.contains(&candidate_root.join("Cargo.toml")) {
        return Err(GeneratorError::usage(
            "candidate source has no regular root Cargo.toml",
        ));
    }
    for manifest in manifests {
        validate_candidate_cargo_manifest(&candidate_root, &manifest)?;
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CargoLockedPackage {
    name: String,
    version: String,
    source: Option<String>,
}

fn cargo_lock_packages(path: &Path, label: &str) -> Result<BTreeSet<CargoLockedPackage>, GeneratorError> {
    let bytes = read_regular_file(path, 32 * 1024 * 1024).map_err(|error| {
        GeneratorError::usage(format!("{label} Cargo.lock is not safe to read: {error}"))
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        GeneratorError::usage(format!("{label} Cargo.lock is not UTF-8: {error}"))
    })?;
    let document: toml::Value = toml::from_str(text).map_err(|error| {
        GeneratorError::usage(format!("{label} Cargo.lock is invalid TOML: {error}"))
    })?;
    let packages = document
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| GeneratorError::usage(format!("{label} Cargo.lock has no package array")))?;
    let mut locked = BTreeSet::new();
    for package in packages {
        let table = package.as_table().ok_or_else(|| {
            GeneratorError::usage(format!("{label} Cargo.lock has a non-table package row"))
        })?;
        let name = table
            .get("name")
            .and_then(toml::Value::as_str)
            .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
            .ok_or_else(|| {
                GeneratorError::usage(format!("{label} Cargo.lock has an invalid package name"))
            })?;
        let version = table
            .get("version")
            .and_then(toml::Value::as_str)
            .filter(|version| !version.is_empty() && !version.chars().any(char::is_control))
            .ok_or_else(|| {
                GeneratorError::usage(format!("{label} Cargo.lock has an invalid package version"))
            })?;
        let source = table.get("source").map(|source| {
            let source = source.as_str().ok_or_else(|| {
                GeneratorError::usage(format!("{label} Cargo.lock has a non-string package source"))
            })?;
            if source.is_empty() || source.chars().any(char::is_control) {
                return Err(GeneratorError::usage(format!(
                    "{label} Cargo.lock has an invalid package source {source:?}"
                )));
            }
            Ok(source.to_owned())
        }).transpose()?;
        locked.insert(CargoLockedPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            source,
        });
    }
    Ok(locked)
}

fn collect_cargo_manifests(
    root: &Path,
    directory: &Path,
    manifests: &mut Vec<PathBuf>,
) -> Result<(), GeneratorError> {
    const MAX_CARGO_MANIFESTS: usize = 20_000;
    for entry in fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read candidate Cargo directory", directory, &error))?
    {
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("read candidate Cargo entry: {error}")))?;
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect candidate Cargo path", &path, &error))?;
        if metadata.file_type().is_symlink() {
            let target_is_directory = fs::metadata(&path).is_ok_and(|target| target.is_dir());
            if name == "Cargo.toml" || name == "Cargo.lock" || target_is_directory {
                return Err(GeneratorError::usage(format!(
                    "candidate Cargo source path {} is a symlink that Cargo could traverse",
                    path.display()
                )));
            }
            continue;
        }
        if metadata.is_dir() {
            collect_cargo_manifests(root, &path, manifests)?;
            continue;
        }
        if name == "Cargo.toml" {
            if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 {
                return Err(GeneratorError::usage(format!(
                    "candidate Cargo manifest {} is not a regular file below 16 MiB",
                    path.display()
                )));
            }
            let canonical = path.canonicalize().map_err(|error| {
                GeneratorError::io("resolve candidate Cargo manifest", &path, &error)
            })?;
            if !canonical.starts_with(root) {
                return Err(GeneratorError::usage(format!(
                    "candidate Cargo manifest {} resolves outside its source root",
                    path.display()
                )));
            }
            manifests.push(path);
            if manifests.len() > MAX_CARGO_MANIFESTS {
                return Err(GeneratorError::usage(format!(
                    "candidate source exceeds the {MAX_CARGO_MANIFESTS}-manifest Cargo limit"
                )));
            }
        }
    }
    Ok(())
}

fn validate_candidate_cargo_manifest(root: &Path, path: &Path) -> Result<(), GeneratorError> {
    let bytes = read_regular_file(path, 16 * 1024 * 1024).map_err(|error| {
        GeneratorError::usage(format!("candidate Cargo manifest {}: {error}", path.display()))
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        GeneratorError::usage(format!("candidate Cargo manifest {} is not UTF-8: {error}", path.display()))
    })?;
    let document: toml::Value = toml::from_str(text).map_err(|error| {
        GeneratorError::usage(format!("candidate Cargo manifest {} is invalid TOML: {error}", path.display()))
    })?;
    let root_table = document.as_table().ok_or_else(|| {
        GeneratorError::usage(format!("candidate Cargo manifest {} is not a table", path.display()))
    })?;
    if let Some(workspace) = root_table.get("workspace") {
        let workspace = workspace.as_table().ok_or_else(|| {
            GeneratorError::usage(format!("candidate Cargo workspace in {} is not a table", path.display()))
        })?;
        for field in ["members", "exclude"] {
            let Some(rows) = workspace.get(field) else {
                continue;
            };
            let rows = rows.as_array().ok_or_else(|| {
                GeneratorError::usage(format!("candidate Cargo workspace {field} in {} is not an array", path.display()))
            })?;
            for row in rows {
                let pattern = row.as_str().ok_or_else(|| {
                    GeneratorError::usage(format!("candidate Cargo workspace {field} entry in {} is not a string", path.display()))
                })?;
                validate_workspace_pattern(pattern, field, path)?;
            }
        }
    }
    if let Some(package) = root_table.get("package").and_then(toml::Value::as_table)
        && let Some(workspace) = package.get("workspace")
    {
        let workspace = workspace.as_str().ok_or_else(|| {
            GeneratorError::usage(format!("candidate Cargo package.workspace in {} is not a string", path.display()))
        })?;
        validate_candidate_path(root, path.parent().unwrap_or(root), workspace, path)?;
    }
    validate_cargo_path_values(root, path.parent().unwrap_or(root), path, &document)
}

fn validate_workspace_pattern(pattern: &str, field: &str, manifest: &Path) -> Result<(), GeneratorError> {
    if pattern.is_empty()
        || pattern.starts_with('/')
        || pattern
            .chars()
            .any(|character| matches!(character, '\\' | ':' | '\0'))
        || pattern.split('/').any(|component| component == "..")
    {
        return Err(GeneratorError::usage(format!(
            "candidate Cargo workspace {field} pattern {pattern:?} in {} can escape the source tree",
            manifest.display()
        )));
    }
    Ok(())
}

fn validate_cargo_path_values(
    root: &Path,
    manifest_directory: &Path,
    manifest: &Path,
    value: &toml::Value,
) -> Result<(), GeneratorError> {
    match value {
        toml::Value::Table(table) => {
            if let Some(path_value) = table.get("path") {
                let path_value = path_value.as_str().ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "candidate Cargo path in {} is not a string",
                        manifest.display()
                    ))
                })?;
                validate_candidate_path(root, manifest_directory, path_value, manifest)?;
            }
            for nested in table.values() {
                validate_cargo_path_values(root, manifest_directory, manifest, nested)?;
            }
        }
        toml::Value::Array(rows) => {
            for nested in rows {
                validate_cargo_path_values(root, manifest_directory, manifest, nested)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_candidate_path(
    root: &Path,
    base: &Path,
    candidate_path: &str,
    manifest: &Path,
) -> Result<(), GeneratorError> {
    if candidate_path.is_empty()
        || candidate_path.starts_with('/')
        || candidate_path
            .chars()
            .any(|character| matches!(character, '\\' | ':' | '\0'))
    {
        return Err(GeneratorError::usage(format!(
            "candidate Cargo path {candidate_path:?} in {} is not a confined relative path",
            manifest.display()
        )));
    }
    let resolved = base.join(candidate_path).canonicalize().map_err(|error| {
        GeneratorError::usage(format!(
            "candidate Cargo path {candidate_path:?} in {} cannot be resolved: {error}",
            manifest.display()
        ))
    })?;
    if !resolved.starts_with(root) {
        return Err(GeneratorError::usage(format!(
            "candidate Cargo path {candidate_path:?} in {} resolves outside the source tree",
            manifest.display()
        )));
    }
    Ok(())
}

fn seal_source_vendor_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = candidate_render_options(arguments, &["artifact-root", "cargo-config"])?;
    let artifact_root = PathBuf::from(required_option(&options, "artifact-root")?);
    let cargo_config = PathBuf::from(required_option(&options, "cargo-config")?);
    let root_metadata = fs::symlink_metadata(&artifact_root)
        .map_err(|error| GeneratorError::io("inspect candidate source artifact", &artifact_root, &error))?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage("candidate source artifact is not a real directory"));
    }
    let artifact_root = artifact_root.canonicalize().map_err(|error| {
        GeneratorError::io("resolve candidate source artifact", &artifact_root, &error)
    })?;
    let config_metadata = fs::symlink_metadata(&cargo_config)
        .map_err(|error| GeneratorError::io("inspect cargo vendor config", &cargo_config, &error))?;
    if !config_metadata.is_file() || config_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage("Cargo vendor output is not a regular file"));
    }
    let config_parent = cargo_config.parent().ok_or_else(|| {
        GeneratorError::usage("Cargo vendor output has no parent directory")
    })?;
    if fs::canonicalize(config_parent).ok().as_deref() != Some(artifact_root.as_path())
        || cargo_config.file_name().and_then(|name| name.to_str()) != Some("cargo-vendor-config.raw.toml")
    {
        return Err(GeneratorError::usage(
            "Cargo vendor output must be candidate-source artifact's private raw config file",
        ));
    }
    require_exact_artifact_entries(
        &artifact_root,
        &[
            "candidate-source.pack",
            "candidate-source-manifest.json",
            "policy-tool",
            "vendor",
            "cargo-vendor-config.raw.toml",
        ],
        "unsealed vendored source",
    )?;
    let manifest_path = artifact_root.join("candidate-source-manifest.json");
    let manifest_bytes = read_regular_file(&manifest_path, 1024 * 1024).map_err(GeneratorError::usage)?;
    let mut manifest: CandidateSourceManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| GeneratorError::usage(format!("invalid candidate source manifest: {error}")))?;
    validate_source_manifest(
        &manifest,
        &manifest.repository,
        &manifest.source_repository,
        &manifest.run_id,
        &manifest.revision,
        &manifest.base_revision,
    )?;
    if manifest.vendor_sha256.is_some() {
        return Err(GeneratorError::usage("candidate source vendor tree was already sealed"));
    }
    let vendor_root = artifact_root.join("vendor");
    let registry_root = vendor_root.join("registry");
    let vendor_metadata = fs::symlink_metadata(&vendor_root)
        .map_err(|error| GeneratorError::io("inspect candidate vendor root", &vendor_root, &error))?;
    let registry_metadata = fs::symlink_metadata(&registry_root)
        .map_err(|error| GeneratorError::io("inspect Cargo vendor registry", &registry_root, &error))?;
    if !vendor_metadata.is_dir()
        || vendor_metadata.file_type().is_symlink()
        || !registry_metadata.is_dir()
        || registry_metadata.file_type().is_symlink()
    {
        return Err(GeneratorError::usage("Cargo vendor output contains an invalid directory"));
    }
    let raw_config = read_regular_file(&cargo_config, 16 * 1024 * 1024)
        .map_err(GeneratorError::usage)?;
    let raw_config = std::str::from_utf8(&raw_config)
        .map_err(|error| GeneratorError::usage(format!("Cargo vendor config is not UTF-8: {error}")))?;
    let rewritten = rewrite_vendor_config(raw_config, &registry_root)?;
    write_exclusive(&vendor_root.join("config.toml"), rewritten.as_bytes())?;
    fs::remove_file(&cargo_config)
        .map_err(|error| GeneratorError::io("remove raw Cargo vendor config", &cargo_config, &error))?;
    manifest.vendor_sha256 = Some(render_tree_digest(&vendor_root)?);
    replace_json_atomically(&manifest_path, &manifest)?;
    verify_source_vendor(&artifact_root, &manifest)?;
    require_exact_artifact_entries(
        &artifact_root,
        &[
            "candidate-source.pack",
            "candidate-source-manifest.json",
            "policy-tool",
            "vendor",
        ],
        "sealed candidate source",
    )
}

fn rewrite_vendor_config(raw_config: &str, expected_registry: &Path) -> Result<String, GeneratorError> {
    let mut config: toml::Value = toml::from_str(raw_config).map_err(|error| {
        GeneratorError::usage(format!("Cargo vendor emitted invalid config TOML: {error}"))
    })?;
    let source = config
        .get_mut("source")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| GeneratorError::usage("Cargo vendor config has no [source] table"))?;
    let vendored = source
        .get_mut("vendored-sources")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| GeneratorError::usage("Cargo vendor config has no vendored-sources table"))?;
    let directory = vendored
        .get("directory")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| GeneratorError::usage("Cargo vendor config has no vendored directory"))?;
    let configured_registry = PathBuf::from(directory).canonicalize().map_err(|error| {
        GeneratorError::usage(format!("Cargo vendor registry path cannot be resolved: {error}"))
    })?;
    let expected_registry = expected_registry.canonicalize().map_err(|error| {
        GeneratorError::io("resolve Cargo vendor registry", expected_registry, &error)
    })?;
    if configured_registry != expected_registry {
        return Err(GeneratorError::usage(format!(
            "Cargo vendor config points at {}, expected {}",
            configured_registry.display(),
            expected_registry.display()
        )));
    }
    vendored.insert(
        "directory".to_owned(),
        toml::Value::String("/vendor/registry".to_owned()),
    );
    for (name, source) in source.iter() {
        if name == "vendored-sources" {
            continue;
        }
        let source = source.as_table().ok_or_else(|| {
            GeneratorError::usage(format!("Cargo vendor source {name:?} is not a table"))
        })?;
        if source.get("replace-with").and_then(toml::Value::as_str) != Some("vendored-sources") {
            return Err(GeneratorError::usage(format!(
                "Cargo vendor source {name:?} is not replaced by the sealed vendor directory"
            )));
        }
    }
    toml::to_string(&config)
        .map_err(|error| GeneratorError::usage(format!("serialize sealed Cargo vendor config: {error}")))
}

fn verify_source_vendor(
    artifact_root: &Path,
    manifest: &CandidateSourceManifest,
) -> Result<(), GeneratorError> {
    let expected = manifest
        .vendor_sha256
        .as_deref()
        .ok_or_else(|| GeneratorError::usage("candidate source manifest has no sealed vendor digest"))?;
    if !is_sha256(expected) {
        return Err(GeneratorError::usage("candidate source vendor digest is malformed"));
    }
    let vendor_root = artifact_root.join("vendor");
    let metadata = fs::symlink_metadata(&vendor_root)
        .map_err(|error| GeneratorError::io("inspect candidate vendor tree", &vendor_root, &error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage("candidate vendor tree is not a real directory"));
    }
    require_exact_artifact_entries(&vendor_root, &["config.toml", "registry"], "candidate vendor")?;
    let config_path = vendor_root.join("config.toml");
    let config_bytes = read_regular_file(&config_path, 16 * 1024 * 1024)
        .map_err(GeneratorError::usage)?;
    let config_text = std::str::from_utf8(&config_bytes)
        .map_err(|error| GeneratorError::usage(format!("sealed vendor config is not UTF-8: {error}")))?;
    validate_sealed_vendor_config(config_text)?;
    let actual = render_tree_digest(&vendor_root)?;
    if actual != expected {
        return Err(GeneratorError::usage(format!(
            "candidate vendor digest mismatch: manifest {expected}, actual {actual}"
        )));
    }
    Ok(())
}

fn validate_sealed_vendor_config(config_text: &str) -> Result<(), GeneratorError> {
    let config: toml::Value = toml::from_str(config_text).map_err(|error| {
        GeneratorError::usage(format!("sealed vendor config is invalid TOML: {error}"))
    })?;
    let source = config
        .get("source")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| GeneratorError::usage("sealed vendor config has no [source] table"))?;
    let vendored = source
        .get("vendored-sources")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| GeneratorError::usage("sealed vendor config has no vendored-sources table"))?;
    if vendored.get("directory").and_then(toml::Value::as_str) != Some("/vendor/registry") {
        return Err(GeneratorError::usage(
            "sealed vendor config has an unexpected registry directory",
        ));
    }
    for (name, source) in source {
        if name == "vendored-sources" {
            continue;
        }
        let source = source.as_table().ok_or_else(|| {
            GeneratorError::usage(format!("sealed vendor source {name:?} is not a table"))
        })?;
        if source.get("replace-with").and_then(toml::Value::as_str) != Some("vendored-sources") {
            return Err(GeneratorError::usage(format!(
                "sealed vendor source {name:?} bypasses vendored-sources"
            )));
        }
    }
    Ok(())
}

fn replace_json_atomically(path: &Path, value: &impl serde::Serialize) -> Result<(), GeneratorError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| GeneratorError::usage(format!("serialize {}: {error}", path.display())))?;
    let temporary = path.with_extension("json.tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| GeneratorError::io("create staged source manifest", &temporary, &error))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|error| GeneratorError::io("write staged source manifest", &temporary, &error))?;
    fs::rename(&temporary, path)
        .map_err(|error| GeneratorError::io("replace source manifest", path, &error))
}

fn seal_candidate_render_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = candidate_render_options(
        arguments,
        &[
            "artifact-root",
            "rendered-root",
            "source-pack",
            "source-manifest",
            "source-artifact",
            "repository",
            "source-repository",
            "run-id",
            "platform",
            "head-sha",
            "base-sha",
            "builder-image",
        ],
    )?;
    let artifact_root = PathBuf::from(required_option(&options, "artifact-root")?);
    let rendered_root = PathBuf::from(required_option(&options, "rendered-root")?);
    let pack = PathBuf::from(required_option(&options, "source-pack")?);
    let source_manifest_path = PathBuf::from(required_option(&options, "source-manifest")?);
    let source_artifact = PathBuf::from(required_option(&options, "source-artifact")?);
    let repository = required_option(&options, "repository")?;
    let source_repository = required_option(&options, "source-repository")?;
    let run_id = required_option(&options, "run-id")?;
    let platform = required_option(&options, "platform")?;
    let head = required_option(&options, "head-sha")?;
    let base = required_option(&options, "base-sha")?;
    let builder_image = required_option(&options, "builder-image")?;
    validate_source_identity(repository, source_repository, run_id, head, base)?;
    let source_manifest_bytes = read_regular_file(&source_manifest_path, 1024 * 1024)
        .map_err(GeneratorError::usage)?;
    let source_manifest: CandidateSourceManifest = serde_json::from_slice(&source_manifest_bytes)
        .map_err(|error| GeneratorError::usage(format!("invalid candidate source manifest: {error}")))?;
    validate_source_manifest(
        &source_manifest,
        repository,
        source_repository,
        run_id,
        head,
        base,
    )?;
    verify_source_vendor(&source_artifact, &source_manifest)?;
    if source_artifact.join("candidate-source.pack") != pack
        || source_artifact.join("candidate-source-manifest.json") != source_manifest_path
    {
        return Err(GeneratorError::usage(
            "candidate render sealing inputs do not belong to the verified source artifact",
        ));
    }
    let pack_metadata = fs::symlink_metadata(&pack)
        .map_err(|error| GeneratorError::io("inspect candidate source pack", &pack, &error))?;
    if !pack_metadata.file_type().is_file()
        || pack_metadata.file_type().is_symlink()
        || pack_metadata.len() == 0
        || pack_metadata.len() > 1024 * 1024 * 1024
    {
        return Err(GeneratorError::usage(
            "candidate source pack is not a non-empty regular file below 1 GiB",
        ));
    }
    let pack_digest = sha256_file(&pack).map_err(GeneratorError::usage)?;
    if pack_digest != source_manifest.pack_sha256 {
        return Err(GeneratorError::usage(format!(
            "candidate source pack digest mismatch: manifest {}, actual {pack_digest}",
            source_manifest.pack_sha256
        )));
    }
    if !is_digest_pinned_image(builder_image) {
        return Err(GeneratorError::usage(format!(
            "candidate builder image must be pinned by sha256 digest, got {builder_image:?}"
        )));
    }
    let artifact_metadata = fs::symlink_metadata(&artifact_root)
        .map_err(|error| GeneratorError::io("inspect candidate render artifact", &artifact_root, &error))?;
    if !artifact_metadata.is_dir() || artifact_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "candidate render artifact {} is not a real directory",
            artifact_root.display()
        )));
    }
    if rendered_root != artifact_root.join("rendered") {
        return Err(GeneratorError::usage(
            "candidate render output must be exactly the artifact's rendered/ directory",
        ));
    }
    require_exact_artifact_entries(artifact_root, &["rendered"], "unsealed candidate render")?;
    let render_sha256 = render_tree_digest(&rendered_root)?;
    let manifest = CandidateRenderManifest {
        schema: "velnor.candidate-render/v2".to_owned(),
        repository: source_manifest.repository,
        source_repository: source_manifest.source_repository,
        run_id: source_manifest.run_id,
        platform: platform.to_owned(),
        revision: source_manifest.revision,
        base_revision: source_manifest.base_revision,
        tree: source_manifest.tree,
        closure: source_manifest.closure,
        source_pack_sha256: source_manifest.pack_sha256,
        vendor_sha256: source_manifest
            .vendor_sha256
            .ok_or_else(|| GeneratorError::usage("candidate source has no sealed vendor digest"))?,
        builder_image: builder_image.to_owned(),
        render_sha256,
    };
    write_json_create_new(&artifact_root.join("candidate-render-manifest.json"), &manifest)
}

fn validate_source_identity(
    repository: &str,
    source_repository: &str,
    run_id: &str,
    head: &str,
    base: &str,
) -> Result<(), GeneratorError> {
    for (label, slug) in [("repository", repository), ("source repository", source_repository)] {
        let mut parts = slug.split('/');
        let valid_segment = |segment: &str| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        if !parts.next().is_some_and(valid_segment)
            || !parts.next().is_some_and(valid_segment)
            || parts.next().is_some()
        {
            return Err(GeneratorError::usage(format!(
                "candidate {label} must be an owner/name repository slug, got {slug:?}"
            )));
        }
    }
    if run_id.is_empty() || !run_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(GeneratorError::usage(format!(
            "candidate workflow run ID must be decimal digits, got {run_id:?}"
        )));
    }
    if !super::is_full_revision(head) || !super::is_full_revision(base) {
        return Err(GeneratorError::usage(
            "candidate head and base must be full 40-character commit SHAs",
        ));
    }
    Ok(())
}

fn validate_source_manifest(
    manifest: &CandidateSourceManifest,
    repository: &str,
    source_repository: &str,
    run_id: &str,
    head: &str,
    base: &str,
) -> Result<(), GeneratorError> {
    if manifest.schema != "velnor.candidate-source/v2"
        || manifest.repository != repository
        || manifest.source_repository != source_repository
        || manifest.run_id != run_id
        || manifest.revision != head
        || manifest.base_revision != base
        || !super::is_full_revision(&manifest.tree)
        || !closure_identity::is_full_closure(&manifest.closure)
        || !is_sha256(&manifest.pack_sha256)
        || !super::is_full_revision(&manifest.policy_tool_revision)
        || !closure_identity::is_full_closure(&manifest.policy_tool_closure)
        || !is_sha256(&manifest.policy_tool_sha256)
        || manifest
            .vendor_sha256
            .as_deref()
            .is_some_and(|digest| !is_sha256(digest))
    {
        return Err(GeneratorError::usage(
            "candidate source manifest does not match the trusted repository, run, head, base, tree, and closure identity",
        ));
    }
    Ok(())
}

fn write_json_create_new(path: &Path, value: &impl serde::Serialize) -> Result<(), GeneratorError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| GeneratorError::usage(format!("serialize {}: {error}", path.display())))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| GeneratorError::io("create JSON manifest", path, &error))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|error| GeneratorError::io("write JSON manifest", path, &error))
}

fn require_commit(root: &Path, revision: &str) -> Result<(), GeneratorError> {
    if !super::is_full_revision(revision) {
        return Err(GeneratorError::usage(format!(
            "revision must be a full 40-character SHA, got {revision:?}"
        )));
    }
    let actual = git(
        root,
        &["rev-parse", "--verify", "--end-of-options", &format!("{revision}^{{commit}}")],
    )?
    .ok_or_else(|| {
        GeneratorError::usage(format!(
            "commit {revision} is unavailable in {}",
            root.display()
        ))
    })?;
    if actual != revision {
        return Err(GeneratorError::usage(format!(
            "requested commit {revision} resolved to a different object {actual}"
        )));
    }
    Ok(())
}

fn create_candidate_source_pack(
    checkout: &Path,
    head: &str,
    output: &Path,
) -> Result<(), GeneratorError> {
    let tree = git_output(
        checkout,
        &["ls-tree", "-r", "-t", "-z", "--full-tree", head],
    )?;
    if !tree.status.success() {
        return Err(GeneratorError::usage(format!(
            "read candidate Git tree for {head}: {}",
            String::from_utf8_lossy(&tree.stderr).trim()
        )));
    }
    let entries = parse_raw_tree_entries(&tree.stdout)?;
    validate_snapshot_symlink_targets(checkout, &entries)?;
    const MAX_SOURCE_ENTRIES: usize = 100_000;
    if entries.len() > MAX_SOURCE_ENTRIES {
        return Err(GeneratorError::usage(format!(
            "candidate tree has {} entries, above the {MAX_SOURCE_ENTRIES}-entry limit",
            entries.len()
        )));
    }
    let root_tree = git(
        checkout,
        &["rev-parse", "--verify", &format!("{head}^{{tree}}")],
    )?
    .ok_or_else(|| GeneratorError::usage(format!("candidate commit {head} has no root tree")))?;
    if !super::is_full_revision(&root_tree) {
        return Err(GeneratorError::usage(format!(
            "candidate commit {head} has malformed root tree {root_tree:?}"
        )));
    }
    let mut total_blob_bytes = 0_u64;
    const MAX_SOURCE_TREE_BYTES: u64 = 1024 * 1024 * 1024;
    for entry in &entries {
        match (entry.mode.as_str(), entry.kind.as_str()) {
            ("040000" | "40000", "tree") => {}
            ("100644" | "100755" | "120000", "blob") => {
                let size = git_output(checkout, &["cat-file", "-s", &entry.object])?;
                if !size.status.success() {
                    return Err(GeneratorError::usage(format!(
                        "read candidate blob size at {:?}: {}",
                        entry.path,
                        String::from_utf8_lossy(&size.stderr).trim()
                    )));
                }
                let size = String::from_utf8_lossy(&size.stdout)
                    .trim()
                    .parse::<u64>()
                    .map_err(|error| {
                        GeneratorError::usage(format!("invalid Git blob size: {error}"))
                    })?;
                total_blob_bytes = total_blob_bytes.checked_add(size).ok_or_else(|| {
                    GeneratorError::usage("candidate source tree byte count overflow")
                })?;
                if total_blob_bytes > MAX_SOURCE_TREE_BYTES {
                    return Err(GeneratorError::usage(
                        "candidate source tree exceeds the 1 GiB raw-blob limit",
                    ));
                }
            }
            ("160000", "commit") => {
                return Err(GeneratorError::usage(format!(
                    "candidate tree contains unsupported Git submodule at {:?}; materializing it requires a separately reviewed source policy",
                    entry.path
                )));
            }
            _ => {
                return Err(GeneratorError::usage(format!(
                    "unsupported Git tree entry {} {} at {:?}",
                    entry.mode, entry.kind, entry.path
                )));
            }
        }
    }
    // Feed the complete source object closure directly. `pack-objects --revs`
    // with HEAD and its parents excludes every object shared with a parent,
    // which is unsafe for the empty receiver used by the render job. This
    // explicitly includes the commit, root tree, every nested tree, and every
    // blob named by the raw tree listing; duplicates are removed first.
    let objects = std::iter::once(head.to_owned())
        .chain(std::iter::once(root_tree))
        .chain(entries.iter().map(|entry| entry.object.clone()))
        .collect::<BTreeSet<_>>();
    let mut child = closure_identity::sanitized_git_command()
        .arg("-C")
        .arg(checkout)
        .args(["pack-objects", "--stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)
                .map_err(|error| GeneratorError::io("create candidate source pack", output, &error))?,
        ))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| GeneratorError::usage(format!("start git pack-objects: {error}")))?;
    let write_result = (|| {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            GeneratorError::usage("git pack-objects did not expose its input")
        })?;
        for object in &objects {
            stdin
                .write_all(object.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                .map_err(|error| {
                    GeneratorError::usage(format!("write candidate object list: {error}"))
                })?;
        }
        Ok(())
    })();
    drop(child.stdin.take());
    let output_status = child
        .wait_with_output()
        .map_err(|error| GeneratorError::usage(format!("wait for git pack-objects: {error}")))?;
    write_result?;
    if !output_status.status.success() {
        return Err(GeneratorError::usage(format!(
            "git pack-objects failed: {}",
            String::from_utf8_lossy(&output_status.stderr).trim()
        )));
    }
    let metadata = fs::symlink_metadata(output)
        .map_err(|error| GeneratorError::io("inspect candidate source pack", output, &error))?;
    const MAX_SOURCE_PACK_BYTES: u64 = 1024 * 1024 * 1024;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_SOURCE_PACK_BYTES {
        return Err(GeneratorError::usage(format!(
            "candidate source pack has invalid size {} (limit 1 GiB)",
            metadata.len()
        )));
    }
    Ok(())
}

fn create_source_checkout(
    pack: &Path,
    head: &str,
    expected_tree: &str,
    destination: &Path,
) -> Result<(), GeneratorError> {
    if !super::is_full_revision(head) || !super::is_full_revision(expected_tree) {
        return Err(GeneratorError::usage(
            "candidate source materialization requires full head and tree SHAs",
        ));
    }
    let parent = destination.parent().ok_or_else(|| {
        GeneratorError::usage(format!("candidate source destination {} has no parent", destination.display()))
    })?;
    let file_name = destination.file_name().filter(|name| !name.is_empty()).ok_or_else(|| {
        GeneratorError::usage(format!("candidate source destination {} has no final component", destination.display()))
    })?;
    let parent = fs::canonicalize(parent)
        .map_err(|error| GeneratorError::io("resolve candidate source destination", parent, &error))?;
    let destination = parent.join(file_name);
    fs::create_dir(&destination)
        .map_err(|error| GeneratorError::io("create candidate source worktree", &destination, &error))?;
    set_private_directory(&destination)?;
    let result = (|| {
        git_command_in(&destination, &["init", "-q", "--template="])?;
        let mut child = closure_identity::sanitized_git_command()
            .arg("-C")
            .arg(&destination)
            .args(["index-pack", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| GeneratorError::usage(format!("start git index-pack: {error}")))?;
        let copy_result = (|| {
            let mut input = fs::File::open(pack)
                .map_err(|error| GeneratorError::io("open candidate source pack", pack, &error))?;
            let mut stdin = child.stdin.take().ok_or_else(|| {
                GeneratorError::usage("git index-pack did not expose its input")
            })?;
            std::io::copy(&mut input, &mut stdin)
                .map_err(|error| GeneratorError::usage(format!("stream candidate source pack: {error}")))?;
            Ok(())
        })();
        let indexed = child
            .wait_with_output()
            .map_err(|error| GeneratorError::usage(format!("wait for git index-pack: {error}")))?;
        copy_result?;
        if !indexed.status.success() {
            return Err(GeneratorError::usage(format!(
                "git index-pack rejected candidate source pack: {}",
                String::from_utf8_lossy(&indexed.stderr).trim()
            )));
        }
        write_exclusive(&destination.join(".git/shallow"), format!("{head}\n").as_bytes())?;
        git_command_in(&destination, &["update-ref", "refs/heads/candidate-source", head])?;
        git_command_in(&destination, &["symbolic-ref", "HEAD", "refs/heads/candidate-source"])?;
        require_commit(&destination, head)?;
        let tree = git(
            &destination,
            &["rev-parse", "--verify", &format!("{head}^{{tree}}")],
        )?
        .ok_or_else(|| GeneratorError::usage(format!("candidate commit {head} has no tree")))?;
        if tree != expected_tree {
            return Err(GeneratorError::usage(format!(
                "candidate source pack tree mismatch: expected {expected_tree}, actual {tree}"
            )));
        }
        materialize_snapshot_contents(&destination, head, &destination)
    })();
    if let Err(error) = result {
        let cleanup = fs::remove_dir_all(&destination);
        if let Err(cleanup) = cleanup {
            return Err(GeneratorError::usage(format!(
                "{error}; also failed to remove partial candidate checkout {}: {cleanup}",
                destination.display()
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn git_command_in(root: &Path, arguments: &[&str]) -> Result<(), GeneratorError> {
    let output = git_output(root, arguments)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(GeneratorError::usage(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn verify_candidate_source_artifact(
    artifact_root: &Path,
    git_root: &Path,
    materialized_root: &Path,
    expected_head: &str,
    expected_base: &str,
    expected_repository: &str,
    expected_source_repository: &str,
    expected_run_id: &str,
) -> Result<String, GeneratorError> {
    validate_source_identity(
        expected_repository,
        expected_source_repository,
        expected_run_id,
        expected_head,
        expected_base,
    )?;
    validate_clean_head(git_root, expected_base)?;
    require_commit(git_root, expected_head)?;

    let artifact_metadata = fs::symlink_metadata(artifact_root).map_err(|error| {
        GeneratorError::io("inspect candidate source artifact", artifact_root, &error)
    })?;
    if !artifact_metadata.is_dir() || artifact_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "candidate source artifact {} is not a real directory",
            artifact_root.display()
        )));
    }
    require_exact_artifact_entries(
        artifact_root,
        &[
            "candidate-source.pack",
            "candidate-source-manifest.json",
            "policy-tool",
            "vendor",
        ],
        "candidate source",
    )?;
    let manifest_path = artifact_root.join("candidate-source-manifest.json");
    let manifest_bytes = read_regular_file(&manifest_path, 1024 * 1024)
        .map_err(GeneratorError::usage)?;
    let manifest: CandidateSourceManifest = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        GeneratorError::usage(format!(
            "{}: invalid candidate source manifest: {error}",
            manifest_path.display()
        ))
    })?;
    validate_source_manifest(
        &manifest,
        expected_repository,
        expected_source_repository,
        expected_run_id,
        expected_head,
        expected_base,
    )?;
    verify_source_vendor(artifact_root, &manifest)?;

    let source_pack = artifact_root.join("candidate-source.pack");
    let pack_metadata = fs::symlink_metadata(&source_pack)
        .map_err(|error| GeneratorError::io("inspect candidate source pack", &source_pack, &error))?;
    if !pack_metadata.file_type().is_file()
        || pack_metadata.file_type().is_symlink()
        || pack_metadata.len() == 0
        || pack_metadata.len() > 1024 * 1024 * 1024
    {
        return Err(GeneratorError::usage(
            "candidate source pack is not a non-empty regular file below 1 GiB",
        ));
    }
    let pack_digest = sha256_file(&source_pack).map_err(GeneratorError::usage)?;
    if pack_digest != manifest.pack_sha256 {
        return Err(GeneratorError::usage(format!(
            "candidate source pack digest mismatch: manifest {}, actual {pack_digest}",
            manifest.pack_sha256
        )));
    }

    // The source artifact is made by the base workflow. Bind its tool copy to
    // the exact base commit and the release, no-default-features closure so an
    // artifact cannot substitute a different helper and rewrite its manifest.
    let expected_tool_closure = closure_identity::closure_of_tree(
        git_root,
        expected_base,
        closure_identity::CI_FEATURES,
        closure_identity::PROFILE_RELEASE,
    )?;
    if manifest.policy_tool_revision != expected_base
        || manifest.policy_tool_closure != expected_tool_closure
    {
        return Err(GeneratorError::usage(format!(
            "candidate source policy tool is not the trusted base build: revision {}, closure {}; expected revision {expected_base}, closure {expected_tool_closure}",
            manifest.policy_tool_revision, manifest.policy_tool_closure
        )));
    }
    let policy_tool = artifact_root.join("policy-tool");
    let tool_bytes = read_regular_file(&policy_tool, 512 * 1024 * 1024)
        .map_err(GeneratorError::usage)?;
    let tool_digest = sha256_bytes(&tool_bytes);
    if tool_digest != manifest.policy_tool_sha256 {
        return Err(GeneratorError::usage(format!(
            "trusted policy tool digest mismatch: manifest {}, actual {tool_digest}",
            manifest.policy_tool_sha256
        )));
    }

    let actual_tree = git(
        git_root,
        &["rev-parse", "--verify", &format!("{expected_head}^{{tree}}")],
    )?
    .ok_or_else(|| GeneratorError::usage(format!("candidate commit {expected_head} has no tree")))?;
    let actual_closure = closure_identity::candidate_closure_of_tree(git_root, expected_head)?;
    if manifest.tree != actual_tree || manifest.closure != actual_closure {
        return Err(GeneratorError::usage(format!(
            "candidate source manifest differs from exact Git objects: manifest tree {}, closure {}; actual tree {actual_tree}, closure {actual_closure}",
            manifest.tree, manifest.closure
        )));
    }
    let raw_tree = git_output(
        git_root,
        &["ls-tree", "-r", "-z", "--full-tree", expected_head],
    )?;
    if !raw_tree.status.success() {
        return Err(GeneratorError::usage(format!(
            "read candidate tree before policy input: {}",
            String::from_utf8_lossy(&raw_tree.stderr).trim()
        )));
    }
    let raw_entries = parse_raw_tree_entries(&raw_tree.stdout)?;
    validate_snapshot_symlink_targets(git_root, &raw_entries)?;
    verify_materialized_source_tree(git_root, expected_head, materialized_root)?;
    Ok(actual_closure)
}

fn verify_materialized_source_tree(
    git_root: &Path,
    revision: &str,
    materialized_root: &Path,
) -> Result<(), GeneratorError> {
    let root_metadata = fs::symlink_metadata(materialized_root).map_err(|error| {
        GeneratorError::io("inspect materialized candidate source", materialized_root, &error)
    })?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "materialized candidate source {} is not a real directory",
            materialized_root.display()
        )));
    }
    let listing = git_output(
        git_root,
        &["ls-tree", "-r", "-z", "--full-tree", revision],
    )?;
    if !listing.status.success() {
        return Err(GeneratorError::usage(format!(
            "read raw candidate source tree: {}",
            String::from_utf8_lossy(&listing.stderr).trim()
        )));
    }
    let entries = parse_raw_tree_entries(&listing.stdout)?;
    let mut expected_paths = BTreeSet::new();
    for entry in &entries {
        if !expected_paths.insert(entry.path.clone()) {
            return Err(GeneratorError::usage(format!(
                "candidate source Git tree repeats path {:?}",
                entry.path
            )));
        }
        let path = materialized_root.join(&entry.path);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            GeneratorError::io("inspect materialized source entry", &path, &error)
        })?;
        match (entry.mode.as_str(), entry.kind.as_str()) {
            ("100644" | "100755", "blob") if metadata.file_type().is_file() => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt as _;
                    if metadata.nlink() != 1 {
                        return Err(GeneratorError::usage(format!(
                            "materialized source has a hard-linked file at {:?}",
                            entry.path
                        )));
                    }
                }
                let expected_size = git(
                    git_root,
                    &["cat-file", "-s", &entry.object],
                )?
                .ok_or_else(|| {
                    GeneratorError::usage(format!("candidate blob {} is unavailable", entry.object))
                })?
                .parse::<u64>()
                .map_err(|error| GeneratorError::usage(format!("invalid candidate blob size: {error}")))?;
                if expected_size != metadata.len() {
                    return Err(GeneratorError::usage(format!(
                        "materialized source size differs at {:?}: expected {expected_size}, got {}",
                        entry.path,
                        metadata.len()
                    )));
                }
                let expected = git_output(git_root, &["cat-file", "blob", &entry.object])?;
                if !expected.status.success() {
                    return Err(GeneratorError::usage(format!(
                        "read candidate blob {}: {}",
                        entry.object,
                        String::from_utf8_lossy(&expected.stderr).trim()
                    )));
                }
                let actual = fs::read(&path)
                    .map_err(|error| GeneratorError::io("read materialized source entry", &path, &error))?;
                if actual != expected.stdout {
                    return Err(GeneratorError::usage(format!(
                        "materialized candidate source differs from Git at {:?}",
                        entry.path
                    )));
                }
            }
            ("120000", "blob") if metadata.file_type().is_symlink() => {
                let expected = git_output(git_root, &["cat-file", "blob", &entry.object])?;
                if !expected.status.success() {
                    return Err(GeneratorError::usage(format!(
                        "read candidate symlink blob {}: {}",
                        entry.object,
                        String::from_utf8_lossy(&expected.stderr).trim()
                    )));
                }
                let target = fs::read_link(&path)
                    .map_err(|error| GeneratorError::io("read materialized source symlink", &path, &error))?;
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt as _;
                    if target.as_os_str().as_bytes() != expected.stdout {
                        return Err(GeneratorError::usage(format!(
                            "materialized candidate symlink differs from Git at {:?}",
                            entry.path
                        )));
                    }
                }
                #[cfg(not(unix))]
                if target.to_string_lossy().as_bytes() != expected.stdout {
                    return Err(GeneratorError::usage(format!(
                        "materialized candidate symlink differs from Git at {:?}",
                        entry.path
                    )));
                }
            }
            _ => {
                return Err(GeneratorError::usage(format!(
                    "materialized candidate source has wrong file type at {:?}",
                    entry.path
                )));
            }
        }
    }
    let mut actual_paths = BTreeSet::new();
    collect_materialized_paths(materialized_root, materialized_root, &mut actual_paths)?;
    if actual_paths != expected_paths {
        let missing = expected_paths.difference(&actual_paths).next();
        let extra = actual_paths.difference(&expected_paths).next();
        return Err(GeneratorError::usage(format!(
            "materialized source path set differs from Git: first missing {missing:?}, first extra {extra:?}"
        )));
    }
    Ok(())
}

fn collect_materialized_paths(
    root: &Path,
    directory: &Path,
    paths: &mut BTreeSet<String>,
) -> Result<(), GeneratorError> {
    for entry in fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read materialized source directory", directory, &error))?
    {
        let entry = entry.map_err(|error| GeneratorError::usage(format!("read source entry: {error}")))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect materialized source path", &path, &error))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|error| GeneratorError::usage(format!("source path escaped root: {error}")))?;
        let relative_text = relative
            .to_str()
            .ok_or_else(|| GeneratorError::usage("materialized source has a non-UTF-8 path"))?;
        if relative_text == ".git" {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(GeneratorError::usage("materialized source .git is not a real directory"));
            }
            continue;
        }
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect_materialized_paths(root, &path, paths)?;
        } else if metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            paths.insert(relative_text.to_owned());
        } else {
            return Err(GeneratorError::usage(format!(
                "materialized source has a special file at {relative_text:?}"
            )));
        }
    }
    Ok(())
}

fn verify_candidate_render_artifact(
    artifact_root: &Path,
    source_artifact: &Path,
    checkout: &Path,
    tree: &Path,
    expected_head: &str,
    expected_base: &str,
    excludes: &BTreeSet<String>,
    expected_repository: Option<&str>,
    expected_source_repository: Option<&str>,
    expected_run_id: Option<&str>,
    expected_platform: Option<&str>,
    expected_builder_image: Option<&str>,
) -> Result<Option<String>, GeneratorError> {
    let repository = expected_repository.ok_or_else(|| {
        GeneratorError::usage("--candidate-repository is required with --candidate-render")
    })?;
    let source_repository = expected_source_repository.ok_or_else(|| {
        GeneratorError::usage(
            "--candidate-source-repository is required with --candidate-render",
        )
    })?;
    let run_id = expected_run_id.ok_or_else(|| {
        GeneratorError::usage("--candidate-run-id is required with --candidate-render")
    })?;
    let platform = expected_platform.ok_or_else(|| {
        GeneratorError::usage("--candidate-platform is required with --candidate-render")
    })?;
    let builder_image = expected_builder_image.ok_or_else(|| {
        GeneratorError::usage("--candidate-builder-image is required with --candidate-render")
    })?;
    if !is_digest_pinned_image(builder_image) {
        return Err(GeneratorError::usage(format!(
            "expected candidate builder image is not digest-pinned: {builder_image:?}"
        )));
    }
    let source_metadata = fs::symlink_metadata(source_artifact).map_err(|error| {
        GeneratorError::io("inspect candidate source artifact", source_artifact, &error)
    })?;
    if !source_metadata.is_dir() || source_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "candidate source artifact {} is not a real directory",
            source_artifact.display()
        )));
    }
    let source_manifest_path = source_artifact.join("candidate-source-manifest.json");
    let source_manifest_bytes =
        read_regular_file(&source_manifest_path, 1024 * 1024).map_err(GeneratorError::usage)?;
    let source_manifest: CandidateSourceManifest = serde_json::from_slice(&source_manifest_bytes)
        .map_err(|error| {
            GeneratorError::usage(format!(
                "{}: invalid candidate source manifest: {error}",
                source_manifest_path.display()
            ))
        })?;
    validate_source_manifest(
        &source_manifest,
        repository,
        source_repository,
        run_id,
        expected_head,
        expected_base,
    )?;
    let source_pack = source_artifact.join("candidate-source.pack");
    let source_pack_metadata = fs::symlink_metadata(&source_pack)
        .map_err(|error| GeneratorError::io("inspect candidate source pack", &source_pack, &error))?;
    if !source_pack_metadata.file_type().is_file()
        || source_pack_metadata.file_type().is_symlink()
        || source_pack_metadata.len() == 0
        || source_pack_metadata.len() > 1024 * 1024 * 1024
    {
        return Err(GeneratorError::usage(
            "candidate source pack is not a non-empty regular file below 1 GiB",
        ));
    }
    let source_pack_digest = sha256_file(&source_pack).map_err(GeneratorError::usage)?;
    if source_pack_digest != source_manifest.pack_sha256 {
        return Err(GeneratorError::usage(format!(
            "candidate source pack digest mismatch: manifest {}, actual {source_pack_digest}",
            source_manifest.pack_sha256
        )));
    }
    let policy_tool = source_artifact.join("policy-tool");
    let tool_bytes = read_regular_file(&policy_tool, 512 * 1024 * 1024)
        .map_err(GeneratorError::usage)?;
    let tool_digest = sha256_bytes(&tool_bytes);
    if tool_digest != source_manifest.policy_tool_sha256 {
        return Err(GeneratorError::usage(format!(
            "trusted policy tool digest mismatch: manifest {}, actual {tool_digest}",
            source_manifest.policy_tool_sha256
        )));
    }
    require_exact_artifact_entries(
        source_artifact,
        &[
            "candidate-source.pack",
            "candidate-source-manifest.json",
            "policy-tool",
            "vendor",
        ],
        "candidate source",
    )?;
    verify_source_vendor(source_artifact, &source_manifest)?;
    let root_metadata = fs::symlink_metadata(artifact_root).map_err(|error| {
        GeneratorError::io("inspect candidate render artifact", artifact_root, &error)
    })?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "candidate render artifact {} is not a real directory",
            artifact_root.display()
        )));
    }
    let render_manifest_path = artifact_root.join("candidate-render-manifest.json");
    let render_manifest_bytes =
        read_regular_file(&render_manifest_path, 1024 * 1024).map_err(GeneratorError::usage)?;
    let render_manifest: CandidateRenderManifest = serde_json::from_slice(&render_manifest_bytes)
        .map_err(|error| {
            GeneratorError::usage(format!(
                "{}: invalid candidate render manifest: {error}",
                render_manifest_path.display()
            ))
        })?;
    let expected_tree = git(
        checkout,
        &["rev-parse", "--verify", &format!("{expected_head}^{{tree}}")],
    )?
    .ok_or_else(|| GeneratorError::usage(format!("candidate commit {expected_head} has no tree")))?;
    let expected_closure = closure_identity::candidate_closure_of_tree(checkout, expected_head)?;
    if source_manifest.tree != expected_tree || source_manifest.closure != expected_closure {
        return Err(GeneratorError::usage(format!(
            "candidate source manifest tree/closure differs from the exact Git objects: manifest tree {}, closure {}; actual tree {}, closure {}",
            source_manifest.tree,
            source_manifest.closure,
            expected_tree,
            expected_closure
        )));
    }
    if render_manifest.schema != "velnor.candidate-render/v2"
        || render_manifest.repository != repository
        || render_manifest.source_repository != source_repository
        || render_manifest.run_id != run_id
        || render_manifest.platform != platform
        || render_manifest.revision != expected_head
        || render_manifest.base_revision != expected_base
        || render_manifest.tree != expected_tree
        || render_manifest.closure != expected_closure
        || render_manifest.source_pack_sha256 != source_manifest.pack_sha256
        || Some(render_manifest.vendor_sha256.as_str())
            != source_manifest.vendor_sha256.as_deref()
        || render_manifest.builder_image != builder_image
        || !is_sha256(&render_manifest.render_sha256)
    {
        return Err(GeneratorError::usage(format!(
            "candidate render manifest identity does not match the trusted source/run: repository={}, source_repository={}, run_id={}, platform={}, revision={}, closure={}",
            render_manifest.repository,
            render_manifest.source_repository,
            render_manifest.run_id,
            render_manifest.platform,
            render_manifest.revision,
            render_manifest.closure
        )));
    }
    let rendered_root = artifact_root.join("rendered");
    let rendered_metadata = fs::symlink_metadata(&rendered_root).map_err(|error| {
        GeneratorError::io("inspect rendered candidate tree", &rendered_root, &error)
    })?;
    if !rendered_metadata.is_dir() || rendered_metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "candidate render output {} is not a real directory",
            rendered_root.display()
        )));
    }
    let actual_render_digest = render_tree_digest(&rendered_root)?;
    if actual_render_digest != render_manifest.render_sha256 {
        return Err(GeneratorError::usage(format!(
            "candidate render digest mismatch: manifest {}, actual {actual_render_digest}",
            render_manifest.render_sha256
        )));
    }
    require_exact_artifact_entries(
        artifact_root,
        &["candidate-render-manifest.json", "rendered"],
        "candidate render",
    )?;
    if compare_candidate_rendered_tree(&rendered_root, tree, excludes)?.is_empty() {
        Ok(Some(expected_closure))
    } else {
        Ok(None)
    }
}

fn require_exact_artifact_entries(
    root: &Path,
    expected: &[&str],
    label: &str,
) -> Result<(), GeneratorError> {
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(root)
        .map_err(|error| GeneratorError::io(&format!("read {label} artifact"), root, &error))?
    {
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("read {label} artifact entry: {error}")))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            GeneratorError::usage(format!("{label} artifact has a non-UTF-8 entry name"))
        })?;
        if !actual.insert(name.to_owned()) {
            return Err(GeneratorError::usage(format!(
                "{label} artifact has duplicate entry {name:?}"
            )));
        }
    }
    if actual.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected {
        return Err(GeneratorError::usage(format!(
            "{label} artifact entries differ from the trusted contract: expected [{}], actual [{}]",
            expected.into_iter().collect::<Vec<_>>().join(", "),
            actual.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn render_tree_digest(root: &Path) -> Result<String, GeneratorError> {
    use sha2::Digest as _;

    let mut files = BTreeMap::new();
    collect_candidate_files(root, root, &mut files)?;
    if files.is_empty() {
        return Err(GeneratorError::usage(format!(
            "candidate renderer produced no files in {}",
            root.display()
        )));
    }
    let mut digest = sha2::Sha256::new();
    for (path, content) in files {
        let path = path.to_str().ok_or_else(|| {
            GeneratorError::usage("candidate render contains a non-UTF-8 path")
        })?;
        let path_bytes = path.as_bytes();
        digest.update((path_bytes.len() as u64).to_be_bytes());
        digest.update(path_bytes);
        digest.update((content.len() as u64).to_be_bytes());
        digest.update(content);
    }
    let mut output = String::with_capacity(64);
    for byte in digest.finalize() {
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

fn collect_candidate_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), GeneratorError> {
    let mut total_bytes = 0_u64;
    collect_candidate_files_inner(root, directory, files, &mut total_bytes)
}

fn collect_candidate_files_inner(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
    total_bytes: &mut u64,
) -> Result<(), GeneratorError> {
    const MAX_RENDER_ENTRIES: usize = 100_000;
    const MAX_RENDER_BYTES: u64 = 1024 * 1024 * 1024;
    const MAX_RENDER_FILE_BYTES: u64 = 64 * 1024 * 1024;
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| GeneratorError::io("inspect candidate render directory", directory, &error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "candidate render contains a non-directory path at {}",
            directory.display()
        )));
    }
    for entry in fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read candidate render directory", directory, &error))?
    {
        let path = entry
            .map_err(|error| GeneratorError::usage(format!("read candidate render entry: {error}")))?
            .path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect candidate render entry", &path, &error))?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return Err(GeneratorError::usage(format!(
                "candidate render contains a symlink at {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            collect_candidate_files_inner(root, &path, files, total_bytes)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(GeneratorError::usage(format!(
                "candidate render contains a non-regular path at {}",
                path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.nlink() > 1 {
                return Err(GeneratorError::usage(format!(
                    "candidate render contains a hard-linked file at {}",
                    path.display()
                )));
            }
        }
        let relative = path.strip_prefix(root).map_err(|error| {
            GeneratorError::usage(format!(
                "candidate render path {} is outside {}: {error}",
                path.display(),
                root.display()
            ))
        })?;
        let relative_text = relative.to_str().ok_or_else(|| {
            GeneratorError::usage(format!(
                "candidate render contains a non-UTF-8 path at {}",
                path.display()
            ))
        })?;
        validate_snapshot_path(relative_text)?;
        if files.len() >= MAX_RENDER_ENTRIES {
            return Err(GeneratorError::usage(format!(
                "candidate render exceeds the {MAX_RENDER_ENTRIES}-file limit"
            )));
        }
        if metadata.len() > MAX_RENDER_FILE_BYTES {
            return Err(GeneratorError::usage(format!(
                "candidate render file {} exceeds the 64 MiB limit",
                path.display()
            )));
        }
        *total_bytes = (*total_bytes).checked_add(metadata.len()).ok_or_else(|| {
            GeneratorError::usage("candidate render byte count overflow")
        })?;
        if *total_bytes > MAX_RENDER_BYTES {
            return Err(GeneratorError::usage(
                "candidate render exceeds the 1 GiB aggregate file limit",
            ));
        }
        let bytes = read_regular_file(&path, 64 * 1024 * 1024).map_err(GeneratorError::usage)?;
        if bytes.len() as u64 != metadata.len() {
            return Err(GeneratorError::usage(format!(
                "candidate render file changed size while being read: {}",
                path.display()
            )));
        }
        if files.insert(relative.to_path_buf(), bytes).is_some() {
            return Err(GeneratorError::usage(format!(
                "candidate render contains duplicate path {}",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn compare_candidate_rendered_tree(
    rendered_root: &Path,
    tree: &Path,
    excludes: &BTreeSet<String>,
) -> Result<Vec<String>, GeneratorError> {
    let mut rendered = BTreeMap::new();
    collect_candidate_files(rendered_root, rendered_root, &mut rendered)?;
    if rendered.is_empty() {
        return Err(GeneratorError::usage(format!(
            "candidate renderer produced no files in {}",
            rendered_root.display()
        )));
    }
    let mut differences = Vec::new();
    for (relative, content) in &rendered {
        let actual = tree.join(relative);
        let metadata = match fs::symlink_metadata(&actual) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                differences.push(format!("{}: missing from the tree", relative.display()));
                continue;
            }
            Err(error) => return Err(GeneratorError::io("inspect audited tree file", &actual, &error)),
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            differences.push(format!(
                "{}: not a regular file in the audited tree",
                relative.display()
            ));
            continue;
        }
        if metadata.len() > 64 * 1024 * 1024 {
            return Err(GeneratorError::usage(format!(
                "audited tree file {} exceeds the 64 MiB comparison limit",
                actual.display()
            )));
        }
        let bytes = fs::read(&actual)
            .map_err(|error| GeneratorError::io("read audited tree file", &actual, &error))?;
        if bytes.len() as u64 != metadata.len() {
            return Err(GeneratorError::usage(format!(
                "audited tree file {} changed size while being read",
                actual.display()
            )));
        }
        if &bytes != content {
            differences.push(format!(
                "{}: differs from the candidate render",
                relative.display()
            ));
        }
    }
    let workflows = tree.join(".github/workflows");
    match fs::symlink_metadata(&workflows) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            for entry in fs::read_dir(&workflows).map_err(|error| {
                GeneratorError::io("read audited workflow directory", &workflows, &error)
            })? {
                let path = entry
                    .map_err(|error| {
                        GeneratorError::usage(format!("read audited workflow entry: {error}"))
                    })?
                    .path();
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    GeneratorError::io("inspect audited workflow entry", &path, &error)
                })?;
                if metadata.file_type().is_symlink()
                    || !metadata.file_type().is_file()
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
        Ok(_) => differences.push(".github/workflows: not a real directory in the audited tree".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(GeneratorError::io("inspect audited workflow directory", &workflows, &error)),
    }
    differences.sort();
    Ok(differences)
}

/// The SHA-256 hex digest of the bytes at `path`, computed before any
/// execution: `binary_closure` itself runs the binary, so the digest gate
/// must come first.
fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("{}: cannot read: {error}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut digest = String::with_capacity(64);
    for byte in sha2::Sha256::digest(bytes) {
        let _ = write!(digest, "{byte:02x}");
    }
    digest
}

/// The executable representation of one digest-verified product. Linux
/// consumers execute a sealed memfd through an inherited descriptor; the
/// content-addressed job file remains useful for diagnostics but is never the
/// object passed to `exec` there.
struct VerifiedBinarySnapshot {
    path: PathBuf,
    _guard: Option<fs::File>,
}

/// Materialize the bytes whose manifest digest was checked into an isolated,
/// content-addressed job file before executing the product. On Linux, also
/// seal those bytes in a memfd and execute through its inherited descriptor;
/// a shared slot or even the content-addressed pathname can then be replaced
/// without changing the renderer at the later execution boundary.
fn snapshot_verified_binary(
    source: &Path,
    expected: &str,
    scratch: &Path,
    label: &str,
) -> Result<VerifiedBinarySnapshot, GeneratorError> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GeneratorError::usage(format!(
            "{label} binary manifest digest is not a full SHA-256 digest"
        )));
    }
    let bytes = fs::read(source).map_err(|error| {
        GeneratorError::usage(format!("read {label} binary {}: {error}", source.display()))
    })?;
    let actual = sha256_bytes(&bytes);
    if actual != expected {
        return Err(GeneratorError::usage(format!(
            "{label} binary digest mismatch before execution: expected {expected}, got {actual}"
        )));
    }
    let scratch_name = scratch
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("policy-scratch");
    let directory = scratch.with_file_name(format!(
        ".{scratch_name}.verified-binaries/{label}-{expected}"
    ));
    fs::create_dir_all(&directory).map_err(|error| {
        GeneratorError::io(
            "create content-addressed binary directory",
            &directory,
            &error,
        )
    })?;
    set_private_directory(&directory)?;
    let snapshot = directory.join(format!("velnor-workflow{}", env::consts::EXE_SUFFIX));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&snapshot)
        .map_err(|error| {
            GeneratorError::io("create verified binary snapshot", &snapshot, &error)
        })?;
    use std::io::Write as IoWrite;
    file.write_all(&bytes)
        .map_err(|error| GeneratorError::io("write verified binary snapshot", &snapshot, &error))?;
    drop(file);
    set_readonly_executable(&snapshot)?;
    let copied = sha256_file(&snapshot).map_err(GeneratorError::usage)?;
    if copied != expected {
        return Err(GeneratorError::usage(format!(
            "{label} snapshot digest mismatch: expected {expected}, got {copied}"
        )));
    }
    #[cfg(target_os = "linux")]
    let (path, guard) = {
        use rustix::fs::{fchmod, fcntl_add_seals, memfd_create, MemfdFlags, Mode, SealFlags};
        use rustix::io::{fcntl_getfd, fcntl_setfd, FdFlags};
        use std::io::{Read as IoRead, Seek as IoSeek, SeekFrom, Write as IoWrite};
        use std::os::fd::AsRawFd as _;

        let descriptor = memfd_create(
            format!("{label}-{expected}"),
            MemfdFlags::ALLOW_SEALING | MemfdFlags::EXEC,
        )
        .map_err(|error| {
            GeneratorError::usage(format!(
                "create sealed execution handle for {label} product: {error}"
            ))
        })?;
        let mut file = fs::File::from(descriptor);
        file.write_all(&bytes).map_err(|error| {
            GeneratorError::usage(format!("write sealed {label} execution handle: {error}"))
        })?;
        file.flush().map_err(|error| {
            GeneratorError::usage(format!("flush sealed {label} execution handle: {error}"))
        })?;
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            GeneratorError::usage(format!("rewind sealed {label} execution handle: {error}"))
        })?;
        let mut sealed_bytes = Vec::new();
        file.read_to_end(&mut sealed_bytes).map_err(|error| {
            GeneratorError::usage(format!(
                "read back sealed {label} execution handle: {error}"
            ))
        })?;
        let sealed_digest = sha256_bytes(&sealed_bytes);
        if sealed_digest != expected {
            return Err(GeneratorError::usage(format!(
                "{label} execution handle digest mismatch: expected {expected}, got {sealed_digest}"
            )));
        }
        fchmod(
            &file,
            Mode::RUSR | Mode::XUSR | Mode::RGRP | Mode::XGRP | Mode::ROTH | Mode::XOTH,
        )
        .map_err(|error| {
            GeneratorError::usage(format!(
                "mark sealed {label} execution handle executable: {error}"
            ))
        })?;
        fcntl_add_seals(
            &file,
            SealFlags::WRITE | SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL,
        )
        .map_err(|error| {
            GeneratorError::usage(format!(
                "seal {label} execution handle against replacement: {error}"
            ))
        })?;
        let mut flags = fcntl_getfd(&file).map_err(|error| {
            GeneratorError::usage(format!("read {label} execution descriptor flags: {error}"))
        })?;
        flags.remove(FdFlags::CLOEXEC);
        fcntl_setfd(&file, flags).map_err(|error| {
            GeneratorError::usage(format!(
                "inherit sealed {label} execution descriptor: {error}"
            ))
        })?;
        let path = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
        (path, Some(file))
    };
    #[cfg(not(target_os = "linux"))]
    let (path, guard) = (snapshot, None);

    Ok(VerifiedBinarySnapshot {
        path,
        _guard: guard,
    })
}

fn set_private_directory(path: &Path) -> Result<(), GeneratorError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            GeneratorError::io("restrict content-addressed binary directory", path, &error)
        })?;
    }
    Ok(())
}

fn set_readonly_executable(path: &Path) -> Result<(), GeneratorError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o555)).map_err(|error| {
            GeneratorError::io("make verified policy binary read-only", path, &error)
        })?;
    }
    Ok(())
}

fn set_snapshot_file_mode(path: &Path, executable: bool) -> Result<(), GeneratorError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = if executable { 0o555 } else { 0o444 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
            GeneratorError::io("set candidate snapshot file mode", path, &error)
        })?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|error| GeneratorError::io("inspect candidate snapshot file", path, &error))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).map_err(|error| {
            GeneratorError::io("set candidate snapshot file mode", path, &error)
        })?;
    }
    Ok(())
}

fn set_snapshot_directory_mode(path: &Path) -> Result<(), GeneratorError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o555)).map_err(|error| {
            GeneratorError::io("set candidate snapshot directory mode", path, &error)
        })?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)
            .map_err(|error| GeneratorError::io("inspect candidate snapshot directory", path, &error))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).map_err(|error| {
            GeneratorError::io("set candidate snapshot directory mode", path, &error)
        })?;
    }
    Ok(())
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
            let status = closure_identity::sanitized_git_command()
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
            let status = closure_identity::sanitized_git_command()
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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let path = base.join(format!(
        "velnor-workflow-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path)
        .map_err(|error| GeneratorError::io("create scratch directory", &path, &error))?;
    set_private_directory(&path)?;
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
    expected_head: &str,
    expected_base: &str,
    pin: &str,
    default_branch: &str,
    excludes: &BTreeSet<String>,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
    candidate_source: Option<&Path>,
    candidate_render: Option<&Path>,
    candidate_repository: Option<&str>,
    candidate_source_repository: Option<&str>,
    candidate_run_id: Option<&str>,
    candidate_platform: Option<&str>,
    candidate_builder_image: Option<&str>,
) -> Result<TreeComparison, GeneratorError> {
    validate_history_checkout(checkout, expected_head, expected_base)?;
    // Unit jobs use shallow checkouts. Fetch a missing owner-repository D19
    // pin here before ancestry and closure evaluation; a foreign consumer
    // continues through the product release and revision fallback.
    let expected = match source {
        PinSource::Checkout(_) => {
            ensure_pin_present(checkout, pin)?;
            Some(expected_closures(checkout, pin)?)
        }
        PinSource::Remote(_) => expected_closures(checkout, pin).ok(),
    };
    if let Some(candidate_render) = candidate_render {
        let closure = verify_candidate_render_artifact(
            candidate_render,
            candidate_source.ok_or_else(|| {
                GeneratorError::usage("--candidate-source is required with --candidate-render")
            })?,
            checkout,
            tree,
            expected_head,
            expected_base,
            excludes,
            candidate_repository,
            candidate_source_repository,
            candidate_run_id,
            candidate_platform,
            candidate_builder_image,
        )?;
        return Ok(closure.map_or_else(
            || {
                TreeComparison::Differences(vec![
                    "the sealed isolated candidate render differs from the audited tree".to_owned(),
                ])
            },
            TreeComparison::Candidate,
        ));
    }
    let scratch = scratch_directory("policy-render")?;
    let result = (|| {
        let lookup = lookup.snapshot_pinned_binary(&scratch)?;
        let binary = resolve_pinned_binary(pin, expected.as_deref(), &lookup, source)?;
        let bound_digest = (lookup.pinned_binary.as_deref() == Some(binary.as_path()))
            .then_some(lookup.pinned_binary_sha256.as_deref())
            .flatten();
        if let Some(expected_revision) = &lookup.pinned_binary_revision
            && lookup.pinned_binary.as_deref() == Some(binary.as_path())
        {
            let reported = binary_revision(&binary).map_err(GeneratorError::usage)?;
            if &reported != expected_revision {
                return Err(GeneratorError::usage(format!(
                    "pinned policy binary reports revision {reported}, but its verified manifest names {expected_revision}"
                )));
            }
        }
        let source_root = if candidate_source.is_some() {
            tree
        } else {
            checkout
        };
        render_and_compare(
            &binary,
            bound_digest,
            source_root,
            tree,
            &scratch,
            default_branch,
            excludes,
        )
        .and_then(|differences| {
            if differences.is_empty() {
                return Ok(TreeComparison::Pin);
            }
            let Some(candidate_render) = candidate_render else {
                return Ok(TreeComparison::Differences(differences));
            };
            match verify_candidate_render_artifact(
                candidate_render,
                candidate_source.ok_or_else(|| {
                    GeneratorError::usage(
                        "--candidate-source is required with --candidate-render",
                    )
                })?,
                checkout,
                tree,
                expected_head,
                expected_base,
                excludes,
                candidate_repository,
                candidate_source_repository,
                candidate_run_id,
                candidate_platform,
                candidate_builder_image,
            )? {
                Some(closure) => Ok(TreeComparison::Candidate(closure)),
                None => Ok(TreeComparison::Differences(differences)),
            }
        })
    })();
    let _ = fs::remove_dir_all(&scratch);
    result
}

fn render_and_compare(
    binary: &Path,
    expected_digest: Option<&str>,
    checkout: &Path,
    root: &Path,
    scratch: &Path,
    default_branch: &str,
    excludes: &BTreeSet<String>,
) -> Result<Vec<String>, GeneratorError> {
    if let Some(expected) = expected_digest {
        let actual = sha256_file(binary).map_err(GeneratorError::usage)?;
        if actual != expected {
            return Err(GeneratorError::usage(format!(
                "policy runtime changed before execution: expected digest {expected}, got {actual}"
            )));
        }
    }
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
    let mut aggregate_yaml = None;
    let aggregate_document = match fs::read_to_string(&aggregate) {
        Ok(yaml) => {
            aggregate_yaml = Some(yaml.clone());
            let parser = serde_yaml::ParserConfig::default()
                .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
            match serde_yaml::from_str_with_config::<Value>(&yaml, &parser) {
                Ok(document) => Some(document),
                Err(error) => {
                    findings.push(format!("{PULL_REQUEST_AGGREGATE}: parse: {error}"));
                    None
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            findings.push(format!("{PULL_REQUEST_AGGREGATE}: {error}"));
            None
        }
    };
    let emitted = aggregate_yaml
        .as_deref()
        .and_then(|yaml| super::workflow_job_display_names(yaml).ok());
    if aggregate_document.is_some() && emitted.is_none() {
        findings.push(format!(
            "{PULL_REQUEST_AGGREGATE}: jobs must be a valid workflow mapping"
        ));
    }
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
    if declared.required_checks.iter().any(|context| context == "ci-required") {
        let required_job = aggregate_document
            .as_ref()
            .and_then(Value::as_mapping)
            .and_then(|workflow| mapping_value(workflow, "jobs"))
            .and_then(Value::as_mapping)
            .and_then(|jobs| mapping_value(jobs, "ci-required"))
            .and_then(Value::as_mapping);
        match required_job {
            Some(job)
                if mapping_value(job, "name").and_then(Value::as_str) == Some("ci-required")
                    && mapping_value(job, "if")
                        .and_then(Value::as_str)
                        .is_some_and(|condition| {
                            normalize_gate_expression(condition) == "always()"
                        }) => {}
            Some(_) => findings.push(format!(
                "{PULL_REQUEST_AGGREGATE} job ci-required must have name `ci-required` and `if: always()` so the required context cannot skip"
            )),
            None => findings.push(format!(
                "{PULL_REQUEST_AGGREGATE} must define the required `ci-required` job"
            )),
        }
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
    audit_entrypoint_triggers(workflow, &velnor_policy.default_branch, &mut audit);
    audit_entrypoint_privileges(workflow, &content, velnor_policy, &mut audit);
    Ok(audit)
}

/// The privileged entrypoint is base-owned and runs only for pull requests
/// targeting the configured default branch. It has no branch-selectable
/// dispatch path.
fn audit_entrypoint_triggers(
    workflow: &Mapping,
    default_branch: &str,
    audit: &mut EntrypointAudit,
) {
    let finding = |message: &str| format!("{POLICY_ENTRYPOINT}: {message}");
    match mapping_value(workflow, "on").and_then(Value::as_mapping) {
        Some(on) => {
            for key in on.keys() {
                if key.as_str() != Some("pull_request_target") {
                    audit.trigger.push(finding(&format!(
                        "trigger `{key}` is not admitted; the entrypoint runs only on pull_request_target"
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
                    let branches = mapping_value(event, "branches")
                        .and_then(Value::as_sequence)
                        .map(|branches| {
                            branches
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if branches != [default_branch] {
                        audit.trigger.push(finding(&format!(
                            "pull_request_target branches must be exactly [{default_branch:?}], got [{}]",
                            branches
                                .iter()
                                .map(|branch| format!("{branch:?}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )));
                    }
                    for key in event.keys() {
                        if !matches!(key.as_str(), Some("types" | "branches")) {
                            audit.trigger.push(finding(&format!(
                                "pull_request_target filter `{key}` is not admitted"
                            )));
                        }
                    }
                }
                None => audit.trigger.push(finding(
                    "must run on pull_request_target with explicit default-branch and activity-type filters",
                )),
            }
        }
        None => audit
            .trigger
            .push(finding("`on` must be a mapping of triggers")),
    }
}

/// Privileges: no workflow-level token grants, three jobs with explicit narrow
/// scopes, and a required always-run aggregator that fails on any failed or
/// skipped dependency. Candidate rendering runs in the permissions-free job.
fn audit_entrypoint_privileges(
    workflow: &Mapping,
    content: &str,
    velnor_policy: &VelnorPolicyContract,
    audit: &mut EntrypointAudit,
) {
    let finding = |message: &str| format!("{POLICY_ENTRYPOINT}: {message}");
    if !is_no_permissions(mapping_value(workflow, "permissions")) {
        audit.privileges.push(finding(
            "workflow permissions must be `{}`; each job declares its own minimum scope",
        ));
    }
    let references_secrets = content.lines().any(|line| line.contains("secrets."));
    if references_secrets {
        audit
            .privileges
            .push(finding("must not reference `secrets.`"));
    }
    let Some(jobs) = mapping_value(workflow, "jobs").and_then(Value::as_mapping) else {
        audit.privileges.push(finding("`jobs` must be a mapping"));
        return;
    };
    let expected_jobs = ["acquire-source", "render-candidate", "policy"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let actual_jobs = jobs.keys().filter_map(Value::as_str).collect::<BTreeSet<_>>();
    if actual_jobs != expected_jobs {
        audit.privileges.push(finding(&format!(
            "jobs must be exactly [{}], got [{}]",
            expected_jobs.iter().copied().collect::<Vec<_>>().join(", "),
            actual_jobs.iter().copied().collect::<Vec<_>>().join(", ")
        )));
    }
    for (job_id, job) in jobs {
        let name = job_id.as_str().unwrap_or("<non-string>");
        let Some(job) = job.as_mapping() else {
            audit
                .privileges
                .push(finding(&format!("job {name} must be a YAML mapping")));
            continue;
        };
        if mapping_value(job, "continue-on-error").is_some_and(|value| value != &Value::Bool(false)) {
            audit.privileges.push(finding(&format!(
                "job {name} must not turn a failed required job into success with continue-on-error"
            )));
        }
        let expected_permissions = match name {
            "acquire-source" | "policy" => is_contents_read_only(mapping_value(job, "permissions")),
            "render-candidate" => is_no_permissions(mapping_value(job, "permissions")),
            _ => false,
        };
        if !expected_permissions {
            audit.privileges.push(finding(&format!(
                "job {name} permissions do not match its least-privilege contract"
            )));
        }
        if mapping_value(job, "environment").is_some() {
            audit.privileges.push(finding(&format!(
                "job {name} must not bind a deployment environment"
            )));
        }
        let Some(runs_on) = mapping_value(job, "runs-on") else {
            audit
                .privileges
                .push(finding(&format!("job {name} declares no runs-on")));
            continue;
        };
        let mut resolving = BTreeSet::new();
        let analysis = analyze_runner(runs_on, None, &mut resolving, velnor_policy);
        let provider = static_provider_for_runs_on(runs_on, velnor_policy);
        if analysis.dynamic
            || analysis.invalid
            || analysis.local_provider
            || analysis.foreign
            || provider != Some("github-hosted")
        {
            audit.privileges.push(finding(&format!(
                "job {name} must run on the trusted static GitHub-hosted selector"
            )));
        }
        if let Some(needs) = mapping_value(job, "needs") {
            let needs = needs
                .as_sequence()
                .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .or_else(|| needs.as_str().map(|value| vec![value]))
                .unwrap_or_default();
            let expected_needs: &[&str] = match name {
                "acquire-source" => &[],
                "render-candidate" => &["acquire-source"],
                "policy" => &["acquire-source", "render-candidate"],
                _ => &[],
            };
            if needs != expected_needs {
                audit.privileges.push(finding(&format!(
                    "job {name} needs must be [{}], got [{}]",
                    expected_needs.join(", "),
                    needs.join(", ")
                )));
            }
        } else if name != "acquire-source" {
            audit
                .privileges
                .push(finding(&format!("job {name} must declare its required needs")));
        }
        let steps = mapping_value(job, "steps")
            .and_then(Value::as_sequence)
            .cloned()
            .unwrap_or_default();
        let expected_step_names: &[&str] = match name {
            "acquire-source" => &[
                "Check out trusted base source",
                "Build trusted source-object tool",
                "Fetch the exact pull request head as Git objects",
                "Snapshot candidate source without executing it",
                "Validate and vendor approved locked dependencies",
                "Upload raw source objects",
            ],
            "render-candidate" => &[
                "Download base-produced source objects",
                "Render candidate with no host credentials or command files",
                "Upload render data only",
            ],
            "policy" => &[
                "Require source and render jobs to succeed",
                "Check out trusted base history",
                "Download base-produced source objects",
                "Download isolated render data",
                "Fetch exact PR head objects for ancestry and closure checks",
                "Verify base tool and materialize audited source",
                "Resolve required status checks",
                "Run base-owned policy on immutable source and render data",
                "Set up actionlint",
                "Lint caller workflows",
            ],
            _ => &[],
        };
        let actual_step_names = steps
            .iter()
            .map(|step| {
                step.as_mapping()
                    .and_then(|step| mapping_value(step, "name"))
                    .and_then(Value::as_str)
            })
            .collect::<Option<Vec<_>>>();
        if actual_step_names.as_deref() != Some(expected_step_names) {
            audit.privileges.push(finding(&format!(
                "job {name} step sequence must be [{}]",
                expected_step_names.join(", ")
            )));
        }
        let action_pins_match = match name {
            "acquire-source" => {
                entrypoint_step_uses(&steps, 0, super::ActionPin::Checkout)
                    && entrypoint_step_uses(&steps, 5, super::ActionPin::UploadArtifact)
            }
            "render-candidate" => {
                entrypoint_step_uses(&steps, 0, super::ActionPin::DownloadArtifact)
                    && entrypoint_step_uses(&steps, 2, super::ActionPin::UploadArtifact)
            }
            "policy" => {
                entrypoint_step_uses(&steps, 1, super::ActionPin::Checkout)
                    && entrypoint_step_uses(&steps, 2, super::ActionPin::DownloadArtifact)
                    && entrypoint_step_uses(&steps, 3, super::ActionPin::DownloadArtifact)
                    && entrypoint_step_uses(&steps, 8, super::ActionPin::Mise)
            }
            _ => false,
        };
        if !action_pins_match {
            audit.privileges.push(finding(&format!(
                "job {name} must use the reviewed checkout, artifact, and tool actions at their fixed steps"
            )));
        }
        let scripts = steps
            .iter()
            .filter_map(|step| {
                let step = step.as_mapping()?;
                if mapping_value(step, "continue-on-error")
                    .is_some_and(|value| value != &Value::Bool(false))
                {
                    let step_name = mapping_value(step, "name")
                        .and_then(Value::as_str)
                        .unwrap_or("<unnamed>");
                    audit.privileges.push(finding(&format!(
                        "job {name} step {step_name} must not use continue-on-error"
                    )));
                }
                mapping_value(step, "run").and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        let combined_scripts = scripts.join("\n");
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
                        "job {name} checkout must set `persist-credentials: false`"
                    )));
                }
            }
        }
        if name == "policy" {
            let job_if = mapping_value(job, "if").and_then(Value::as_str);
            if !job_if.is_some_and(|condition| normalize_gate_expression(condition) == "always()") {
                audit.privileges.push(finding(
                    "required Policy job must use `if: always()` so failed, skipped, or cancelled dependencies cannot turn it into a skipped-success check",
                ));
            }
            let gate = steps.first().and_then(Value::as_mapping);
            if !gate.is_some_and(|step| {
                mapping_value(step, "name").and_then(Value::as_str)
                    == Some("Require source and render jobs to succeed")
                    && mapping_value(step, "if")
                        .and_then(Value::as_str)
                        .is_some_and(|condition| normalize_gate_expression(condition) == "always()")
                    && mapping_value(step, "run")
                        .and_then(Value::as_str)
                        .is_some_and(|script| {
                            script.contains("SOURCE_RESULT\") != success")
                                && script.contains("RENDER_RESULT\") != success")
                        })
            }) {
                audit.privileges.push(finding(
                    "Policy must start with an always-run gate that fails unless both dependencies succeeded",
                ));
            }
            let policy_command = scripts.iter().any(|script| {
                script.contains("\"$policy_tool\" policy \\")
                    && script.contains("--trusted-root \"$GITHUB_WORKSPACE/policy-base\"")
                    && script.contains("--git-root \"$GITHUB_WORKSPACE/policy-base\"")
                    && script.contains("--candidate-source \"$source_artifact\"")
                    && script.contains("--candidate-render \"$RUNNER_TEMP/candidate-render-artifact\"")
                    && script.contains("--head-sha \"$HEAD_SHA\"")
                    && script.contains("--base-sha \"$BASE_SHA\"")
                    && script.contains("--candidate-repository \"$SOURCE_REPOSITORY\"")
                    && script.contains("--candidate-source-repository \"$HEAD_REPOSITORY\"")
                    && script.contains("--candidate-run-id \"$RUN_ID\"")
                    && script.contains("--candidate-platform Linux-X64")
                    && script.contains("--candidate-builder-image \"$CANDIDATE_BUILDER_IMAGE\"")
                    && script.contains("sha256sum \"$policy_tool\"")
                    && script.contains("\"$actual\" == \"$EXPECTED_POLICY_TOOL_SHA256\"")
            });
            if !policy_command {
                audit.privileges.push(finding(
                    "Policy must execute the base-acquired tool against the trusted checkout, raw source artifact, and isolated render artifact",
                ));
            }
            let actionlint = scripts.iter().any(|script| {
                script.contains("mise exec actionlint@") && script.contains("-- actionlint")
            });
            if !actionlint {
                audit.privileges.push(finding(
                    "Policy must lint the caller workflows with the pinned actionlint command",
                ));
            }
        } else if name == "acquire-source" {
            let source_pipeline = [
                "cargo build --locked --release --no-default-features --package velnor-workflow --bin velnor-workflow",
                "--manifest-path \"$GITHUB_WORKSPACE/policy-base/Cargo.toml\"",
                "policy snapshot-source",
                "policy materialize-source",
                "policy validate-cargo-sources",
                "cargo vendor --locked --manifest-path \"$source_root/Cargo.toml\"",
                "policy seal-source-vendor",
            ]
            .iter()
            .all(|fragment| combined_scripts.contains(fragment));
            if !source_pipeline
                || combined_scripts.contains("cargo run")
                || combined_scripts.contains("cargo test")
                || combined_scripts.contains("cargo check")
            {
                audit.privileges.push(finding(
                    "source acquisition must build only the trusted base tool, validate candidate locks, and vendor without executing candidate code",
                ));
            }
        } else if name == "render-candidate" {
            if mapping_value(job, "container").is_some()
                || mapping_value(job, "services").is_some()
            {
                audit.privileges.push(finding(
                    "candidate render must not use an Actions job container or service that can expose runner files",
                ));
            }
            let render_sandbox = scripts.iter().any(|script| {
                script.contains("docker run --rm --network=none")
                    && script.contains("--user 65532:65532")
                    && script.contains("--read-only")
                    && script.contains("--cap-drop=ALL")
                    && script.contains("--security-opt=no-new-privileges")
                    && script.contains("dst=/source,readonly")
                    && script.contains("dst=/vendor,readonly")
                    && script.contains("dst=/rendered")
                    && script.contains("cargo build --locked --offline")
                    && script.contains("policy seal-candidate-render")
                    && !script.contains("--privileged")
                    && !script.contains("--network=bridge")
                    && !script.contains("--network=host")
                    && !script.contains("/var/run/docker.sock")
                    && !script.contains("GITHUB_ENV")
                    && !script.contains("GITHUB_PATH")
                    && !script.contains("GITHUB_OUTPUT")
            });
            if !render_sandbox {
                audit.privileges.push(finding(
                    "candidate code must build and render only in an unprivileged, read-only-source, networkless sandbox with no Actions command files or sockets",
                ));
            }
            let builder_pin = steps.iter().any(|step| {
                step.as_mapping().is_some_and(|step| {
                    mapping_value(step, "env")
                        .and_then(Value::as_mapping)
                        .and_then(|env| mapping_value(env, "BUILDER_IMAGE"))
                        .and_then(Value::as_str)
                        == Some(super::CANDIDATE_BUILDER_IMAGE)
                })
            });
            if !builder_pin {
                audit.privileges.push(finding(
                    "candidate renderer must use the base-pinned digest builder image",
                ));
            }
            let hidden_upload = steps.iter().any(|step| {
                step.as_mapping().is_some_and(|step| {
                    mapping_value(step, "uses")
                        .and_then(Value::as_str)
                        .is_some_and(|uses| uses.starts_with("actions/upload-artifact@"))
                        && mapping_value(step, "with")
                            .and_then(Value::as_mapping)
                            .and_then(|with| mapping_value(with, "include-hidden-files"))
                            .and_then(Value::as_bool)
                            == Some(true)
                })
            });
            if !hidden_upload {
                audit.privileges.push(finding(
                    "candidate render artifact upload must include generated dotfiles",
                ));
            }
        }
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

fn is_no_permissions(permissions: Option<&Value>) -> bool {
    permissions
        .and_then(Value::as_mapping)
        .is_some_and(Mapping::is_empty)
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
    let velnor_policy = configured_velnor_policy(root)?;
    let policy_excludes = configured_policy_excludes(root);
    audit_workflows_with_contract(root, &velnor_policy, &policy_excludes)
}

fn audit_workflows_with_contract(
    root: &Path,
    velnor_policy: &VelnorPolicyContract,
    _policy_excludes: &BTreeSet<String>,
) -> Result<WorkflowAudit, GeneratorError> {
    let workflows = root.join(".github/workflows");
    let metadata = fs::symlink_metadata(&workflows)
        .map_err(|error| GeneratorError::io("inspect workflow directory", &workflows, &error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(GeneratorError::usage(format!(
            "workflow directory {} must be a real directory",
            workflows.display()
        )));
    }
    let entries = fs::read_dir(&workflows)
        .map_err(|error| GeneratorError::io("read workflow directory", &workflows, &error))?;
    let policy_entrypoint = workflows.join("ci-policy.yml");
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
        if !matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| GeneratorError::io("inspect workflow file", &path, &error))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            findings.record(
                Rule::Structure,
                &path,
                "workflow must be a regular file, not a symlink or special file",
            );
            continue;
        }
        paths.push(path);
    }
    paths.sort();
    let mut documents = BTreeMap::new();
    for path in paths {
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
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| GeneratorError::usage("workflow filename is not UTF-8"))?
            .to_owned();
        inspect_workflow(
            workflow,
            &path,
            path == policy_entrypoint,
            velnor_policy,
            &mut findings,
        );
        documents.insert(name, document);
    }
    audit_local_workflow_graph(root, &documents, velnor_policy, &mut findings);
    Ok(findings.audit)
}

fn audit_local_workflow_graph(
    root: &Path,
    documents: &BTreeMap<String, Value>,
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    let workflows = root.join(".github/workflows");
    let mut edges = BTreeMap::<String, Vec<String>>::new();
    let mut has_local_runner = BTreeMap::<String, bool>::new();

    for (name, document) in documents {
        let path = workflows.join(name);
        let Some(workflow) = document.as_mapping() else {
            continue;
        };
        if workflow_events(workflow).is_none() {
            failures.record(
                Rule::Structure,
                &path,
                "workflow must declare a valid `on` trigger mapping or sequence",
            );
        }
        let jobs = mapping_value(workflow, "jobs").and_then(Value::as_mapping);
        let mut local = false;
        let mut targets = Vec::new();
        if let Some(jobs) = jobs {
            for (job_id, job) in jobs {
                let Some(job) = job.as_mapping() else {
                    continue;
                };
                if mapping_value(job, "runs-on")
                    .and_then(|runs_on| static_provider_for_runs_on(runs_on, velnor_policy))
                    .is_some_and(VelnorPolicyContract::is_local_provider)
                {
                    local = true;
                }
                let uses = mapping_value(job, "uses").and_then(Value::as_str);
                let Some(uses) = uses.filter(|uses| is_approved_local_reusable(uses)) else {
                    continue;
                };
                let target = uses
                    .strip_prefix("./.github/workflows/")
                    .unwrap_or_default()
                    .to_owned();
                if !documents.contains_key(&target) {
                    failures.job = job_id.as_str().map(str::to_owned);
                    failures.record(
                        Rule::Structure,
                        &path,
                        &format!("local reusable workflow {uses:?} does not exist"),
                    );
                    failures.job = None;
                    continue;
                }
                let target_is_reusable = documents
                    .get(&target)
                    .and_then(Value::as_mapping)
                    .and_then(|target| mapping_value(target, "on"))
                    .and_then(Value::as_mapping)
                    .is_some_and(|events| mapping_value(events, "workflow_call").is_some());
                if !target_is_reusable {
                    failures.job = job_id.as_str().map(str::to_owned);
                    failures.record(
                        Rule::Structure,
                        &path,
                        &format!("local reusable workflow {uses:?} has no on.workflow_call trigger"),
                    );
                    failures.job = None;
                    continue;
                }
                targets.push(target);
            }
        }
        targets.sort();
        targets.dedup();
        edges.insert(name.clone(), targets);
        has_local_runner.insert(name.clone(), local);
    }

    let mut memo = BTreeMap::new();
    let mut active = BTreeSet::new();
    let mut cycles = BTreeSet::new();
    let roots = documents
        .iter()
        .filter_map(|(name, document)| {
            let workflow = document.as_mapping()?;
            let events = workflow_events(workflow)?;
            events
                .iter()
                .any(|event| event != "workflow_call")
                .then_some(name.as_str())
        })
        .collect::<Vec<_>>();
    for name in roots {
        let reaches_local = workflow_reaches_local_runner(
            name,
            &edges,
            &has_local_runner,
            &mut memo,
            &mut active,
            &mut cycles,
        );
        if reaches_local {
            let path = workflows.join(name);
            let is_safe = documents
                .get(name)
                .and_then(Value::as_mapping)
                .is_some_and(|workflow| {
                    is_default_branch_push_only(workflow, &velnor_policy.default_branch)
                });
            if !is_safe {
                failures.record(
                    Rule::TrustedRunners,
                    &path,
                    "a workflow that can reach a local runner must be triggered only by push to the trusted default branch",
                );
            }
        }
    }
    for cycle in cycles {
        failures.record(
            Rule::Structure,
            &workflows.join(&cycle),
            "local reusable workflow call graph contains a cycle",
        );
    }
}

fn workflow_reaches_local_runner(
    name: &str,
    edges: &BTreeMap<String, Vec<String>>,
    has_local_runner: &BTreeMap<String, bool>,
    memo: &mut BTreeMap<String, bool>,
    active: &mut BTreeSet<String>,
    cycles: &mut BTreeSet<String>,
) -> bool {
    if let Some(result) = memo.get(name) {
        return *result;
    }
    if !active.insert(name.to_owned()) {
        cycles.insert(name.to_owned());
        return false;
    }
    let mut reaches = has_local_runner.get(name).copied().unwrap_or(false);
    for target in edges.get(name).into_iter().flatten() {
        reaches |= workflow_reaches_local_runner(
            target,
            edges,
            has_local_runner,
            memo,
            active,
            cycles,
        );
    }
    active.remove(name);
    memo.insert(name.to_owned(), reaches);
    reaches
}

fn workflow_events(workflow: &Mapping) -> Option<BTreeSet<String>> {
    let on = mapping_value(workflow, "on")?;
    match on {
        Value::Mapping(events) => events
            .keys()
            .map(|event| event.as_str().map(str::to_owned))
            .collect(),
        Value::Sequence(events) => events
            .iter()
            .map(|event| event.as_str().map(str::to_owned))
            .collect(),
        Value::String(event) => Some(BTreeSet::from([event.clone()])),
        _ => None,
    }
}

fn is_default_branch_push_only(workflow: &Mapping, default_branch: &str) -> bool {
    let Some(on) = mapping_value(workflow, "on").and_then(Value::as_mapping) else {
        return false;
    };
    if on.len() != 1 {
        return false;
    }
    let Some(push) = mapping_value(on, "push").and_then(Value::as_mapping) else {
        return false;
    };
    if push.len() != 1 {
        return false;
    }
    mapping_value(push, "branches")
        .and_then(Value::as_sequence)
        .is_some_and(|branches| {
            branches.len() == 1 && branches[0].as_str() == Some(default_branch)
        })
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
    let provider = static_provider_for_runs_on(runs_on, velnor_policy)?;
    VelnorPolicyContract::is_local_provider(provider).then(|| provider.to_owned())
}

fn static_provider_for_runs_on<'a>(
    runs_on: &'a Value,
    velnor_policy: &'a VelnorPolicyContract,
) -> Option<&'a str> {
    let labels = match runs_on {
        Value::String(label) => vec![label.as_str()],
        Value::Sequence(sequence) => sequence
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?,
        Value::Mapping(mapping) => mapping_value(mapping, "labels")
            .and_then(Value::as_sequence)?
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()?,
        _ => return None,
    };
    if labels.iter().any(|label| label.contains("${{")) {
        return None;
    }
    velnor_policy.provider_for_labels(&labels)
}

fn local_runner_group_is_valid(
    runs_on: &Value,
    velnor_policy: &VelnorPolicyContract,
) -> bool {
    if !velnor_policy.require_local_runner_group {
        return true;
    }
    let Some(mapping) = runs_on.as_mapping() else {
        return false;
    };
    let Some(group) = mapping_value(mapping, "group").and_then(Value::as_str) else {
        return false;
    };
    let Some(provider) = static_provider_for_runs_on(runs_on, velnor_policy) else {
        return false;
    };
    VelnorPolicyContract::is_local_provider(provider)
        && velnor_policy
            .selector_groups
            .get(provider)
            .is_some_and(|expected| expected == group)
}

fn has_safe_runner_gate(
    condition: &str,
    job: &Mapping,
    velnor_policy: &VelnorPolicyContract,
) -> bool {
    let Some(provider) = static_local_provider(job, velnor_policy) else {
        return false;
    };
    (velnor_policy.automatic_providers.iter().any(|item| item == &provider)
        && has_trusted_runner_gate(
            condition,
            &velnor_policy.default_branch,
            velnor_policy.repository.as_deref(),
        ))
        || is_generated_provider_gate(condition, &provider, velnor_policy)
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

/// Trusted provider facts come only from the base generation config. The
/// generated runtime config and the candidate tree cannot redefine runner
/// labels, groups, automatic admission, or the default branch.
#[derive(Clone, Debug, Default)]
struct VelnorPolicyContract {
    repository: Option<String>,
    providers: Vec<String>,
    automatic_providers: Vec<String>,
    selectors: BTreeMap<String, Vec<String>>,
    selector_groups: BTreeMap<String, String>,
    require_local_runner_group: bool,
    default_branch: String,
}

impl VelnorPolicyContract {
    /// The provider whose declared selector `labels` equals, if any. Labels
    /// are compared as sets: order is not a routing fact.
    fn provider_for_labels(&self, labels: &[&str]) -> Option<&str> {
        let mut sorted = labels
            .iter()
            .map(|label| normalize_runner_label(label))
            .collect::<Vec<_>>();
        sorted.sort_unstable();
        self.selectors.iter().find_map(|(provider, selector)| {
            let mut expected = selector
                .iter()
                .map(|label| normalize_runner_label(label))
                .collect::<Vec<_>>();
            expected.sort_unstable();
            (expected == sorted).then_some(provider.as_str())
        })
    }

    fn is_local_provider(provider: &str) -> bool {
        matches!(provider, "github-self-hosted" | "velnor")
    }
}

fn configured_velnor_policy(root: &Path) -> Result<VelnorPolicyContract, GeneratorError> {
    let generation = generation_workflow(root)?;
    if generation.is_none() {
        return Ok(VelnorPolicyContract {
            repository: None,
            default_branch: "main".to_owned(),
            ..VelnorPolicyContract::default()
        });
    }
    let generation_workflow = generation.as_ref().and_then(toml::Value::as_table);
    let providers = toml_string_array(
        generation_workflow.and_then(|workflow| workflow.get("providers")),
        "[workflow] providers",
    )?;
    let automatic_providers = toml_string_array(
        generation_workflow.and_then(|workflow| workflow.get("automatic_providers")),
        "[workflow] automatic_providers",
    )?;
    let require_local_runner_group = generation_workflow
        .and_then(|workflow| workflow.get("require_local_runner_group"))
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                GeneratorError::usage(
                    "[workflow] require_local_runner_group must be a boolean",
                )
            })
        })
        .transpose()?
        .unwrap_or(false);
    // Selectors are generation-time facts. Resolve them only from the
    // repository-owned declaration; a policy audit must not reconstruct the
    // generator's former scan defaults.
    let mut selectors: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut selector_groups = BTreeMap::new();
    if let Some(tables) = generation_workflow
        .and_then(|workflow| workflow.get("selectors"))
        .and_then(toml::Value::as_table)
    {
        for (provider, table) in tables {
            let runs_on = toml_string_array(
                table.get("runs_on"),
                &format!("[workflow.selectors.{provider}] runs_on"),
            )?;
            if runs_on.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "[workflow.selectors.{provider}] runs_on must name at least one label"
                )));
            }
            selectors.insert(provider.clone(), runs_on);
            if let Some(group) = table.get("group") {
                let group = group.as_str().ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "[workflow.selectors.{provider}] group must be a static string"
                    ))
                })?;
                selector_groups.insert(provider.clone(), group.to_owned());
            }
        }
    }
    let default_branch = generation_workflow
        .and_then(|workflow| workflow.get("default_branch"))
        .and_then(toml::Value::as_str)
        .unwrap_or("main")
        .to_owned();
    let policy = VelnorPolicyContract {
        repository: config::discover(root)?
            .as_ref()
            .and_then(config::RepoGenerationConfig::repository)
            .map(str::to_owned),
        providers,
        automatic_providers,
        selectors,
        selector_groups,
        require_local_runner_group,
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
        if self.providers.is_empty() {
            return Err(GeneratorError::usage(
                "workflow policy requires an explicit non-empty provider universe",
            ));
        }
        if self.providers.iter().any(|provider| Self::is_local_provider(provider))
            && self.repository.as_deref().is_none_or(|repository| {
                !repository.contains('/')
                    || repository.split('/').count() != 2
                    || repository
                        .split('/')
                        .any(|segment| segment.is_empty() || segment.chars().any(char::is_control))
            })
        {
            return Err(GeneratorError::usage(
                "workflow policy with local providers requires a trusted generation repository slug",
            ));
        }
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
        let provider_set = self
            .providers
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if provider_set.len() != self.providers.len() {
            return Err(GeneratorError::usage(
                "workflow policy provider universe contains a duplicate provider",
            ));
        }
        let mut automatic_seen = BTreeSet::new();
        for provider in &self.automatic_providers {
            if !automatic_seen.insert(provider.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy automatic provider set contains `{provider}` more than once"
                )));
            }
            if !provider_set.contains(provider.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy found automatic provider `{provider}` outside the configured provider universe"
                )));
            }
        }
        for (provider, selector) in &self.selectors {
            if !matches!(
                provider.as_str(),
                "github-hosted" | "github-self-hosted" | "velnor"
            ) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy found unknown provider `{provider}` in [workflow.selectors]"
                )));
            }
            if selector.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "workflow policy found provider `{provider}` with an empty selector"
                )));
            }
            let mut labels = BTreeSet::new();
            for label in selector {
                if label.is_empty() || label.chars().any(char::is_control) {
                    return Err(GeneratorError::usage(format!(
                        "workflow policy selector `{provider}` has an empty or control-character label"
                    )));
                }
                let normalized = normalize_runner_label(label);
                if !labels.insert(normalized) {
                    return Err(GeneratorError::usage(format!(
                        "workflow policy selector `{provider}` repeats label `{label}`; GitHub runner labels are case-insensitive"
                    )));
                }
                if provider == "github-hosted"
                    && (label.trim().eq_ignore_ascii_case("self-hosted")
                        || label.contains("${{")
                        || label.contains("}}"))
                {
                    return Err(GeneratorError::usage(format!(
                        "workflow policy hosted selector cannot name self-hosted runners or use expressions: `{label}`"
                    )));
                }
            }
            if !provider_set.contains(provider.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy found selector for `{provider}` outside the configured provider universe"
                )));
            }
        }
        for provider in &self.providers {
            if !self.selectors.contains_key(provider) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy requires [workflow.selectors.{provider}] for every configured provider"
                )));
            }
        }
        if !runtime::valid_branch(&self.default_branch) {
            return Err(GeneratorError::usage(format!(
                "workflow policy has an invalid default branch {:?}",
                self.default_branch
            )));
        }
        for (provider, group) in &self.selector_groups {
            if !VelnorPolicyContract::is_local_provider(provider) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy runner group is valid only for local providers, found `{provider}`"
                )));
            }
            if !provider_set.contains(provider.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy runner group for `{provider}` is outside the configured provider universe"
                )));
            }
            if group.is_empty()
                || group.chars().any(char::is_control)
                || group.contains("${{")
                || group.contains("}}")
            {
                return Err(GeneratorError::usage(format!(
                    "workflow policy runner group for `{provider}` must be a non-empty static name"
                )));
            }
        }
        if self.require_local_runner_group {
            for provider in &self.providers {
                if VelnorPolicyContract::is_local_provider(provider)
                    && !self.selector_groups.contains_key(provider)
                {
                    return Err(GeneratorError::usage(format!(
                        "workflow policy requires [workflow.selectors.{provider}] group for local runner trust"
                    )));
                }
            }
        }
        let mut claimed_labels: BTreeMap<String, (&str, &str)> = BTreeMap::new();
        for (provider, selector) in &self.selectors {
            for label in selector {
                let normalized = normalize_runner_label(label);
                if let Some((owner, owner_label)) = claimed_labels.get(&normalized) {
                    let local_pair = (*owner == "github-self-hosted" && provider == "velnor")
                        || (*owner == "velnor" && provider == "github-self-hosted");
                    if local_pair {
                        return Err(GeneratorError::usage(format!(
                            "workflow policy local selectors share `{label}` (also `{owner_label}`); official and Velnor selectors must be disjoint ignoring case"
                        )));
                    }
                    return Err(GeneratorError::usage(format!(
                        "workflow policy selectors `{owner}` (`{owner_label}`) and `{provider}` (`{label}`) overlap; GitHub runner labels are case-insensitive"
                    )));
                }
                claimed_labels.insert(normalized, (provider.as_str(), label.as_str()));
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

/// The normalized exclusion every generated local-provider admission carries.
/// It is redundant on the only allowed event (default-branch push), but stays
/// in sync with the generator's single provider-admission expression.
fn trusted_event_conjunct() -> &'static str {
    "!(github.event_name=='pull_request'&&(github.event.pull_request.head.repo.fork||github.event.pull_request.user.type=='Bot'))"
}

/// Whether `value` is the generated admission for a local provider. Local
/// execution has one admitted source: push to the trusted default branch.
/// Branch-selected dispatch and every pull-request/workflow-run event stay
/// denied even when the job-level YAML contains this gate.
fn is_generated_provider_gate(
    value: &str,
    provider: &str,
    velnor_policy: &VelnorPolicyContract,
) -> bool {
    let normalized = normalize_gate_expression(value);
    let value = strip_reusable_unit_selector(&normalized).unwrap_or(&normalized);
    let dispatch = "false";
    let automatic = if velnor_policy.automatic_providers.iter().any(|item| item == provider) {
        match velnor_policy.repository.as_deref() {
            Some(repository) => format!(
                "github.repository=='{repository}'&&github.event_name=='push'&&github.ref=='refs/heads/{}'",
                velnor_policy.default_branch
            ),
            None => "false".to_owned(),
        }
    } else {
        "false".to_owned()
    };
    let trusted = trusted_event_conjunct();
    let event = format!("({dispatch})||({automatic})");
    value == format!("({event})&&({trusted})") || value == format!("{event}&&({trusted})")
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
    if analysis.local_provider && !local_runner_group_is_valid(value, velnor_policy) {
        failures.record(
            Rule::TrustedRunners,
            path,
            "local-provider jobs must use the exact configured runs-on group and labels",
        );
    }
}

/// Classify one static label only by an exact selector from the trusted base
/// config. Prefixes such as `ubuntu-*` are not a provider identity.
fn classify_static_label(label: &str, velnor_policy: &VelnorPolicyContract) -> RunnerAnalysis {
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
                RunnerAnalysis {
                    dynamic: true,
                    ..RunnerAnalysis::default()
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
            if static_provider_for_runs_on(value, velnor_policy) == Some("github-hosted") {
                // `runs-on: {group, labels}` selects a self-hosted group.
                // A hosted label must never be reinterpreted through it.
                result.invalid = true;
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

fn has_trusted_runner_gate(
    value: &str,
    default_branch: &str,
    repository: Option<&str>,
) -> bool {
    let Some(repository) = repository else {
        return false;
    };
    let normalized = normalize_gate_expression(value);
    let value = strip_reusable_unit_selector(&normalized).unwrap_or(&normalized);
    let expected = format!(
        "github.repository=='{repository}'&&github.event_name=='push'&&github.ref=='refs/heads/{default_branch}'"
    );
    value == expected
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

#[cfg(test)]
mod tests;

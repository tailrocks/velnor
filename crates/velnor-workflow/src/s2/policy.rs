//! `velnor-workflow policy`: the workflow trust validator.
//!
//! The base branch's `ci-policy.yml` runs this subcommand under
//! `pull_request_target` with the binary the base branch pins, against the
//! pull request's tree. The validator never executes a generator, action, or
//! artifact from that tree. It treats the candidate's configuration and
//! workflows as hostile data and applies base-pinned semantic checks. A
//! candidate source or render change can pass only when the actual PR YAML
//! satisfies those checks.
//!
//! * **Is the declared pin admissible?** The tree declares the generator
//!   revision (`[generator] revision` in `.github-gen/velnor-workflow.toml`).
//!   The validator checks repository identity, reachability, monotonicity,
//!   and agreement with the workflow's declared pin. It never obtains or runs
//!   the generator at that revision in pull-request policy.
//! * **Is the tree safe?** The validator's own semantic rules run on the
//!   tree's YAML: no `pull_request_target` outside the policy entrypoint,
//!   every self-hosted job behind a trusted-event gate, every action pinned to
//!   a full SHA, the entrypoint restricted to `contents: read` with no
//!   secrets, PR-reachable workflows held to least privilege, and the
//!   ruleset's required contexts emitted by `ci-pr.yml`.
//!
//! Every rule reports `PASS` or `FAIL` with a one-line reason; the report is
//! what a reviewer reads in the job log.
//!
//! The separate generator `--verify-pinned` path can use a provisioned pin or
//! explicitly build one for local regeneration checks; pull-request policy
//! never uses that path.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_yaml::{Mapping, Value};

use super::{
    closure as closure_identity, config, runtime, GeneratorError, ProjectConfig, SOURCE_CLOSURE,
};

/// The generation config the audited tree declares itself with.
pub(crate) const GENERATION_CONFIG: &str = ".github-gen/velnor-workflow.toml";
/// The runtime contract, kept beside the generation config for the Velnor
/// lane fields the advisory audit needs.
const RUNTIME_CONFIG: &str = ".github/ci/project.toml";
/// The base-owned policy entrypoint: the only workflow allowed to run on
/// `pull_request_target`.
const POLICY_ENTRYPOINT: &str = ".github/workflows/ci-policy.yml";
/// The policy command runs on a fixed disposable GitHub-hosted image. The
/// audited tree must not select a runner label or runner group for this job.
const POLICY_RUNNER_LABEL: &str = "ubuntu-24.04";
/// The pull-request aggregate whose job display names are the ruleset's
/// status-check contexts.
const PULL_REQUEST_AGGREGATE: &str = ".github/workflows/ci-pr.yml";
/// Names a `velnor-workflow` binary built at the pinned revision.
#[cfg(test)]
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
    /// Repository identity supplied by the trusted runner environment. The
    /// audited tree may declare this identity but cannot choose it.
    pub(crate) trusted_repository: Option<String>,
    /// Default branch supplied by the trusted GitHub event payload.
    pub(crate) trusted_default_branch: Option<String>,
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
/// [--base-revision SHA] [--ruleset-contexts a,b]`.
///
/// Policy statically audits the candidate tree. It never executes the
/// candidate's declared generator.
///
/// # Errors
/// Usage errors, unreadable inputs, and a failed evaluation (the rendered
/// report is printed first; the error names the failing rules).
#[cfg(test)]
fn run_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let parsed = parse_policy_arguments(arguments)?;
    let root = resolve_workflow_root(&parsed.values)?;
    let safe_root = super::safe_fs::SafeRoot::open(&root)?;
    run_cli_with_safe_root(arguments, safe_root)
}

/// Run policy with the root already captured by parser-aware dispatch.
///
/// Parsing uses the same root precedence as dispatch. Once captured, every
/// repository read stays on the descriptor instead of reopening its path.
pub(crate) fn run_policy_with_safe_root(
    arguments: &[OsString],
    safe_root: super::safe_fs::SafeRoot,
) -> Result<(), GeneratorError> {
    run_cli_with_safe_root(arguments, safe_root)
}

fn run_cli_with_safe_root(
    arguments: &[OsString],
    safe_root: super::safe_fs::SafeRoot,
) -> Result<(), GeneratorError> {
    let parsed = parse_policy_arguments(arguments)?;
    let requested_root = resolve_workflow_root(&parsed.values)?;
    let requested_identity = super::safe_fs::SafeRoot::open(&requested_root)?.identity()?;
    if requested_identity != safe_root.identity()? {
        return Err(GeneratorError::usage(
            "policy workflow root differs from the repository captured by dispatch",
        ));
    }
    let root = safe_root.command_directory().to_path_buf();
    let base_revision = parsed
        .values
        .get("base-revision")
        .cloned()
        .or_else(|| env::var(BASE_REVISION_ENV).ok())
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "--base-revision or {BASE_REVISION_ENV} is required: the validator revision the base branch runs"
            ))
        })?;
    let ruleset_contexts = parsed.values.get("ruleset-contexts").map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    let trusted_repository = env::var("GITHUB_REPOSITORY").map_err(|_| {
        GeneratorError::usage(
            "GITHUB_REPOSITORY is required to anchor workflow policy to the trusted caller",
        )
    })?;
    let trusted_default_branch = trusted_default_branch_from_event()?;
    let report = evaluate_with_safe_root(
        &PolicyOptions {
            head_sha: parsed.values.get("head-sha").cloned(),
            base_sha: parsed.values.get("base-sha").cloned(),
            base_revision,
            ruleset_contexts,
            trusted_repository: Some(trusted_repository),
            trusted_default_branch: Some(trusted_default_branch),
        },
        safe_root,
    )?;
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

struct ParsedPolicyArguments {
    values: BTreeMap<String, String>,
}

fn parse_policy_arguments(arguments: &[OsString]) -> Result<ParsedPolicyArguments, GeneratorError> {
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_str().ok_or_else(|| {
            GeneratorError::usage("policy arguments must be valid UTF-8".to_owned())
        })?;
        if argument == "--pin-build" {
            return Err(GeneratorError::usage(
                "unsupported policy option: --pin-build; policy never executes the declared generator",
            ));
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
            "workflow-root" | "head-sha" | "base-sha" | "base-revision" | "ruleset-contexts"
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
    Ok(ParsedPolicyArguments { values: options })
}

fn resolve_workflow_root(options: &BTreeMap<String, String>) -> Result<PathBuf, GeneratorError> {
    options
        .get("workflow-root")
        .map(PathBuf::from)
        .or_else(|| env::var_os("WORKFLOW_ROOT").map(PathBuf::from))
        .or_else(|| env::var_os("GITHUB_WORKSPACE").map(PathBuf::from))
        // Keep the process CWD as a descriptor-resolved root. Resolving it to
        // an absolute pathname before dispatch would allow rename-and-replace
        // to select a different tree at the old name.
        .or_else(|| Some(PathBuf::from(".")))
        .ok_or_else(|| GeneratorError::usage("resolve workflow root"))
}

/// Resolve root using the policy parser and precedence before schema dispatch.
pub(crate) fn workflow_root_for_dispatch(
    arguments: &[OsString],
) -> Result<PathBuf, GeneratorError> {
    let parsed = parse_policy_arguments(arguments)?;
    resolve_workflow_root(&parsed.values)
}

fn trusted_default_branch_from_event() -> Result<String, GeneratorError> {
    let path = env::var_os("GITHUB_EVENT_PATH")
        .map(PathBuf::from)
        .ok_or_else(|| {
            GeneratorError::usage(
            "GITHUB_EVENT_PATH is required to anchor workflow policy to the trusted default branch",
        )
        })?;
    let bytes = fs::read(&path)
        .map_err(|error| GeneratorError::io("read trusted GitHub event", &path, &error))?;
    let event: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        GeneratorError::usage(format!(
            "parse trusted GitHub event {}: {error}",
            path.display()
        ))
    })?;
    let branch = event
        .pointer("/repository/default_branch")
        .or_else(|| event.pointer("/pull_request/base/repo/default_branch"))
        .and_then(serde_json::Value::as_str)
        .filter(|branch| runtime::valid_branch(branch))
        .ok_or_else(|| {
            GeneratorError::usage("trusted GitHub event has no valid repository.default_branch")
        })?;
    Ok(branch.to_owned())
}

fn evaluate_with_safe_root(
    options: &PolicyOptions,
    safe_root: super::safe_fs::SafeRoot,
) -> Result<PolicyReport, GeneratorError> {
    safe_root.validate_root_binding()?;
    let result = (|| {
        let generation = required_generation_config_with_safe_root(&safe_root)?;
        let declared = DeclaredTree::read_with_safe_root(
            &safe_root,
            generation,
            options.trusted_repository.as_deref(),
            options.trusted_default_branch.as_deref(),
        )?;
        let mut report = PolicyReport::default();
        let pin = declared_pin_rule(&declared, &mut report);
        pin_rules_with_safe_root(&safe_root, &declared, pin.as_deref(), options, &mut report);
        semantic_rules_with_safe_root(&safe_root, &declared, options, &mut report)?;
        Ok(report)
    })();
    safe_root.validate_root_binding()?;
    result
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

/// `pin-reachable`, `pin-monotonic`, and `entrypoint-pin` validate the
/// candidate's declared generator revision without obtaining or executing it.
fn pin_rules_with_safe_root(
    root: &super::safe_fs::SafeRoot,
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
            "candidate-static-audit",
        ] {
            report
                .rules
                .push(RuleReport::fail(rule, "no declared pin", Vec::new()));
        }
        return;
    };
    let source = declared.pin_source(root.command_directory());
    let head = resolve_head(root, options.head_sha.as_deref());
    let base = options.base_sha.clone().or_else(|| head.clone().ok());
    let foreign = |rule: &'static str| {
        RuleReport::pass(
            rule,
            format!(
                "not applicable: {} consumes the generator from {}; pull-request policy treats that remote pin as metadata and never obtains or executes it",
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
    report.rules.push(entrypoint_pin_with_safe_root(root, pin));
    report.rules.push(RuleReport::pass(
        "candidate-static-audit",
        format!(
            "candidate files are audited as hostile data; generator {pin} is not obtained or executed by pull-request policy"
        ),
    ));
}

/// The validator's own rules over the tree's YAML, independent of any pin.
fn semantic_rules_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    declared: &DeclaredTree,
    options: &PolicyOptions,
    report: &mut PolicyReport,
) -> Result<(), GeneratorError> {
    let audit = audit_workflows_with_safe_root(
        root,
        options.trusted_repository.as_deref(),
        options.trusted_default_branch.as_deref(),
    )?;
    let entrypoint = audit_policy_entrypoint_with_safe_root(root, &declared.velnor_policy)?;
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
    report.rules.push(required_checks_with_safe_root(
        root,
        declared,
        options.ruleset_contexts.as_deref(),
    ));
    Ok(())
}

/// D19 guard for `--verify-pinned`: the generator the tree declares must
/// render it byte-identically.
///
/// # Errors
/// When the pinned generator cannot be obtained or renders any generated file
/// differently.
pub(crate) fn verify_declared_pin_renders_tree_with_safe_roots(
    output_root: &super::safe_fs::SafeRoot,
    checkout: &super::safe_fs::SafeRoot,
    config: &ProjectConfig,
    build_pin: bool,
) -> Result<(), GeneratorError> {
    let pin = &config.workflow_revision;
    if !super::is_full_revision(pin) {
        return Err(GeneratorError::usage(format!(
            "declared workflow revision must be a full 40-character SHA, got {pin:?}"
        )));
    }
    checkout.validate_root_binding()?;
    output_root.validate_root_binding()?;
    let generation = generation_config_with_safe_root(checkout)?.ok_or_else(|| {
        GeneratorError::usage(format!(
            "schema-2 operation requires `{GENERATION_CONFIG}` with `schema = 2`"
        ))
    })?;
    let source = pin_source(checkout.command_directory(), generation.repository());
    let lookup = PinnedBinaryLookup::for_policy(pin, build_pin);
    match regenerate_and_compare_with_safe_roots(
        checkout,
        output_root,
        pin,
        &config.default_branch,
        &lookup,
        &source,
    )? {
        TreeComparison::Pin => {
            checkout.validate_root_binding()?;
            output_root.validate_root_binding()?;
            Ok(())
        }
        TreeComparison::Differences(differences) => {
            checkout.validate_root_binding()?;
            output_root.validate_root_binding()?;
            Err(GeneratorError::usage(format!(
                "the declared generator pin {pin} renders the tree differently; set `[generator] revision` in {GENERATION_CONFIG} to the last generator commit and regenerate:\n{}",
                differences.join("\n")
            )))
        }
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
    velnor_policy: VelnorPolicyContract,
    /// Ruleset contexts `ci-pr.yml` must emit as job display names.
    required_checks: Vec<String>,
    /// Ruleset contexts reported by GitHub Apps rather than workflows.
    external_checks: Vec<String>,
}

impl DeclaredTree {
    fn read_with_safe_root(
        root: &super::safe_fs::SafeRoot,
        generation: config::RepoGenerationConfig,
        trusted_repository: Option<&str>,
        trusted_default_branch: Option<&str>,
    ) -> Result<Self, GeneratorError> {
        let pin = match generation.revision() {
            Some(revision) if !super::is_full_revision(revision) => {
                return Err(GeneratorError::usage(format!(
                    "[generator] revision must be a full 40-character commit SHA, got {revision:?}"
                )));
            }
            Some(revision) => Some(DeclaredPin::Config(revision.to_owned())),
            None => entrypoint_policy_revision_with_safe_root(root)?.map(DeclaredPin::Entrypoint),
        };
        let velnor_policy = configured_velnor_policy_with_safe_root(
            root,
            Some(&generation),
            trusted_repository,
            trusted_default_branch,
        )?;
        let repository = velnor_policy
            .repository
            .clone()
            .or_else(|| generation.repository().map(str::to_owned));
        let configured_required_checks = generation.ruleset_required_status_checks().to_vec();
        let required_checks = if !configured_required_checks.is_empty() {
            configured_required_checks
        } else if generation.ci_required().unwrap_or(true) {
            vec!["ci-required".to_owned()]
        } else {
            Vec::new()
        };
        let external_checks = generation.ruleset_external_status_checks().to_vec();
        Ok(Self {
            pin,
            repository,
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

fn entrypoint_policy_revision_with_safe_root(
    root: &super::safe_fs::SafeRoot,
) -> Result<Option<String>, GeneratorError> {
    Ok(
        read_text_with_safe_root(root, Path::new(POLICY_ENTRYPOINT), "read policy entrypoint")?
            .and_then(|content| entrypoint_policy_revision_from_content(&content)),
    )
}

fn entrypoint_policy_revision_from_content(content: &str) -> Option<String> {
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

fn read_text_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    relative: &Path,
    operation: &str,
) -> Result<Option<String>, GeneratorError> {
    let Some(bytes) = root.read_file_if_exists(relative)? else {
        return Ok(None);
    };
    let path = root.command_directory().join(relative);
    String::from_utf8(bytes).map(Some).map_err(|error| {
        GeneratorError::usage(format!("{operation} {} as UTF-8: {error}", path.display()))
    })
}

fn generation_config_with_safe_root(
    root: &super::safe_fs::SafeRoot,
) -> Result<Option<config::RepoGenerationConfig>, GeneratorError> {
    let Some(bytes) = root.read_file_if_exists(Path::new(GENERATION_CONFIG))? else {
        return Ok(None);
    };
    config::parse(Path::new(GENERATION_CONFIG), &bytes).map(Some)
}

fn required_generation_config_with_safe_root(
    root: &super::safe_fs::SafeRoot,
) -> Result<config::RepoGenerationConfig, GeneratorError> {
    generation_config_with_safe_root(root)?.ok_or_else(|| {
        GeneratorError::usage(format!(
            "schema-2 policy requires `{GENERATION_CONFIG}` with `schema = 2`"
        ))
    })
}

// ---------------------------------------------------------------------------
// Git facts
// ---------------------------------------------------------------------------

trait PolicyGitRoot {
    fn git_output(&self, arguments: &[&str]) -> Result<Output, GeneratorError>;
}

impl PolicyGitRoot for Path {
    fn git_output(&self, arguments: &[&str]) -> Result<Output, GeneratorError> {
        let mut command = Command::new("git");
        command
            .env_remove(super::safe_fs::pinned_command::OUTPUT_ROOT_FD_ENV)
            .env_remove(super::safe_fs::pinned_command::SOURCE_ROOT_FD_ENV)
            .env_remove(super::safe_fs::pinned_command::SOURCE_ROOT_PATH_ENV)
            .arg("-C")
            .arg(self)
            .args(arguments)
            .output()
            .map_err(|error| {
                GeneratorError::usage(format!("run git {}: {error}", arguments.join(" ")))
            })
    }
}

impl PolicyGitRoot for PathBuf {
    fn git_output(&self, arguments: &[&str]) -> Result<Output, GeneratorError> {
        self.as_path().git_output(arguments)
    }
}

impl PolicyGitRoot for super::safe_fs::SafeRoot {
    fn git_output(&self, arguments: &[&str]) -> Result<Output, GeneratorError> {
        super::safe_fs::pinned_command::output(self, "git", arguments).map_err(|error| {
            GeneratorError::usage(format!("run git {}: {error}", arguments.join(" ")))
        })
    }
}

fn git<R: PolicyGitRoot + ?Sized>(
    root: &R,
    arguments: &[&str],
) -> Result<Option<String>, GeneratorError> {
    let output = root.git_output(arguments)?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    } else {
        Ok(None)
    }
}

fn resolve_head<R: PolicyGitRoot + ?Sized>(root: &R, head: Option<&str>) -> Result<String, String> {
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

fn commit_exists<R: PolicyGitRoot + ?Sized>(root: &R, revision: &str) -> bool {
    git(root, &["cat-file", "-e", &format!("{revision}^{{commit}}")])
        .is_ok_and(|result| result.is_some())
}

/// `Some(true)` when `ancestor` is reachable from `descendant`; `None` when
/// either commit is absent from the checkout.
fn is_ancestor<R: PolicyGitRoot + ?Sized>(
    root: &R,
    ancestor: &str,
    descendant: &str,
) -> Option<bool> {
    if ancestor == descendant {
        return commit_exists(root, ancestor).then_some(true);
    }
    if !commit_exists(root, ancestor) || !commit_exists(root, descendant) {
        return None;
    }
    let status = root
        .git_output(&["merge-base", "--is-ancestor", ancestor, descendant])
        .ok()?
        .status;
    match status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn pin_reachable<R: PolicyGitRoot + ?Sized>(
    root: &R,
    pin: &str,
    head: &str,
    base: Option<&str>,
) -> RuleReport {
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
fn is_shallow_checkout<R: PolicyGitRoot + ?Sized>(root: &R) -> bool {
    git(root, &["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|output| output.is_some_and(|value| value.trim() == "true"))
}

/// Why `pin-reachable` could not decide: names the commits the checkout
/// lacks and separates the two causes — a shallow checkout that cut the
/// history the rule walks (the job must check out with `fetch-depth: 0`)
/// from a full checkout that genuinely lacks the commit (the pin or head is
/// not in this repository's history).
fn missing_commit_reason<R: PolicyGitRoot + ?Sized>(root: &R, pin: &str, head: &str) -> String {
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
fn merge_base_pin<R: PolicyGitRoot + ?Sized>(root: &R, head: &str, base: &str) -> Option<String> {
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
    root: &impl PolicyGitRoot,
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
fn entrypoint_pin_with_safe_root(root: &super::safe_fs::SafeRoot, pin: &str) -> RuleReport {
    let content = match read_text_with_safe_root(
        root,
        Path::new(POLICY_ENTRYPOINT),
        "read policy entrypoint",
    ) {
        Ok(Some(content)) => content,
        Ok(None) => {
            return RuleReport::fail(
                "entrypoint-pin",
                format!("{POLICY_ENTRYPOINT}: file is missing"),
                Vec::new(),
            );
        }
        Err(error) => {
            return RuleReport::fail(
                "entrypoint-pin",
                format!("{POLICY_ENTRYPOINT}: {error}"),
                Vec::new(),
            );
        }
    };
    entrypoint_pin_content(&content, pin)
}

fn entrypoint_pin_content(content: &str, pin: &str) -> RuleReport {
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

/// The source-closure digest a `velnor-workflow` binary reports for itself.
#[cfg(test)]
fn binary_closure(binary: &Path) -> Result<String, String> {
    binary_report(binary, "--closure")
}

#[cfg(test)]
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
/// release and debug CI products, plus the default-feature debug product.
/// Its TUI is TTY-gated and policy always renders through pipes, so the
/// reached code is identical in all three.
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
        closure_identity::closure_of_tree(
            repo,
            pin,
            closure_identity::DEFAULT_FEATURES,
            closure_identity::PROFILE_DEBUG,
        )?,
    ])
}

fn expected_closures_with_safe_root(
    repo: &super::safe_fs::SafeRoot,
    pin: &str,
) -> Result<Vec<String>, GeneratorError> {
    Ok(vec![
        closure_identity::closure_of_tree_with_safe_root(
            repo,
            pin,
            closure_identity::CI_FEATURES,
            closure_identity::PROFILE_RELEASE,
        )?,
        closure_identity::closure_of_tree_with_safe_root(
            repo,
            pin,
            closure_identity::CI_FEATURES,
            closure_identity::PROFILE_DEBUG,
        )?,
        closure_identity::closure_of_tree_with_safe_root(
            repo,
            pin,
            closure_identity::DEFAULT_FEATURES,
            closure_identity::PROFILE_DEBUG,
        )?,
    ])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExecutableVersion {
    identity: ExecutableIdentity,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl ExecutableVersion {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            identity: ExecutableIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

/// A renderer opened once, then invoked by its held file descriptor.
struct PinnedExecutable {
    path: PathBuf,
    file: fs::File,
    version: ExecutableVersion,
}

impl PinnedExecutable {
    fn open(path: &Path) -> Result<Self, String> {
        let normalized = path.canonicalize().map_err(|error| {
            format!(
                "{}: cannot normalize executable path: {error}",
                path.display()
            )
        })?;
        let descriptor = rustix::fs::open(
            &normalized,
            rustix::fs::OFlags::RDONLY
                .union(rustix::fs::OFlags::CLOEXEC)
                .union(rustix::fs::OFlags::NOFOLLOW)
                .union(rustix::fs::OFlags::NONBLOCK),
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            let error = std::io::Error::from(error);
            format!("{}: cannot open executable: {error}", normalized.display())
        })?;
        let file: fs::File = descriptor.into();
        let metadata = file.metadata().map_err(|error| {
            format!(
                "{}: cannot inspect opened executable: {error}",
                normalized.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "{}: opened executable is not a regular file",
                normalized.display()
            ));
        }
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "{}: opened file has no executable permission",
                normalized.display()
            ));
        }
        Ok(Self {
            path: normalized,
            file,
            version: ExecutableVersion::from_metadata(&metadata),
        })
    }

    fn ensure_unchanged(&self, stage: &str) -> Result<(), String> {
        let metadata = self.file.metadata().map_err(|error| {
            format!("{}: inspect bound executable: {error}", self.path.display())
        })?;
        if ExecutableVersion::from_metadata(&metadata) != self.version {
            return Err(format!(
                "{}: executable inode or contents changed {stage}",
                self.path.display()
            ));
        }
        Ok(())
    }

    fn output_in_safe_root<I, A>(
        &self,
        root: &super::safe_fs::SafeRoot,
        args: I,
    ) -> Result<Output, String>
    where
        I: IntoIterator<Item = A>,
        A: AsRef<OsStr>,
    {
        self.ensure_unchanged("before execution")?;
        let output =
            super::safe_fs::pinned_command::output_executable_in_safe_root(root, &self.file, args)
                .map_err(|error| {
                    format!(
                        "{}: cannot execute the bound descriptor from the captured root: {error}",
                        self.path.display()
                    )
                })?;
        self.ensure_unchanged("during execution")?;
        Ok(output)
    }

    fn render_with_safe_roots<I, A>(
        &self,
        source_root: &super::safe_fs::SafeRoot,
        output_root: &super::safe_fs::SafeRoot,
        args: I,
    ) -> Result<Output, String>
    where
        I: IntoIterator<Item = A>,
        A: AsRef<OsStr>,
    {
        self.ensure_unchanged("before rendering")?;
        let output = super::safe_fs::pinned_command::output_with_executable_and_output_root(
            source_root,
            &self.file,
            output_root,
            args,
        )
        .map_err(|error| {
            format!(
                "{}: cannot execute the bound renderer: {error}",
                self.path.display()
            )
        })?;
        self.ensure_unchanged("during rendering")?;
        Ok(output)
    }
}

/// Resolve the trusted closure for the configured generator pin. Consumer
/// checkouts do not necessarily carry generator history, so fetch the exact
/// commit from the configured generator source before trusting any binary
/// report. A missing or truncated pin object is a hard error.
fn expected_closures_from_source(
    pin: &str,
    source: &PinSource,
) -> Result<Vec<String>, GeneratorError> {
    match source {
        PinSource::Checkout(repository) => expected_closures(repository, pin),
        PinSource::Remote(url) => {
            let source_checkout = scratch_directory("policy-pin-source")?;
            let result = (|| {
                let init = Command::new("git")
                    .args(["init", "--quiet"])
                    .arg(&source_checkout)
                    .status()
                    .map_err(|error| {
                        GeneratorError::usage(format!(
                            "initialize temporary generator history for pin {pin}: {error}"
                        ))
                    })?;
                if !init.success() {
                    return Err(GeneratorError::usage(format!(
                        "initialize temporary generator history for pin {pin} failed"
                    )));
                }
                let remote = Command::new("git")
                    .arg("-C")
                    .arg(&source_checkout)
                    .args(["remote", "add", "origin"])
                    .arg(url)
                    .status()
                    .map_err(|error| {
                        GeneratorError::usage(format!(
                            "configure generator source {url} for pin {pin}: {error}"
                        ))
                    })?;
                if !remote.success() {
                    return Err(GeneratorError::usage(format!(
                        "configure generator source {url} for pin {pin} failed"
                    )));
                }
                let fetch = Command::new("git")
                    .arg("-C")
                    .arg(&source_checkout)
                    .args(["fetch", "--no-tags", "--depth", "1", "origin", pin])
                    .status()
                    .map_err(|error| {
                        GeneratorError::usage(format!(
                            "fetch generator pin {pin} from {url}: {error}"
                        ))
                    })?;
                if !fetch.success() || !commit_exists(&source_checkout, pin) {
                    return Err(GeneratorError::usage(format!(
                        "cannot fetch generator pin {pin} from {url}; fetch the pin's source history or update the configured pin"
                    )));
                }
                expected_closures(&source_checkout, pin)
            })();
            let _ = fs::remove_dir_all(&source_checkout);
            result.map_err(|error| {
                GeneratorError::usage(format!(
                    "cannot establish trusted source closures for pin {pin}: {error}"
                ))
            })
        }
    }
}

/// Resolve a renderer for the declared pin. Prefer the binary and closure
/// provisioned by the trusted setup step; local development can explicitly
/// build the pin when no provisioned renderer exists.
pub(crate) struct PinnedBinaryLookup {
    /// Absolute path exported by the trusted runtime provisioner.
    provisioned_binary: Option<PathBuf>,
    /// Independently verified source closure exported beside the binary path.
    provisioned_closure: Option<String>,
    /// Where an explicitly requested build writes the pin.
    install_root: PathBuf,
    /// Never build without an explicit `--pin-build`, or under
    /// `CARGO_NET_OFFLINE=true`.
    build_forbidden: bool,
}

impl PinnedBinaryLookup {
    pub(crate) fn for_policy(revision: &str, build_pin: bool) -> Self {
        Self {
            provisioned_binary: env::var_os(super::VELNOR_WORKFLOW_PINNED_BINARY_ENV)
                .map(PathBuf::from),
            provisioned_closure: env::var_os(super::VELNOR_WORKFLOW_PINNED_CLOSURE_ENV)
                .map(|value| value.into_string().unwrap_or_default()),
            install_root: policy_install_root(revision),
            build_forbidden: !build_pin
                || env::var("CARGO_NET_OFFLINE").is_ok_and(|value| value == "true"),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_env(revision: &str, build_pin: bool) -> Self {
        Self {
            provisioned_binary: env::var_os(super::VELNOR_WORKFLOW_PINNED_BINARY_ENV)
                .map(PathBuf::from),
            provisioned_closure: env::var_os(super::VELNOR_WORKFLOW_PINNED_CLOSURE_ENV)
                .map(|value| value.into_string().unwrap_or_default()),
            install_root: policy_install_root(revision),
            build_forbidden: !build_pin
                || env::var("CARGO_NET_OFFLINE").is_ok_and(|value| value == "true"),
        }
    }

    fn provisioned_identity(
        &self,
        revision: &str,
        expected: &[String],
    ) -> Result<Option<(PathBuf, String)>, GeneratorError> {
        let (Some(binary), Some(closure)) = (&self.provisioned_binary, &self.provisioned_closure)
        else {
            if self.provisioned_binary.is_some() || self.provisioned_closure.is_some() {
                return Err(GeneratorError::usage(format!(
                    "trusted pinned renderer requires both {} and {}; reprovision the renderer or clear both values",
                    super::VELNOR_WORKFLOW_PINNED_BINARY_ENV,
                    super::VELNOR_WORKFLOW_PINNED_CLOSURE_ENV
                )));
            }
            return Ok(None);
        };
        if !binary.is_absolute() {
            return Err(GeneratorError::usage(format!(
                "trusted pinned renderer path from {} must be absolute: {}",
                super::VELNOR_WORKFLOW_PINNED_BINARY_ENV,
                binary.display()
            )));
        }
        if closure.len() != 64
            || !closure
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GeneratorError::usage(format!(
                "trusted pinned renderer closure from {} must be 64 lowercase hex digits",
                super::VELNOR_WORKFLOW_PINNED_CLOSURE_ENV
            )));
        }
        if !expected.iter().any(|expected| expected == closure) {
            return Err(GeneratorError::usage(format!(
                "trusted provisioned closure {closure} does not identify declared generator pin {revision}"
            )));
        }
        Ok(Some((binary.clone(), closure.clone())))
    }
}

/// Test-only path resolver. Production verification uses the captured-root
/// variant below so provisioned executables stay bound to an opened handle.
#[cfg(test)]
pub(crate) fn resolve_pinned_binary(
    revision: &str,
    expected: &[String],
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
) -> Result<PathBuf, GeneratorError> {
    if expected.is_empty() {
        return Err(GeneratorError::usage(format!(
            "cannot verify the source closure for pin {revision}; refusing a binary that proves only its own revision/closure report. Fetch the pinned generator source history or provide a trusted source-closure attestation"
        )));
    }
    if let Some((path, expected_closure)) = lookup.provisioned_identity(revision, expected)? {
        let reported = binary_closure(&path).map_err(GeneratorError::usage)?;
        if reported != expected_closure {
            return Err(GeneratorError::usage(format!(
                "trusted provisioned renderer at {} reports closure {reported}, expected {expected_closure}",
                path.display()
            )));
        }
        return Ok(path);
    }
    if expected.iter().any(|closure| closure == SOURCE_CLOSURE)
        && let Ok(current) = env::current_exe()
    {
        return Ok(current);
    }
    if lookup.build_forbidden {
        return Err(GeneratorError::usage(format!(
            "no trusted renderer matches pin {revision} and building is forbidden here; provision {} with its closure or use --pin-build with --verify-pinned to compile the pin from source",
            super::VELNOR_WORKFLOW_PINNED_BINARY_ENV
        )));
    }
    let install_root = &lookup.install_root;
    let installed = install_root.join("bin").join("velnor-workflow");
    build_pinned_binary(revision, expected, source, install_root, &installed, None)
}

fn resolve_pinned_binary_with_safe_root(
    revision: &str,
    expected: &[String],
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
    checkout_root: &super::safe_fs::SafeRoot,
) -> Result<PinnedExecutable, GeneratorError> {
    if expected.is_empty() {
        return Err(GeneratorError::usage(format!(
            "cannot verify the source closure for pin {revision}; refusing a binary that proves only its own revision/closure report"
        )));
    }
    checkout_root.validate_root_binding()?;
    let mut attempts = Vec::new();
    if let Some((path, expected_closure)) = lookup.provisioned_identity(revision, expected)? {
        let binary = PinnedExecutable::open(&path).map_err(GeneratorError::usage)?;
        let reported =
            binary_closure_with_safe_root(&binary, checkout_root).map_err(GeneratorError::usage)?;
        if reported != expected_closure {
            return Err(GeneratorError::usage(format!(
                "trusted provisioned renderer at {} reports closure {reported}, expected {expected_closure}",
                binary.path.display()
            )));
        }
        checkout_root.validate_root_binding()?;
        return Ok(binary);
    }
    if expected.iter().any(|closure| closure == SOURCE_CLOSURE) {
        match env::current_exe()
            .map_err(|error| format!("locate running velnor-workflow: {error}"))
            .and_then(|current| {
                PinnedExecutable::open(&current)
                    .map_err(|detail| format!("{}: {detail}", current.display()))
            })
            .and_then(|binary| {
                let reported = binary_closure_with_safe_root(&binary, checkout_root)?;
                if expected.contains(&reported) {
                    Ok(binary)
                } else {
                    Err(format!(
                        "reports closure {reported}, which is not the pin's closure"
                    ))
                }
            }) {
            Ok(binary) => return Ok(binary),
            Err(detail) => attempts.push(detail),
        }
    } else {
        attempts.push(format!(
            "running velnor-workflow has closure {SOURCE_CLOSURE}, which is not the declared pin's closure"
        ));
    }
    if lookup.build_forbidden {
        return Err(GeneratorError::usage(format!(
            "no trusted renderer matches pin {revision} and building is forbidden here ({}); provision {} with its closure or use --pin-build with --verify-pinned to compile the pin from source",
            attempts.join("; "),
            super::VELNOR_WORKFLOW_PINNED_BINARY_ENV
        )));
    }
    let installed = lookup.install_root.join("bin").join("velnor-workflow");
    let built = build_pinned_binary(
        revision,
        expected,
        source,
        &lookup.install_root,
        &installed,
        Some(checkout_root),
    )?;
    let binary = PinnedExecutable::open(&built).map_err(GeneratorError::usage)?;
    let reported =
        binary_closure_with_safe_root(&binary, checkout_root).map_err(GeneratorError::usage)?;
    if !expected.contains(&reported) {
        return Err(GeneratorError::usage(format!(
            "velnor-workflow built for pin {revision} reports closure {reported}, which is not the pin's closure"
        )));
    }
    Ok(binary)
}

fn binary_closure_with_safe_root(
    binary: &PinnedExecutable,
    root: &super::safe_fs::SafeRoot,
) -> Result<String, String> {
    let output = binary.output_in_safe_root(root, [OsStr::new("--closure")])?;
    if !output.status.success() {
        return Err(format!(
            "{}: `--closure` failed ({}): {}",
            binary.path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Whether `binary` reports a source closure independently derived from the
/// fetched pin's Git tree. Path-based proof exists only for unit tests.
#[cfg(test)]
fn prove_pinned_renderer(binary: &Path, expected: &[String]) -> Result<(), String> {
    let reported = binary_closure(binary)?;
    if expected.contains(&reported) {
        return Ok(());
    }
    Err(format!(
        "reports closure {reported}, which is not the pin's closure"
    ))
}

/// Build the lean `velnor-workflow` product at `revision` with
/// `cargo install --locked --no-default-features` and every cache wrapper
/// and caller flag removed, so the build never touches a shared store. The
/// generator's own repository builds from a local clone of the audited
/// checkout (hardlinked objects) checked out at the pin; a consumer installs
/// from the generator's git URL. The result must prove the pin exactly like
/// the captured-root resolver proves it through the pinned executable handle.
fn build_pinned_binary(
    revision: &str,
    expected: &[String],
    source: &PinSource,
    install_root: &Path,
    installed: &Path,
    checkout_root: Option<&super::safe_fs::SafeRoot>,
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
            let status = if let Some(checkout_root) = checkout_root {
                checkout_root.validate_root_binding()?;
                let arguments = [
                    OsStr::new("clone"),
                    OsStr::new("--quiet"),
                    OsStr::new("--no-checkout"),
                    OsStr::new("."),
                    clone.as_os_str(),
                ];
                let output =
                    super::safe_fs::pinned_command::output(checkout_root, "git", arguments)
                        .map_err(|error| {
                            GeneratorError::usage(format!("clone the captured checkout: {error}"))
                        })?;
                checkout_root.validate_root_binding()?;
                output.status
            } else {
                Command::new("git")
                    .arg("clone")
                    .arg("--quiet")
                    .arg("--no-checkout")
                    .arg(checkout)
                    .arg(&clone)
                    .status()
                    .map_err(|error| {
                        GeneratorError::usage(format!("clone the audited checkout: {error}"))
                    })?
            };
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
    if let Some(checkout_root) = checkout_root {
        checkout_root.validate_root_binding()?;
        let binary = PinnedExecutable::open(installed).map_err(GeneratorError::usage)?;
        let reported =
            binary_closure_with_safe_root(&binary, checkout_root).map_err(GeneratorError::usage)?;
        if !expected.contains(&reported) {
            return Err(GeneratorError::usage(format!(
                "velnor-workflow built for pin {revision} reports closure {reported}, which is not the pin's closure"
            )));
        }
    } else {
        #[cfg(test)]
        prove_pinned_renderer(installed, expected).map_err(GeneratorError::usage)?;
        #[cfg(not(test))]
        return Err(GeneratorError::usage(
            "pinned generator builds require the captured SafeRoot executor",
        ));
    }
    Ok(installed.to_path_buf())
}

fn scratch_directory(label: &str) -> Result<PathBuf, GeneratorError> {
    let base = env::var_os("RUNNER_TEMP")
        .or_else(|| env::var_os("TMPDIR"))
        .map_or_else(env::temp_dir, PathBuf::from);
    for _ in 0..16 {
        let path = base.join(format!(
            "velnor-workflow-{label}-{}-{}",
            std::process::id(),
            crate::unique_suffix()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(GeneratorError::io(
                    "create scratch directory",
                    &path,
                    &error,
                ));
            }
        }
    }
    Err(GeneratorError::usage(format!(
        "could not allocate a unique policy scratch directory under {}",
        base.display()
    )))
}

struct SafeScratchDirectory {
    parent: super::safe_fs::SafeRoot,
    name: OsString,
    path: PathBuf,
    root: super::safe_fs::SafeRoot,
}

impl SafeScratchDirectory {
    fn create(label: &str) -> Result<Self, GeneratorError> {
        let base = env::var_os("RUNNER_TEMP")
            .or_else(|| env::var_os("TMPDIR"))
            .map_or_else(env::temp_dir, PathBuf::from)
            .canonicalize()
            .map_err(|error| {
                GeneratorError::usage(format!("resolve policy scratch root: {error}"))
            })?;
        let parent = super::safe_fs::SafeRoot::open(&base)?;
        let name = OsString::from(format!(
            "velnor-workflow-{label}-{}-{}",
            std::process::id(),
            crate::unique_suffix()
        ));
        let root = parent.create_directory_child(&name)?;
        let path = parent.command_directory().join(&name);
        Ok(Self {
            parent,
            name,
            path,
            root,
        })
    }

    fn cleanup(self) -> Result<(), GeneratorError> {
        let identity = self.root.identity()?;
        self.parent
            .remove_named_tree_if_matches(&self.name, &identity)
    }
}

fn regenerate_and_compare_with_safe_roots(
    checkout: &super::safe_fs::SafeRoot,
    tree: &super::safe_fs::SafeRoot,
    pin: &str,
    default_branch: &str,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
) -> Result<TreeComparison, GeneratorError> {
    checkout.validate_root_binding()?;
    tree.validate_root_binding()?;
    let expected = match source {
        PinSource::Checkout(_) => expected_closures_with_safe_root(checkout, pin)?,
        PinSource::Remote(_) => expected_closures_from_source(pin, source)?,
    };
    let binary = resolve_pinned_binary_with_safe_root(pin, &expected, lookup, source, checkout)?;
    let scratch = SafeScratchDirectory::create("policy-render")?;
    let result = render_and_compare_with_safe_roots(
        &binary,
        checkout,
        tree,
        &scratch.path,
        &scratch.root,
        default_branch,
    );
    let cleanup = scratch.cleanup();
    checkout.validate_root_binding()?;
    tree.validate_root_binding()?;
    let differences = match (result, cleanup) {
        (Err(error), _) => return Err(error),
        (Ok(_), Err(error)) => return Err(error),
        (Ok(differences), Ok(())) => differences,
    };
    if differences.is_empty() {
        Ok(TreeComparison::Pin)
    } else {
        Ok(TreeComparison::Differences(differences))
    }
}

fn render_and_compare_with_safe_roots(
    binary: &PinnedExecutable,
    checkout: &super::safe_fs::SafeRoot,
    tree: &super::safe_fs::SafeRoot,
    scratch_path: &Path,
    scratch_root: &super::safe_fs::SafeRoot,
    default_branch: &str,
) -> Result<Vec<String>, GeneratorError> {
    let output = binary
        .render_with_safe_roots(
            checkout,
            scratch_root,
            [
                OsStr::new("."),
                OsStr::new("--output"),
                scratch_path.as_os_str(),
                OsStr::new("--plain"),
                OsStr::new("--force"),
                OsStr::new("--default-branch"),
                OsStr::new(default_branch),
            ],
        )
        .map_err(|error| {
            GeneratorError::usage(format!(
                "run {} to regenerate the captured tree: {error}",
                binary.path.display()
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
            binary.path.display(),
            detail.trim()
        )));
    }
    scratch_root.validate_root_binding()?;
    let mut rendered = BTreeMap::new();
    collect_files_with_safe_root(scratch_root, Path::new(""), &mut rendered)?;
    compare_rendered_tree_with_safe_root(tree, &rendered)
}

fn compare_rendered_tree_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    rendered: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Vec<String>, GeneratorError> {
    let mut differences = Vec::new();
    for (relative, content) in rendered {
        match root.read_file_if_exists(relative)? {
            Some(bytes) if &bytes == content => {}
            Some(_) => differences.push(format!(
                "{}: differs from the pinned render",
                relative.display()
            )),
            None => differences.push(format!("{}: missing from the tree", relative.display())),
        }
    }
    if let Some(workflows) = root.open_directory(Path::new(".github/workflows"))? {
        for entry in workflows.entries()? {
            let name = entry.name.to_str().unwrap_or_default();
            if !matches!(
                Path::new(name).extension().and_then(|value| value.to_str()),
                Some("yml" | "yaml")
            ) {
                continue;
            }
            let relative = PathBuf::from(".github/workflows").join(name);
            if !rendered.contains_key(&relative) {
                differences.push(format!(
                    "{}: not present in rendered output",
                    relative.display()
                ));
            }
        }
    }
    differences.sort();
    Ok(differences)
}

fn collect_files_with_safe_root(
    directory: &super::safe_fs::SafeRoot,
    relative_directory: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), GeneratorError> {
    for entry in directory.entries()? {
        let relative = relative_directory.join(&entry.name);
        match entry.kind {
            super::safe_fs::SafeEntryKind::Directory(child) => {
                collect_files_with_safe_root(&child, &relative, files)?;
            }
            super::safe_fs::SafeEntryKind::File => {
                let bytes = directory.read_file(Path::new(&entry.name))?;
                files.insert(relative, bytes);
            }
            super::safe_fs::SafeEntryKind::Symlink | super::safe_fs::SafeEntryKind::Other => {
                return Err(GeneratorError::usage(format!(
                    "rendered output contains a non-regular path: {}",
                    relative.display()
                )));
            }
        }
    }
    Ok(())
}

/// What `regenerate_and_compare` proved about the tree.
pub(crate) enum TreeComparison {
    /// The tree is byte-identical to the declared pin's render.
    Pin,
    /// Neither renderer reproduces the tree.
    Differences(Vec<String>),
}

/// Scan `checkout` with the generator built at `pin`, render into a scratch
/// directory, and return every path under `tree` that differs, in sorted
/// order. This path-based helper is test-only; production policy uses captured
/// SafeRoots.
///
/// # Errors
/// When the pinned generator cannot be obtained or its render fails.
#[cfg(test)]
pub(crate) fn regenerate_and_compare(
    checkout: &Path,
    tree: &Path,
    pin: &str,
    default_branch: &str,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
) -> Result<TreeComparison, GeneratorError> {
    let expected = expected_closures_from_source(pin, source)?;
    let binary = resolve_pinned_binary(pin, &expected, lookup, source)?;
    let scratch = scratch_directory("policy-render")?;
    let verdict =
        render_and_compare(&binary, checkout, tree, &scratch, default_branch).map(|differences| {
            if differences.is_empty() {
                TreeComparison::Pin
            } else {
                TreeComparison::Differences(differences)
            }
        });
    let _ = fs::remove_dir_all(&scratch);
    verdict
}

#[cfg(test)]
fn render_and_compare(
    binary: &Path,
    checkout: &Path,
    root: &Path,
    scratch: &Path,
    default_branch: &str,
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
            let relative = PathBuf::from(".github/workflows").join(name);
            if !rendered.contains_key(&relative) {
                differences.push(format!(
                    "{}: not generator-owned (hand-written workflows are refused)",
                    relative.display()
                ));
            }
        }
    }
    differences.sort();
    Ok(differences)
}

#[cfg(test)]
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

fn required_checks_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    declared: &DeclaredTree,
    live: Option<&[String]>,
) -> RuleReport {
    let mut findings = Vec::new();
    let emitted = match read_text_with_safe_root(
        root,
        Path::new(PULL_REQUEST_AGGREGATE),
        "read pull request aggregate",
    ) {
        Ok(Some(yaml)) => {
            if declared
                .required_checks
                .iter()
                .chain(&declared.external_checks)
                .any(|context| context == "ci-required")
                || live
                    .is_some_and(|contexts| contexts.iter().any(|context| context == "ci-required"))
            {
                findings.extend(audit_ci_required_aggregate_with_contract(
                    &yaml,
                    Some(&declared.velnor_policy),
                ));
            }
            match super::workflow_job_display_names(&yaml) {
                Ok(names) => Some(names),
                Err(error) => {
                    findings.push(format!("{PULL_REQUEST_AGGREGATE}: {error}"));
                    None
                }
            }
        }
        Ok(None) => None,
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
        let entrypoint_contexts =
            read_text_with_safe_root(root, Path::new(POLICY_ENTRYPOINT), "read policy entrypoint")
                .ok()
                .flatten()
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

fn audit_ci_required_aggregate_with_contract(
    yaml: &str,
    velnor_policy: Option<&VelnorPolicyContract>,
) -> Vec<String> {
    let finding = |message: &str| format!("{PULL_REQUEST_AGGREGATE}: ci-required {message}");
    let parser = serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
    let document: Value = match serde_yaml::from_str_with_config(yaml, &parser) {
        Ok(document) => document,
        Err(error) => return vec![finding(&format!("workflow parse failed: {error}"))],
    };
    let Some(workflow) = document.as_mapping() else {
        return vec![finding("workflow must be a mapping")];
    };
    let Some(jobs) = mapping_value(workflow, "jobs").and_then(Value::as_mapping) else {
        return vec![finding("requires a jobs mapping")];
    };
    let Some(job) = mapping_value(jobs, "ci-required").and_then(Value::as_mapping) else {
        return vec![finding("job must exist as a mapping")];
    };
    let mut findings = Vec::new();
    if mapping_value(workflow, "env").is_some() {
        findings.push(finding(
            "workflow-level environment overrides are not admitted",
        ));
    }
    if mapping_value(job, "env").is_some() {
        findings.push(finding(
            "ci-required job-level environment overrides are not admitted",
        ));
    }
    if mapping_value(job, "uses").is_some() {
        findings.push(finding(
            "ci-required must execute its verdict step, not call a reusable workflow",
        ));
    }
    if mapping_value(job, "container").is_some() {
        findings.push(finding(
            "ci-required must not run inside a caller-selected container",
        ));
    }
    if mapping_value(job, "strategy").is_some() {
        findings.push(finding(
            "must not use a matrix strategy that can skip or split the required verdict",
        ));
    }
    if mapping_value(job, "name").and_then(Value::as_str) != Some("ci-required") {
        findings.push(finding("must keep the status context name `ci-required`"));
    }
    let condition = mapping_value(job, "if").and_then(Value::as_str);
    if condition.map(normalize_gate_expression).as_deref() != Some("always()") {
        findings.push(finding(
            "must use `if: always()` so skipped dependencies are inspected",
        ));
    }
    let expected = jobs
        .keys()
        .map(String::as_str)
        .filter(|job_id| !matches!(*job_id, "ci-required" | "required"))
        .collect::<BTreeSet<_>>();
    if expected.iter().any(|job_id| !valid_shell_word(job_id)) {
        findings.push(finding("aggregate job ids must be safe static shell words"));
    }
    if !expected.contains("plan") {
        findings.push(finding("dependency graph must include the `plan` job"));
    }
    let callers = match ci_required_callers(jobs, &expected, velnor_policy) {
        Ok(callers) => Some(callers),
        Err(reason) => {
            findings.push(finding(reason));
            None
        }
    };
    let needs = mapping_value(job, "needs").and_then(Value::as_sequence);
    match needs {
        Some(needs) => {
            let parsed = needs
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>();
            if parsed.len() != needs.len() {
                findings.push(finding("needs must contain only unique string job ids"));
            }
            if !parsed.contains("plan") {
                findings.push(finding("dependency graph must include the `plan` job"));
            }
            if parsed != expected {
                findings.push(finding(
                    "needs must include every aggregate job except itself and the final required mirror",
                ));
            }
        }
        None => findings.push(finding("needs must be an array of aggregate job ids")),
    }
    let Some(steps) = mapping_value(job, "steps").and_then(Value::as_sequence) else {
        findings.push(finding("must contain its generated verdict step"));
        return findings;
    };
    if steps.len() != 1 {
        findings.push(finding("must contain exactly one fail-closed verdict step"));
    }
    let verdict_steps = steps
        .iter()
        .filter_map(Value::as_mapping)
        .filter(|step| {
            mapping_value(step, "name").and_then(Value::as_str)
                == Some("Validate generated stack results")
        })
        .collect::<Vec<_>>();
    if verdict_steps.len() != 1 {
        findings.push(finding(
            "must run exactly one `Validate generated stack results` step",
        ));
    }
    if let Some(step) = verdict_steps.first() {
        if mapping_value(step, "if").is_some() {
            findings.push(finding("verdict step must not have a skip condition"));
        }
        if mapping_value(step, "continue-on-error").is_some() {
            findings.push(finding("verdict step must not continue on error"));
        }
        if mapping_value(step, "shell").and_then(Value::as_str) != Some("bash") {
            findings.push(finding("verdict step must use `shell: bash`"));
        }
        let env = mapping_value(step, "env").and_then(Value::as_mapping);
        let mut admission_envs = BTreeSet::new();
        if let Some(environment) = env {
            for (key, value) in environment {
                let name = key.as_str();
                if matches!(
                    name,
                    "NEEDS_JSON" | "SELECTED_UNITS" | "PLAN_DIGEST" | "EXCLUDED"
                ) {
                    continue;
                }
                if !is_provider_admission_env(name) {
                    findings.push(finding(&format!(
                        "verdict step environment key `{name}` is not admitted"
                    )));
                    continue;
                }
                admission_envs.insert(name.to_owned());
                if let Some(contract) = velnor_policy {
                    let expected = expected_provider_admission_expression(name, contract);
                    let expected =
                        expected.map(|expression| normalize_gate_expression(&expression));
                    let actual = value.as_str().map(normalize_gate_expression);
                    if expected.as_deref() != actual.as_deref() {
                        findings.push(finding(&format!(
                            "verdict step must bind `{name}` to the configured provider admission"
                        )));
                    }
                }
            }
        }
        for (name, expected) in [
            ("NEEDS_JSON", "${{ toJSON(needs) }}"),
            ("SELECTED_UNITS", "${{ needs.plan.outputs.units }}"),
            ("PLAN_DIGEST", "${{ needs.plan.outputs.plan_digest }}"),
            ("EXCLUDED", "${{ needs.plan.outputs.excluded }}"),
        ] {
            if env
                .and_then(|environment| mapping_value(environment, name))
                .and_then(Value::as_str)
                != Some(expected)
            {
                findings.push(finding(&format!(
                    "verdict step must bind `{name}` from the aggregate needs/plan outputs"
                )));
            }
        }
        let script = mapping_value(step, "run").and_then(Value::as_str);
        match script {
            Some(script) => {
                if !callers.as_ref().is_some_and(|callers| {
                    ci_required_script_is_fail_closed(
                        script,
                        &expected,
                        &admission_envs,
                        callers,
                        velnor_policy,
                    )
                }) {
                    findings.push(finding(
                        "verdict step must bind every dependency result to its caller unit/provider and exit nonzero on failure",
                    ));
                }
            }
            None => findings.push(finding("verdict step must execute a bash script")),
        }
    }
    if mapping_value(job, "continue-on-error").is_some() {
        findings.push(finding("job must not continue on error"));
    }
    findings
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CiRequiredCaller {
    expected_condition: String,
    admission_env: String,
}

fn ci_required_callers(
    jobs: &Mapping,
    dependencies: &BTreeSet<&str>,
    velnor_policy: Option<&VelnorPolicyContract>,
) -> Result<BTreeMap<String, CiRequiredCaller>, &'static str> {
    let mut callers = BTreeMap::new();
    for job_id in dependencies
        .iter()
        .copied()
        .filter(|job_id| !matches!(*job_id, "plan" | "policy"))
    {
        let job = mapping_value(jobs, job_id)
            .and_then(Value::as_mapping)
            .ok_or("every dependency must be a generated reusable caller mapping")?;
        let uses = mapping_value(job, "uses")
            .and_then(Value::as_str)
            .ok_or("every dependency verdict must name its generated reusable workflow")?;
        if !is_approved_local_reusable(uses) {
            return Err("every dependency verdict must name an approved local reusable workflow");
        }
        let inputs = mapping_value(job, "with")
            .and_then(Value::as_mapping)
            .ok_or("every reusable dependency must declare static unit/provider inputs")?;
        let unit = mapping_value(inputs, "unit")
            .and_then(Value::as_str)
            .filter(|unit| runtime::is_unit_id(unit) && valid_shell_word(unit))
            .ok_or("every reusable dependency must declare a static valid unit id")?;
        let provider = mapping_value(inputs, "provider")
            .and_then(Value::as_str)
            .ok_or("every reusable dependency must declare a static provider id")?;
        if provider == "control" && job_id == "prepare-cargo" {
            if !uses.ends_with("/ci-unit-rust.yml") {
                return Err("prepare-cargo must call the generated Rust reusable workflow");
            }
            let selected = caller_plan_unit_ids(job)
                .ok_or("prepare-cargo must expose static plan unit selectors")?;
            if selected.first().map(String::as_str) != Some(unit) {
                return Err("prepare-cargo's sample unit must match its first selected unit");
            }
            if velnor_policy.is_some_and(|contract| {
                !super::provider::ProviderId::LOCAL.iter().any(|provider| {
                    contract
                        .providers
                        .iter()
                        .any(|known| known == provider.as_str())
                })
            }) {
                return Err("prepare-cargo requires a configured local provider");
            }
            let expected_condition = selected
                .iter()
                .map(|unit| format!("plan_expects_local \"{unit}\""))
                .collect::<Vec<_>>()
                .join(" || ");
            callers.insert(
                job_id.to_owned(),
                CiRequiredCaller {
                    expected_condition,
                    admission_env: "PROVIDER_ADMITTED_ANY_LOCAL_TRUSTED".to_owned(),
                },
            );
            continue;
        }
        if provider == "control" || job_id == "prepare-cargo" {
            return Err("only prepare-cargo may use the control provider");
        }
        if !matches!(provider, "github-hosted" | "github-self-hosted" | "velnor") {
            return Err("reusable dependency provider must use a canonical provider id");
        }
        if velnor_policy
            .is_some_and(|contract| !contract.providers.iter().any(|known| known == provider))
        {
            return Err("reusable dependency provider must belong to the configured provider set");
        }
        let expected_job_id = format!("{provider}-{unit}");
        if job_id != expected_job_id {
            return Err("reusable dependency id must bind its declared provider and unit");
        }
        let unit_admission = mapping_value(inputs, "unit_admission")
            .and_then(Value::as_str)
            .ok_or("reusable dependency must declare its unit admission class")?;
        let admission_env = ci_caller_admission_env(provider, unit_admission)
            .ok_or("reusable dependency unit admission must match its provider")?;
        callers.insert(
            job_id.to_owned(),
            CiRequiredCaller {
                expected_condition: format!("plan_expects \"{unit}\" \"{provider}\""),
                admission_env,
            },
        );
    }
    Ok(callers)
}

fn ci_caller_admission_env(provider: &str, unit_admission: &str) -> Option<String> {
    let suffix = match (provider, unit_admission) {
        ("github-hosted", "github-hosted") => "",
        ("github-hosted", "github-hosted-trust-gated")
        | ("github-self-hosted", "github-self-hosted-trust-gated")
        | ("velnor", "velnor-trust-gated") => "_TRUSTED",
        _ => return None,
    };
    let provider = provider.replace('-', "_").to_ascii_uppercase();
    Some(format!("PROVIDER_ADMITTED_{provider}{suffix}"))
}

fn caller_plan_unit_ids(job: &Mapping) -> Option<Vec<String>> {
    let condition = mapping_value(job, "if")?.as_str()?;
    let normalized = normalize_gate_expression(condition);
    const PREFIX: &str = r#"contains(needs.plan.outputs.units,'"unit_id":"#;
    const SUFFIX: &str = "\"')";
    let mut remaining = normalized.as_str();
    let mut units = Vec::new();
    while let Some(start) = remaining.find(PREFIX) {
        let after = &remaining[start + PREFIX.len()..];
        let end = after.find(SUFFIX)?;
        let unit = &after[..end];
        if !runtime::is_unit_id(unit) || !valid_shell_word(unit) {
            return None;
        }
        units.push(unit.to_owned());
        remaining = &after[end + SUFFIX.len()..];
    }
    (!units.is_empty() && units.iter().collect::<BTreeSet<_>>().len() == units.len())
        .then_some(units)
}

fn ci_required_script_is_fail_closed(
    script: &str,
    dependencies: &BTreeSet<&str>,
    admission_envs: &BTreeSet<String>,
    caller_contracts: &BTreeMap<String, CiRequiredCaller>,
    velnor_policy: Option<&VelnorPolicyContract>,
) -> bool {
    let lines = script.lines().map(str::trim).collect::<Vec<_>>();
    if lines.is_empty()
        || lines
            .iter()
            .any(|line| line.is_empty() || line.starts_with('#'))
    {
        return false;
    }
    let mut cursor = 0;
    if !consume_script_lines(&lines, &mut cursor, &["set -euo pipefail"])
        || !consume_script_lines(
            &lines,
            &mut cursor,
            &[
                "if [[ -z \"$PLAN_DIGEST\" ]]; then",
                "echo \"plan did not freeze a plan digest: the expected set has no identity\" >&2",
                "exit 1",
                "fi",
                "echo \"verdict binds plan digest $PLAN_DIGEST\"",
                "result_for_job() {",
                "jq -r --arg job \"$1\" '.[$job].result // empty' <<<\"$NEEDS_JSON\"",
                "}",
                "plan_expects() {",
                "[[ \"$(jq -r --arg unit \"$1\" --arg provider \"$2\" '[.[] | select(.unit_id == $unit) | .providers[] | select(. == $provider)] | length' <<<\"$SELECTED_UNITS\")\" -gt 0 ]]",
                "}",
            ],
        )
    {
        return false;
    }

    let needs_local_expect_function = caller_contracts
        .values()
        .any(|caller| caller.expected_condition.starts_with("plan_expects_local "));
    let has_local_expect_function = lines.get(cursor) == Some(&"plan_expects_local() {");
    if has_local_expect_function != needs_local_expect_function
        || (has_local_expect_function
            && !consume_plan_expects_local_function(&lines, &mut cursor, velnor_policy))
    {
        return false;
    }

    for prerequisite in ["plan", "policy"] {
        if prerequisite == "policy" && !dependencies.contains("policy") {
            continue;
        }
        if !consume_script_lines(
            &lines,
            &mut cursor,
            &[
                &format!("result=\"$(result_for_job {prerequisite})\""),
                "if [[ \"$result\" != success ]]; then",
                &format!(
                    "echo \"required CI prerequisite {prerequisite} did not pass: $result\" >&2"
                ),
                "exit 1",
                "fi",
            ],
        ) {
            return false;
        }
    }

    let callers = dependencies
        .iter()
        .copied()
        .filter(|job| !matches!(*job, "plan" | "policy"))
        .collect::<BTreeSet<_>>();
    if callers.len() != caller_contracts.len()
        || callers
            .iter()
            .any(|job| !caller_contracts.contains_key(*job))
    {
        return false;
    }
    let mut checked = BTreeSet::new();
    let mut checked_admission_envs = BTreeSet::new();
    while cursor < lines.len() {
        let Some(header) = lines[cursor]
            .strip_prefix("if ")
            .and_then(|line| line.strip_suffix("; then"))
        else {
            return false;
        };
        let Some(line) = lines.get(cursor + 1) else {
            return false;
        };
        let Some(job) = line
            .strip_prefix("result=\"$(result_for_job ")
            .and_then(|line| line.strip_suffix(")\""))
        else {
            return false;
        };
        let Some(expected_caller) = caller_contracts.get(job) else {
            return false;
        };
        if !callers.contains(job) || !checked.insert(job) {
            return false;
        }
        if header != expected_caller.expected_condition {
            return false;
        }
        let Some(admission_line) = lines.get(cursor + 2) else {
            return false;
        };
        let Some(admission) = admission_line
            .strip_prefix("if [[ \"$")
            .and_then(|line| line.strip_suffix("\" == true ]]; then"))
        else {
            return false;
        };
        if !admission_envs.contains(admission)
            || !is_provider_admission_env(admission)
            || admission != expected_caller.admission_env
        {
            return false;
        }
        checked_admission_envs.insert(admission.to_owned());
        let noun = if job == "prepare-cargo" {
            "CI prerequisite"
        } else {
            "CI job"
        };
        let expected = required_caller_verdict_lines(header, job, admission, noun);
        if lines.get(cursor..cursor + expected.len())
            != Some(
                expected
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
        {
            return false;
        }
        cursor += expected.len();
    }
    checked == callers && checked_admission_envs == *admission_envs
}

fn consume_script_lines(lines: &[&str], cursor: &mut usize, expected: &[&str]) -> bool {
    let Some(actual) = lines.get(*cursor..cursor.saturating_add(expected.len())) else {
        return false;
    };
    if actual != expected {
        return false;
    }
    *cursor += expected.len();
    true
}

fn consume_plan_expects_local_function(
    lines: &[&str],
    cursor: &mut usize,
    velnor_policy: Option<&VelnorPolicyContract>,
) -> bool {
    let Some(function) = lines.get(*cursor..cursor.saturating_add(3)) else {
        return false;
    };
    let prefix =
        "[[ \"$(jq -r --arg unit \"$1\" '[.[] | select(.unit_id == $unit) | .providers[] | select(";
    let suffix = ")] | length' <<<\"$SELECTED_UNITS\")\" -gt 0 ]]";
    let Some(alternation) = function[1]
        .strip_prefix(prefix)
        .and_then(|line| line.strip_suffix(suffix))
    else {
        return false;
    };
    let providers = alternation.split(" or ").collect::<Vec<_>>();
    if providers.is_empty()
        || providers
            .iter()
            .any(|provider| !matches!(*provider, ". == \"github-self-hosted\"" | ". == \"velnor\""))
        || providers.windows(2).any(|pair| pair[0] == pair[1])
        || function[0] != "plan_expects_local() {"
        || function[2] != "}"
    {
        return false;
    }
    if let Some(contract) = velnor_policy {
        let expected = super::provider::ProviderId::LOCAL
            .iter()
            .filter(|provider| {
                contract
                    .providers
                    .iter()
                    .any(|known| known == provider.as_str())
            })
            .map(|provider| format!(". == \"{}\"", provider.as_str()))
            .collect::<Vec<_>>();
        if providers != expected.iter().map(String::as_str).collect::<Vec<_>>() {
            return false;
        }
    }
    *cursor += 3;
    true
}

fn valid_shell_word(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
}

fn is_provider_admission_env(value: &str) -> bool {
    value.starts_with("PROVIDER_ADMITTED_")
        && value.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn expected_provider_admission_expression(
    name: &str,
    contract: &VelnorPolicyContract,
) -> Option<String> {
    if name == "PROVIDER_ADMITTED_ANY_LOCAL_TRUSTED" {
        let mut providers = contract
            .providers
            .iter()
            .map(String::as_str)
            .filter(|provider| VelnorPolicyContract::is_local_provider(provider))
            .collect::<Vec<_>>();
        providers.sort_unstable();
        if providers.is_empty() {
            return Some("false".to_owned());
        }
        let expressions = providers
            .into_iter()
            .map(|provider| single_provider_admission_expression(provider, true, contract))
            .collect::<Option<Vec<_>>>()?;
        return Some(format!("({})", expressions.join(") || (")));
    }
    let (provider, trusted) = match name {
        "PROVIDER_ADMITTED_GITHUB_HOSTED" => ("github-hosted", false),
        "PROVIDER_ADMITTED_GITHUB_HOSTED_TRUSTED" => ("github-hosted", true),
        "PROVIDER_ADMITTED_GITHUB_SELF_HOSTED_TRUSTED" => ("github-self-hosted", true),
        "PROVIDER_ADMITTED_VELNOR_TRUSTED" => ("velnor", true),
        _ => return None,
    };
    if !contract.providers.iter().any(|known| known == provider) {
        return None;
    }
    single_provider_admission_expression(provider, trusted, contract)
}

fn single_provider_admission_expression(
    provider: &str,
    trusted: bool,
    contract: &VelnorPolicyContract,
) -> Option<String> {
    let automatic = contract
        .automatic_providers
        .iter()
        .any(|automatic| automatic == provider);
    let event = if VelnorPolicyContract::is_local_provider(provider) {
        let repository = contract.repository.as_deref()?;
        let event = if automatic {
            format!(
                "github.event_name == 'push' && github.ref == 'refs/heads/{}'",
                contract.default_branch
            )
        } else {
            "false".to_owned()
        };
        return Some(format!("github.repository == '{repository}' && ({event})"));
    } else {
        let dispatch = format!(
            "github.event_name == 'workflow_dispatch' && contains(format(',{{0}},', github.event.inputs.providers), ',{provider},')"
        );
        let automatic = if automatic {
            "github.event_name != 'workflow_dispatch'"
        } else {
            "false"
        };
        format!("({dispatch}) || ({automatic})")
    };
    if trusted {
        let trusted_event = "!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'))";
        Some(format!("({event}) && ({trusted_event})"))
    } else {
        Some(event)
    }
}

fn required_caller_verdict_lines(
    condition: &str,
    job: &str,
    admitted: &str,
    noun: &str,
) -> Vec<String> {
    vec![
        format!("if {condition}; then"),
        format!("result=\"$(result_for_job {job})\""),
        format!("if [[ \"${admitted}\" == true ]]; then"),
        "case \"$result\" in".to_owned(),
        "success) ;;".to_owned(),
        format!("skipped) echo \"expected {noun} {job} was skipped: a skipped expected result cannot pass\" >&2; exit 1 ;;"),
        format!("cancelled) echo \"expected {noun} {job} was cancelled: a cancelled expected result cannot pass\" >&2; exit 1 ;;"),
        format!("*) echo \"expected {noun} {job} did not pass: $result\" >&2; exit 1 ;;"),
        "esac".to_owned(),
        "else".to_owned(),
        "case \"$result\" in".to_owned(),
        "skipped) ;;".to_owned(),
        format!("*) echo \"selected {noun} {job} ran outside its provider admission ({admitted}=${admitted}): $result\" >&2; exit 1 ;;"),
        "esac".to_owned(),
        "fi".to_owned(),
        "else".to_owned(),
        format!("result=\"$(result_for_job {job})\""),
        "case \"$result\" in".to_owned(),
        "skipped) ;;".to_owned(),
        format!("success) echo \"unexpected {noun} {job} succeeded outside the expected set: the plan did not declare it\" >&2; exit 1 ;;"),
        format!("*) echo \"unexpected {noun} {job} ran outside the expected set: $result\" >&2; exit 1 ;;"),
        "esac".to_owned(),
        "fi".to_owned(),
    ]
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

fn audit_policy_entrypoint_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    velnor_policy: &VelnorPolicyContract,
) -> Result<EntrypointAudit, GeneratorError> {
    let mut audit = EntrypointAudit::default();
    let content = match read_text_with_safe_root(
        root,
        Path::new(POLICY_ENTRYPOINT),
        "read policy entrypoint",
    )? {
        Some(content) => content,
        None => {
            audit.trigger.push(format!(
                "{POLICY_ENTRYPOINT}: the base-owned policy entrypoint is missing"
            ));
            return Ok(audit);
        }
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

/// Trigger set from the trusted renderer: base-controlled
/// `pull_request_target` only. No other event or filter is admitted.
fn audit_entrypoint_triggers(workflow: &Mapping, audit: &mut EntrypointAudit) {
    let finding = |message: &str| format!("{POLICY_ENTRYPOINT}: {message}");
    match mapping_value(workflow, "on").and_then(Value::as_mapping) {
        Some(on) => {
            let triggers = on.keys().map(String::as_str).collect::<BTreeSet<_>>();
            if triggers != BTreeSet::from(["pull_request_target"]) || triggers.len() != on.len() {
                for key in on.keys() {
                    if key.as_str() != "pull_request_target" {
                        audit.trigger.push(finding(&format!(
                            "trigger `{}` is not admitted; the entrypoint runs only on pull_request_target",
                            key.as_str()
                        )));
                    }
                }
                if !triggers.contains("pull_request_target") {
                    audit.trigger.push(finding(
                        "must run on pull_request_target with explicit activity types",
                    ));
                }
            }
            match mapping_value(on, "pull_request_target").and_then(Value::as_mapping) {
                Some(event) => {
                    for key in event.keys() {
                        if key.as_str() != "types" {
                            audit.trigger.push(finding(&format!(
                                "pull_request_target `{}` filters are not admitted",
                                key.as_str()
                            )));
                        }
                    }
                    let expected = ["opened", "synchronize", "reopened"];
                    let types = mapping_value(event, "types").and_then(Value::as_sequence);
                    if !types.is_some_and(|types| {
                        types.len() == expected.len()
                            && types
                                .iter()
                                .zip(expected)
                                .all(|(value, expected)| value.as_str() == Some(expected))
                    }) {
                        let actual = types.map_or_else(String::new, |types| {
                            types
                                .iter()
                                .map(|value| value.as_str().unwrap_or("<non-string>"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        });
                        audit.trigger.push(finding(&format!(
                            "pull_request_target types must be [{}], got [{}]",
                            expected.join(", "),
                            actual
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
/// credentials, and one exact job on the fixed GitHub-hosted image.
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
    if mapping_value(workflow, "env").is_some() {
        audit.privileges.push(finding(
            "workflow-level environment overrides are not admitted",
        ));
    }
    if mapping_value(workflow, "defaults").is_some() {
        audit
            .privileges
            .push(finding("workflow-level run defaults are not admitted"));
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
            if job_id.as_str() != "policy" {
                audit.privileges.push(finding(
                    "required policy job id must be the canonical `policy`",
                ));
            }
            if mapping_value(job, "name").and_then(Value::as_str) != Some("Policy") {
                audit
                    .privileges
                    .push(finding("required policy job name must be `Policy`"));
            }
            if mapping_value(job, "if").is_some() {
                audit.privileges.push(finding(
                    "required policy job must not have a skip condition",
                ));
            }
            if mapping_value(job, "continue-on-error").is_some() {
                audit
                    .privileges
                    .push(finding("required policy job must not continue on error"));
            }
            if mapping_value(job, "env").is_some() {
                audit.privileges.push(finding(
                    "policy job-level environment overrides are not admitted",
                ));
            }
            if mapping_value(job, "uses").is_some() {
                audit.privileges.push(finding(
                    "required policy job must run its checked steps, not call a reusable workflow",
                ));
            }
            if mapping_value(job, "container").is_some() {
                audit.privileges.push(finding(
                    "required policy job must not run inside a caller-selected container",
                ));
            }
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
            let policy_steps = steps
                .iter()
                .filter_map(Value::as_mapping)
                .filter(|step| {
                    mapping_value(step, "name").and_then(Value::as_str)
                        == Some("Enforce workflow policy")
                })
                .collect::<Vec<_>>();
            if policy_steps.len() != 1 {
                audit.privileges.push(finding(
                    "required policy job must run exactly one `Enforce workflow policy` step",
                ));
            }
            if let Some(step) = policy_steps.first() {
                if mapping_value(step, "if").is_some() {
                    audit.privileges.push(finding(
                        "policy enforcement step must not have a skip condition",
                    ));
                }
                if mapping_value(step, "continue-on-error").is_some() {
                    audit.privileges.push(finding(
                        "policy enforcement step must not continue on error",
                    ));
                }
                if mapping_value(step, "shell").and_then(Value::as_str) != Some("bash") {
                    audit
                        .privileges
                        .push(finding("policy enforcement step must use `shell: bash`"));
                }
                let root = mapping_value(step, "env")
                    .and_then(Value::as_mapping)
                    .and_then(|environment| mapping_value(environment, "WORKFLOW_ROOT"))
                    .and_then(Value::as_str);
                if root != Some("${{ github.workspace }}/policy-checkout") {
                    audit.privileges.push(finding(
                        "policy enforcement must audit the approved `${{ github.workspace }}/policy-checkout` path",
                    ));
                }
                let script = mapping_value(step, "run").and_then(Value::as_str);
                if !script.is_some_and(policy_enforcement_script_is_fail_closed) {
                    audit.privileges.push(finding(
                        "policy enforcement step must execute `velnor-workflow policy` and propagate its failure",
                    ));
                }
            }
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
                Some(Value::String(label)) if label == POLICY_RUNNER_LABEL => {
                    match policy_job_matches_renderer(job, velnor_policy) {
                        Some(true) => {}
                        Some(false) => audit.privileges.push(finding(
                            "policy job must match the trusted renderer's complete enforced job shape",
                        )),
                        None => audit.privileges.push(finding(
                            "policy job lacks the trusted revision or ruleset input needed to verify its complete shape",
                        )),
                    }
                }
                Some(_) => audit.privileges.push(finding(&format!(
                    "policy job must use the fixed trusted GitHub-hosted image `{POLICY_RUNNER_LABEL}`; dynamic, candidate-selected, and runner-group selectors are forbidden"
                ))),
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

/// Build the generated policy job from the trusted caller facts and the
/// candidate's non-authoritative renderer inputs. Comparing the complete job
/// mapping rejects extra steps, environment overrides, reusable calls,
/// matrices, containers, and changes that could prevent the enforced command
/// from running. Repository and default-branch identity come from the trusted
/// runner contract; the candidate can supply neither.
fn policy_job_matches_renderer(
    job: &Mapping,
    velnor_policy: &VelnorPolicyContract,
) -> Option<bool> {
    let revision = policy_enforcement_revision(job)?;
    let declared_ruleset_contexts = policy_declared_ruleset_contexts(job)?;
    let spec = super::PolicyJobSpec {
        name: "Policy",
        revision,
        repository: velnor_policy.repository.as_deref().unwrap_or_default(),
        default_branch: &velnor_policy.default_branch,
        declared_ruleset_contexts: &declared_ruleset_contexts,
    };
    let expected = format!("jobs:\n{}", super::policy_job(&spec));
    let expected_document = serde_yaml::from_str::<Value>(&expected).ok()?;
    let expected_job = expected_document
        .as_mapping()
        .and_then(|workflow| mapping_value(workflow, "jobs"))
        .and_then(Value::as_mapping)
        .and_then(|jobs| mapping_value(jobs, "policy"))?;
    Some(expected_job == &Value::Mapping(job.clone()))
}

fn policy_enforcement_revision(job: &Mapping) -> Option<&str> {
    let steps = mapping_value(job, "steps")?.as_sequence()?;
    let step = steps.iter().filter_map(Value::as_mapping).find(|step| {
        mapping_value(step, "name").and_then(Value::as_str) == Some("Enforce workflow policy")
    })?;
    mapping_value(step, "env")?
        .as_mapping()
        .and_then(|environment| mapping_value(environment, BASE_REVISION_ENV))
        .and_then(Value::as_str)
        .filter(|revision| super::is_full_revision(revision))
}

fn policy_declared_ruleset_contexts(job: &Mapping) -> Option<String> {
    let steps = mapping_value(job, "steps")?.as_sequence()?;
    let step = steps.iter().filter_map(Value::as_mapping).find(|step| {
        mapping_value(step, "name").and_then(Value::as_str)
            == Some("Resolve required status checks")
    })?;
    mapping_value(step, "env")?
        .as_mapping()
        .and_then(|environment| mapping_value(environment, "DECLARED_RULESET_CONTEXTS"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn policy_enforcement_script_is_fail_closed(script: &str) -> bool {
    let mut lines = script
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    if lines.next() != Some("set -euo pipefail") {
        return false;
    }
    let Some("velnor-workflow policy \\") = lines.next() else {
        return false;
    };
    let Some("--workflow-root \"$WORKFLOW_ROOT\" \\") = lines.next() else {
        return false;
    };
    let Some("--head-sha \"$HEAD_SHA\" \\") = lines.next() else {
        return false;
    };
    match (lines.next(), lines.next()) {
        (Some("--base-sha \"$BASE_SHA\""), None) => true,
        (Some(r#"--base-sha "$BASE_SHA" \"#), Some("--ruleset-contexts \"$RULESET_CONTEXTS\"")) => {
            lines.next().is_none()
        }
        _ => false,
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
/// `pull_request_target` rule here; [`audit_policy_entrypoint_with_safe_root`] judges it.
///
/// # Errors
/// When the workflow directory or a workflow file cannot be read.
#[cfg(test)]
pub(crate) fn audit_workflows(root: &Path) -> Result<WorkflowAudit, GeneratorError> {
    let safe_root = super::safe_fs::SafeRoot::open(root)?;
    audit_workflows_with_safe_root(&safe_root, None, None)
}

fn audit_workflows_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    trusted_repository: Option<&str>,
    trusted_default_branch: Option<&str>,
) -> Result<WorkflowAudit, GeneratorError> {
    let workflows_relative = Path::new(".github/workflows");
    let workflows_path = root.command_directory().join(workflows_relative);
    let workflows = root.open_directory(workflows_relative)?.ok_or_else(|| {
        GeneratorError::io(
            "read workflow directory",
            &workflows_path,
            &std::io::Error::from(std::io::ErrorKind::NotFound),
        )
    })?;
    let policy_entrypoint = workflows_path.join("ci-policy.yml");
    let generation = generation_config_with_safe_root(root)?;
    let velnor_policy = configured_velnor_policy_with_safe_root(
        root,
        generation.as_ref(),
        trusted_repository,
        trusted_default_branch,
    )?;
    let mut findings = PolicyFindings {
        root: root.command_directory().to_path_buf(),
        ..PolicyFindings::default()
    };
    // GitHub rejects a workflow whose YAML carries duplicate keys, so the
    // auditor must fail closed on exactly the inputs GitHub refuses instead of
    // silently auditing the last-key-wins rewrite of them.
    let parser = serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
    let mut entries = workflows.entries()?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let mut documents = Vec::new();
    for entry in entries {
        let relative_name = Path::new(&entry.name);
        if !matches!(
            relative_name.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        if !matches!(entry.kind, super::safe_fs::SafeEntryKind::File) {
            return Err(GeneratorError::usage(format!(
                "refusing non-regular workflow path: {}",
                workflows_path.join(&entry.name).display()
            )));
        }
        let path = workflows_path.join(&entry.name);
        let bytes = workflows.read_file(relative_name)?;
        let content = String::from_utf8(bytes).map_err(|error| {
            GeneratorError::usage(format!(
                "read workflow {} as UTF-8: {error}",
                path.display()
            ))
        })?;
        let document: Value = match serde_yaml::from_str_with_config(&content, &parser) {
            Ok(document) => document,
            Err(error) => {
                findings.record(Rule::Structure, &path, &format!("parse workflow: {error}"));
                continue;
            }
        };
        if document.as_mapping().is_none() {
            findings.record(
                Rule::Structure,
                &path,
                "workflow document must be a YAML mapping",
            );
            continue;
        }
        documents.push((path, document));
    }
    let untrusted_pr_paths = untrusted_pr_reachable_workflows(&documents);
    for (path, document) in documents {
        let Some(workflow) = document.as_mapping() else {
            continue;
        };
        inspect_workflow(
            workflow,
            &path,
            path == policy_entrypoint,
            untrusted_pr_paths.contains(&path),
            &velnor_policy,
            &mut findings,
        );
        if path == workflows_path.join("ci-pr.yml") {
            for finding in audit_ci_pr_trigger(workflow) {
                findings.record(Rule::Structure, &path, &finding);
            }
        }
    }
    Ok(findings.audit)
}

/// The ruleset context must run for every pull request. Candidate-controlled
/// branch/path filters could otherwise leave an old green context in place.
fn audit_ci_pr_trigger(workflow: &Mapping) -> Vec<String> {
    let mut findings = Vec::new();
    let Some(on) = mapping_value(workflow, "on").and_then(Value::as_mapping) else {
        return vec!["`on` must be a trigger mapping containing pull_request".to_owned()];
    };
    for key in on.keys() {
        if !matches!(key.as_str(), "pull_request" | "workflow_dispatch") {
            findings.push(format!(
                "trigger `{}` is not admitted for the pull-request aggregate",
                key.as_str()
            ));
        }
    }
    let Some(event) = mapping_value(on, "pull_request") else {
        findings.push("must run on pull_request without target-branch or path filters".to_owned());
        return findings;
    };
    match event {
        Value::Null => {}
        Value::Mapping(event) => {
            for key in event.keys() {
                match key.as_str() {
                    "branches" | "branches-ignore" | "paths" | "paths-ignore" => {
                        findings.push(format!(
                            "pull_request `{}` filters can bypass the required status check",
                            key.as_str()
                        ));
                    }
                    "types" => {}
                    other => findings.push(format!(
                        "pull_request key `{other}` is not admitted by the aggregate trigger contract"
                    )),
                }
            }
            if let Some(types) = mapping_value(event, "types") {
                let Some(types) = types.as_sequence() else {
                    findings.push("pull_request types must be an array".to_owned());
                    return findings;
                };
                let parsed = types
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>();
                let required = BTreeSet::from(["opened", "reopened", "synchronize"]);
                if parsed.len() != types.len() || !required.is_subset(&parsed) {
                    findings.push(
                        "pull_request types must include opened, reopened, and synchronize"
                            .to_owned(),
                    );
                }
            }
        }
        _ => findings.push("pull_request must be an empty event or mapping".to_owned()),
    }
    findings
}

fn inspect_workflow(
    workflow: &Mapping,
    path: &Path,
    is_policy_entrypoint: bool,
    untrusted_pr_path: bool,
    velnor_policy: &VelnorPolicyContract,
    failures: &mut PolicyFindings,
) {
    if untrusted_pr_path {
        audit_untrusted_pr_workflow_privileges(workflow, path, failures);
    }
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

/// Workflow files triggered by pull request events, their local reusable
/// callees, and `workflow_run` handlers are reachable from untrusted PR input.
/// Resolve the call graph from parsed workflow data so reusable callee events
/// cannot weaken the caller's admission.
fn untrusted_pr_reachable_workflows(documents: &[(PathBuf, Value)]) -> BTreeSet<PathBuf> {
    let mut by_name = BTreeMap::new();
    let mut by_path = BTreeMap::new();
    for (path, document) in documents {
        if let Some(name) = path.file_name().and_then(OsStr::to_str) {
            by_name.insert(name.to_owned(), path.clone());
        }
        by_path.insert(path.clone(), document);
    }

    let mut reachable = BTreeSet::new();
    let mut pending = VecDeque::new();
    for (path, document) in documents {
        if document
            .as_mapping()
            .is_some_and(workflow_has_untrusted_pr_trigger)
        {
            reachable.insert(path.clone());
            pending.push_back(path.clone());
        }
    }

    while let Some(path) = pending.pop_front() {
        let Some(workflow) = by_path
            .get(&path)
            .and_then(|document| document.as_mapping())
        else {
            continue;
        };
        for target in local_reusable_workflow_targets(workflow) {
            let Some(target_path) = by_name.get(&target) else {
                continue;
            };
            if reachable.insert(target_path.clone()) {
                pending.push_back(target_path.clone());
            }
        }
    }
    reachable
}

fn workflow_has_untrusted_pr_trigger(workflow: &Mapping) -> bool {
    const UNTRUSTED_PR_EVENTS: &[&str] = &[
        "pull_request",
        "pull_request_target",
        "merge_group",
        "pull_request_review",
        "pull_request_review_comment",
        "issue_comment",
        "workflow_run",
    ];
    let is_pr_event = |event: &str| UNTRUSTED_PR_EVENTS.contains(&event);
    match mapping_value(workflow, "on") {
        Some(Value::Mapping(events)) => events.keys().any(|event| is_pr_event(event.as_str())),
        Some(Value::Sequence(events)) => events.iter().filter_map(Value::as_str).any(is_pr_event),
        Some(Value::String(event)) => is_pr_event(event),
        _ => false,
    }
}

fn local_reusable_workflow_targets(workflow: &Mapping) -> Vec<String> {
    let Some(jobs) = mapping_value(workflow, "jobs").and_then(Value::as_mapping) else {
        return Vec::new();
    };
    jobs.values()
        .filter_map(Value::as_mapping)
        .filter_map(|job| mapping_value(job, "uses").and_then(Value::as_str))
        .filter_map(|uses| {
            uses.split_once('@')
                .map_or(uses, |(path, _)| path)
                .strip_prefix("./.github/workflows/")
        })
        .filter(|target| {
            !target.is_empty()
                && !target.contains('/')
                && !target.contains('\\')
                && Path::new(target)
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("yml")
                            || extension.eq_ignore_ascii_case("yaml")
                    })
        })
        .map(str::to_owned)
        .collect()
}

fn audit_untrusted_pr_workflow_privileges(
    workflow: &Mapping,
    path: &Path,
    failures: &mut PolicyFindings,
) {
    let workflow_permissions = mapping_value(workflow, "permissions");
    if workflow_has_untrusted_pr_trigger(workflow) && workflow_permissions.is_none() {
        failures.record(
            Rule::Structure,
            path,
            "PR-triggered workflows must declare explicit read-only `permissions`",
        );
    }
    audit_pr_permissions(workflow_permissions, "workflow", path, failures);
    for (key, value) in workflow {
        if key.as_str() != "jobs" {
            audit_pr_secret_references(value, path, failures);
        }
    }

    if let Some(on) = mapping_value(workflow, "on").and_then(Value::as_mapping)
        && mapping_value(on, "workflow_call")
            .and_then(Value::as_mapping)
            .is_some_and(|call| mapping_value(call, "secrets").is_some())
    {
        failures.record(
            Rule::Structure,
            path,
            "PR-reachable reusable workflows must not declare secret inputs",
        );
    }

    let Some(jobs) = mapping_value(workflow, "jobs").and_then(Value::as_mapping) else {
        return;
    };
    for (job_id, job) in jobs {
        let Some(job) = job.as_mapping() else {
            continue;
        };
        failures.job = Some(job_id.clone());
        audit_pr_permissions(mapping_value(job, "permissions"), "job", path, failures);
        if mapping_value(job, "environment").is_some() {
            failures.record(
                Rule::Structure,
                path,
                "deployment environments are forbidden on untrusted PR paths",
            );
        }
        if let Some(secrets) = mapping_value(job, "secrets") {
            if secrets.as_str() == Some("inherit") {
                failures.record(
                    Rule::Structure,
                    path,
                    "`secrets: inherit` is forbidden on an untrusted PR path",
                );
            } else {
                failures.record(
                    Rule::Structure,
                    path,
                    "jobs on untrusted PR paths must not pass secrets to reusable workflows",
                );
            }
        }
        for value in job.values() {
            audit_pr_secret_references(value, path, failures);
        }
        failures.job = None;
    }
}

fn audit_pr_permissions(
    permissions: Option<&Value>,
    scope: &str,
    path: &Path,
    failures: &mut PolicyFindings,
) {
    let Some(permissions) = permissions else {
        return;
    };
    match permissions {
        Value::String(value) if value.eq_ignore_ascii_case("read-all") => {}
        Value::String(value) if value.eq_ignore_ascii_case("write-all") => failures.record(
            Rule::Structure,
            path,
            &format!("{scope} permissions must not use `write-all` on an untrusted PR path"),
        ),
        Value::Mapping(permissions) => {
            for (permission, value) in permissions {
                let permission = permission.as_str();
                match value.as_str().map(str::to_ascii_lowercase).as_deref() {
                    Some("read" | "none") => {}
                    Some("write") if permission.eq_ignore_ascii_case("id-token") => failures
                        .record(
                            Rule::Structure,
                            path,
                            &format!(
                                "{scope} `id-token: write` must not expose OIDC credentials on an untrusted PR path"
                            ),
                        ),
                    Some("write") => failures.record(
                        Rule::Structure,
                        path,
                        &format!(
                            "{scope} permission `{permission}: write` is forbidden on an untrusted PR path"
                        ),
                    ),
                    _ => failures.record(
                        Rule::Structure,
                        path,
                        &format!(
                            "{scope} permission `{permission}` must be the static value `read` or `none` on an untrusted PR path"
                        ),
                    ),
                }
            }
        }
        _ => failures.record(
            Rule::Structure,
            path,
            &format!(
                "{scope} permissions must be a static read-only mapping on an untrusted PR path"
            ),
        ),
    }
}

fn audit_pr_secret_references(value: &Value, path: &Path, failures: &mut PolicyFindings) {
    match value {
        Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if key.as_str() == "secrets"
                    && value
                        .as_str()
                        .is_some_and(|value| value.eq_ignore_ascii_case("inherit"))
                {
                    failures.record(
                        Rule::Structure,
                        path,
                        "`secrets: inherit` is forbidden on an untrusted PR path",
                    );
                }
                audit_pr_secret_references(value, path, failures);
            }
        }
        Value::Sequence(sequence) => {
            for value in sequence {
                audit_pr_secret_references(value, path, failures);
            }
        }
        Value::Tagged(tagged) => audit_pr_secret_references(tagged.value(), path, failures),
        Value::String(text) => {
            for expression in github_expressions(text) {
                if github_expression_uses_secrets(&expression) {
                    failures.record(
                        Rule::Structure,
                        path,
                        "references the `secrets` context on an untrusted PR path",
                    );
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn github_expression_uses_secrets(expression: &str) -> bool {
    let mut chars = expression.char_indices().peekable();
    let mut quote = None;
    while let Some((index, character)) = chars.next() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            continue;
        }
        if !(character.is_ascii_alphabetic() || character == '_') {
            continue;
        }
        let start = index;
        let mut end = index + character.len_utf8();
        while let Some((next_index, next)) = chars.peek().copied() {
            if !(next.is_ascii_alphanumeric() || next == '_') {
                break;
            }
            chars.next();
            end = next_index + next.len_utf8();
        }
        if &expression[start..end] == "secrets" {
            return true;
        }
    }
    false
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

/// A documented GitHub-hosted image label. Prefix checks are unsafe here:
/// self-hosted runners accept custom labels and can claim an `ubuntu-*` name.
fn is_github_owned_label(label: &str) -> bool {
    const GITHUB_HOSTED_IMAGES: &[&str] = &[
        "ubuntu-slim",
        "ubuntu-latest",
        "ubuntu-22.04",
        "ubuntu-24.04",
        "ubuntu-26.04",
        "ubuntu-22.04-arm",
        "ubuntu-24.04-arm",
        "ubuntu-26.04-arm",
        "windows-latest",
        "windows-2022",
        "windows-2025",
        "windows-2025-vs2026",
        "windows-11-arm",
        "windows-11-vs2026-arm",
        "macos-latest",
        "macos-14",
        "macos-15",
        "macos-26",
        "macos-15-intel",
        "macos-26-intel",
        "xcode-27",
    ];
    GITHUB_HOSTED_IMAGES
        .iter()
        .any(|image| label.eq_ignore_ascii_case(image))
}

fn has_safe_runner_gate(
    condition: &str,
    job: &Mapping,
    velnor_policy: &VelnorPolicyContract,
) -> bool {
    // Generated provider jobs bind local capacity to the configured caller
    // repository and admit only the exact default-branch push. That generated
    // shape is recognized even when the advisory checkout lacks
    // `.github/ci/project.toml` and only carries `.github-gen`.
    let Some(provider) = static_local_provider(job, velnor_policy) else {
        return false;
    };
    is_generated_provider_gate(condition, &provider, velnor_policy)
}

fn generation_workflow_with_safe_root(
    root: &super::safe_fs::SafeRoot,
) -> Result<Option<toml::Value>, GeneratorError> {
    let Some(content) = read_text_with_safe_root(
        root,
        Path::new(GENERATION_CONFIG),
        "read generation workflow config",
    )?
    else {
        return Ok(None);
    };
    let path = root.command_directory().join(GENERATION_CONFIG);
    let value = toml::from_str::<toml::Value>(&content).map_err(|error| {
        GeneratorError::usage(format!("parse workflow config {}: {error}", path.display()))
    })?;
    Ok(value.get("workflow").cloned())
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
/// repository identity, provider universe, automatic set, and selectors.
/// Labels carry no authority here; they only name which declared selector a
/// job's `runs-on` resolves to.
#[derive(Clone, Debug, Default)]
struct VelnorPolicyContract {
    providers: Vec<String>,
    automatic_providers: Vec<String>,
    selectors: BTreeMap<String, Vec<String>>,
    default_branch: String,
    repository: Option<String>,
}

impl VelnorPolicyContract {
    /// The provider whose declared selector `labels` equals, if any. Labels
    /// use GitHub's case-insensitive set semantics; ambiguous matches fail
    /// closed.
    fn provider_for_labels(&self, labels: &[&str]) -> Option<&str> {
        let mut sorted = labels
            .iter()
            .map(|label| label.to_ascii_lowercase())
            .collect::<Vec<_>>();
        sorted.sort_unstable();
        if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
            return None;
        }
        let mut matched = None;
        for (provider, selector) in &self.selectors {
            if !self
                .providers
                .iter()
                .any(|configured| configured == provider)
            {
                continue;
            }
            let mut expected = selector
                .iter()
                .map(|label| label.to_ascii_lowercase())
                .collect::<Vec<_>>();
            expected.sort_unstable();
            if expected == sorted {
                if matched.is_some() {
                    return None;
                }
                matched = Some(provider.as_str());
            }
        }
        matched
    }

    fn is_local_provider(provider: &str) -> bool {
        matches!(provider, "github-self-hosted" | "velnor")
    }
}

fn configured_velnor_policy_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    generation_config: Option<&config::RepoGenerationConfig>,
    trusted_repository: Option<&str>,
    trusted_default_branch: Option<&str>,
) -> Result<VelnorPolicyContract, GeneratorError> {
    // Advisory sparse-checkout omits `.github/ci`. Do not default the
    // contract on a missing project.toml: the generation config still
    // carries providers and selectors.
    let runtime =
        match read_text_with_safe_root(root, Path::new(RUNTIME_CONFIG), "read workflow config")? {
            Some(content) => Some(toml::from_str::<toml::Value>(&content).map_err(|error| {
                let path = root.command_directory().join(RUNTIME_CONFIG);
                GeneratorError::usage(format!("parse workflow config {}: {error}", path.display()))
            })?),
            None => None,
        };
    let generation = generation_workflow_with_safe_root(root)?;
    if runtime.is_none() && generation.is_none() {
        let repository = trusted_repository.map(str::to_owned);
        let default_branch = trusted_default_branch.unwrap_or("main").to_owned();
        return Ok(VelnorPolicyContract {
            repository,
            default_branch,
            ..VelnorPolicyContract::default()
        });
    }
    let generation_workflow = generation.as_ref().and_then(toml::Value::as_table);
    let runtime_repository = runtime
        .as_ref()
        .and_then(|value| value.get("repository"))
        .and_then(toml::Value::as_str);
    let generation_repository =
        generation_config.and_then(config::RepoGenerationConfig::repository);
    if runtime_repository.is_some()
        && generation_repository.is_some()
        && runtime_repository != generation_repository
    {
        return Err(GeneratorError::usage(format!(
            "workflow policy repository identity differs between {RUNTIME_CONFIG} and {GENERATION_CONFIG}"
        )));
    }
    let repository = if let Some(trusted_repository) = trusted_repository {
        if !valid_repository_slug(trusted_repository) {
            return Err(GeneratorError::usage(
                "trusted workflow repository identity must be a GitHub owner/repository slug",
            ));
        }
        for (source, declared) in [
            (RUNTIME_CONFIG, runtime_repository),
            (GENERATION_CONFIG, generation_repository),
        ] {
            if declared.is_some_and(|declared| declared != trusted_repository) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy repository identity in {source} does not match trusted repository {trusted_repository}"
                )));
            }
        }
        Some(trusted_repository.to_owned())
    } else {
        runtime_repository
            .or(generation_repository)
            .map(str::to_owned)
    };
    if repository
        .as_deref()
        .is_some_and(|repository| !valid_repository_slug(repository))
    {
        return Err(GeneratorError::usage(
            "workflow policy repository identity must be a GitHub owner/repository slug",
        ));
    }
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
    let declared_default_branch = runtime
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
    let default_branch = if let Some(trusted_default_branch) = trusted_default_branch {
        if !runtime::valid_branch(trusted_default_branch) {
            return Err(GeneratorError::usage(
                "trusted workflow default branch is invalid",
            ));
        }
        for (source, declared) in [
            (
                RUNTIME_CONFIG,
                runtime
                    .as_ref()
                    .and_then(|value| value.get("default_branch"))
                    .and_then(toml::Value::as_str),
            ),
            (
                GENERATION_CONFIG,
                generation_workflow
                    .and_then(|workflow| workflow.get("default_branch"))
                    .and_then(toml::Value::as_str),
            ),
        ] {
            if declared.is_some_and(|declared| declared != trusted_default_branch) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy default branch in {source} does not match trusted default branch {trusted_default_branch}"
                )));
            }
        }
        trusted_default_branch.to_owned()
    } else {
        declared_default_branch
    };
    let policy = VelnorPolicyContract {
        providers,
        automatic_providers,
        selectors,
        default_branch,
        repository,
    };
    policy.validate()?;
    Ok(policy)
}

fn valid_repository_slug(value: &str) -> bool {
    let mut segments = value.split('/');
    let (Some(owner), Some(repository), None) = (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    [owner, repository].into_iter().all(|segment| {
        !segment.is_empty()
            && !segment.starts_with('.')
            && !Path::new(segment)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("git"))
            && segment.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '.' | '_')
            })
    })
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
        for provider in self.selectors.keys() {
            if !matches!(
                provider.as_str(),
                "github-hosted" | "github-self-hosted" | "velnor"
            ) {
                return Err(GeneratorError::usage(format!(
                    "workflow policy found unknown provider `{provider}` in configured selectors"
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
            if provider == "github-hosted"
                && (selector.len() != 1
                    || !selector.iter().all(|label| is_github_owned_label(label)))
            {
                return Err(GeneratorError::usage(
                    "workflow policy requires github-hosted selectors to be one static GitHub-hosted image label",
                ));
            }
        }
        let mut claimed = BTreeMap::<String, &str>::new();
        for (provider, selector) in &self.selectors {
            let mut own = BTreeSet::new();
            for label in selector {
                let normalized = label.to_ascii_lowercase();
                if !own.insert(normalized.clone()) {
                    return Err(GeneratorError::usage(format!(
                        "workflow policy found repeated runner label `{label}` in provider `{provider}`"
                    )));
                }
                if let Some(owner) = claimed.get(&normalized) {
                    return Err(GeneratorError::usage(format!(
                        "workflow policy runner label `{label}` is shared by providers `{owner}` and `{provider}`"
                    )));
                }
                claimed.insert(normalized, provider.as_str());
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

/// Whether `value` is the generated admission for `provider`: a repository
/// identity guard and the provider's exact automatic-admission contract. The
/// reusable provider/unit selector prefix is stripped first; the remaining
/// expression must be exactly the default-branch push or `false`.
fn is_generated_provider_gate(
    value: &str,
    provider: &str,
    contract: &VelnorPolicyContract,
) -> bool {
    let Some(repository) = contract.repository.as_deref() else {
        return false;
    };
    let normalized = normalize_gate_expression(value);
    let selected = strip_reusable_unit_selector(&normalized).unwrap_or(&normalized);
    let Some(value) = strip_repository_identity_guard(selected, repository) else {
        return false;
    };
    let automatic = contract
        .automatic_providers
        .iter()
        .any(|automatic| automatic == provider);
    let expected = if automatic {
        format!(
            "github.event_name=='push'&&github.ref=='refs/heads/{}'",
            contract.default_branch
        )
    } else {
        "false".to_owned()
    };
    value == expected || value == format!("({expected})")
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
            // A runner group is caller-managed routing. Require a generated
            // local-provider gate even when its labels happen to resemble a
            // hosted image; labels can be custom on self-hosted runners.
            let mut result = RunnerAnalysis {
                local_provider: true,
                ..RunnerAnalysis::default()
            };
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

#[cfg(test)]
fn has_trusted_runner_gate(value: &str, contract: &VelnorPolicyContract) -> bool {
    let Some(repository) = contract.repository.as_deref() else {
        return false;
    };
    let normalized = normalize_gate_expression(value);
    let selected = strip_reusable_unit_selector(&normalized).unwrap_or(&normalized);
    let Some(value) = strip_repository_identity_guard(selected, repository) else {
        return false;
    };
    let expected = format!(
        "github.event_name=='push'&&github.ref=='refs/heads/{}'",
        contract.default_branch
    );
    value == expected || value == format!("({expected})")
}

fn strip_repository_identity_guard<'a>(value: &'a str, repository: &str) -> Option<&'a str> {
    let guard = format!("github.repository=='{repository}'&&");
    value.strip_prefix(&guard)
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
mod safe_root_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn scratch(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "velnor-policy-{name}-{}-{}",
            std::process::id(),
            crate::unique_suffix()
        ));
        fs::create_dir(&path).expect("create policy scratch directory");
        path
    }

    #[test]
    fn safe_policy_read_stays_on_captured_root_after_path_replacement() {
        let parent = scratch("captured-root");
        let selected = parent.join("selected");
        let old = parent.join("selected-old");
        fs::create_dir(&selected).expect("create selected root");
        fs::create_dir_all(selected.join(".github/ci")).expect("create config directory");
        fs::write(
            selected.join(RUNTIME_CONFIG),
            "repository = 'original/repository'\n",
        )
        .expect("write original config");

        let captured =
            super::super::safe_fs::SafeRoot::open(&selected).expect("capture selected root");
        fs::rename(&selected, &old).expect("move captured root");
        fs::create_dir(&selected).expect("replace selected root");
        fs::create_dir_all(selected.join(".github/ci")).expect("create replacement config dir");
        fs::write(
            selected.join(RUNTIME_CONFIG),
            "repository = 'replacement/repository'\n",
        )
        .expect("write replacement config");

        let content =
            read_text_with_safe_root(&captured, Path::new(RUNTIME_CONFIG), "read workflow config")
                .expect("read captured config")
                .expect("captured config exists");
        assert!(content.contains("original/repository"));
        assert!(!content.contains("replacement/repository"));
        assert!(captured.validate_root_binding().is_err());

        drop(captured);
        fs::remove_dir_all(parent).expect("remove policy scratch directory");
    }

    #[test]
    fn safe_policy_directory_walk_rejects_symlinked_workflow_directory() {
        let parent = scratch("workflow-symlink");
        let selected = parent.join("selected");
        let outside = parent.join("outside");
        fs::create_dir_all(selected.join(".github")).expect("create selected workflow parent");
        fs::create_dir(&outside).expect("create outside workflow directory");
        fs::write(outside.join("ci-policy.yml"), "outside: true\n")
            .expect("write outside workflow");
        symlink(&outside, selected.join(".github/workflows"))
            .expect("link selected workflows to outside");

        let captured =
            super::super::safe_fs::SafeRoot::open(&selected).expect("capture selected root");
        assert!(captured
            .open_directory(Path::new(".github/workflows"))
            .is_err());

        drop(captured);
        fs::remove_dir_all(parent).expect("remove policy scratch directory");
    }

    #[test]
    fn dispatch_root_uses_the_policy_argument_parser() {
        let split = workflow_root_for_dispatch(&[
            OsString::from("--workflow-root"),
            OsString::from("audited-tree"),
        ])
        .expect("parse split workflow root");
        let equals = workflow_root_for_dispatch(&[OsString::from("--workflow-root=audited-tree")])
            .expect("parse equals workflow root");
        assert_eq!(split, PathBuf::from("audited-tree"));
        assert_eq!(equals, PathBuf::from("audited-tree"));
        assert!(matches!(
            workflow_root_for_dispatch(&[
                OsString::from("--workflow-root"),
                OsString::from("audited-tree"),
                OsString::from("--pin-build"),
            ]),
            Err(error) if error.to_string().contains("unsupported policy option: --pin-build")
        ));
        assert!(workflow_root_for_dispatch(&[
            OsString::from("--candidate-manifest"),
            OsString::from("candidate.json"),
        ])
        .is_err());
    }
}

#[cfg(test)]
mod tests;

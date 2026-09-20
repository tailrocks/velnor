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
//! Candidate artifacts are not accepted: an artifact and its self-authored
//! digest do not prove that it was built from the audited source. Until the
//! base Stage-0 validator can rebuild that source inside a trusted sandbox,
//! a tree that differs from its declared pin fails closed.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
/// [--base-revision SHA] [--ruleset-contexts a,b] [--pin-build]`.
///
/// `--pin-build` is a local-development and bootstrap escape hatch: without
/// it an unprovisioned pin fails closed instead of compiling from source.
/// # Errors
/// Usage errors, unreadable inputs, and a failed evaluation (the rendered
/// report is printed first; the error names the failing rules).
pub(crate) fn run_cli(arguments: &[OsString]) -> Result<(), GeneratorError> {
    run_cli_with_safe_root(arguments, None)
}

/// Policy dispatch passes the descriptor that proved the selected root's
/// schema. Keep it through evaluation so a later path replacement cannot
/// redirect reads or Git subprocesses to another checkout.
pub(crate) fn run_cli_with_safe_root(
    arguments: &[OsString],
    safe_root: Option<super::safe_fs::SafeRoot>,
) -> Result<(), GeneratorError> {
    let parsed = parse_policy_arguments(arguments)?;
    let root = resolve_workflow_root(&parsed.values)?;
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
    let report = evaluate_with_safe_root(
        &PolicyOptions {
            root,
            head_sha: parsed.values.get("head-sha").cloned(),
            base_sha: parsed.values.get("base-sha").cloned(),
            base_revision,
            ruleset_contexts,
            build_pin: parsed.build_pin,
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
    build_pin: bool,
}

fn parse_policy_arguments(arguments: &[OsString]) -> Result<ParsedPolicyArguments, GeneratorError> {
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
    Ok(ParsedPolicyArguments {
        values: options,
        build_pin,
    })
}

fn resolve_workflow_root(options: &BTreeMap<String, String>) -> Result<PathBuf, GeneratorError> {
    Ok(options
        .get("workflow-root")
        .map(PathBuf::from)
        .or_else(|| env::var_os("WORKFLOW_ROOT").map(PathBuf::from))
        .or_else(|| env::var_os("GITHUB_WORKSPACE").map(PathBuf::from))
        // Keep the process CWD as a descriptor-resolved root. Converting it
        // to an absolute path here and opening that path during dispatch
        // would allow a rename-and-replace race to select a different tree.
        .unwrap_or_else(|| PathBuf::from(".")))
}

/// Resolve with the policy parser and precedence so runtime schema probing
/// cannot choose a different tree from `run_cli`.
pub(crate) fn workflow_root_for_dispatch(
    arguments: &[OsString],
) -> Result<PathBuf, GeneratorError> {
    let parsed = parse_policy_arguments(arguments)?;
    resolve_workflow_root(&parsed.values)
}

/// Evaluate every rule against `options.root`.
///
/// # Errors
/// Only when an input cannot be read or a tool cannot run; policy violations
/// are `FAIL` rules in the returned report.
pub(crate) fn evaluate(options: &PolicyOptions) -> Result<PolicyReport, GeneratorError> {
    evaluate_with_safe_root(options, None)
}

fn evaluate_with_safe_root(
    options: &PolicyOptions,
    captured_root: Option<super::safe_fs::SafeRoot>,
) -> Result<PolicyReport, GeneratorError> {
    let safe_root = match captured_root {
        Some(root) => root,
        None => super::safe_fs::SafeRoot::open(&options.root)?,
    };
    safe_root.validate_root_binding()?;
    let result = (|| {
        let resolved = options.root.canonicalize().map_err(|error| {
            GeneratorError::io("canonicalize workflow root", &options.root, &error)
        })?;
        if resolved != safe_root.command_directory() {
            return Err(GeneratorError::usage(format!(
                "workflow root changed after schema dispatch: selected {}, captured {}",
                resolved.display(),
                safe_root.command_directory().display()
            )));
        }
        let declared = DeclaredTree::read_with_safe_root(&safe_root)?;
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

/// `pin-reachable`, `pin-monotonic`, `entrypoint-pin`, `generated-tree`: is
/// the tree what the generator it names renders, and is that generator one
/// the base branch may adopt?
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
            "generated-tree",
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
    report.rules.push(entrypoint_pin_with_safe_root(root, pin));
    let lookup = PinnedBinaryLookup::from_env(pin, options.build_pin);
    let comparison = regenerate_and_compare_with_safe_roots(
        root,
        root,
        pin,
        &declared.default_branch,
        &lookup,
        &source,
    );
    if let Err(error) = root.validate_root_binding() {
        report.rules.push(RuleReport::fail(
            "generated-tree",
            format!("could not regenerate the tree with velnor-workflow at {pin}"),
            vec![error.to_string()],
        ));
    } else {
        report.rules.push(generated_tree_report(pin, comparison));
    }
}

/// Report the `generated-tree` verdict.
fn generated_tree_report(
    pin: &str,
    comparison: Result<TreeComparison, GeneratorError>,
) -> RuleReport {
    match comparison {
        Ok(TreeComparison::Pin) => RuleReport::pass(
            "generated-tree",
            format!(
                "every generated file is byte-identical to the render of velnor-workflow at {pin}"
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
fn semantic_rules_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    declared: &DeclaredTree,
    options: &PolicyOptions,
    report: &mut PolicyReport,
) -> Result<(), GeneratorError> {
    let audit = audit_workflows_with_safe_root(root)?;
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
    let generation = config::require_with_safe_root(checkout)?;
    let source = pin_source(checkout.command_directory(), generation.repository());
    let lookup = PinnedBinaryLookup::from_env(pin, build_pin);
    let comparison = regenerate_and_compare_with_safe_roots(
        checkout,
        output_root,
        pin,
        &config.default_branch,
        &lookup,
        &source,
    )?;
    checkout.validate_root_binding()?;
    output_root.validate_root_binding()?;
    match comparison {
        TreeComparison::Pin => Ok(()),
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
    velnor_policy: VelnorPolicyContract,
    /// Ruleset contexts `ci-pr.yml` must emit as job display names.
    required_checks: Vec<String>,
    /// Ruleset contexts reported by GitHub Apps rather than workflows.
    external_checks: Vec<String>,
}

impl DeclaredTree {
    fn read_with_safe_root(root: &super::safe_fs::SafeRoot) -> Result<Self, GeneratorError> {
        let generation = config::require_with_safe_root(root)?;
        let pin = generation
            .revision()
            .filter(|revision| super::is_full_revision(revision))
            .map(|revision| DeclaredPin::Config(revision.to_owned()));
        let pin = match pin {
            Some(pin) => Some(pin),
            None => entrypoint_policy_revision_with_safe_root(root)?.map(DeclaredPin::Entrypoint),
        };
        let repository = generation.repository().map(str::to_owned);
        let velnor_policy = configured_velnor_policy_with_generation(root, &generation)?;
        let configured_required_checks = generation.ruleset_required_status_checks();
        let required_checks = if configured_required_checks.is_empty() {
            if generation.ci_required().unwrap_or(true) {
                vec!["ci-required".to_owned()]
            } else {
                Vec::new()
            }
        } else {
            configured_required_checks.to_vec()
        };
        let external_checks = generation.ruleset_external_status_checks().to_vec();
        Ok(Self {
            pin,
            repository,
            default_branch: velnor_policy.default_branch.clone(),
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

// ---------------------------------------------------------------------------
// Git facts
// ---------------------------------------------------------------------------

trait PolicyGitRoot {
    fn git_output(&self, arguments: &[&str]) -> Result<Output, GeneratorError>;
}

impl PolicyGitRoot for Path {
    fn git_output(&self, arguments: &[&str]) -> Result<Output, GeneratorError> {
        let mut command = Command::new("git");
        clear_root_handoff_environment(&mut command);
        command
            .arg("-C")
            .arg(self)
            .args(arguments)
            .output()
            .map_err(|error| {
                GeneratorError::usage(format!("run git {}: {error}", arguments.join(" ")))
            })
    }
}

fn clear_root_handoff_environment(command: &mut Command) {
    command
        .env_remove(super::safe_fs::pinned_command::OUTPUT_ROOT_FD_ENV)
        .env_remove(super::safe_fs::pinned_command::SOURCE_ROOT_FD_ENV)
        .env_remove(super::safe_fs::pinned_command::SOURCE_ROOT_PATH_ENV);
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

fn git_with_safe_root(
    root: &super::safe_fs::SafeRoot,
    arguments: &[&str],
) -> Result<Option<String>, GeneratorError> {
    let output =
        super::safe_fs::pinned_command::output(root, "git", arguments).map_err(|error| {
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
fn pin_monotonic<R: PolicyGitRoot + ?Sized>(
    root: &R,
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
    entrypoint_pin_content(&content, pin)
}

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

/// An executable opened once and retained from content proof through render.
/// `path` is normalized for diagnostics only; every probe and render launches
/// through the retained descriptor.
#[derive(Debug)]
pub(crate) struct PinnedExecutable {
    path: PathBuf,
    file: fs::File,
    version: ExecutableVersion,
}

impl PinnedExecutable {
    fn open(path: &Path) -> Result<Self, String> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            env::current_dir()
                .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?
                .join(path)
        };
        let normalized = absolute.canonicalize().map_err(|error| {
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
        Self::from_open_file(normalized, descriptor.into())
    }

    /// Open the kernel's reference to the running image where the platform
    /// exposes it. A path from `current_exe()` alone does not prove identity.
    fn running() -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        {
            let file = fs::File::open("/proc/self/exe")
                .map_err(|error| format!("cannot open the running executable handle: {error}"))?;
            let path = match env::current_exe() {
                Ok(path) => path,
                Err(_) => PathBuf::from("/proc/self/exe"),
            };
            Self::from_open_file(path, file)
        }

        #[cfg(not(target_os = "linux"))]
        {
            Err("the running executable's inode cannot be proved on this platform".to_owned())
        }
    }

    fn from_open_file(path: PathBuf, file: fs::File) -> Result<Self, String> {
        let metadata = file.metadata().map_err(|error| {
            format!(
                "{}: cannot inspect opened executable: {error}",
                path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "{}: opened executable is not a regular file",
                path.display()
            ));
        }
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "{}: opened file has no executable permission",
                path.display()
            ));
        }
        Ok(Self {
            path,
            file,
            version: ExecutableVersion::from_metadata(&metadata),
        })
    }

    fn identity(&self) -> ExecutableIdentity {
        self.version.identity
    }

    fn is_same_file(&self, other: &Self) -> bool {
        self.identity() == other.identity()
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

    fn output<I, A>(&self, args: I) -> Result<Output, String>
    where
        I: IntoIterator<Item = A>,
        A: AsRef<std::ffi::OsStr>,
    {
        self.ensure_unchanged("before execution")?;
        let output = super::safe_fs::pinned_command::output_executable(&self.file, args).map_err(
            |error| {
                format!(
                    "{}: cannot execute the bound descriptor: {error}",
                    self.path.display()
                )
            },
        )?;
        self.ensure_unchanged("during execution")?;
        Ok(output)
    }

    fn output_in_safe_root<I, A>(
        &self,
        root: &super::safe_fs::SafeRoot,
        args: I,
    ) -> Result<Output, String>
    where
        I: IntoIterator<Item = A>,
        A: AsRef<std::ffi::OsStr>,
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
        A: AsRef<std::ffi::OsStr>,
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

impl PartialEq<PathBuf> for PinnedExecutable {
    fn eq(&self, other: &PathBuf) -> bool {
        self.path == *other
    }
}

/// The source-closure digest a `velnor-workflow` binary reports for itself.
fn binary_closure(binary: &PinnedExecutable) -> Result<String, String> {
    binary_report(binary, "--closure")
}

fn binary_closure_with_safe_root(
    binary: &PinnedExecutable,
    root: &super::safe_fs::SafeRoot,
) -> Result<String, String> {
    binary_report_with_safe_root(binary, "--closure", root)
}

fn binary_report(binary: &PinnedExecutable, flag: &str) -> Result<String, String> {
    binary_report_with_root(binary, flag, None)
}

fn binary_report_with_safe_root(
    binary: &PinnedExecutable,
    flag: &str,
    root: &super::safe_fs::SafeRoot,
) -> Result<String, String> {
    binary_report_with_root(binary, flag, Some(root))
}

fn binary_report_with_root(
    binary: &PinnedExecutable,
    flag: &str,
    root: Option<&super::safe_fs::SafeRoot>,
) -> Result<String, String> {
    let output = match root {
        Some(root) => binary.output_in_safe_root(root, [flag])?,
        None => binary.output([flag])?,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{}: `{flag}` failed ({}): {}",
            binary.path.display(),
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Closures a renderer for `pin` (a commit of `repo`) must report: the lean
/// release and debug CI products.
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
    ])
}

fn expected_remote_closure(
    pin: &str,
    lookup: &PinnedBinaryLookup,
) -> Result<String, GeneratorError> {
    let closure = lookup
        .trusted_closure
        .as_deref()
        .or_else(|| (SOURCE_REVISION == pin).then_some(SOURCE_CLOSURE));
    let Some(closure) = closure.filter(|closure| closure_identity::is_full_closure(closure)) else {
        return Err(GeneratorError::usage(format!(
            "no trusted closure is available for remote pin {pin}; the trusted setup action must export {}",
            crate::VELNOR_WORKFLOW_PINNED_CLOSURE_ENV
        )));
    };
    Ok(closure.to_owned())
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
    ])
}

/// The process environment the pinned-binary lookup reads, captured once so
/// tests can drive the resolver without mutating global state.
pub(crate) struct PinnedBinaryLookup {
    /// [`VELNOR_WORKFLOW_PINNED_BINARY_ENV`].
    pinned_binary: Option<PathBuf>,
    /// Closure output from the trusted setup action that verified the pinned
    /// release manifest. Required when the audited checkout lacks the pin.
    trusted_closure: Option<String>,
    /// `PATH`.
    search_path: Option<OsString>,
    /// Where an earlier resolution built the pin.
    install_root: PathBuf,
    /// Never build without an explicit `--pin-build`, or under
    /// `CARGO_NET_OFFLINE=true`.
    build_forbidden: bool,
}

impl PinnedBinaryLookup {
    pub(crate) fn from_env(revision: &str, build_pin: bool) -> Self {
        Self {
            pinned_binary: env::var_os(VELNOR_WORKFLOW_PINNED_BINARY_ENV).map(PathBuf::from),
            trusted_closure: env::var(crate::VELNOR_WORKFLOW_PINNED_CLOSURE_ENV).ok(),
            search_path: env::var_os("PATH"),
            install_root: policy_install_root(revision),
            build_forbidden: !build_pin
                || env::var("CARGO_NET_OFFLINE").is_ok_and(|value| value == "true"),
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
/// `expected` holds the pin's closures ([`expected_closures`]) when the pin is
/// in the audited history, or the closure reported by the trusted setup action
/// when it is not. A renderer cannot establish its own trust by reporting a
/// matching revision and a wellformed digest.
///
/// Fails closed with every attempt listed when none of these yields the pin.
pub(crate) fn resolve_pinned_binary(
    revision: &str,
    expected: Option<&[String]>,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
) -> Result<PinnedExecutable, GeneratorError> {
    resolve_pinned_binary_inner(revision, expected, lookup, source, None)
}

fn resolve_pinned_binary_with_safe_root(
    revision: &str,
    expected: Option<&[String]>,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
    checkout_root: &super::safe_fs::SafeRoot,
) -> Result<PinnedExecutable, GeneratorError> {
    resolve_pinned_binary_inner(revision, expected, lookup, source, Some(checkout_root))
}

#[cfg(target_os = "linux")]
fn resolve_pinned_binary_inner(
    revision: &str,
    expected: Option<&[String]>,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
    checkout_root: Option<&super::safe_fs::SafeRoot>,
) -> Result<PinnedExecutable, GeneratorError> {
    match expected {
        Some(set) => {
            if set.contains(&SOURCE_CLOSURE.to_owned())
                && let Ok(current) = PinnedExecutable::running()
            {
                return Ok(current);
            }
        }
        None => {
            if SOURCE_REVISION == revision
                && closure_identity::is_full_closure(SOURCE_CLOSURE)
                && let Ok(current) = PinnedExecutable::running()
            {
                return Ok(current);
            }
        }
    }
    let mut attempts = Vec::new();
    if let Some(binary) = lookup.pinned_binary.clone() {
        return match PinnedExecutable::open(&binary).and_then(|opened| {
            prove_candidate_with_root(&opened, expected, checkout_root).map(|()| opened)
        }) {
            Ok(opened) => Ok(opened),
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
        match PinnedExecutable::open(&candidate).and_then(|opened| {
            prove_candidate_with_root(&opened, expected, checkout_root).map(|()| opened)
        }) {
            Ok(opened) => return Ok(opened),
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
    build_pinned_binary(
        revision,
        expected,
        source,
        install_root,
        &installed,
        checkout_root,
    )
}

#[cfg(not(target_os = "linux"))]
fn resolve_pinned_binary_inner(
    revision: &str,
    expected: Option<&[String]>,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
    checkout_root: Option<&super::safe_fs::SafeRoot>,
) -> Result<PinnedExecutable, GeneratorError> {
    let _ = (revision, expected, lookup, source, checkout_root);
    Err(GeneratorError::usage(
        "descriptor-bound renderer execution is unavailable on this platform; refusing pinned policy verification",
    ))
}

fn prove_candidate_with_root(
    binary: &PinnedExecutable,
    expected: Option<&[String]>,
    root: Option<&super::safe_fs::SafeRoot>,
) -> Result<(), String> {
    let expected = expected.ok_or_else(|| {
        "no trusted closure is available for this pin; refusing renderer self-attestation"
            .to_owned()
    })?;
    let reported = match root {
        Some(root) => binary_closure_with_safe_root(binary, root)?,
        None => binary_closure(binary)?,
    };
    if expected.contains(&reported) {
        Ok(())
    } else {
        Err(format!(
            "reports closure {reported}, which is not the trusted pin closure"
        ))
    }
}

/// Build the lean `velnor-workflow` product at `revision` with
/// `cargo install --locked --no-default-features` and every cache wrapper
/// and caller flag removed, so the build never touches a shared store. The
/// generator's own repository builds from a local clone of the audited
/// checkout (hardlinked objects) checked out at the pin; a consumer installs
/// from the generator's git URL. The result must prove the pin using the same
/// trusted source closure as a provisioned renderer.
fn build_pinned_binary(
    revision: &str,
    expected: Option<&[String]>,
    source: &PinSource,
    install_root: &Path,
    installed: &Path,
    checkout_root: Option<&super::safe_fs::SafeRoot>,
) -> Result<PinnedExecutable, GeneratorError> {
    let install_root = if install_root.is_absolute() {
        install_root.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| {
                GeneratorError::usage(format!(
                    "resolve pinned generator install root {}: {error}",
                    install_root.display()
                ))
            })?
            .join(install_root)
    };
    fs::create_dir_all(&install_root).map_err(|error| {
        GeneratorError::usage(format!(
            "prepare pinned generator root at {}: {error}",
            install_root.display()
        ))
    })?;
    let mut command = Command::new("cargo");
    clear_root_handoff_environment(&mut command);
    command.args(["install", "--locked", "--no-default-features", "--force"]);
    match source {
        PinSource::Checkout(checkout) => {
            let clone = install_root.join("source");
            if clone.exists() {
                fs::remove_dir_all(&clone).map_err(|error| {
                    GeneratorError::io("remove stale pinned source clone", &clone, &error)
                })?;
            }
            let output = if let Some(checkout_root) = checkout_root {
                use std::ffi::OsStr;

                super::safe_fs::pinned_command::output(
                    checkout_root,
                    "git",
                    [
                        OsStr::new("clone"),
                        OsStr::new("--quiet"),
                        OsStr::new("--no-checkout"),
                        OsStr::new("."),
                        clone.as_os_str(),
                    ],
                )
                .map_err(|error| {
                    GeneratorError::usage(format!("clone the audited checkout: {error}"))
                })?
            } else {
                let mut git = Command::new("git");
                clear_root_handoff_environment(&mut git);
                git.arg("clone")
                    .arg("--quiet")
                    .arg("--no-checkout")
                    .arg(checkout)
                    .arg(&clone)
                    .output()
                    .map_err(|error| {
                        GeneratorError::usage(format!("clone the audited checkout: {error}"))
                    })?
            };
            let status = output.status;
            if !status.success() {
                return Err(GeneratorError::usage(format!(
                    "clone the audited checkout {} for the pinned build failed",
                    checkout.display()
                )));
            }
            let clone_root = checkout_root
                .map(|_| super::safe_fs::SafeRoot::open(&clone))
                .transpose()?;
            let output = if let Some(clone_root) = clone_root.as_ref() {
                super::safe_fs::pinned_command::output(
                    clone_root,
                    "git",
                    ["checkout", "--quiet", "--detach", revision],
                )
                .map_err(|error| {
                    GeneratorError::usage(format!("check out pin {revision}: {error}"))
                })?
            } else {
                let mut git = Command::new("git");
                clear_root_handoff_environment(&mut git);
                git.arg("-C")
                    .arg(&clone)
                    .args(["checkout", "--quiet", "--detach", revision])
                    .output()
                    .map_err(|error| {
                        GeneratorError::usage(format!("check out pin {revision}: {error}"))
                    })?
            };
            let status = output.status;
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
        .arg(&install_root)
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
    let status = if let Some(checkout_root) = checkout_root {
        super::safe_fs::pinned_command::spawn(checkout_root, command)
            .and_then(|mut child| child.wait())
    } else {
        let current_directory = env::current_dir().map_err(|error| {
            GeneratorError::usage(format!("resolve pinned generator build directory: {error}"))
        })?;
        command
            .env("PWD", &current_directory)
            .env("GITHUB_WORKSPACE", &current_directory)
            .status()
    }
    .map_err(|error| {
        GeneratorError::usage(format!("build velnor-workflow at pin {revision}: {error}"))
    })?;
    if !status.success() {
        return Err(GeneratorError::usage(format!(
            "build velnor-workflow at pin {revision} failed"
        )));
    }
    let opened = PinnedExecutable::open(installed).map_err(|detail| {
        GeneratorError::usage(format!(
            "velnor-workflow built for pin {revision} at {} is unusable: {detail}",
            installed.display()
        ))
    })?;
    match prove_candidate_with_root(&opened, expected, checkout_root) {
        Ok(()) => Ok(opened),
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
    Ok(path)
}

struct BoundScratchDirectory {
    parent: super::safe_fs::SafeRoot,
    name: OsString,
    path: PathBuf,
    root: super::safe_fs::SafeRoot,
    identity: super::safe_fs::SafeRootIdentity,
}

struct BoundScratchChild {
    path: PathBuf,
    root: super::safe_fs::SafeRoot,
}

impl BoundScratchDirectory {
    fn create(label: &str) -> Result<Self, GeneratorError> {
        let base = env::var_os("RUNNER_TEMP")
            .or_else(|| env::var_os("TMPDIR"))
            .map_or_else(env::temp_dir, PathBuf::from);
        let parent = super::safe_fs::SafeRoot::open(&base)?;
        let name = OsString::from(format!(
            "velnor-workflow-{label}-{}",
            super::unique_suffix()
        ));
        Self::create_under_parent(parent, name)
    }

    fn create_under_parent(
        parent: super::safe_fs::SafeRoot,
        name: OsString,
    ) -> Result<Self, GeneratorError> {
        let path = parent.command_directory().join(&name);
        let root = parent.create_directory_child(&name)?;
        let identity = root.identity()?;
        Ok(Self {
            parent,
            name,
            path,
            root,
            identity,
        })
    }

    fn create_child(&self, name: &str, mode: u16) -> Result<BoundScratchChild, GeneratorError> {
        let root = self
            .root
            .create_directory_child(std::ffi::OsStr::new(name))?;
        root.set_mode(mode)?;
        Ok(BoundScratchChild {
            path: self.path.join(name),
            root,
        })
    }

    fn cleanup(self) -> Result<(), GeneratorError> {
        // Do not treat a missing or rebound display name as successful
        // cleanup. The parent operation repeats the identity check before
        // deleting the name-relative tree.
        self.root.validate_root_binding()?;
        self.parent
            .remove_named_tree_if_matches(&self.name, &self.identity)
    }
}

/// What the regeneration comparison established about the tree.
pub(crate) enum TreeComparison {
    /// The tree is byte-identical to the declared pin's render.
    Pin,
    /// Neither renderer reproduces the tree.
    Differences(Vec<String>),
}

/// Scan the checkout with the generator built at the declared pin, render into
/// a scratch directory, and return every path under the tree that differs.
/// Every rendered file must match byte for byte, and every workflow must
/// appear in rendered output.
///
/// # Errors
/// When the pinned generator cannot be obtained or its render fails.
pub(crate) fn regenerate_and_compare(
    checkout: &Path,
    tree: &Path,
    pin: &str,
    default_branch: &str,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
) -> Result<TreeComparison, GeneratorError> {
    let checkout_root = super::safe_fs::SafeRoot::open(checkout)?;
    let tree_root = super::safe_fs::SafeRoot::open(tree)?;
    regenerate_and_compare_with_safe_roots(
        &checkout_root,
        &tree_root,
        pin,
        default_branch,
        lookup,
        source,
    )
}

/// D19's CLI check and runtime policy keep the checkout and compared output
/// rooted in handles captured before generation. Path-based
/// `regenerate_and_compare` remains for callers that own their path boundary.
fn regenerate_and_compare_with_safe_roots(
    checkout: &super::safe_fs::SafeRoot,
    tree: &super::safe_fs::SafeRoot,
    pin: &str,
    default_branch: &str,
    lookup: &PinnedBinaryLookup,
    source: &PinSource,
) -> Result<TreeComparison, GeneratorError> {
    let expected = match source {
        PinSource::Checkout(_) => Some(expected_closures_with_safe_root(checkout, pin)?),
        PinSource::Remote(_) => Some(vec![expected_remote_closure(pin, lookup)?]),
    };
    let binary =
        resolve_pinned_binary_with_safe_root(pin, expected.as_deref(), lookup, source, checkout)?;
    let scratch = BoundScratchDirectory::create("policy-render")?;
    let verdict = (|| {
        let pin_scratch = scratch.create_child("pin-render", 0o700)?;
        render_and_compare_with_safe_roots(
            &binary,
            checkout,
            tree,
            &pin_scratch.path,
            &pin_scratch.root,
            default_branch,
        )
        .map(|differences| {
            if differences.is_empty() {
                TreeComparison::Pin
            } else {
                TreeComparison::Differences(differences)
            }
        })
    })();
    scratch.cleanup()?;
    verdict
}

fn render_and_compare_with_safe_roots(
    binary: &PinnedExecutable,
    checkout: &super::safe_fs::SafeRoot,
    root: &super::safe_fs::SafeRoot,
    scratch: &Path,
    scratch_root: &super::safe_fs::SafeRoot,
    default_branch: &str,
) -> Result<Vec<String>, GeneratorError> {
    let output = binary
        .render_with_safe_roots(
            checkout,
            scratch_root,
            [
                std::ffi::OsStr::new("."),
                std::ffi::OsStr::new("--output"),
                scratch.as_os_str(),
                std::ffi::OsStr::new("--plain"),
                std::ffi::OsStr::new("--force"),
                std::ffi::OsStr::new("--default-branch"),
                std::ffi::OsStr::new(default_branch),
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
    require_minimum_render_inventory(&rendered)?;
    compare_rendered_tree_with_safe_root(root, &rendered)
}

/// Both generator backends always render these fixed artifacts. Requiring
/// them independently of the target tree prevents an empty or partial scratch
/// tree from proving the pin.
const MINIMUM_RENDER_OUTPUTS: [&str; 3] = [
    ".github/actionlint.yaml",
    ".github/ci/project.toml",
    "config/fleet/velnor-host.env",
];

fn require_minimum_render_inventory(
    rendered: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), GeneratorError> {
    for required in MINIMUM_RENDER_OUTPUTS {
        if !rendered.contains_key(Path::new(required)) {
            return Err(GeneratorError::usage(format!(
                "pinned renderer omitted required output {required}; refusing policy verification"
            )));
        }
    }
    Ok(())
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
            let is_workflow_file = matches!(
                entry.kind,
                super::safe_fs::SafeEntryKind::File | super::safe_fs::SafeEntryKind::Symlink
            );
            if !is_workflow_file
                || !matches!(
                    Path::new(name).extension().and_then(|value| value.to_str()),
                    Some("yml" | "yaml")
                )
            {
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
        Ok(Some(yaml)) => match super::workflow_job_display_names(&yaml) {
            Ok(names) => Some(names),
            Err(error) => {
                findings.push(format!("{PULL_REQUEST_AGGREGATE}: {error}"));
                None
            }
        },
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
        // The entrypoint's own job is the gate this validator runs behind; a
        // ruleset that does not require it makes every rule here advisory.
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

/// Path-based test and caller wrapper. Runtime evaluation uses the captured
/// root variant above so the entrypoint is read from the schema-probed tree.
fn audit_policy_entrypoint(
    root: &Path,
    velnor_policy: &VelnorPolicyContract,
) -> Result<EntrypointAudit, GeneratorError> {
    let safe_root = super::safe_fs::SafeRoot::open(root)?;
    audit_policy_entrypoint_with_safe_root(&safe_root, velnor_policy)
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
/// `pull_request_target` rule here; [`audit_policy_entrypoint_with_safe_root`] judges it.
///
/// # Errors
/// When the workflow directory or a workflow file cannot be read.
pub(crate) fn audit_workflows(root: &Path) -> Result<WorkflowAudit, GeneratorError> {
    let safe_root = super::safe_fs::SafeRoot::open(root)?;
    audit_workflows_with_safe_root(&safe_root)
}

fn audit_workflows_with_safe_root(
    root: &super::safe_fs::SafeRoot,
) -> Result<WorkflowAudit, GeneratorError> {
    let workflow_directory = Path::new(".github/workflows");
    let workflow_path = root.command_directory().join(workflow_directory);
    let workflows = root.open_directory(workflow_directory)?.ok_or_else(|| {
        GeneratorError::io(
            "read workflow directory",
            &workflow_path,
            &std::io::Error::from(std::io::ErrorKind::NotFound),
        )
    })?;
    let policy_entrypoint = workflow_path.join("ci-policy.yml");
    let velnor_policy = configured_velnor_policy_with_safe_root(root)?;
    let mut findings = PolicyFindings {
        root: root.command_directory().to_path_buf(),
        ..PolicyFindings::default()
    };
    // GitHub rejects a workflow whose YAML carries duplicate keys, so the
    // auditor must fail closed on exactly the inputs GitHub refuses instead of
    // silently auditing the last-key-wins rewrite of them.
    let parser = serde_yaml::ParserConfig::default()
        .duplicate_key_policy(serde_yaml::DuplicateKeyPolicy::Error);
    let mut paths = Vec::new();
    for entry in workflows.entries()? {
        if matches!(
            entry.kind,
            super::safe_fs::SafeEntryKind::File | super::safe_fs::SafeEntryKind::Symlink
        ) && matches!(
            Path::new(&entry.name)
                .extension()
                .and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        ) {
            paths.push(entry.name);
        }
    }
    paths.sort();
    for name in paths {
        let relative_path = workflow_directory.join(&name);
        let path = root.command_directory().join(&relative_path);
        let content =
            read_text_with_safe_root(root, &relative_path, "read workflow")?.ok_or_else(|| {
                GeneratorError::io(
                    "read workflow",
                    &path,
                    &std::io::Error::from(std::io::ErrorKind::NotFound),
                )
            })?;
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

fn configured_velnor_policy_with_safe_root(
    root: &super::safe_fs::SafeRoot,
) -> Result<VelnorPolicyContract, GeneratorError> {
    let generation = config::require_with_safe_root(root)?;
    configured_velnor_policy_with_generation(root, &generation)
}

fn configured_velnor_policy_with_generation(
    root: &super::safe_fs::SafeRoot,
    generation: &config::RepoGenerationConfig,
) -> Result<VelnorPolicyContract, GeneratorError> {
    let relative = Path::new(RUNTIME_CONFIG);
    let path = root.command_directory().join(relative);
    // Advisory sparse-checkout omits `.github/ci`. Do not default the
    // contract on a missing project.toml: the generation config still
    // carries providers and selectors.
    let runtime = match read_text_with_safe_root(root, relative, "read workflow config")? {
        Some(content) => Some(toml::from_str::<toml::Value>(&content).map_err(|error| {
            GeneratorError::usage(format!("parse workflow config {}: {error}", path.display()))
        })?),
        None => None,
    };
    let mut providers = runtime
        .as_ref()
        .and_then(|value| value.get("providers"))
        .map(|value| toml_string_array(Some(value), "providers"))
        .transpose()?
        .unwrap_or_default();
    if providers.is_empty() {
        providers = generation.providers().unwrap_or_default().to_vec();
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
    for (provider, selector) in generation.selectors() {
        if !selector.runs_on.is_empty() {
            selectors.insert(provider.clone(), selector.runs_on.clone());
        }
    }
    let default_branch = runtime
        .as_ref()
        .and_then(|value| value.get("default_branch"))
        .and_then(toml::Value::as_str)
        .or_else(|| generation.default_branch())
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

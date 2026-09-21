//! Rust replacement for the generated CI, policy, and release shell helpers.
//!
//! The generator still records project commands as data in `project.toml`.
//! This module owns parsing, dependency ordering, affected-unit selection,
//! policy validation, and release packaging. Project commands are the only
//! shell boundary left: they are explicit repository inputs and run under the
//! same Bash contract GitHub Actions provides for `run` steps.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use sha2::{Digest, Sha256};

use super::primitives::prepared_tools::{
    classify_and_verify, fetch_producer_conclusion, format_failure_output, format_install_outputs,
    format_save_check_output, save_is_legal, ApiResponse, HandoffFailure, OutcomeFetchError,
    Resolution, ToolManifest, ToolRequest, TransferBounds, VerifiedBundle,
};
use super::primitives::snapshot::{
    budget_report, plan_evictions, CacheEntry as SnapshotCacheEntry, RetentionPolicy,
};
use super::{lanes_support_unit_kind, GeneratorError, RunnerMode, UnitKind, ValidationPhase};

const DEFAULT_CONFIG: &str = ".github/ci/project.toml";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

#[expect(
    dead_code,
    reason = "runtime preserves the complete generated TOML contract while commands consume only their relevant fields"
)]
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiConfig {
    schema: u32,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    profile: String,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    default_branch: String,
    #[serde(default)]
    runners: String,
    #[serde(default)]
    automatic: String,
    #[serde(default)]
    analysis: Analysis,
    #[serde(default)]
    workflow: Workflow,
    #[serde(default)]
    release: Release,
    #[serde(default)]
    unit: Vec<CiUnit>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Analysis {
    method: String,
    detected: Vec<String>,
    limitations: Vec<String>,
}

#[expect(
    dead_code,
    reason = "runtime preserves the complete generated workflow contract while execution consumes selected fields"
)]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Workflow {
    github_runner: String,
    #[serde(default)]
    macos_runner: String,
    velnor_labels: Vec<String>,
    files: Vec<String>,
    notes: Vec<String>,
    #[serde(default)]
    version_bump_units: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Release {
    enabled: bool,
    reason: String,
    kind: String,
    package: String,
    packages: Vec<String>,
    binary: String,
    targets: Vec<String>,
    image: String,
    source_repository: String,
    consumer_repository: String,
    artifact_path: String,
    description: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Cache {
    key_files: Vec<String>,
    paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CiUnit {
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    root: String,
    watch: Vec<String>,
    github_pr_commands: Vec<String>,
    github_full_commands: Vec<String>,
    velnor_pr_commands: Vec<String>,
    velnor_full_commands: Vec<String>,
    /// Positional phase tags over the command vectors, as the generator
    /// emitted them. Empty on units the scan left unphased and on TOML the
    /// phase model predates; those units run without `--phase`.
    #[serde(default)]
    phases: Vec<ValidationPhase>,
    /// The prerequisite-tier check commands the scan built. Empty exactly
    /// when `phases` is empty.
    #[serde(default)]
    check_commands: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    tool_version: Option<String>,
    #[serde(default)]
    cache: Option<Cache>,
    /// A workspace-wide Rust verification gate. Its watch paths may stay
    /// narrow; affected Rust changes select it explicitly to preserve the
    /// workspace coverage without broadening every topology match.
    #[serde(default)]
    workspace_check: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunnerLane {
    Github,
    Velnor,
}

impl RunnerLane {
    /// Velnor's job container advertises its authoritative execution backend;
    /// hosted GitHub jobs and local invocations deliberately fall back to the
    /// GitHub command set.
    fn from_execution_backend(backend: Option<&str>) -> Self {
        if backend.is_some_and(|value| {
            value.trim().eq_ignore_ascii_case("docker")
                || value.trim().eq_ignore_ascii_case("microvm")
        }) {
            Self::Velnor
        } else {
            Self::Github
        }
    }

    fn current() -> Self {
        Self::from_execution_backend(env::var("VELNOR_EXECUTION_BACKEND").ok().as_deref())
    }
}

impl CiUnit {
    fn commands(&self, lane: RunnerLane, scope: Scope) -> &[String] {
        match (lane, scope) {
            (RunnerLane::Github, Scope::Affected) => &self.github_pr_commands,
            (RunnerLane::Github, Scope::Full) => &self.github_full_commands,
            (RunnerLane::Velnor, Scope::Affected) => &self.velnor_pr_commands,
            (RunnerLane::Velnor, Scope::Full) => &self.velnor_full_commands,
        }
    }

    /// The unit's commands for one `--phase` selection: the check phase
    /// selects the stored prerequisite commands, a runnable phase selects
    /// the tagged commands positionally. Unphased units and tag/command
    /// misalignments fail closed; a phased unit that genuinely carries no
    /// command for the phase is a generator bug, not an empty selection.
    fn commands_for_phase(
        &self,
        lane: RunnerLane,
        scope: Scope,
        phase: ValidationPhase,
    ) -> Result<Vec<String>, GeneratorError> {
        if phase == ValidationPhase::Check {
            if self.phases.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "CI unit `{}` has no validation phases; run it without --phase",
                    self.id
                )));
            }
            if self.check_commands.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "CI unit `{}` carries validation phases without prerequisite check commands",
                    self.id
                )));
            }
            return Ok(self.check_commands_for_lane(lane));
        }
        if self.phases.is_empty() {
            return Err(GeneratorError::usage(format!(
                "CI unit `{}` has no validation phases; run it without --phase",
                self.id
            )));
        }
        let commands = self.commands(lane, scope);
        if commands.len() != self.phases.len() {
            return Err(GeneratorError::usage(format!(
                "CI unit `{}` carries {} validation phases for {} commands; refusing a misaligned --phase selection",
                self.id,
                self.phases.len(),
                commands.len()
            )));
        }
        let selected = commands
            .iter()
            .zip(self.phases.iter())
            .filter(|(_, candidate)| **candidate == phase)
            .map(|(command, _)| command.clone())
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err(GeneratorError::usage(format!(
                "CI unit `{}` has no {} phase commands",
                self.id,
                phase.as_str()
            )));
        }
        Ok(selected)
    }

    /// The stored prerequisite check commands for a lane. The Velnor lane
    /// drops Cargo's `--no-deps`: `mbx check` does not expose it. The flag
    /// adaptation is the preserved lane rule; phase selection itself never
    /// inspects command text.
    fn check_commands_for_lane(&self, lane: RunnerLane) -> Vec<String> {
        if lane == RunnerLane::Velnor {
            self.check_commands
                .iter()
                .map(|command| command.replace(" --no-deps", ""))
                .collect()
        } else {
            self.check_commands.clone()
        }
    }

    /// Whether affected Rust changes should also select this workspace gate.
    /// The generator keeps the flag in generation config only; pinned Planning
    /// runtimes infer the gate from the emitted `cargo check --workspace`
    /// command contract.
    fn is_workspace_check(&self) -> bool {
        if self.workspace_check {
            return true;
        }
        [
            &self.github_pr_commands,
            &self.github_full_commands,
            &self.velnor_pr_commands,
            &self.velnor_full_commands,
        ]
        .into_iter()
        .flatten()
        .any(|command| {
            command.contains("check --workspace --all-targets")
                && (command.contains("cargo check --workspace")
                    || command.contains("mbx check --workspace"))
        })
    }

    /// Return the Cargo lockfile root recorded by the generator metadata.
    ///
    /// Workspace members carry the repository-root `Cargo.lock` in their
    /// watch/cache contract, while an independent manifest tree carries its
    /// own `<root>/Cargo.lock`. Keep this derived from those serialized
    /// paths rather than from package names so the runtime follows the
    /// generator's lockfile discovery for every repository.
    fn cargo_lockfile_root(&self) -> &str {
        if self.root == "." {
            return ".";
        }
        let local_lockfile = format!("{}/Cargo.lock", self.root);
        let has_local_lockfile = self.watch.iter().any(|path| path == &local_lockfile)
            || self
                .cache
                .as_ref()
                .is_some_and(|cache| cache.key_files.iter().any(|path| path == &local_lockfile));
        if has_local_lockfile {
            &self.root
        } else {
            "."
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scope {
    Affected,
    Full,
}

impl Scope {
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "affected" => Ok(Self::Affected),
            "full" => Ok(Self::Full),
            _ => Err(GeneratorError::usage(format!(
                "unsupported CI scope: {value}"
            ))),
        }
    }
}

/// `velnor-workflow version [--json]`: the crate version and the source
/// revision stamped at build time (the same value `--revision` prints), so a
/// candidate binary can prove which commit it was built from.
fn print_version(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let json = match arguments {
        [] => false,
        [flag] if flag == "--json" => true,
        other => {
            return Err(GeneratorError::usage(format!(
                "version accepts only --json, got {}",
                other
                    .iter()
                    .map(|value| value.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            )));
        }
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "crate_version": env!("CARGO_PKG_VERSION"),
                "revision": crate::SOURCE_REVISION,
                "closure": crate::SOURCE_CLOSURE,
                "features": env!("VELNOR_WORKFLOW_FEATURES"),
            })
        );
    } else {
        println!(
            "velnor-workflow {} ({})",
            env!("CARGO_PKG_VERSION"),
            crate::SOURCE_REVISION
        );
    }
    Ok(())
}

/// `velnor-workflow closure --rev SHA [--profile release|debug] [--repo PATH] [--candidate]`:
/// print the source-closure digest of `SHA` in the repository at `--repo`
/// (the current directory by default). CI jobs use it to name the product
/// they need; policy resolves the same digest when it verifies.
/// `--candidate` names the unit job's own debug build (default features); it
/// cannot combine with `--profile`.
fn print_closure(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let (candidate, rest): (Vec<&OsString>, Vec<&OsString>) =
        arguments.iter().partition(|argument| {
            argument
                .to_str()
                .is_some_and(|value| value == "--candidate")
        });
    if candidate.len() > 1 {
        return Err(GeneratorError::usage(
            "duplicate option: --candidate".to_owned(),
        ));
    }
    let candidate = !candidate.is_empty();
    let rest: Vec<OsString> = rest.into_iter().cloned().collect();
    let options = parse_options(&rest, &["rev", "profile", "repo"])?;
    let rev = options
        .get("rev")
        .ok_or_else(|| GeneratorError::usage("closure requires --rev SHA".to_owned()))?;
    if !crate::is_full_revision(rev) {
        return Err(GeneratorError::usage(format!(
            "closure --rev must be a full 40-character commit SHA, got {rev:?}"
        )));
    }
    if candidate && options.contains_key("profile") {
        return Err(GeneratorError::usage(
            "closure --candidate cannot combine with --profile".to_owned(),
        ));
    }
    let profile = options
        .get("profile")
        .map_or(crate::closure::PROFILE_RELEASE, String::as_str);
    if !matches!(
        profile,
        crate::closure::PROFILE_RELEASE | crate::closure::PROFILE_DEBUG
    ) {
        return Err(GeneratorError::usage(format!(
            "closure --profile must be release or debug, got {profile:?}"
        )));
    }
    let repo = match options.get("repo") {
        Some(path) => PathBuf::from(path.as_str()),
        None => env::current_dir()
            .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?,
    };
    let digest = if candidate {
        crate::closure::candidate_closure_of_tree(&repo, rev)?
    } else {
        crate::closure::closure_of_tree(&repo, rev, crate::closure::CI_FEATURES, profile)?
    };
    println!("{digest}");
    Ok(())
}

/// `velnor-workflow promote --rev SHA|HEAD --publication-readiness PATH
/// [--repo PATH] [--generator-repo PATH] [--default-branch BRANCH]
/// [--runners MODE] [--message TEXT] [--dry-run]`:
/// stamp the D19 pin and regenerate the whole tree in one atomic commit. The
/// running binary must render with exactly the source closure the pin names
/// (render with X ⇒ stamp X), and the trusted publication-readiness handoff
/// must prove every supported renderer product exists for that exact identity;
/// anything else fails closed before touching the tree.
/// `--generator-repo` points fleet promotion at a product checkout holding the
/// pin's history; it defaults to the promoted repository itself.
/// `velnor-workflow visibility (check|refresh) [--repo PATH]
/// [--repository owner/name]`: the metadata-acquisition side of the
/// visibility-based runner policy. `check` compares the checked-in evidence
/// against the live visibility; `refresh` reacquires and rewrites it. Both
/// are explicit and fail closed; rendering itself never calls either.
fn visibility_command(arguments: &[OsString]) -> Result<(), GeneratorError> {
    const USAGE: &str =
        "usage: velnor-workflow visibility (check|refresh) [--repo PATH] [--repository owner/name]";
    let (verb, rest) = arguments
        .split_first()
        .ok_or_else(|| GeneratorError::usage(format!("visibility needs a verb; {USAGE}")))?;
    let verb = verb
        .to_str()
        .ok_or_else(|| GeneratorError::usage("CI argument must be valid UTF-8"))?;
    if !matches!(verb, "check" | "refresh") {
        return Err(GeneratorError::usage(format!(
            "unsupported visibility verb: {verb}; {USAGE}"
        )));
    }
    let options = parse_options(rest, &["repo", "repository"])?;
    let root = options.get("repo").map_or_else(
        || {
            std::env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("resolve repository root: {error}")))
        },
        |repo| Ok(PathBuf::from(repo)),
    )?;
    match verb {
        "check" => {
            if options.contains_key("repository") {
                return Err(GeneratorError::usage(
                    "visibility check takes no --repository: it verifies the checked-in evidence as is".to_owned(),
                ));
            }
            crate::visibility::check(&root)
        }
        "refresh" => {
            crate::visibility::refresh(&root, options.get("repository").map(String::as_str))
                .map(|_| ())
        }
        _ => Err(GeneratorError::usage(format!(
            "unsupported visibility verb: {verb}; {USAGE}"
        ))),
    }
}

fn promote_command(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let (flags, rest): (Vec<&OsString>, Vec<&OsString>) = arguments
        .iter()
        .partition(|argument| argument.to_str().is_some_and(|value| value == "--dry-run"));
    if flags.len() > 1 {
        return Err(GeneratorError::usage(
            "duplicate option: --dry-run".to_owned(),
        ));
    }
    let dry_run = !flags.is_empty();
    let rest: Vec<OsString> = rest.into_iter().cloned().collect();
    let options = parse_options(
        &rest,
        &[
            "rev",
            "repo",
            "generator-repo",
            "publication-readiness",
            "default-branch",
            "runners",
            "message",
        ],
    )?;
    let rev = options
        .get("rev")
        .ok_or_else(|| GeneratorError::usage("promote requires --rev SHA".to_owned()))?;
    let publication_readiness = options.get("publication-readiness").ok_or_else(|| {
        GeneratorError::usage(
            "promote requires --publication-readiness PATH (trusted publication evidence)"
                .to_owned(),
        )
    })?;
    let repo = match options.get("repo") {
        Some(path) => PathBuf::from(path.as_str()),
        None => env::current_dir()
            .map_err(|error| GeneratorError::usage(format!("resolve promote root: {error}")))?,
    };
    let runners = options
        .get("runners")
        .map_or(Ok(crate::RunnerMode::Both), |value| {
            crate::parse_runner_mode(value)
        })?;
    let report = crate::promote::run_promote(&crate::promote::PromoteOptions {
        rev: rev.clone(),
        repo,
        generator_repo: options
            .get("generator-repo")
            .map(|path| PathBuf::from(path.as_str())),
        publication_readiness: PathBuf::from(publication_readiness),
        default_branch: options.get("default-branch").cloned(),
        runners,
        message: options.get("message").cloned(),
        dry_run,
    })?;
    print!("{}", crate::promote::render_report(&report));
    Ok(())
}

/// Dispatch the binary-only subcommands. `false` means the arguments belong
/// to the workflow generator CLI proper.
pub(crate) fn try_run(arguments: &[OsString]) -> Result<bool, GeneratorError> {
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else {
        return Ok(false);
    };
    match command {
        "plan" => {
            let options = parse_options(&arguments[1..], &["config"])?;
            plan(&resolve_config_path(options.get("config")))?;
            Ok(true)
        }
        "run" => {
            let options = parse_options(&arguments[1..], &["config", "scope", "unit", "phase"])?;
            let root = env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
            let config = resolve_config_path(options.get("config"));
            let scope = options
                .get("scope")
                .map_or(Ok(Scope::Full), |value| Scope::parse(value))?;
            let phase = options
                .get("phase")
                .map(|value| {
                    ValidationPhase::parse(value).ok_or_else(|| {
                        GeneratorError::usage(format!(
                            "unsupported --phase: {value}; use fmt, clippy, test, doctest, or check"
                        ))
                    })
                })
                .transpose()?;
            run_units(
                &root,
                &config,
                scope,
                options.get("unit").map(String::as_str),
                phase,
            )?;
            Ok(true)
        }
        "test-crates" => {
            let options = parse_options(&arguments[1..], &["config"])?;
            let root = env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
            test_crates(&root, &resolve_config_path(options.get("config")), None)?;
            Ok(true)
        }
        "policy" => {
            crate::policy::run_cli(&arguments[1..])?;
            Ok(true)
        }
        "release" => {
            release(&arguments[1..])?;
            Ok(true)
        }
        "version" => {
            print_version(arguments.get(1..).unwrap_or_default())?;
            Ok(true)
        }
        "closure" => {
            print_closure(arguments.get(1..).unwrap_or_default())?;
            Ok(true)
        }
        "promote" => {
            promote_command(arguments.get(1..).unwrap_or_default())?;
            Ok(true)
        }
        "visibility" => {
            visibility_command(arguments.get(1..).unwrap_or_default())?;
            Ok(true)
        }
        "prepared-tool-install" => {
            prepared_tool_install(&arguments[1..])?;
            Ok(true)
        }
        "cache-plan" => {
            let options = parse_options(&arguments[1..], &["entries", "now", "mode"])?;
            let mode = options.get("mode").map_or("plan", String::as_str);
            if !matches!(mode, "plan" | "budget") {
                return Err(GeneratorError::usage(format!(
                    "unsupported cache-plan mode: {mode}; use --mode=plan or --mode=budget"
                )));
            }
            if mode == "budget" {
                if let Some(entries_path) = options.get("entries") {
                    cache_budget_report(entries_path)?;
                } else {
                    println!("{}", retention_policy_for_plan().total_bytes);
                }
                return Ok(true);
            }
            cache_plan(
                options.get("entries").map(String::as_str),
                options.get("now").map(String::as_str),
            )?;
            Ok(true)
        }
        _ => try_run_reuse(command, arguments),
    }
}

/// Dispatch the slice-C subcommands (`aggregate`, `select`, `fingerprint`,
/// `reuse-decision`). `false` means the arguments belong to the workflow
/// generator CLI proper.
fn try_run_reuse(command: &str, arguments: &[OsString]) -> Result<bool, GeneratorError> {
    match command {
        "aggregate" => {
            let options = parse_options(&arguments[1..], &["expected", "results"])?;
            let expected = options.get("expected").ok_or_else(|| {
                GeneratorError::usage("aggregate requires --expected PATH".to_owned())
            })?;
            let results = options.get("results").ok_or_else(|| {
                GeneratorError::usage("aggregate requires --results PATH".to_owned())
            })?;
            aggregate_command(Path::new(expected), Path::new(results))?;
            Ok(true)
        }
        "select" => {
            let options = parse_options(&arguments[1..], &["config", "base", "head", "scope"])?;
            let root = env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
            let config = resolve_config_path(options.get("config"));
            let scope = options
                .get("scope")
                .map_or(Ok(Scope::Affected), |value| Scope::parse(value))?;
            let base = options.get("base").cloned().unwrap_or_default();
            let head = options
                .get("head")
                .cloned()
                .unwrap_or_else(|| "HEAD".to_owned());
            select_command(&root, &config, scope, &base, &head)?;
            Ok(true)
        }
        "fingerprint" => {
            let options = parse_options(&arguments[1..], &["config", "unit", "rev", "live"])?;
            let root = env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
            let config = resolve_config_path(options.get("config"));
            let rev = options
                .get("rev")
                .cloned()
                .unwrap_or_else(|| "HEAD".to_owned());
            fingerprint_command(
                &root,
                &config,
                options.get("unit").map(String::as_str),
                options.get("live").map(String::as_str),
                &rev,
            )?;
            Ok(true)
        }
        "reuse-decision" => {
            let options = parse_options(&arguments[1..], &["evidence", "request", "now"])?;
            let evidence = options.get("evidence").ok_or_else(|| {
                GeneratorError::usage("reuse-decision requires --evidence PATH".to_owned())
            })?;
            let request = options.get("request").ok_or_else(|| {
                GeneratorError::usage("reuse-decision requires --request PATH".to_owned())
            })?;
            let now = options
                .get("now")
                .map(String::as_str)
                .map(|pinned| {
                    pinned.parse::<u64>().map_err(|_| {
                        GeneratorError::usage(format!(
                            "unsupported --now value {pinned}: name seconds since the epoch"
                        ))
                    })
                })
                .transpose()?;
            reuse_decision_command(Path::new(evidence), Path::new(request), now)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// One Actions cache entry as the maintenance job collects it from the API.
#[derive(Deserialize)]
struct CacheEntryRecord {
    #[serde(deserialize_with = "deserialize_cache_id")]
    id: String,
    key: String,
    size_in_bytes: u64,
    created_at: String,
}

/// Accept the Actions API's numeric cache id and carry it losslessly as the
/// planner's string identity. The API has shipped both JSON shapes; treating
/// its current numeric encoding as invalid turns retention into a cold-cache
/// failure instead of the bounded maintenance it exists to perform.
fn deserialize_cache_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CacheIdVisitor;

    impl serde::de::Visitor<'_> for CacheIdVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a numeric or string Actions cache id")
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(value.to_owned())
        }
    }

    deserializer.deserialize_any(CacheIdVisitor)
}

fn read_cache_entries(entries_path: &str) -> Result<Vec<SnapshotCacheEntry>, GeneratorError> {
    let entries_text = fs::read_to_string(entries_path).map_err(|error| {
        GeneratorError::io(
            "read cache account snapshot",
            Path::new(entries_path),
            &error,
        )
    })?;
    let records: Vec<CacheEntryRecord> = serde_json::from_str(&entries_text).map_err(|error| {
        GeneratorError::usage(format!(
            "the cache account snapshot is not an array of cache records: {error}"
        ))
    })?;
    Ok(records
        .into_iter()
        .map(|record| SnapshotCacheEntry {
            id: record.id,
            key: record.key,
            size_in_bytes: record.size_in_bytes,
            created_at: record.created_at,
        })
        .collect())
}

/// Resolve the GitHub Actions retention policy from `.github-gen/velnor-workflow.toml`
/// when present, otherwise the generator default.
fn retention_policy_for_plan() -> RetentionPolicy {
    let discovered = std::env::current_dir()
        .ok()
        .and_then(|cwd| crate::config::discover(&cwd).ok().flatten());
    let policy = discovered
        .as_ref()
        .map_or_else(RetentionPolicy::default_policy, |config| {
            RetentionPolicy::from_config(config.cache_github())
        });
    retention_policy_with_declared_tools(policy, discovered.as_ref())
}

/// Extend a retention policy with the prepared-tools class when the
/// discovered generation config declares a `prepared-tool` row. Factored
/// from the current-directory lookup so tests pin the conditional without
/// changing the process directory. Undeclared repositories keep the exact
/// default policy.
fn retention_policy_with_declared_tools(
    policy: RetentionPolicy,
    discovered: Option<&crate::config::RepoGenerationConfig>,
) -> RetentionPolicy {
    let declares = discovered.is_some_and(|config| {
        config
            .declare()
            .iter()
            .any(|row| row.primitive() == super::primitives::PREPARED_TOOL)
    });
    if declares {
        policy.with_prepared_tools()
    } else {
        policy
    }
}

/// Verify and install one restored prepared-tool bundle: the consumer half
/// of the handoff, running in CI. The rendered consumer steps pass the
/// request as literals (`--tool`, `--inputs`, `--abi`, `--producers`,
/// `--run-id`) and the restore layout (`--manifest`, `--dir`, `--dest`);
/// `--repo` authorizes the producer-outcome check and `--curl` names the
/// HTTP client (a test seam; CI uses `curl`). Exact current-run hits skip
/// the outcome check — the producing job is this run — while historical
/// bundles must prove their producer run concluded `success`.
///
/// Every exit records the taxonomy `outcome` beside the human message: a
/// later producer step tells a miss (build) from a refusal (fail) without
/// parsing text.
fn prepared_tool_install(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let output = env::var_os("GITHUB_OUTPUT").map(PathBuf::from);
    prepared_tool_install_to(arguments, output.as_deref(), &mut |wait| {
        thread::sleep(wait);
    })
}

/// The install with its step-output path and sleeper injected: the process
/// environment stays at the boundary so tests pin behavior without touching
/// it.
fn prepared_tool_install_to(
    arguments: &[OsString],
    output: Option<&Path>,
    sleeper: &mut dyn FnMut(Duration),
) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "manifest",
            "dir",
            "dest",
            "tool",
            "inputs",
            "abi",
            "producers",
            "run-id",
            "repo",
            "curl",
            "check-save-key",
        ],
    )?;
    let get = |name: &str| {
        options
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| GeneratorError::usage(format!("prepared-tool-install needs --{name}")))
    };
    let (manifest_path, dir, dest, tool, inputs, abi, producers, run_id, repo) = (
        get("manifest")?,
        get("dir")?,
        get("dest")?,
        get("tool")?,
        get("inputs")?,
        get("abi")?,
        get("producers")?,
        get("run-id")?,
        get("repo")?,
    );
    let curl = options.get("curl").map_or("curl", String::as_str);
    let request = ToolRequest {
        tool_id: tool.to_owned(),
        inputs_digest: inputs.to_owned(),
        platform_abi: abi.to_owned(),
        authorized_producers: producers.split(',').map(str::to_owned).collect(),
        run_id: run_id.to_owned(),
    };
    let (resolution, verified) = classify_restored_bundle(manifest_path, dir, &request, output)?;
    if let Some(candidate) = options.get("check-save-key") {
        return check_save_key(output, tool, candidate, &verified);
    }
    if !resolution.is_exact {
        let token = env::var("GH_TOKEN").map_err(|_| {
            GeneratorError::usage(
                "prepared-tool-install needs GH_TOKEN to validate the producer run outcome",
            )
        })?;
        let producer_run = verified.manifest().producer.run_id.clone();
        check_historical_outcome(
            &producer_run,
            &mut || curl_producer_run(curl, repo, &producer_run, &token),
            sleeper,
            output,
        )?;
    }
    verified
        .install_to(Path::new(dest))
        .map_err(|error| GeneratorError::io("install prepared tool", Path::new(dest), &error))?;
    for (path, sha256, executable) in verified.install_plan() {
        println!(
            "prepared-tool installed {path} {sha256} {}",
            if executable { "executable" } else { "data" }
        );
    }
    if let Some(path) = output {
        append_step_output(path, &format_install_outputs(&resolution))
            .map_err(|error| GeneratorError::io("record prepared-tool outputs", path, &error))?;
    }
    println!(
        "prepared-tool installed {tool} ({} -> {})",
        resolution.requested.as_str(),
        resolution.resolved.as_str()
    );
    Ok(())
}

/// Read one restored bundle and prove it against its request: the manifest
/// must exist and parse, the arrived files are read, and classification
/// yields the requested/resolved key pair with the verified bytes.
fn classify_restored_bundle(
    manifest_path: &str,
    dir: &str,
    request: &ToolRequest,
    output: Option<&Path>,
) -> Result<(Resolution, VerifiedBundle), GeneratorError> {
    let manifest_bytes = match fs::read(manifest_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(fail_install(
                output,
                &HandoffFailure::Miss {
                    detail: format!(
                        "no bundle restored for tool `{}` at `{manifest_path}`",
                        request.tool_id
                    ),
                },
            ));
        }
        Err(error) => {
            return Err(GeneratorError::io(
                "read tool manifest",
                Path::new(manifest_path),
                &error,
            ));
        }
    };
    let manifest = match ToolManifest::from_json(&manifest_bytes) {
        Ok(manifest) => manifest,
        Err(failure) => return Err(fail_install(output, &failure)),
    };
    let files = read_arrived_files(dir, &manifest)?;
    match classify_and_verify(request, &manifest, &files) {
        Ok(resolved) => Ok(resolved),
        Err(failure) => Err(fail_install(output, &failure)),
    }
}

/// Gate a save step: prove the verified bytes may be saved under the
/// candidate key. The check answers key-vs-manifest identity only — no
/// install, no outcome fetch — so a save step calls it before writing any
/// cache entry. Fallback bytes under the requested exact key fail here, at
/// save time, instead of shadowing the exact entry.
fn check_save_key(
    output: Option<&Path>,
    tool: &str,
    candidate: &str,
    verified: &VerifiedBundle,
) -> Result<(), GeneratorError> {
    let manifest = verified.manifest();
    if save_is_legal(candidate, manifest) {
        if let Some(path) = output {
            append_step_output(path, &format_save_check_output(candidate)).map_err(|error| {
                GeneratorError::io("record prepared-tool save check", path, &error)
            })?;
        }
        println!("prepared-tool save allowed for {tool} under {candidate}");
        return Ok(());
    }
    Err(fail_install(
        output,
        &HandoffFailure::Corrupt {
            detail: format!(
                "refusing to save tool `{tool}` bytes from run {} under `{candidate}`, which names another run",
                manifest.producer.run_id
            ),
        },
    ))
}

/// Prove a historical bundle's producer run concluded `success` through the
/// Actions API: the outcome half of historical validation. Anything but a
/// proven `success` fails closed as denied — an unprovable bundle is a
/// policy refusal, never an install.
fn check_historical_outcome(
    producer_run_id: &str,
    executor: &mut dyn FnMut() -> Result<ApiResponse, String>,
    sleeper: &mut dyn FnMut(Duration),
    output: Option<&Path>,
) -> Result<(), GeneratorError> {
    let started = Instant::now();
    let mut elapsed = || started.elapsed();
    match fetch_producer_conclusion(
        &TransferBounds::DEFAULT,
        producer_run_id,
        executor,
        sleeper,
        &mut elapsed,
    ) {
        Ok(conclusion) if conclusion == "success" => Ok(()),
        Ok(conclusion) => Err(fail_install(
            output,
            &HandoffFailure::Denied {
                detail: format!(
                    "producer run {producer_run_id} concluded `{conclusion}`, not `success`"
                ),
            },
        )),
        Err(OutcomeFetchError::Failure(failure)) => Err(fail_install(output, &failure)),
        Err(OutcomeFetchError::Transport(detail)) => Err(GeneratorError::usage(detail)),
    }
}

/// Record a taxonomy failure as the step's `outcome` and return it as the
/// verb's error: the message names the failure, the output names it for
/// machines.
fn fail_install(output: Option<&Path>, failure: &HandoffFailure) -> GeneratorError {
    if let Some(path) = output
        && let Err(error) = append_step_output(path, &format_failure_output(failure))
    {
        return GeneratorError::io("record prepared-tool outcome", path, &error);
    }
    GeneratorError::usage(failure.to_string())
}

/// Read the restored bytes for every file the manifest lists. A file the
/// restore did not bring is left absent for [`classify_and_verify`] to
/// report precisely; an unreadable file is an environment failure.
fn read_arrived_files(
    dir: &str,
    manifest: &ToolManifest,
) -> Result<BTreeMap<String, Vec<u8>>, GeneratorError> {
    let mut files = BTreeMap::new();
    for file in &manifest.files {
        let path = Path::new(dir).join(&file.path);
        match fs::read(&path) {
            Ok(bytes) => {
                files.insert(file.path.clone(), bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(GeneratorError::io("read restored tool file", &path, &error));
            }
        }
    }
    Ok(files)
}

/// Fetch one producer workflow run through the Actions API: status, headers,
/// and body for the taxonomy to classify. `curl` owns no retries here —
/// `--retry 0` keeps the bounded Rust loop the only retry authority — and
/// `--max-time` ceilings one attempt inside the total wait budget. `GH_TOKEN`
/// authorizes the call, as the rendered consumer step provides it.
fn curl_producer_run(
    curl: &str,
    repo: &str,
    run_id: &str,
    token: &str,
) -> Result<ApiResponse, String> {
    let endpoint = format!("https://api.github.com/repos/{repo}/actions/runs/{run_id}");
    let output = Command::new(curl)
        .args([
            "-sS",
            "--max-time",
            "30",
            "--retry",
            "0",
            "--dump-header",
            "-",
            "--output",
            "-",
            "--write-out",
            "\n__PREPARED_TOOL_STATUS:%{http_code}\n",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            &format!("Authorization: Bearer {token}"),
            &endpoint,
        ])
        .env("GH_TOKEN", token)
        .output()
        .map_err(|error| format!("run {curl}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{curl} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|error| format!("{curl} answered non-UTF-8: {error}"))?;
    split_curl_response(&text).ok_or_else(|| format!("{curl} answered an unreadable response"))
}

/// Split a `--dump-header - --output - --write-out` capture into status,
/// headers, and body. The status trailer is split from the right, so a body
/// containing the marker cannot shift the parse.
fn split_curl_response(text: &str) -> Option<ApiResponse> {
    let (head, status_text) = text.rsplit_once("__PREPARED_TOOL_STATUS:")?;
    let status: u16 = status_text.trim().parse().ok()?;
    let (headers, body) = head
        .split_once("\r\n\r\n")
        .or_else(|| head.split_once("\n\n"))?;
    Some(ApiResponse {
        status,
        headers: headers.to_owned(),
        body: body.to_owned(),
    })
}

/// Append `text` to a step-output file (`GITHUB_OUTPUT` in CI).
fn append_step_output(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(text.as_bytes())
}

/// Emit per-class totals and headroom for the maintenance budget step.
fn cache_budget_report(entries_path: &str) -> Result<(), GeneratorError> {
    let entries = read_cache_entries(entries_path)?;
    let report = budget_report(&entries, &retention_policy_for_plan());
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, &report)
        .map_err(|error| GeneratorError::usage(format!("write budget report: {error}")))?;
    writeln!(handle)
        .map_err(|error| GeneratorError::usage(format!("write budget report: {error}")))?;
    Ok(())
}

/// Compute the retention eviction plan for the Actions cache account: the same
/// [`RetentionPolicy`] and the same `plan_evictions` the generator's tests
/// exercise, run against the account snapshot the maintenance job collects.
/// The plan is the workflow's only eviction decision — the job applies it
/// verbatim and records every eviction's class and reason in the summary.
///
/// `--now` pins the clock for tests; a live run uses the system clock.
fn cache_plan(entries_path: Option<&str>, now: Option<&str>) -> Result<(), GeneratorError> {
    let entries = if let Some(path) = entries_path {
        read_cache_entries(path)?
    } else {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).map_err(|error| {
            GeneratorError::usage(format!("read cache account snapshot: {error}"))
        })?;
        let records: Vec<CacheEntryRecord> = serde_json::from_str(&text).map_err(|error| {
            GeneratorError::usage(format!(
                "the cache account snapshot is not an array of cache records: {error}"
            ))
        })?;
        records
            .into_iter()
            .map(|record| SnapshotCacheEntry {
                id: record.id,
                key: record.key,
                size_in_bytes: record.size_in_bytes,
                created_at: record.created_at,
            })
            .collect()
    };
    let now_epoch = match now {
        Some(pinned) => parse_pinned_epoch(pinned)?,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| GeneratorError::usage(format!("resolve current time: {error}")))?
            .as_secs()
            .cast_signed(),
    };
    let plan = plan_evictions(&entries, &retention_policy_for_plan(), now_epoch);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, &plan)
        .map_err(|error| GeneratorError::usage(format!("write eviction plan: {error}")))?;
    writeln!(handle)
        .map_err(|error| GeneratorError::usage(format!("write eviction plan: {error}")))?;
    Ok(())
}

/// Score reported results against the planner's expected work: the same
/// [`crate::reuse::aggregate`] the generator's tests exercise, run against
/// the expected-work file the plan wrote and the results file the unit jobs
/// collected. The file's recorded plan SHAs must match this job's own
/// checkout (`BASE_SHA`/`HEAD_SHA`, defaulting exactly like the selection
/// artifact's binding); a stale or mis-threaded file fails before any
/// verdict. Prints the audit report and fails when the aggregate rejects.
fn aggregate_command(expected: &Path, results: &Path) -> Result<(), GeneratorError> {
    let expected_text = fs::read_to_string(expected)
        .map_err(|error| GeneratorError::io("read expected work", expected, &error))?;
    let results_text = fs::read_to_string(results)
        .map_err(|error| GeneratorError::io("read reported results", results, &error))?;
    let base_sha = env::var("BASE_SHA").unwrap_or_default();
    let head_sha = env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned());
    let verdict =
        crate::reuse::aggregate_files(&expected_text, &results_text, &base_sha, &head_sha)
            .map_err(GeneratorError::usage)?;
    print!("{}", crate::reuse::render_report(&verdict));
    if let Some(line) = explicit_no_work_line(&expected_text, &verdict) {
        println!("{line}");
    }
    if verdict.passed {
        Ok(())
    } else {
        Err(GeneratorError::usage(
            "aggregate: expected work did not complete",
        ))
    }
}

/// The machine-readable no-work line for an explicit-no-work PASS: the
/// planner's marker plus zero expected units plus a passing verdict. `None`
/// for every other verdict — real-work output, and failed no-work verdicts,
/// stay byte-identical. A missing `units` key reads as empty, matching the
/// schema default the aggregate itself scores.
fn explicit_no_work_line(
    expected_json: &str,
    verdict: &crate::reuse::AggregateVerdict,
) -> Option<String> {
    if !verdict.passed {
        return None;
    }
    let document: serde_json::Value = serde_json::from_str(expected_json).ok()?;
    if document
        .get("planned_no_work")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let empty = document
        .get("units")
        .and_then(serde_json::Value::as_array)
        .is_none_or(Vec::is_empty);
    if !empty {
        return None;
    }
    let extras = verdict
        .explanations
        .iter()
        .filter(|explanation| explanation.disposition == crate::reuse::Disposition::Extra)
        .count();
    Some(match extras {
        0 => "no_work_reason=planner selected zero workload units and no workload results were reported"
            .to_owned(),
        1 => "no_work_reason=planner selected zero workload units; 1 reported result ignored as outside the plan"
            .to_owned(),
        extras => format!(
            "no_work_reason=planner selected zero workload units; {extras} reported results ignored as outside the plan"
        ),
    })
}

/// Print the affected selection for a diff as JSON: the auditable
/// [`crate::reuse::select_affected`] core over the runtime's own unit table.
/// Unlike `plan`, this command answers one question only — which units a
/// change list affects — without lane filtering, workspace gates, or outputs.
fn select_command(
    root: &Path,
    config_path: &Path,
    scope: Scope,
    base: &str,
    head: &str,
) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    let watched: Vec<crate::reuse::WatchedUnit> = config
        .unit
        .iter()
        .map(|unit| crate::reuse::WatchedUnit {
            id: unit.id.clone(),
            watch: unit.watch.clone(),
            depends_on: unit.depends_on.clone(),
            kind: unit.kind.clone(),
            commands: unit
                .github_pr_commands
                .iter()
                .chain(&unit.github_full_commands)
                .chain(&unit.velnor_pr_commands)
                .chain(&unit.velnor_full_commands)
                .cloned()
                .collect(),
        })
        .collect();
    if scope == Scope::Full {
        return print_selection(&crate::reuse::AffectedSelection {
            required: watched.iter().map(|unit| unit.id.clone()).collect(),
            full_units: watched.iter().map(|unit| unit.id.clone()).collect(),
            fallback_full: false,
            explanations: watched
                .iter()
                .map(|unit| (unit.id.clone(), "full scope requested".to_owned()))
                .collect(),
            fallback_reason: None,
        });
    }
    if base.is_empty() || base.chars().all(|character| character == '0') {
        return print_selection(&crate::reuse::fallback_selection(
            &watched,
            "no affected base; fell back to full",
        ));
    }
    let Some(lines) = git_name_status(root, base, head)? else {
        return print_selection(&crate::reuse::fallback_selection(
            &watched,
            "git diff unavailable; fell back to full",
        ));
    };
    let mut changes = Vec::with_capacity(lines.len());
    for line in &lines {
        let Some(change) = crate::reuse::parse_name_status_line(line) else {
            return print_selection(&crate::reuse::fallback_selection(
                &watched,
                "unparseable change entry; fell back to full",
            ));
        };
        changes.push(change);
    }
    print_selection(&crate::reuse::select_affected(
        &watched,
        &changes,
        crate::reuse::FULL_SELECTION_PREFIXES,
    )?)
}

fn print_selection(selection: &crate::reuse::AffectedSelection) -> Result<(), GeneratorError> {
    let mut json = serde_json::to_string(selection)
        .map_err(|error| GeneratorError::usage(format!("serialize affected selection: {error}")))?;
    json.push('\n');
    print!("{json}");
    Ok(())
}

fn git_name_status(
    root: &Path,
    base: &str,
    head: &str,
) -> Result<Option<Vec<String>>, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-status", "-M"])
        .arg(format!("{base}...{head}"))
        .output()
        .map_err(|error| GeneratorError::usage(format!("run git diff: {error}")))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
    ))
}

/// One `fingerprint` report: the revision fingerprinted and the per-unit
/// identities in dependency order.
#[derive(serde::Serialize)]
struct FingerprintOutput {
    rev: String,
    units: Vec<crate::reuse::FingerprintReport>,
}

/// Print per-unit fingerprints as JSON: the canonical [`crate::reuse`]
/// artifact identity over the runtime's own unit table at one revision.
/// `--unit` reports one unit; `--live` names the live-state units explicitly,
/// since the runtime table cannot observe that property.
fn fingerprint_command(
    root: &Path,
    config_path: &Path,
    only_unit: Option<&str>,
    live: Option<&str>,
    rev: &str,
) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    let known: BTreeSet<&str> = config.unit.iter().map(|unit| unit.id.as_str()).collect();
    if let Some(id) = only_unit
        && !known.contains(id)
    {
        return Err(GeneratorError::usage(format!("unknown unit: {id}")));
    }
    let live_ids = resolve_live_ids(live, &known)?;
    let tree = git_ls_tree(root, rev)?;
    let ordered = ordered_units(&config.unit, None)?;
    let check_impl = crate::reuse::current_check_impl();
    let mut fingerprints: BTreeMap<String, String> = BTreeMap::new();
    let mut reports = Vec::new();
    for unit in ordered {
        let commands = BTreeMap::from([
            (
                "github/affected".to_owned(),
                unit.github_pr_commands.clone(),
            ),
            ("github/full".to_owned(), unit.github_full_commands.clone()),
            (
                "velnor/affected".to_owned(),
                unit.velnor_pr_commands.clone(),
            ),
            ("velnor/full".to_owned(), unit.velnor_full_commands.clone()),
        ]);
        let recipe = crate::reuse::recipe_digest(&commands, crate::GENERATOR_REVISION, &check_impl);
        let pinned: BTreeSet<String> = unit.cache.as_ref().map_or_else(BTreeSet::new, |cache| {
            cache.key_files.iter().cloned().collect()
        });
        let source = crate::reuse::source_digests(&tree, &unit.watch, &pinned)?;
        let view = crate::reuse::UnitConfigView {
            id: unit.id.clone(),
            kind: unit.kind.clone(),
            root: unit.root.clone(),
            watch: unit.watch.clone(),
            commands,
            depends_on: unit.depends_on.clone(),
            tool_version: unit.tool_version.clone(),
            cache_key_files: unit
                .cache
                .as_ref()
                .map_or_else(Vec::new, |cache| cache.key_files.clone()),
            cache_paths: unit
                .cache
                .as_ref()
                .map_or_else(Vec::new, |cache| cache.paths.clone()),
        };
        let config_digest = crate::reuse::config_digest(&view);
        let mut tool_pins = BTreeMap::new();
        if let Some(version) = &unit.tool_version {
            tool_pins.insert("tool_version".to_owned(), version.clone());
        }
        let pins = crate::reuse::unit_pin_set(&tool_pins);
        let mut transitive = BTreeMap::new();
        for dependency in &unit.depends_on {
            let Some(digest) = fingerprints.get(dependency) else {
                return Err(GeneratorError::usage(format!(
                    "unit `{}` depends on unknown unit `{dependency}`",
                    unit.id
                )));
            };
            transitive.insert(dependency.clone(), digest.clone());
        }
        let input = crate::reuse::FingerprintInput {
            unit_id: unit.id.clone(),
            kind: unit.kind.clone(),
            root: unit.root.clone(),
            source,
            config_digest,
            recipe_digest: recipe.clone(),
            pins,
            transitive,
            live_state: live_ids.contains(unit.id.as_str()),
        };
        let fingerprint = crate::reuse::canonical_fingerprint(&input);
        fingerprints.insert(unit.id.clone(), fingerprint.clone());
        if only_unit.is_none_or(|id| id == unit.id) {
            reports.push(crate::reuse::FingerprintReport {
                unit: unit.id.clone(),
                locator: crate::reuse::artifact_locator(&unit.id, &fingerprint),
                fingerprint,
                recipe,
                live: input.live_state,
            });
        }
    }
    let output = FingerprintOutput {
        rev: rev.to_owned(),
        units: reports,
    };
    let mut json = serde_json::to_string(&output)
        .map_err(|error| GeneratorError::usage(format!("serialize fingerprints: {error}")))?;
    json.push('\n');
    print!("{json}");
    Ok(())
}

/// Resolve the `--live` unit list against the known unit ids. Every named id
/// must exist: a live-state marker for an unknown unit would silently leave
/// the real unit content-addressed.
fn resolve_live_ids<'a>(
    live: Option<&'a str>,
    known: &BTreeSet<&str>,
) -> Result<BTreeSet<&'a str>, GeneratorError> {
    let ids: BTreeSet<&'a str> = live.map_or_else(BTreeSet::new, |list| {
        list.split(',').filter(|id| !id.is_empty()).collect()
    });
    for id in &ids {
        if !known.contains(id) {
            return Err(GeneratorError::usage(format!(
                "unknown live-state unit: {id}"
            )));
        }
    }
    Ok(ids)
}

fn git_ls_tree(root: &Path, rev: &str) -> Result<String, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-tree", "-r", rev])
        .output()
        .map_err(|error| GeneratorError::usage(format!("run git ls-tree: {error}")))?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "revision {rev} is not available for fingerprinting"
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Decide whether a prior successful result may back the current verdict and
/// print the decision: `reuse <run>` or `execute`, then one audit reason per
/// line. A refused reuse is a normal decision, so the command always exits
/// success once the files parse. `--now` pins the freshness clock; a live run
/// without it uses the system clock.
fn reuse_decision_command(
    evidence: &Path,
    request: &Path,
    now: Option<u64>,
) -> Result<(), GeneratorError> {
    let evidence_text = fs::read_to_string(evidence)
        .map_err(|error| GeneratorError::io("read producing evidence", evidence, &error))?;
    let request_text = fs::read_to_string(request)
        .map_err(|error| GeneratorError::io("read reuse request", request, &error))?;
    let now = match now {
        Some(pinned) => Some(pinned),
        None => Some(system_now_secs()?),
    };
    let decision = crate::reuse::reuse_decision_files(&evidence_text, &request_text, now)
        .map_err(GeneratorError::usage)?;
    print!("{}", crate::reuse::render_decision(&decision));
    Ok(())
}

fn system_now_secs() -> Result<u64, GeneratorError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| GeneratorError::usage(format!("resolve current time: {error}")))
        .map(|duration| duration.as_secs())
}

/// Parse the pinned `--now` clock as seconds since the epoch, so a test run
/// names a reproducible instant.
fn parse_pinned_epoch(pinned: &str) -> Result<i64, GeneratorError> {
    pinned.parse::<i64>().map_err(|_| {
        GeneratorError::usage(format!(
            "unsupported --now value {pinned}: name seconds since the epoch"
        ))
    })
}

fn parse_options(
    arguments: &[OsString],
    allowed: &[&str],
) -> Result<BTreeMap<String, String>, GeneratorError> {
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < arguments.len() {
        let raw = arguments[index]
            .to_str()
            .ok_or_else(|| GeneratorError::usage("CI argument must be valid UTF-8"))?;
        let (name, inline) = raw
            .strip_prefix("--")
            .and_then(|value| {
                value
                    .split_once('=')
                    .map_or(Some((value, None)), |(n, v)| Some((n, Some(v))))
            })
            .ok_or_else(|| GeneratorError::usage(format!("unsupported CI argument: {raw}")))?;
        if !allowed.contains(&name) {
            return Err(GeneratorError::usage(format!(
                "unsupported CI argument: --{name}"
            )));
        }
        let value = match inline {
            Some(value) if !value.is_empty() => value.to_owned(),
            Some(_) => return Err(GeneratorError::usage(format!("--{name} needs a value"))),
            None => {
                index += 1;
                arguments
                    .get(index)
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| GeneratorError::usage(format!("--{name} needs a value")))?
                    .to_owned()
            }
        };
        if options.insert(name.to_owned(), value).is_some() {
            return Err(GeneratorError::usage(format!("duplicate option: --{name}")));
        }
        index += 1;
    }
    Ok(options)
}

fn resolve_config_path(config: Option<&String>) -> PathBuf {
    config.map_or_else(|| PathBuf::from(DEFAULT_CONFIG), PathBuf::from)
}

/// Parse a generated `project.toml` through the real runtime contract.
/// Emitter tests use it to prove the generator never ships a field the
/// runtime rejects.
#[cfg(test)]
pub(crate) fn read_config_for_test(path: &Path) -> Result<(), GeneratorError> {
    read_config(path).map(|_| ())
}

pub(crate) fn read_config(path: &Path) -> Result<CiConfig, GeneratorError> {
    let contents = fs::read_to_string(path)
        .map_err(|error| GeneratorError::io("read CI configuration", path, &error))?;
    let config: CiConfig = toml::from_str(&contents).map_err(|error| {
        GeneratorError::usage(format!(
            "parse CI configuration {}: {error}",
            path.display()
        ))
    })?;
    if config.schema != 2 {
        return Err(GeneratorError::usage(format!(
            "unsupported CI configuration schema: {}",
            config.schema
        )));
    }
    validate_config(&config)
}

fn validate_config(config: &CiConfig) -> Result<CiConfig, GeneratorError> {
    if config.unit.is_empty() {
        return Err(GeneratorError::usage("CI configuration declares no units"));
    }
    let mut known = BTreeSet::new();
    for unit in &config.unit {
        if !is_unit_id(&unit.id) {
            return Err(GeneratorError::usage(format!(
                "invalid CI unit id: {}",
                unit.id
            )));
        }
        if !known.insert(unit.id.clone()) {
            return Err(GeneratorError::usage(format!(
                "duplicate CI unit id: {}",
                unit.id
            )));
        }
        // An empty watch is valid: a unit with no sources never matches an
        // affected diff but still runs in full scope.
        if unit.github_pr_commands.is_empty()
            || unit.github_full_commands.is_empty()
            || unit.velnor_pr_commands.is_empty()
            || unit.velnor_full_commands.is_empty()
        {
            return Err(GeneratorError::usage(format!(
                "CI unit must declare GitHub and Velnor PR/full commands: {}",
                unit.id
            )));
        }
    }
    for unit in &config.unit {
        for dependency in &unit.depends_on {
            if !known.contains(dependency) {
                return Err(GeneratorError::usage(format!(
                    "{} depends on unknown unit: {}",
                    unit.id, dependency
                )));
            }
            if dependency == &unit.id {
                return Err(GeneratorError::usage(format!(
                    "unit cannot depend on itself: {}",
                    unit.id
                )));
            }
        }
    }
    let mut version_bump_units = BTreeSet::new();
    for unit in &config.workflow.version_bump_units {
        if !known.contains(unit) {
            return Err(GeneratorError::usage(format!(
                "workflow.version_bump_units names unknown unit: {unit}"
            )));
        }
        if !version_bump_units.insert(unit) {
            return Err(GeneratorError::usage(format!(
                "workflow.version_bump_units contains duplicate unit: {unit}"
            )));
        }
    }
    Ok(config.clone())
}

pub(crate) fn is_unit_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && value.as_bytes()[0].is_ascii_lowercase()
}

/// The event kind admitted to the schema-1 planner. Parsing happens at the
/// boundary so later routing cannot replace an event with a look-alike string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanEvent {
    PullRequest,
    Push,
    Schedule,
    MergeGroup,
    WorkflowDispatch,
    Local,
}

impl PlanEvent {
    fn from_env() -> Result<Self, GeneratorError> {
        match env::var("EVENT_NAME").unwrap_or_default().as_str() {
            "pull_request" => Ok(Self::PullRequest),
            "push" => Ok(Self::Push),
            "schedule" => Ok(Self::Schedule),
            "merge_group" => Ok(Self::MergeGroup),
            "workflow_dispatch" => Ok(Self::WorkflowDispatch),
            "" => Ok(Self::Local),
            other => Err(GeneratorError::usage(format!(
                "unsupported CI event `{other}`"
            ))),
        }
    }

    const fn scope_name(self) -> &'static str {
        match self {
            Self::PullRequest => "pull_request",
            Self::Push => "push",
            Self::Schedule => "schedule",
            Self::MergeGroup => "merge_group",
            Self::WorkflowDispatch => "workflow_dispatch",
            Self::Local => "",
        }
    }
}

/// Everything `plan` reads from its environment, as one injectable bundle.
/// Production builds it from the live process; tests build fixture values,
/// so the end-to-end plan path runs without mutating process-global env —
/// which parallel tests also read.
struct PlanInputs {
    root: PathBuf,
    event: PlanEvent,
    scope_override: Option<String>,
    base: String,
    head: String,
    lanes: String,
    selection_file: Option<PathBuf>,
    expected_file: Option<PathBuf>,
    github_output: Option<PathBuf>,
}

impl PlanInputs {
    fn from_env() -> Result<Self, GeneratorError> {
        Ok(Self {
            event: PlanEvent::from_env()?,
            scope_override: env::var("CI_SCOPE_OVERRIDE")
                .ok()
                .filter(|value| !value.is_empty()),
            root: env::current_dir()
                .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?,
            base: env::var("BASE_SHA").unwrap_or_default(),
            head: env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned()),
            lanes: env::var("VELNOR_LANES").unwrap_or_default(),
            selection_file: env::var_os("VELNOR_SELECTION_FILE").map(PathBuf::from),
            expected_file: env::var_os("VELNOR_EXPECTED_WORK_FILE").map(PathBuf::from),
            github_output: env::var_os("GITHUB_OUTPUT").map(PathBuf::from),
        })
    }
}

fn plan(config_path: &Path) -> Result<(), GeneratorError> {
    let inputs = PlanInputs::from_env()?;
    plan_with(config_path, &inputs)
}

/// Run the planner against explicit inputs: `plan`'s whole body behind an
/// injectable environment, so tests drive file writing and outputs exactly.
fn plan_with(config_path: &Path, inputs: &PlanInputs) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    let scope = match scope_for_event_values(
        inputs.event.scope_name(),
        inputs.scope_override.as_deref(),
    )? {
        Some(value) => Scope::parse(&value)?,
        None => Scope::Full,
    };
    let lanes = plan_lanes_for_value(&inputs.lanes)?;
    let selection = selection_for_lanes(
        &config,
        selection_for_diff(&inputs.root, &config, scope, &inputs.base, &inputs.head)?,
        lanes,
    );
    let units = selection
        .units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let full_units = selection
        .full_units
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(",");
    // Before any artifact escapes: an unproven empty selection must fail
    // here, not after writing a no-work file for it.
    let no_work = planned_no_work_reason(&selection)?;
    if let Some(path) = &inputs.selection_file {
        write_selection_file(path, &inputs.base, &inputs.head, scope, &units, &full_units)?;
    }
    if let Some(path) = &inputs.expected_file {
        write_expected_work_file(path, &selection, lanes, &inputs.base, &inputs.head)?;
    }
    if let Some(output_path) = &inputs.github_output {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output_path)
            .map_err(|error| GeneratorError::io("open GitHub output", output_path, &error))?;
        writeln!(file, "scope={}", scope_name(scope))
            .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
        writeln!(file, "base_sha={}", inputs.base)
            .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
        writeln!(file, "head_sha={}", inputs.head)
            .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
        writeln!(file, "units={units}")
            .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
        writeln!(file, "full_units={full_units}")
            .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
        if let Some(reason) = &selection.fallback_reason {
            writeln!(file, "fallback_reason={reason}")
                .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
        }
        if let Some(reason) = &no_work {
            writeln!(file, "planned_no_work=true")
                .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
            writeln!(file, "no_work_reason={reason}")
                .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
        }
        write_kind_matrices(&mut file, &config, &selection, output_path)?;
    }
    println!("scope={}", scope_name(scope));
    println!("units={units}");
    println!("full_units={full_units}");
    if let Some(reason) = &selection.fallback_reason {
        println!("fallback_reason={reason}");
    }
    if let Some(reason) = &no_work {
        println!("planned_no_work=true");
        println!("no_work_reason={reason}");
    }
    Ok(())
}

fn write_kind_matrices(
    file: &mut fs::File,
    config: &CiConfig,
    selection: &UnitSelection<'_>,
    output_path: &Path,
) -> Result<(), GeneratorError> {
    let selected = selection
        .units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut matrices: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
    for unit in &config.unit {
        matrices.entry(unit_matrix_output(unit)).or_default();
    }
    for unit in &config.unit {
        if !selected.contains(unit.id.as_str()) {
            continue;
        }
        let label = if unit.label.is_empty() {
            unit.id.clone()
        } else {
            unit.label.clone()
        };
        matrices
            .entry(unit_matrix_output(unit))
            .or_default()
            .push(serde_json::json!({ "unit": unit.id, "label": label }));
    }
    for (matrix_output, entries) in matrices {
        let json = serde_json::to_string(&entries).map_err(|error| {
            GeneratorError::usage(format!("serialize {matrix_output}: {error}"))
        })?;
        writeln!(file, "{matrix_output}={json}")
            .map_err(|error| GeneratorError::io("write GitHub output", output_path, &error))?;
    }
    Ok(())
}

fn unit_matrix_output(unit: &CiUnit) -> String {
    format!("{}_matrix", unit.kind.as_str())
}

pub(crate) fn scope_for_event_values(
    event: &str,
    override_scope: Option<&str>,
) -> Result<Option<String>, GeneratorError> {
    match event {
        // Merge-queue validation is a trusted event like push and schedule:
        // the ephemeral merge ref has no pull_request base to diff against,
        // so it always runs full scope.
        "push" | "schedule" | "merge_group" => {
            if override_scope.is_some_and(|scope| scope != "full") {
                return Err(GeneratorError::usage(
                    "trusted events require full CI scope",
                ));
            }
            Ok(Some("full".to_owned()))
        }
        "workflow_dispatch" => match override_scope {
            None | Some("full") => Ok(Some("full".to_owned())),
            Some("affected") => Ok(Some("affected".to_owned())),
            Some(other) => Err(GeneratorError::usage(format!(
                "unsupported CI scope override `{other}`"
            ))),
        },
        "pull_request" => Ok(override_scope
            .map(ToOwned::to_owned)
            .or_else(|| Some("affected".to_owned()))),
        "" => Ok(override_scope.map(ToOwned::to_owned)),
        other => Err(GeneratorError::usage(format!(
            "unsupported CI event `{other}`"
        ))),
    }
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Affected => "affected",
        Scope::Full => "full",
    }
}

/// The lanes admitted for this plan, from the `VELNOR_LANES` environment the
/// plan step sets to the dispatch `runner` input (or the configured lanes
/// on automatic events). Absent or empty means `both`: local runs and
/// renders that predate lane scoping plan without lane filtering, exactly
/// as before.
fn plan_lanes_for_value(value: &str) -> Result<RunnerMode, GeneratorError> {
    match value.trim() {
        "" | "both" => Ok(RunnerMode::Both),
        "velnor" => Ok(RunnerMode::Velnor),
        "github" => Ok(RunnerMode::Github),
        other => Err(GeneratorError::usage(format!(
            "unsupported CI lanes `{other}`: use velnor, github, or both"
        ))),
    }
}

/// Narrow a diff selection to the units the admitted `lanes` can execute.
/// A velnor-only dispatch drops whatever the Velnor lane cannot run (Swift
/// units); the required gate already tolerates `skipped` for unselected
/// units, so the excluded callers stay green instead of failing a selection
/// they can never satisfy. Units of an unknown kind are kept: dropping a
/// unit the planner does not recognize would silently skip verification.
///
/// A both-lane plan keeps pairable kinds. A GitHub-only kind (Swift) stays
/// selected so an explicit `jobs = ["github"]` opt-out still runs on the
/// hosted lane when the plan admits GitHub.
fn selection_for_lanes<'a>(
    config: &'a CiConfig,
    selection: UnitSelection<'a>,
    lanes: RunnerMode,
) -> UnitSelection<'a> {
    let kinds = config
        .unit
        .iter()
        .map(|unit| (unit.id.as_str(), unit.kind.as_str()))
        .collect::<BTreeMap<_, _>>();
    let supported = |kind: &str| {
        UnitKind::from_prefix(kind).is_none_or(|parsed| {
            if lanes_support_unit_kind(lanes, parsed) {
                return true;
            }
            lanes == RunnerMode::Both
                && (crate::lane_supports_unit_kind(RunnerMode::Github, parsed)
                    || crate::lane_supports_unit_kind(RunnerMode::Velnor, parsed))
        })
    };
    let incoming_empty = selection.units.is_empty();
    let units: Vec<&'a CiUnit> = selection
        .units
        .into_iter()
        .filter(|unit| supported(unit.kind.as_str()))
        .collect();
    // Lane admission narrowing a real selection to zero is no-work for its
    // own reason — nothing runnable, not nothing affected. A recorded
    // no-work reason survives narrowing untouched.
    let no_work_reason = if units.is_empty() {
        selection.no_work_reason.or(if incoming_empty {
            None
        } else {
            Some("no selected workload unit runs on the admitted lanes".to_owned())
        })
    } else {
        None
    };
    UnitSelection {
        units,
        full_units: selection
            .full_units
            .into_iter()
            .filter(|id| kinds.get(id.as_str()).is_none_or(|kind| supported(kind)))
            .collect(),
        fallback_reason: selection.fallback_reason,
        no_work_reason,
    }
}

/// The explicit no-work reason for a finished selection: the planner's
/// recorded reason when it selected nothing, and `None` for real-work
/// plans. An empty selection without a recorded reason is a plan error,
/// never a silent pass: every selection arm that can go empty must prove
/// why, so no future arm can silently become a no-work PASS. `plan` checks
/// this before writing any artifact and surfaces the reason beside the
/// `planned_no_work` marker; the aggregate binds it to its explicit
/// no-work pass.
fn planned_no_work_reason(selection: &UnitSelection<'_>) -> Result<Option<String>, GeneratorError> {
    if !selection.units.is_empty() {
        return Ok(None);
    }
    selection.no_work_reason.clone().map_or_else(
        || {
            Err(GeneratorError::usage(
                "plan selected zero workload units without a recorded no-work reason",
            ))
        },
        |reason| Ok(Some(reason)),
    )
}

#[cfg(test)]
mod scope_event_tests {
    use super::{scope_for_event_values, PlanEvent};
    use crate::GeneratorError;

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must<T>(result: Result<T, GeneratorError>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> GeneratorError {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error,
        }
    }

    #[test]
    fn typed_event_kind_cannot_be_an_arbitrary_runtime_string() {
        assert_eq!(PlanEvent::PullRequest.scope_name(), "pull_request");
        assert_eq!(PlanEvent::MergeGroup.scope_name(), "merge_group");
        assert_eq!(PlanEvent::Local.scope_name(), "");
    }

    #[test]
    fn merge_group_is_a_trusted_full_scope_event() {
        assert_eq!(
            must(scope_for_event_values("merge_group", None), "resolve scope").as_deref(),
            Some("full")
        );
        assert_eq!(
            must(
                scope_for_event_values("merge_group", Some("full")),
                "resolve explicit full scope"
            )
            .as_deref(),
            Some("full")
        );
        let error = must_fail(
            scope_for_event_values("merge_group", Some("affected")),
            "merge_group must reject a narrowed scope",
        );
        assert!(
            error
                .to_string()
                .contains("trusted events require full CI scope"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn unknown_events_are_rejected_and_empty_keeps_local_passthrough() {
        let error = must_fail(
            scope_for_event_values("release", None),
            "unknown events must fail closed",
        );
        assert!(
            error.to_string().contains("unsupported CI event `release`"),
            "unexpected error: {error}"
        );
        assert_eq!(
            must(scope_for_event_values("", None), "resolve empty scope"),
            None
        );
        assert_eq!(
            must(
                scope_for_event_values("", Some("affected")),
                "resolve empty override"
            )
            .as_deref(),
            Some("affected")
        );
    }
}

#[cfg(test)]
mod runner_lane_tests {
    use super::{collect_manifests, expand_affected_units, CiUnit, RunnerLane, Scope};
    use std::path::Path;

    #[test]
    fn github_is_the_default_lane_and_velnor_backend_selects_velnor() {
        assert_eq!(RunnerLane::from_execution_backend(None), RunnerLane::Github);
        assert_eq!(
            RunnerLane::from_execution_backend(Some("github-hosted")),
            RunnerLane::Github
        );
        assert_eq!(
            RunnerLane::from_execution_backend(Some("docker")),
            RunnerLane::Velnor
        );
        assert_eq!(
            RunnerLane::from_execution_backend(Some("MICROVM")),
            RunnerLane::Velnor
        );
        assert_eq!(
            RunnerLane::Github,
            RunnerLane::from_execution_backend(Some("self-hosted"))
        );
        assert_eq!(
            RunnerLane::Github,
            RunnerLane::from_execution_backend(Some("unknown"))
        );
    }

    #[test]
    fn lane_and_scope_select_the_matching_command_array() {
        let unit = CiUnit {
            id: "docker".to_owned(),
            label: "Docker".to_owned(),
            kind: "docker".to_owned(),
            root: ".".to_owned(),
            watch: vec!["Dockerfile".to_owned()],
            github_pr_commands: vec!["github-pr".to_owned()],
            github_full_commands: vec!["github-full".to_owned()],
            velnor_pr_commands: vec!["velnor-pr".to_owned()],
            velnor_full_commands: vec!["velnor-full".to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        assert_eq!(
            unit.commands(RunnerLane::Github, Scope::Affected),
            &["github-pr".to_owned()]
        );
        assert_eq!(
            unit.commands(RunnerLane::Github, Scope::Full),
            &["github-full".to_owned()]
        );
        assert_eq!(
            unit.commands(RunnerLane::Velnor, Scope::Affected),
            &["velnor-pr".to_owned()]
        );
        assert_eq!(
            unit.commands(RunnerLane::Velnor, Scope::Full),
            &["velnor-full".to_owned()]
        );
    }

    #[test]
    fn affected_closure_does_not_follow_dependents_of_added_prerequisites() {
        let units = vec![
            CiUnit {
                id: "base".to_owned(),
                label: "base".to_owned(),
                kind: "rust".to_owned(),
                root: ".".to_owned(),
                watch: Vec::new(),
                github_pr_commands: vec!["base".to_owned()],
                github_full_commands: vec!["base".to_owned()],
                velnor_pr_commands: vec!["base".to_owned()],
                velnor_full_commands: vec!["base".to_owned()],
                phases: Vec::new(),
                check_commands: Vec::new(),
                depends_on: Vec::new(),
                tool_version: None,
                cache: None,
                workspace_check: false,
            },
            CiUnit {
                id: "changed".to_owned(),
                label: "changed".to_owned(),
                kind: "rust".to_owned(),
                root: ".".to_owned(),
                watch: Vec::new(),
                github_pr_commands: vec!["changed".to_owned()],
                github_full_commands: vec!["changed".to_owned()],
                velnor_pr_commands: vec!["changed".to_owned()],
                velnor_full_commands: vec!["changed".to_owned()],
                phases: Vec::new(),
                check_commands: Vec::new(),
                depends_on: vec!["base".to_owned()],
                tool_version: None,
                cache: None,
                workspace_check: false,
            },
            CiUnit {
                id: "sibling".to_owned(),
                label: "sibling".to_owned(),
                kind: "rust".to_owned(),
                root: ".".to_owned(),
                watch: Vec::new(),
                github_pr_commands: vec!["sibling".to_owned()],
                github_full_commands: vec!["sibling".to_owned()],
                velnor_pr_commands: vec!["sibling".to_owned()],
                velnor_full_commands: vec!["sibling".to_owned()],
                phases: Vec::new(),
                check_commands: Vec::new(),
                depends_on: vec!["base".to_owned()],
                tool_version: None,
                cache: None,
                workspace_check: false,
            },
            CiUnit {
                id: "leaf".to_owned(),
                label: "leaf".to_owned(),
                kind: "rust".to_owned(),
                root: ".".to_owned(),
                watch: Vec::new(),
                github_pr_commands: vec!["leaf".to_owned()],
                github_full_commands: vec!["leaf".to_owned()],
                velnor_pr_commands: vec!["leaf".to_owned()],
                velnor_full_commands: vec!["leaf".to_owned()],
                phases: Vec::new(),
                check_commands: Vec::new(),
                depends_on: vec!["changed".to_owned()],
                tool_version: None,
                cache: None,
                workspace_check: false,
            },
        ];
        let selected = expand_affected_units(&units, ["changed".to_owned()].into_iter().collect());
        assert_eq!(
            selected,
            ["base", "changed", "leaf"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    }

    #[test]
    fn affected_closure_follows_cross_kind_depends_on_edges() {
        let unit = |id: &str, kind: &str, depends_on: &[&str]| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: kind.to_owned(),
            root: ".".to_owned(),
            watch: Vec::new(),
            github_pr_commands: vec!["true".to_owned()],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["true".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: depends_on.iter().map(|value| (*value).to_owned()).collect(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        // A Swift consumer of a Rust FFI producer: the closure is kind-blind,
        // so an FFI change selects the Swift unit without naming its kind.
        let units = [
            unit("rust-ffi", "rust", &[]),
            unit("swift-app", "swift", &["rust-ffi"]),
            unit("unrelated", "rust", &[]),
        ];
        let selected = expand_affected_units(&units, ["rust-ffi".to_owned()].into_iter().collect());
        assert_eq!(
            selected,
            ["rust-ffi", "swift-app"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    }

    #[test]
    fn ffi_transitive_inputs_select_the_cross_language_consumer() {
        // Generation compiles a prerequisite edge into `depends_on`, so the
        // existing transitive closure carries producer changes to the
        // consumer: a change to the FFI crate's own dependency selects the
        // FFI crate and, through it, the Swift consumer.
        let unit = |id: &str, kind: &str, depends_on: &[&str]| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: kind.to_owned(),
            root: ".".to_owned(),
            watch: Vec::new(),
            github_pr_commands: vec![id.to_owned()],
            github_full_commands: vec![id.to_owned()],
            velnor_pr_commands: vec![id.to_owned()],
            velnor_full_commands: vec![id.to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: depends_on.iter().map(|name| (*name).to_owned()).collect(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        let units = vec![
            unit("rust-base", "rust", &[]),
            unit("rust-ffi", "rust", &["rust-base"]),
            unit("swift-app", "swift", &["rust-ffi"]),
        ];
        let selected =
            expand_affected_units(&units, ["rust-base".to_owned()].into_iter().collect());
        assert_eq!(
            selected,
            ["rust-base", "rust-ffi", "swift-app"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    }

    #[test]
    fn crate_test_collection_matches_scanner_fixture_exclusions() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut manifests = Vec::new();
        assert!(collect_manifests(root, root, &mut manifests).is_ok());
        assert!(manifests.iter().all(|path| {
            path.strip_prefix(root).is_ok_and(|relative| {
                !super::super::is_test_support_path(&relative.to_string_lossy())
            })
        }));
    }
}

pub(crate) fn run_units(
    root: &Path,
    config_path: &Path,
    scope: Scope,
    only_unit: Option<&str>,
    phase: Option<ValidationPhase>,
) -> Result<(), GeneratorError> {
    let selection_file = env::var_os("VELNOR_SELECTION_FILE").map_or_else(
        || root.join(".velnor-ci-selection/velnor-ci-selection"),
        PathBuf::from,
    );
    run_units_with_selection_file(root, config_path, scope, only_unit, phase, &selection_file)
}

/// Whether an event is trusted: push, schedule, and merge-queue validation
/// always run full scope, mirroring [`scope_for_event_values`]. Unit jobs
/// re-check the plan-time verdict so a narrowed job scope can never execute
/// under a trusted event.
fn event_requires_full_scope(event: &str) -> bool {
    matches!(event, "push" | "schedule" | "merge_group")
}

pub(crate) fn run_units_with_selection_file(
    root: &Path,
    config_path: &Path,
    scope: Scope,
    only_unit: Option<&str>,
    phase: Option<ValidationPhase>,
    selection_file: &Path,
) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    if event_requires_full_scope(&env::var("EVENT_NAME").unwrap_or_default())
        && scope != Scope::Full
    {
        return Err(GeneratorError::usage(
            "trusted events require full CI scope",
        ));
    }
    let selection = read_selection_file(selection_file)?;
    validate_selection_sha(&selection)?;
    if selection.scope != scope {
        return Err(GeneratorError::usage(format!(
            "CI selection artifact scope mismatch: plan={} job={}",
            scope_name(selection.scope),
            scope_name(scope)
        )));
    }
    let selected = ordered_units(&config.unit, Some(&selection.units))?;
    if selected.len() != selection.units.len() {
        return Err(GeneratorError::usage(
            "CI selection artifact names an unknown unit",
        ));
    }
    if selection
        .full_units
        .iter()
        .any(|unit| !selection.units.contains(unit))
    {
        return Err(GeneratorError::usage(
            "CI selection artifact marks an unselected unit as full",
        ));
    }
    let full_units = selection.full_units;
    let selected = select_units_for_job(selected, only_unit)?;
    if selected.is_empty() {
        // `select_units_for_job` already failed closed when a unit was
        // requested: reaching here means nothing was asked for and the
        // validated, SHA-bound artifact selects nothing. That is planner
        // success with no work, reported explicitly — never silent, never
        // an error.
        println!("{}", no_work_report_line());
        return Ok(());
    }
    run_layers(root, &selected, scope, &full_units, phase)
}

/// The machine-readable line a validated empty selection reports instead of
/// succeeding silently.
fn no_work_report_line() -> &'static str {
    "no_work_reason=CI selection artifact selects zero workload units; nothing to run"
}

fn select_units_for_job<'a>(
    selected: Vec<&'a CiUnit>,
    only_unit: Option<&str>,
) -> Result<Vec<&'a CiUnit>, GeneratorError> {
    match only_unit {
        Some(id) => {
            if !selected.iter().any(|unit| unit.id == id) {
                return Err(GeneratorError::usage(format!(
                    "CI selection artifact does not include requested unit `{id}`"
                )));
            }
            Ok(selected.into_iter().filter(|unit| unit.id == id).collect())
        }
        None => Ok(selected),
    }
}

struct UnitSelection<'a> {
    units: Vec<&'a CiUnit>,
    full_units: BTreeSet<String>,
    /// Why a fallback widened the selection: the full set, or the opaque
    /// subset an unmatched path conservatively selects. `None` for
    /// requested-full, proven-narrow, and empty selections; surfaced
    /// additively by `plan`.
    fallback_reason: Option<String>,
    /// Why the planner selected zero workload units. `Some` exactly when
    /// `units` is empty for a proven no-work plan; `None` for real-work
    /// plans. The aggregate binds this reason to its explicit no-work pass.
    no_work_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedSelection {
    base_sha: String,
    head_sha: String,
    scope: Scope,
    units: BTreeSet<String>,
    full_units: BTreeSet<String>,
}

pub(crate) const SELECTION_FILE_VERSION: &str = "1";

/// Write the planner's expected-work file: the aggregate's binding to this
/// plan, in the [`crate::reuse::ExpectedWorkFile`] schema. The aggregate
/// scores exactly this document against the collected results, so the writer
/// mirrors the runner's own rules:
///
/// - lanes: every admitted lane supporting the unit's kind, in canonical
///   github-then-velnor order — one verdict per lane caller, matching what
///   the required check already scores. A selected unit with no lane fails
///   the plan closed: it is contradictory, not empty.
/// - matrix: always empty. Units carry no per-unit matrix in the plan
///   model, and an empty matrix means exactly one unmatrixed item — the
///   unit's single verdict on that lane — never zero, never many.
/// - required: always true. A selected, admitted unit must succeed.
/// - `planned_skip`: never set. The planner removes units from the selection
///   instead of pre-skipping them; a reported skip still needs the
///   planner's recorded reason to hold.
/// - prerequisites: the in-plan `depends_on` edges only. Out-of-plan
///   prerequisites neither gate the runner (`run_layers` filters edges
///   outside the executed set) nor enter the aggregate's graph, so the
///   aggregate scores the same graph the runner executed. An in-plan
///   prerequisite that did not pass fails its dependents closed.
/// - `planned_no_work`: set exactly when the selection is empty, with the
///   [`planned_no_work_reason`] on the plan's outputs beside it.
/// - `base_sha`/`head_sha`: the plan's transport identity, exactly as the
///   plan saw it. The aggregate compares these against its own checkout and
///   rejects any file from another plan — including a stale no-work file.
fn write_expected_work_file(
    path: &Path,
    selection: &UnitSelection<'_>,
    lanes: RunnerMode,
    base_sha: &str,
    head_sha: &str,
) -> Result<(), GeneratorError> {
    let selected: BTreeSet<&str> = selection
        .units
        .iter()
        .map(|unit| unit.id.as_str())
        .collect();
    let mut units = Vec::with_capacity(selection.units.len());
    let mut prerequisites = BTreeMap::new();
    for unit in &selection.units {
        units.push(serde_json::json!({
            "id": unit.id,
            "lanes": expected_lanes_for_unit(unit, lanes)?,
            "matrix": Vec::<String>::new(),
            "required": true,
        }));
        prerequisites.insert(
            unit.id.clone(),
            unit.depends_on
                .iter()
                .map(String::as_str)
                .filter(|dependency| selected.contains(dependency))
                .collect::<Vec<_>>(),
        );
    }
    let document = serde_json::json!({
        "planned_no_work": selection.units.is_empty(),
        "units": units,
        "prerequisites": prerequisites,
        "base_sha": base_sha,
        "head_sha": head_sha,
    });
    let text = serde_json::to_string_pretty(&document)
        .map_err(|error| GeneratorError::usage(format!("serialize expected work: {error}")))?;
    // The plan job runs in a fresh checkout with no parent directory, so
    // the writer creates its own instead of relying on a pre-existing dir.
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            GeneratorError::io("create expected work directory", parent, &error)
        })?;
    }
    fs::write(path, format!("{text}\n"))
        .map_err(|error| GeneratorError::io("write expected work", path, &error))
}

/// The aggregate lanes one selected unit must report: the admitted plan
/// lanes supporting its kind, in canonical github-then-velnor order.
/// Unknown kinds report on every admitted lane — the planner keeps units it
/// does not recognize, so the aggregate must hear from all of them.
fn expected_lanes_for_unit(
    unit: &CiUnit,
    lanes: RunnerMode,
) -> Result<Vec<&'static str>, GeneratorError> {
    let lanes = [
        (RunnerMode::Github, "github"),
        (RunnerMode::Velnor, "velnor"),
    ]
    .into_iter()
    .filter(|(lane, _)| {
        (lanes == RunnerMode::Both || lanes == *lane)
            && UnitKind::from_prefix(unit.kind.as_str())
                .is_none_or(|parsed| crate::lane_supports_unit_kind(*lane, parsed))
    })
    .map(|(_, name)| name)
    .collect::<Vec<_>>();
    if lanes.is_empty() {
        return Err(GeneratorError::usage(format!(
            "CI unit `{}` is selected but runnable on no admitted lane",
            unit.id
        )));
    }
    Ok(lanes)
}

fn write_selection_file(
    path: &Path,
    base_sha: &str,
    head_sha: &str,
    scope: Scope,
    units: &str,
    full_units: &str,
) -> Result<(), GeneratorError> {
    let contents = format!(
        "version={SELECTION_FILE_VERSION}\nbase_sha={base_sha}\nhead_sha={head_sha}\nscope={}\nunits={units}\nfull_units={full_units}\n",
        scope_name(scope)
    );
    // A nested selection path must not need a pre-existing directory.
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| GeneratorError::io("create CI selection directory", parent, &error))?;
    }
    fs::write(path, contents)
        .map_err(|error| GeneratorError::io("write CI selection", path, &error))
}

fn read_selection_file(path: &Path) -> Result<PlannedSelection, GeneratorError> {
    let contents = fs::read_to_string(path)
        .map_err(|error| GeneratorError::io("read CI selection", path, &error))?;
    let mut fields = BTreeMap::new();
    for line in contents.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| GeneratorError::usage("malformed CI selection artifact"))?;
        if fields.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(GeneratorError::usage(
                "CI selection artifact contains duplicate fields",
            ));
        }
    }
    if fields.remove("version").as_deref() != Some(SELECTION_FILE_VERSION) {
        return Err(GeneratorError::usage(
            "unsupported CI selection artifact version",
        ));
    }
    let base_sha = fields
        .remove("base_sha")
        .ok_or_else(|| GeneratorError::usage("CI selection artifact is missing base_sha"))?;
    let head_sha = fields
        .remove("head_sha")
        .ok_or_else(|| GeneratorError::usage("CI selection artifact is missing head_sha"))?;
    let scope = Scope::parse(
        &fields
            .remove("scope")
            .ok_or_else(|| GeneratorError::usage("CI selection artifact is missing scope"))?,
    )?;
    let units = parse_selection_ids(
        &fields
            .remove("units")
            .ok_or_else(|| GeneratorError::usage("CI selection artifact is missing units"))?,
    )?;
    let full_units =
        parse_selection_ids(&fields.remove("full_units").ok_or_else(|| {
            GeneratorError::usage("CI selection artifact is missing full_units")
        })?)?;
    if !fields.is_empty() {
        return Err(GeneratorError::usage(
            "CI selection artifact contains unknown fields",
        ));
    }
    Ok(PlannedSelection {
        base_sha,
        head_sha,
        scope,
        units,
        full_units,
    })
}

fn parse_selection_ids(value: &str) -> Result<BTreeSet<String>, GeneratorError> {
    let mut ids = BTreeSet::new();
    for value in value.split(',').filter(|value| !value.is_empty()) {
        if !is_unit_id(value) {
            return Err(GeneratorError::usage(format!(
                "invalid unit id in CI selection artifact: {value}"
            )));
        }
        if !ids.insert(value.to_owned()) {
            return Err(GeneratorError::usage(format!(
                "duplicate unit id in CI selection artifact: {value}"
            )));
        }
    }
    Ok(ids)
}

fn validate_selection_sha(selection: &PlannedSelection) -> Result<(), GeneratorError> {
    let job_base = env::var("BASE_SHA").unwrap_or_default();
    let job_head = env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned());
    // workflow_dispatch leaves BASE_SHA empty; plan already wrote the resolved
    // base into the artifact. Unit jobs consume that plan and must not re-diff.
    // A non-empty job BASE_SHA still has to match. HEAD always has to match.
    let head_mismatch = selection.head_sha != job_head;
    let base_mismatch = !job_base.is_empty() && selection.base_sha != job_base;
    if head_mismatch || base_mismatch {
        println!(
            "::warning::CI selection artifact SHA mismatch: plan base SHA `{}` vs job base SHA `{}`; plan head SHA `{}` vs job head SHA `{}`",
            selection.base_sha, job_base, selection.head_sha, job_head
        );
        return Err(GeneratorError::usage(
            "CI selection artifact does not match this job checkout",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn selected_units_for_diff<'a>(
    root: &Path,
    config: &'a CiConfig,
    scope: Scope,
    base: &str,
    head: &str,
) -> Result<Vec<&'a CiUnit>, GeneratorError> {
    Ok(selection_for_diff(root, config, scope, base, head)?.units)
}

/// The no-work reason for an affected selection that classified to empty.
/// Unknown paths fail closed to full before classification runs, so every
/// remaining path was irrelevant to every workload unit, or release-scoped,
/// and emptiness here is proven no-work. `None` for real-work selections.
fn empty_selection_no_work_reason(selected: &BTreeSet<String>) -> Option<String> {
    selected
        .is_empty()
        .then_some("no changed path selected a workload unit".to_owned())
}

#[allow(
    clippy::too_many_lines,
    reason = "selection dispatch carries the no-work reason channel on every arm"
)]
fn selection_for_diff<'a>(
    root: &Path,
    config: &'a CiConfig,
    scope: Scope,
    base: &str,
    head: &str,
) -> Result<UnitSelection<'a>, GeneratorError> {
    if scope == Scope::Full {
        return full_selection(config, None);
    }
    if base.is_empty() || base.chars().all(|character| character == '0') {
        return full_selection(config, Some("no affected base; fell back to full"));
    }
    let Some(raw) = git_name_status_nul(root, base, head)? else {
        return full_selection(config, Some("git diff unavailable; fell back to full"));
    };
    let Some(changed) = parse_name_status_nul(&raw) else {
        return full_selection(config, Some("unparseable change entry; fell back to full"));
    };
    if changed.is_empty() {
        return Ok(UnitSelection {
            units: Vec::new(),
            full_units: BTreeSet::new(),
            fallback_reason: None,
            no_work_reason: Some("empty diff selects no workload units".to_owned()),
        });
    }
    if version_bump_matches(
        root,
        base,
        head,
        &changed,
        &config.unit,
        &config.workflow.version_bump_units,
    )? {
        let allowlist = config
            .workflow
            .version_bump_units
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut selected = allowlist.clone();
        extend_workspace_checks_for_cargo_roots(config, &mut selected);
        return Ok(UnitSelection {
            units: ordered_units(&config.unit, Some(&selected))?,
            full_units: selected,
            fallback_reason: None,
            no_work_reason: None,
        });
    }
    let watched: Vec<crate::reuse::WatchedUnit> = config
        .unit
        .iter()
        .map(|unit| crate::reuse::WatchedUnit {
            id: unit.id.clone(),
            watch: unit.watch.clone(),
            depends_on: unit.depends_on.clone(),
            kind: unit.kind.clone(),
            commands: unit
                .github_pr_commands
                .iter()
                .chain(&unit.github_full_commands)
                .chain(&unit.velnor_pr_commands)
                .chain(&unit.velnor_full_commands)
                .cloned()
                .collect(),
        })
        .collect();
    let compiled = crate::reuse::compile_ownership(&watched)?;
    let mut selected = BTreeSet::new();
    let mut opaque_reasons: Vec<String> = Vec::new();
    for file in &changed {
        if let Some(verdict) = crate::reuse::github_verdict(file) {
            match verdict {
                crate::reuse::GithubVerdict::Global { reason }
                | crate::reuse::GithubVerdict::Unknown { reason } => {
                    return full_selection(config, Some(&reason));
                }
                crate::reuse::GithubVerdict::Kind { kind } => {
                    selected.extend(
                        config
                            .unit
                            .iter()
                            .filter(|unit| unit.kind.eq_ignore_ascii_case(&kind))
                            .map(|unit| unit.id.clone()),
                    );
                }
                crate::reuse::GithubVerdict::ReleaseScope => {}
            }
            continue;
        }
        if let Some(reason) =
            fold_affected_file(&compiled, file, &mut selected, &mut opaque_reasons)
        {
            return full_selection(config, Some(&reason));
        }
    }
    extend_workspace_checks_for_cargo_roots(config, &mut selected);
    let (selected, full_units) = expand_affected_units_with_full(&config.unit, selected);
    let no_work_reason = empty_selection_no_work_reason(&selected);
    Ok(UnitSelection {
        units: ordered_units(&config.unit, Some(&selected))?,
        full_units,
        fallback_reason: crate::reuse::join_opaque_reasons(opaque_reasons),
        no_work_reason,
    })
}

/// Fold one non-contract changed file into the affected set: owned units join
/// directly, opaque-narrowed units join with their reason recorded, and an
/// unprovable path returns its full-fallback reason for the caller to honor.
fn fold_affected_file(
    compiled: &[crate::reuse::CompiledUnit<'_>],
    file: &str,
    selected: &mut BTreeSet<String>,
    opaque_reasons: &mut Vec<String>,
) -> Option<String> {
    match crate::reuse::classify_path(compiled, file) {
        crate::reuse::PathVerdict::Owned { units } => {
            selected.extend(units);
            None
        }
        crate::reuse::PathVerdict::Opaque { units, reason } => {
            selected.extend(units);
            opaque_reasons.push(reason);
            None
        }
        crate::reuse::PathVerdict::Unknown { reason } => Some(reason),
        crate::reuse::PathVerdict::Irrelevant => None,
    }
}

/// Select only the workspace checks that share a Cargo lockfile root with a
/// directly selected Rust unit. This keeps independent Cargo projects
/// affected-only while preserving the workspace gate for each matching root.
fn extend_workspace_checks_for_cargo_roots(config: &CiConfig, selected: &mut BTreeSet<String>) {
    let selected_cargo_roots = config
        .unit
        .iter()
        .filter(|unit| {
            selected.contains(&unit.id) && unit.kind == "rust" && !unit.is_workspace_check()
        })
        .map(CiUnit::cargo_lockfile_root)
        .collect::<BTreeSet<_>>();
    if selected_cargo_roots.is_empty() {
        return;
    }
    selected.extend(
        config
            .unit
            .iter()
            .filter(|unit| {
                unit.is_workspace_check()
                    && selected_cargo_roots.contains(unit.cargo_lockfile_root())
            })
            .map(|unit| unit.id.clone()),
    );
}

fn full_selection<'a>(
    config: &'a CiConfig,
    fallback_reason: Option<&str>,
) -> Result<UnitSelection<'a>, GeneratorError> {
    Ok(UnitSelection {
        units: ordered_units(&config.unit, None)?,
        full_units: config.unit.iter().map(|unit| unit.id.clone()).collect(),
        fallback_reason: fallback_reason.map(ToOwned::to_owned),
        no_work_reason: None,
    })
}

fn version_bump_matches(
    root: &Path,
    base: &str,
    head: &str,
    changed: &[String],
    units: &[CiUnit],
    allowlist: &[String],
) -> Result<bool, GeneratorError> {
    if allowlist.is_empty() || changed.is_empty() {
        return Ok(false);
    }
    let independent_lockfiles = units
        .iter()
        .filter(|unit| unit.kind == "rust" && allowlist.iter().any(|allowed| allowed == &unit.id))
        .filter_map(|unit| {
            let cargo_root = unit.cargo_lockfile_root();
            (cargo_root != ".").then(|| format!("{cargo_root}/Cargo.lock"))
        })
        .collect::<BTreeSet<_>>();
    let mut diff_files = Vec::new();
    for file in changed {
        if file == "Cargo.lock" || independent_lockfiles.contains(file) {
            diff_files.push(file.clone());
            continue;
        }
        let Some(crate_name) = file
            .strip_prefix("crates/")
            .and_then(|value| value.strip_suffix("/Cargo.toml"))
        else {
            return Ok(false);
        };
        let unit_id = format!("rust-{crate_name}");
        if !allowlist.iter().any(|allowed| allowed == &unit_id) {
            return Ok(false);
        }
        diff_files.push(file.clone());
    }
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["diff", "--unified=0"])
        .arg(format!("{base}...{head}"))
        .arg("--");
    for file in &diff_files {
        command.arg(file);
    }
    let output = command
        .output()
        .map_err(|error| GeneratorError::usage(format!("run version diff: {error}")))?;
    if !output.status.success() {
        return Ok(false);
    }
    let mut changed_lines = 0;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let Some(content) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) else {
            continue;
        };
        changed_lines += 1;
        if !content.trim_start().starts_with("version = \"") {
            return Ok(false);
        }
    }
    Ok(changed_lines > 0)
}

fn expand_affected_units_with_full(
    units: &[CiUnit],
    changed: BTreeSet<String>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut affected = changed.clone();
    let mut pending = changed.into_iter().collect::<Vec<_>>();
    while let Some(changed_id) = pending.pop() {
        for unit in units {
            if unit
                .depends_on
                .iter()
                .any(|dependency| dependency == &changed_id)
                && affected.insert(unit.id.clone())
            {
                pending.push(unit.id.clone());
            }
        }
    }
    let full_units = affected.clone();
    let mut required = affected;
    let mut pending = required.iter().cloned().collect::<Vec<_>>();
    while let Some(unit_id) = pending.pop() {
        let Some(unit) = units.iter().find(|unit| unit.id == unit_id) else {
            continue;
        };
        for dependency in &unit.depends_on {
            if required.insert(dependency.clone()) {
                pending.push(dependency.clone());
            }
        }
    }
    (required, full_units)
}

#[cfg(test)]
fn expand_affected_units(units: &[CiUnit], changed: BTreeSet<String>) -> BTreeSet<String> {
    expand_affected_units_with_full(units, changed).0
}

/// Collect the plan-path change list as raw `git diff --name-status -M -z`
/// bytes. `-z` NUL-delimits entries so unusual filenames (spaces, quotes,
/// unicode, newlines, glob metacharacters) arrive exactly instead of
/// C-quoted; `-M` reports renames with both sides so the matcher sees the
/// old and the new owner atomically with collection. `None` means git
/// failed: the caller falls back to full, never to empty.
fn git_name_status_nul(
    root: &Path,
    base: &str,
    head: &str,
) -> Result<Option<Vec<u8>>, GeneratorError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--name-status", "-M", "-z"])
        .arg(format!("{base}...{head}"))
        .output()
        .map_err(|error| GeneratorError::usage(format!("run git diff: {error}")))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(output.stdout))
}

/// Parse `--name-status -z` bytes into the matchable path list, mirroring the
/// select path's rename/delete semantics: a rename contributes its target
/// then its source (both owners match), a copy contributes its target only,
/// and a delete keeps its path (its owner still matches). `None` means the
/// input is truncated, malformed, or non-UTF-8: the caller falls back to
/// full instead of matching a mangled name.
fn parse_name_status_nul(output: &[u8]) -> Option<Vec<String>> {
    if output.is_empty() {
        return Some(Vec::new());
    }
    let text = std::str::from_utf8(output).ok()?;
    let mut records = text.split('\0');
    let mut changed = Vec::new();
    loop {
        let status = records.next()?;
        if status.is_empty() {
            // The trailing NUL terminates the stream: anything after it is a
            // malformed record, not an empty diff.
            return records.next().is_none().then_some(changed);
        }
        if let Some(score) = status.strip_prefix('R') {
            if score.is_empty() || !score.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let from = records.next()?;
            let to = records.next()?;
            if from.is_empty() || to.is_empty() {
                return None;
            }
            changed.push(to.to_owned());
            changed.push(from.to_owned());
        } else if let Some(score) = status.strip_prefix('C') {
            if score.is_empty() || !score.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let from = records.next()?;
            let to = records.next()?;
            if from.is_empty() || to.is_empty() {
                return None;
            }
            changed.push(to.to_owned());
        } else {
            if !matches!(status, "A" | "M" | "T" | "D") {
                return None;
            }
            let path = records.next()?;
            if path.is_empty() {
                return None;
            }
            changed.push(path.to_owned());
        }
    }
}

fn ordered_units<'a>(
    units: &'a [CiUnit],
    selected: Option<&BTreeSet<String>>,
) -> Result<Vec<&'a CiUnit>, GeneratorError> {
    let wanted = units
        .iter()
        .filter(|unit| selected.is_none_or(|ids| ids.contains(&unit.id)))
        .collect::<Vec<_>>();
    let wanted_ids = wanted
        .iter()
        .map(|unit| unit.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut emitted = BTreeSet::new();
    let mut result = Vec::with_capacity(wanted.len());
    while result.len() < wanted.len() {
        let mut progress = false;
        for unit in &wanted {
            if emitted.contains(&unit.id)
                || unit.depends_on.iter().any(|dependency| {
                    wanted_ids.contains(dependency.as_str()) && !emitted.contains(dependency)
                })
            {
                continue;
            }
            emitted.insert(unit.id.clone());
            result.push(*unit);
            progress = true;
        }
        if !progress {
            return Err(GeneratorError::usage(
                "CI dependency graph contains a cycle",
            ));
        }
    }
    Ok(result)
}

fn run_layers(
    root: &Path,
    units: &[&CiUnit],
    run_scope: Scope,
    full_units: &BTreeSet<String>,
    phase: Option<ValidationPhase>,
) -> Result<(), GeneratorError> {
    let runner_lane = RunnerLane::current();
    let mut finished = BTreeSet::new();
    while finished.len() < units.len() {
        let ready = units
            .iter()
            .filter(|unit| {
                !finished.contains(&unit.id)
                    && unit
                        .depends_on
                        .iter()
                        .filter(|dependency| {
                            units.iter().any(|candidate| candidate.id == **dependency)
                        })
                        .all(|dependency| finished.contains(dependency))
            })
            .copied()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err(GeneratorError::usage(
                "CI dependency graph contains a cycle or invalid ordering",
            ));
        }
        // Resolve every command list before spawning: a `--phase` selection
        // failure aborts the layer instead of running a partial tier.
        let mut workloads = Vec::with_capacity(ready.len());
        for unit in ready {
            let commands = if full_units.contains(&unit.id) {
                match phase {
                    Some(selected) => unit.commands_for_phase(runner_lane, run_scope, selected)?,
                    None => unit.commands(runner_lane, run_scope).to_vec(),
                }
            } else {
                prerequisite_commands(unit, runner_lane, run_scope, phase)?
            };
            workloads.push((unit, commands));
        }
        let (sender, receiver) = mpsc::channel();
        thread::scope(|thread_scope| {
            for (unit, commands) in workloads {
                let sender = sender.clone();
                thread_scope.spawn(move || {
                    let result = run_unit(root, unit, &commands);
                    let _ = sender.send((unit.id.clone(), result));
                });
            }
        });
        drop(sender);
        for (id, result) in receiver {
            result?;
            finished.insert(id);
        }
    }
    Ok(())
}

fn prerequisite_commands(
    unit: &CiUnit,
    lane: RunnerLane,
    scope: Scope,
    phase: Option<ValidationPhase>,
) -> Result<Vec<String>, GeneratorError> {
    match phase {
        None | Some(ValidationPhase::Fmt | ValidationPhase::Check) => {
            if unit.phases.is_empty() {
                // Units without phase tags — TOML the phase model predates,
                // declared units, regen-gated units — keep the base
                // clippy→check rewrite instead of silently no-oping.
                Ok(legacy_prerequisite_commands(unit, lane, scope))
            } else {
                unit.commands_for_phase(lane, scope, ValidationPhase::Check)
            }
        }
        // The prerequisite tier compiles the unit once, in the first
        // validation step; later phase steps no-op.
        Some(_) => Ok(Vec::new()),
    }
}

/// The prerequisite rewrite for units without phase tags: rust units
/// compile via their clippy command rewritten to `check`. Non-rust units
/// have no prerequisite. Byte-identical to the pre-phase behavior.
fn legacy_prerequisite_commands(unit: &CiUnit, lane: RunnerLane, scope: Scope) -> Vec<String> {
    if unit.kind != "rust" {
        return Vec::new();
    }
    unit.commands(lane, scope)
        .iter()
        .filter(|command| command.contains(" clippy "))
        .map(|command| {
            let command = command
                .replacen(" clippy ", " check ", 1)
                .replace(" -- -D warnings", "");
            if lane == RunnerLane::Velnor {
                // `mbx check` does not expose Cargo's `--no-deps` flag.
                command.replace(" --no-deps", "")
            } else {
                command
            }
        })
        .collect()
}

fn run_unit(root: &Path, unit: &CiUnit, commands: &[String]) -> Result<(), GeneratorError> {
    let limits = crate::exec::RunLimits::from_env();
    for command in commands {
        println!("::group::{}: {}", unit.id, command);
        let started = Instant::now();
        let outcome = crate::exec::run_command(root, &unit.id, command, &limits)
            .map_err(GeneratorError::usage);
        println!(
            "velnor: unit {} command finished in {:.1}s",
            unit.id,
            started.elapsed().as_secs_f64()
        );
        println!("::endgroup::");
        outcome?;
    }
    Ok(())
}

pub(crate) fn test_crates(
    root: &Path,
    config_path: &Path,
    cargo_program: Option<&Path>,
) -> Result<(), GeneratorError> {
    let config = read_config(config_path)?;
    let mut manifests = Vec::new();
    collect_manifests(root, root, &mut manifests)?;
    manifests.sort();
    let locked = root.join("Cargo.lock").is_file();
    let nextest = config
        .analysis
        .detected
        .iter()
        .any(|item| item == "cargo-nextest-policy");
    for manifest in manifests {
        let contents = fs::read_to_string(&manifest)
            .map_err(|error| GeneratorError::io("read Cargo manifest", &manifest, &error))?;
        let value: toml::Value = toml::from_str(&contents).map_err(|error| {
            GeneratorError::usage(format!(
                "parse Cargo manifest {}: {error}",
                manifest.display()
            ))
        })?;
        if !value.get("package").is_some_and(toml::Value::is_table) {
            continue;
        }
        println!("::group::cargo tests: {}", manifest.display());
        let default_cargo =
            if env::var(super::MR_BOXINGTON_ENABLED_ENV).is_ok_and(|value| value == "1") {
                Path::new("mbx")
            } else {
                Path::new("cargo")
            };
        let mut command = Command::new(cargo_program.unwrap_or(default_cargo));
        command
            .arg(if nextest { "nextest" } else { "test" })
            .arg(if nextest { "run" } else { "--all-features" });
        if nextest {
            command.arg("--all-features");
        }
        if locked {
            command.arg("--locked");
        }
        command.arg("--manifest-path").arg(&manifest);
        let status = command
            .current_dir(root)
            .status()
            .map_err(|error| GeneratorError::usage(format!("run Cargo tests: {error}")))?;
        println!("::endgroup::");
        if !status.success() {
            return Err(GeneratorError::usage(format!(
                "Cargo tests failed: {}",
                manifest.display()
            )));
        }
    }
    Ok(())
}

fn collect_manifests(
    root: &Path,
    directory: &Path,
    manifests: &mut Vec<PathBuf>,
) -> Result<(), GeneratorError> {
    for entry in fs::read_dir(directory)
        .map_err(|error| GeneratorError::io("read CI source directory", directory, &error))?
    {
        let entry = entry
            .map_err(|error| GeneratorError::usage(format!("read directory entry: {error}")))?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if entry
            .file_type()
            .map_err(|error| GeneratorError::usage(format!("inspect {}: {error}", path.display())))?
            .is_dir()
        {
            if matches!(relative.to_str(), Some(".git" | "target")) {
                continue;
            }
            collect_manifests(root, &path, manifests)?;
        } else if path.file_name().is_some_and(|name| name == "Cargo.toml")
            && relative
                .to_str()
                .is_none_or(|path| !super::is_test_support_path(path))
        {
            manifests.push(path);
        }
    }
    Ok(())
}

fn release(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let Some(command) = arguments.first().and_then(|value| value.to_str()) else {
        return Err(GeneratorError::usage(
            "usage: release verify-tag | release package-binary | release package-deb | release package-guest | release verify-feed | release update-feed | release apt-resolve-commit | release apt-fetch | release apt-verify | release apt-publish | release apt-previous-pointer | release apt-channel-update | release apt-deploy-guard | release verify-digests | release resolve-mode | release resolve-source | release admit-producer | release assemble-manifest",
        ));
    };
    match command {
        "verify-tag" => verify_tag(&arguments[1..]),
        "package-binary" => package_binary(&arguments[1..]),
        "package-deb" => package_deb(&arguments[1..]),
        "package-guest" => package_guest(&arguments[1..]),
        "verify-feed" => verify_feed(&arguments[1..]),
        "update-feed" => update_feed(&arguments[1..]),
        "apt-resolve-commit" => apt_resolve_commit(&arguments[1..]),
        "apt-fetch" => apt_fetch(&arguments[1..]),
        "apt-verify" => apt_verify(&arguments[1..]),
        "apt-publish" => apt_publish(&arguments[1..]),
        "apt-previous-pointer" => apt_previous_pointer(&arguments[1..]),
        "apt-channel-update" => apt_channel_update(&arguments[1..]),
        "apt-deploy-guard" => apt_deploy_guard(&arguments[1..]),
        "verify-digests" => verify_digests(&arguments[1..]),
        "resolve-mode" => resolve_mode(&arguments[1..]),
        "resolve-source" => resolve_source(&arguments[1..]),
        "admit-producer" => admit_producer(&arguments[1..]),
        "assemble-manifest" => assemble_manifest(&arguments[1..]),
        _ => Err(GeneratorError::usage(format!(
            "unsupported release command: {command}"
        ))),
    }
}

/// Parse an explicit boolean flag value (`true`/`false`), failing closed on
/// anything else. Flag-only booleans do not exist: every option takes a
/// value, so `true` is always spelled out.
fn flag_bool(options: &BTreeMap<String, String>, name: &str) -> Result<bool, GeneratorError> {
    match options.get(name).map(String::as_str) {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(value) => Err(GeneratorError::usage(format!(
            "--{name} must be `true` or `false`, found `{value}`"
        ))),
    }
}

fn apt_resolve_commit(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["source-repo", "version"])?;
    let source = required_option(&options, "source-repo")?;
    let version = required_option(&options, "version")?;
    let commit = crate::apt::run_resolve_commit(source, version, None)?;
    println!("{commit}");
    Ok(())
}

fn apt_fetch(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &["suite", "source-repo", "package", "version", "dir"],
    )?;
    let suite = crate::apt::Suite::parse(required_option(&options, "suite")?)?;
    let source = required_option(&options, "source-repo")?;
    let package = required_option(&options, "package")?;
    let version = required_option(&options, "version")?;
    let dir = required_option(&options, "dir")?;
    crate::apt::run_fetch(suite, source, package, version, Path::new(dir), None)?;
    println!("fetched {} coherence inputs for {version}", suite.as_str());
    Ok(())
}

fn apt_verify(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "suite",
            "source-repo",
            "package",
            "binary",
            "identity-dir",
            "manifest-schema",
            "version",
            "incoming",
            "commit",
            "signer",
            "expect-signer",
            "verify-oci",
        ],
    )?;
    let inputs = crate::apt::VerifyInputs {
        suite: crate::apt::Suite::parse(required_option(&options, "suite")?)?,
        source_repo: required_option(&options, "source-repo")?.to_owned(),
        package: required_option(&options, "package")?.to_owned(),
        binary: required_option(&options, "binary")?.to_owned(),
        manifest_schema: required_option(&options, "manifest-schema")?.to_owned(),
        identity_dir: required_option(&options, "identity-dir")?.to_owned(),
        version: required_option(&options, "version")?.to_owned(),
        commit: options.get("commit").cloned(),
        incoming: Path::new(required_option(&options, "incoming")?),
        signer_live: required_option(&options, "signer")?.to_owned(),
        signer_pinned: required_option(&options, "expect-signer")?.to_owned(),
        verify_oci: flag_bool(&options, "verify-oci")?,
        backend: crate::apt::DebBackend::Auto,
        path_overlay: None,
    };
    crate::apt::verify_suite(&inputs)?;
    println!("{} feed inputs are coherent", inputs.suite.as_str());
    Ok(())
}

fn apt_publish(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "suite",
            "source-repo",
            "package",
            "binary",
            "consumer-repo",
            "manifest-schema",
            "signer",
            "passphrase-env",
            "key-env",
            "keyring",
            "origin",
            "identity-dir",
            "feed-url",
            "description",
            "version",
            "incoming",
            "prev-dir",
            "previous-pointer",
            "staging",
            "bootstrap",
        ],
    )?;
    // The publish boundary validates the whole contract — including fields
    // this step does not consume — so a misrendered feed fails closed.
    let spec = crate::ReleaseSpec {
        kind: "apt".to_owned(),
        package: required_option(&options, "package")?.to_owned(),
        packages: Vec::new(),
        binary: required_option(&options, "binary")?.to_owned(),
        targets: Vec::new(),
        image: String::new(),
        image_package: String::new(),
        source_repository: required_option(&options, "source-repo")?.to_owned(),
        consumer_repository: required_option(&options, "consumer-repo")?.to_owned(),
        artifact_path: String::new(),
        description: required_option(&options, "description")?.to_owned(),
        manifest_schema: required_option(&options, "manifest-schema")?.to_owned(),
        apt_arches: Vec::new(),
        signer_fingerprint: required_option(&options, "signer")?.to_owned(),
        passphrase_secret: required_option(&options, "passphrase-env")?.to_owned(),
        signing_key_secret: required_option(&options, "key-env")?.to_owned(),
        keyring_path: required_option(&options, "keyring")?.to_owned(),
        apt_origin: required_option(&options, "origin")?.to_owned(),
        apt_identity_dir: required_option(&options, "identity-dir")?.to_owned(),
        apt_feed_url: required_option(&options, "feed-url")?.to_owned(),
        retention: 0,
        dockerfile: String::new(),
        context: String::new(),
        platforms: Vec::new(),
        producer_workflow: String::new(),
        producer_conclusion: String::new(),
        modes: Vec::new(),
        archive_members: Vec::new(),
        archive_checksum: String::new(),
        archive_retention_days: 0,
        credentials: Vec::new(),
        tag_pattern: String::new(),
        registry: String::new(),
        registry_username_secret: String::new(),
        registry_password_secret: String::new(),
        jobs: Vec::new(),
    };
    let contract = crate::apt::AptContract::resolve(&spec)?;
    let passphrase_env = required_option(&options, "passphrase-env")?.to_owned();
    let passphrase = std::env::var(&passphrase_env).ok();
    let key_env = required_option(&options, "key-env")?.to_owned();
    let key_material = std::env::var(&key_env).ok();
    let empty_prev;
    let prev_dir = match options.get("prev-dir") {
        Some(dir) if !dir.is_empty() => {
            empty_prev = PathBuf::from(dir);
            Some(empty_prev.as_path())
        }
        _ => None,
    };
    let inputs = crate::apt::PublishInputs {
        suite: crate::apt::Suite::parse(required_option(&options, "suite")?)?,
        contract,
        version: required_option(&options, "version")?.to_owned(),
        incoming: Path::new(required_option(&options, "incoming")?),
        prev_dir,
        previous_pointer: Path::new(required_option(&options, "previous-pointer")?),
        staging: Path::new(required_option(&options, "staging")?),
        bootstrap: flag_bool(&options, "bootstrap")?,
        passphrase_env,
        passphrase,
        key_env,
        key_material,
        backend: crate::apt::DebBackend::Auto,
        path_overlay: None,
    };
    crate::apt::publish_suite(&inputs)?;
    println!("{} suite staged", inputs.suite.as_str());
    Ok(())
}

fn apt_previous_pointer(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "suite",
            "published",
            "prior",
            "candidate",
            "candidate-sha",
            "bootstrap",
        ],
    )?;
    let suite = crate::apt::Suite::parse(required_option(&options, "suite")?)?;
    let bootstrap = flag_bool(&options, "bootstrap")?;
    let pointer = match suite {
        crate::apt::Suite::Stable => {
            if bootstrap {
                return Err(GeneratorError::usage(
                    "previous pointer: --bootstrap applies only to --suite preview",
                ));
            }
            let published = required_option(&options, "published")?;
            let bytes = fs::read(published)
                .map_err(|error| GeneratorError::io("read", Path::new(published), &error))?;
            let document: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
                GeneratorError::usage(format!("published record is not valid JSON: {error}"))
            })?;
            crate::apt::derive_previous_pointer(
                &document,
                required_option(&options, "prior")?,
                required_option(&options, "candidate")?,
                required_option(&options, "candidate-sha")?,
            )?
        }
        crate::apt::Suite::Preview => {
            if bootstrap {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(crate::apt::PREVIEW_TAG.to_owned())
            }
        }
    };
    println!("{pointer}");
    Ok(())
}

fn apt_channel_update(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "suite",
            "source-repo",
            "source-ref",
            "commit",
            "version",
            "package",
            "manifest",
            "staging",
        ],
    )?;
    let inputs = crate::apt::ChannelUpdateInputs {
        suite: crate::apt::Suite::parse(required_option(&options, "suite")?)?,
        source_repo: required_option(&options, "source-repo")?.to_owned(),
        source_ref: required_option(&options, "source-ref")?.to_owned(),
        commit: required_option(&options, "commit")?.to_owned(),
        version: required_option(&options, "version")?.to_owned(),
        package: required_option(&options, "package")?.to_owned(),
        manifest: Path::new(required_option(&options, "manifest")?),
        staging: Path::new(required_option(&options, "staging")?),
    };
    crate::apt::run_channel_update(&inputs)?;
    println!("{} channel state updated", inputs.suite.as_str());
    Ok(())
}

fn apt_deploy_guard(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["suite", "staged", "live-version"])?;
    let suite = crate::apt::Suite::parse(required_option(&options, "suite")?)?;
    let staged = required_option(&options, "staged")?;
    let staged_text = fs::read_to_string(Path::new(staged).join(suite.last_publish_file()))
        .map_err(|error| {
            GeneratorError::io("read the staged last-publish", Path::new(staged), &error)
        })?;
    let live = required_option(&options, "live-version")?;
    let live = match live.trim() {
        "" | "unknown" => None,
        version => Some(version),
    };
    crate::apt::check_deploy_guard(suite, &staged_text, live)?;
    println!("{} deploy guard passed", suite.as_str());
    Ok(())
}

fn verify_tag(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["branch", "package"])?;
    let reference = env::var("GITHUB_REF").unwrap_or_default();
    let tag = reference
        .strip_prefix("refs/tags/v")
        .filter(|value| is_semver(value))
        .ok_or_else(|| GeneratorError::usage("release requires a semver v* tag"))?;
    if env::var("GITHUB_REF_TYPE").unwrap_or_else(|_| "tag".to_owned()) != "tag" {
        return Err(GeneratorError::usage("release ref is not a tag"));
    }
    let branch = options
        .get("branch")
        .map(String::as_str)
        .ok_or_else(|| GeneratorError::usage("release requires --branch"))?;
    if !valid_branch(branch) {
        return Err(GeneratorError::usage("invalid release branch"));
    }
    let branch_reference = format!("refs/remotes/origin/{branch}");
    let tag_commit = git_revision(&reference)?;
    let branch_commit = git_revision(&branch_reference)?;
    if tag_commit != branch_commit {
        return Err(GeneratorError::usage(format!(
            "release tag must equal current origin/{branch} tip"
        )));
    }
    if let Some(package) = options.get("package") {
        if !valid_package(package) {
            return Err(GeneratorError::usage("invalid release package"));
        }
        let metadata = Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
            .output()
            .map_err(|error| GeneratorError::usage(format!("cargo metadata failed: {error}")))?;
        if !metadata.status.success() {
            return Err(GeneratorError::usage("cargo metadata failed"));
        }
        let document: serde_json::Value = serde_json::from_slice(&metadata.stdout)
            .map_err(|error| GeneratorError::usage(format!("parse cargo metadata: {error}")))?;
        let found = document["packages"].as_array().is_some_and(|packages| {
            packages.iter().any(|item| {
                item["name"].as_str() == Some(package) && item["version"].as_str() == Some(tag)
            })
        });
        if !found {
            return Err(GeneratorError::usage(
                "release tag version does not match the declared package",
            ));
        }
    }
    println!("{tag}");
    Ok(())
}

fn package_binary(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "target",
            "version",
            "package",
            "binary",
            "members",
            "deterministic",
        ],
    )?;
    let target = required_option(&options, "target")?;
    let version = required_option(&options, "version")?;
    let package = required_option(&options, "package")?;
    let binary = required_option(&options, "binary")?;
    if !valid_target(target)
        || !is_artifact_version(version)
        || !valid_package(package)
        || !valid_binary(binary)
    {
        return Err(GeneratorError::usage(
            "invalid target, version, package, or binary",
        ));
    }
    let members = package_archive_members(&options, binary)?;
    let deterministic = package_deterministic(&options)?;
    let root = env::current_dir()
        .map_err(|error| GeneratorError::usage(format!("resolve CI root: {error}")))?;
    let source = root
        .join("target")
        .join(target)
        .join("release")
        .join(binary);
    if !source.is_file() {
        return Err(GeneratorError::usage(format!(
            "built binary is missing: {}",
            source.display()
        )));
    }
    let directory = source
        .parent()
        .map_or_else(|| root.clone(), Path::to_path_buf);
    for member in &members {
        if !directory.join(member).is_file() {
            return Err(GeneratorError::usage(format!(
                "declared archive member is missing: {member}"
            )));
        }
    }
    let dist = root.join("dist");
    fs::create_dir_all(&dist)
        .map_err(|error| GeneratorError::io("create release directory", &dist, &error))?;
    let archive = dist.join(format!("{binary}-{version}-{target}.tar.gz"));
    // A deterministic archive is byte-reproducible: sorted entries, a fixed
    // mtime, normalized ownership, and a timestamp-free gzip stream. Without
    // the flag the lane keeps its historical `tar -czf` bytes exactly. The
    // normalizing flags are GNU tar's; any other tar fails closed with
    // guidance instead of shipping a silently skewed archive.
    if deterministic {
        require_gnu_tar()?;
        write_deterministic_archive(&directory, binary, &members, &archive)?;
    } else {
        let status = Command::new("tar")
            .arg("-C")
            .arg(&directory)
            .arg("-czf")
            .arg(&archive)
            .arg(binary)
            .args(&members)
            .status()
            .map_err(|error| GeneratorError::usage(format!("package binary: {error}")))?;
        if !status.success() {
            return Err(GeneratorError::usage("tar failed while packaging binary"));
        }
    }
    let digest = sha256_file(&archive)?;
    // Append, never `with_extension`: the sidecar sits next to its subject
    // as `<subject>.sha256`, the name consumer lanes download it under.
    let checksum = archive.with_file_name(format!(
        "{}.sha256",
        archive
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ));
    fs::write(
        &checksum,
        format!(
            "{digest}  {}\n",
            archive
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
        ),
    )
    .map_err(|error| GeneratorError::io("write release checksum", &checksum, &error))?;
    println!("{}", archive.display());
    Ok(())
}

/// The declared archive members: portable names that must exist beside the
/// built binary, never the binary itself and never a path.
fn package_archive_members(
    options: &BTreeMap<String, String>,
    binary: &str,
) -> Result<Vec<String>, GeneratorError> {
    match options.get("members").map(String::as_str) {
        None | Some("") => Ok(Vec::new()),
        Some(list) => {
            let mut members = Vec::new();
            for member in list.split(',') {
                if !valid_archive_member(member) || member == binary {
                    return Err(GeneratorError::usage(format!(
                        "invalid archive member: {member}"
                    )));
                }
                members.push(member.to_owned());
            }
            Ok(members)
        }
    }
}

/// Whether the lane packages reproducibly. Rendered lanes pass an explicit
/// `true` or `false` per target row; anything else fails closed.
fn package_deterministic(options: &BTreeMap<String, String>) -> Result<bool, GeneratorError> {
    match options.get("deterministic").map(String::as_str) {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(other) => Err(GeneratorError::usage(format!(
            "invalid --deterministic value: {other}"
        ))),
    }
}

/// Write one reproducible archive: GNU tar normalizes entry order, mtime,
/// and ownership onto stdout, and `gzip -n` strips the timestamp. The
/// caller probes for GNU tar first.
fn write_deterministic_archive(
    directory: &Path,
    binary: &str,
    members: &[String],
    archive: &Path,
) -> Result<(), GeneratorError> {
    let mut command = Command::new("tar");
    command.arg("-C").arg(directory).args([
        "--sort=name",
        "--mtime=@0",
        "--owner=0",
        "--group=0",
        "--numeric-owner",
        "-cf",
        "-",
        binary,
    ]);
    command.args(members);
    let tar = command
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| GeneratorError::usage(format!("package binary: {error}")))?;
    let Some(tar_stdout) = tar.stdout else {
        return Err(GeneratorError::usage("tar produced no archive stream"));
    };
    let output = Command::new("gzip")
        .arg("-n")
        .stdin(tar_stdout)
        .output()
        .map_err(|error| GeneratorError::usage(format!("package binary: {error}")))?;
    if !output.status.success() {
        return Err(GeneratorError::usage("gzip failed while packaging binary"));
    }
    fs::write(archive, output.stdout)
        .map_err(|error| GeneratorError::io("write release archive", archive, &error))?;
    Ok(())
}

fn package_deb(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "package",
            "version",
            "guest",
            "target",
            "no-build",
            "asset-name",
        ],
    )?;
    let package = required_option(&options, "package")?;
    let version = required_option(&options, "version")?;
    if !valid_package(package) || !is_artifact_version(version) {
        return Err(GeneratorError::usage("invalid package or version"));
    }
    // A release lane that builds its binaries itself (identity features the
    // packager cannot pass through) tells the packager to skip cargo-deb's
    // own default-feature rebuild.
    let skip_build = match options.get("no-build").map(String::as_str) {
        None => false,
        Some("true") => true,
        Some(other) => {
            return Err(GeneratorError::usage(format!(
                "invalid --no-build value: {other}"
            )));
        }
    };
    if let Some(name) = options.get("asset-name")
        && !valid_deb_asset_name(name)
    {
        return Err(GeneratorError::usage("invalid deb asset name"));
    }
    if let Some(guest) = options.get("guest") {
        let guest = Path::new(guest);
        if !guest.is_dir() {
            return Err(GeneratorError::usage(format!(
                "guest payload directory is missing: {}",
                guest.display()
            )));
        }
        let dest = cargo_package_manifest_dir(package)?.join("release/microvm");
        copy_dir_files(guest, &dest)?;
    }
    if let Some(target) = options.get("target")
        && !valid_target(target)
    {
        return Err(GeneratorError::usage("invalid package target"));
    }
    let status = cargo_deb_command(
        package,
        version,
        skip_build,
        options.get("target").map(String::as_str),
    )
    .status()
    .map_err(|error| GeneratorError::usage(format!("cargo deb: {error}")))?;
    if !status.success() {
        return Err(GeneratorError::usage("cargo deb failed"));
    }
    collect_debian_packages(
        options.get("target").map(String::as_str),
        options.get("asset-name").map(String::as_str),
    )
}

/// The `cargo deb` invocation: `--output` pins the canonical dir cargo-deb
/// already defaults to, which disables the back-compat hard-link twin it
/// otherwise drops under `target/<triple>/debian/` for a single `--target`.
fn cargo_deb_command(
    package: &str,
    version: &str,
    skip_build: bool,
    target: Option<&str>,
) -> Command {
    let mut command = Command::new("cargo");
    command.args([
        "deb",
        "--no-strip",
        "--package",
        package,
        "--deb-version",
        version,
    ]);
    if skip_build {
        command.arg("--no-build");
    }
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    command.args(["--output", "target/debian"]);
    command
}

fn cargo_package_manifest_dir(package: &str) -> Result<PathBuf, GeneratorError> {
    let metadata = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .output()
        .map_err(|error| GeneratorError::usage(format!("cargo metadata failed: {error}")))?;
    if !metadata.status.success() {
        return Err(GeneratorError::usage("cargo metadata failed"));
    }
    let document: serde_json::Value = serde_json::from_slice(&metadata.stdout)
        .map_err(|error| GeneratorError::usage(format!("parse cargo metadata: {error}")))?;
    document["packages"]
        .as_array()
        .and_then(|packages| {
            packages.iter().find_map(|item| {
                (item["name"].as_str() == Some(package)).then(|| {
                    item["manifest_path"]
                        .as_str()
                        .map(PathBuf::from)
                        .and_then(|path| path.parent().map(Path::to_path_buf))
                })
            })
        })
        .flatten()
        .ok_or_else(|| GeneratorError::usage(format!("cargo package `{package}` was not found")))
}

fn copy_dir_files(src: &Path, dest: &Path) -> Result<(), GeneratorError> {
    fs::create_dir_all(dest)
        .map_err(|error| GeneratorError::io("create guest package directory", dest, &error))?;
    let entries =
        fs::read_dir(src).map_err(|error| GeneratorError::io("read guest payload", src, &error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| GeneratorError::io("read guest payload entry", src, &error))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| GeneratorError::io("stat guest payload entry", &from, &error))?;
        if file_type.is_dir() {
            copy_dir_files(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .map_err(|error| GeneratorError::io("copy guest payload file", &to, &error))?;
        }
    }
    Ok(())
}

fn collect_debian_packages(
    target: Option<&str>,
    asset_name: Option<&str>,
) -> Result<(), GeneratorError> {
    let mut sources = vec![PathBuf::from("target/debian")];
    if let Some(target) = target {
        sources.push(PathBuf::from("target").join(target).join("debian"));
    }
    collect_debian_packages_from(&sources, Path::new("dist"), asset_name)
}

fn collect_debian_packages_from(
    sources: &[PathBuf],
    dist: &Path,
    asset_name: Option<&str>,
) -> Result<(), GeneratorError> {
    fs::create_dir_all(dist)
        .map_err(|error| GeneratorError::io("create release directory", dist, &error))?;
    let mut found = Vec::new();
    for source in sources {
        let Ok(entries) = fs::read_dir(source) else {
            continue;
        };
        for entry in entries {
            let entry =
                entry.map_err(|error| GeneratorError::io("read debian output", source, &error))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("deb") {
                continue;
            }
            if path.file_name().is_none() {
                continue;
            }
            found.push(path);
        }
    }
    let mut found = dedup_debian_packages(found);
    found.sort();
    if found.is_empty() {
        return Err(GeneratorError::usage(
            "cargo deb produced no .deb under target/debian",
        ));
    }
    // A lane that names its consumer asset renames exactly one freshly built
    // deb; anything else is a stale corpus and fails closed instead of
    // publishing an ambiguous pick.
    if let Some(asset_name) = asset_name {
        if found.len() != 1 {
            let listed = found
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(GeneratorError::usage(format!(
                "expected exactly one .deb to rename, found {}: {listed}",
                found.len()
            )));
        }
        let destination = dist.join(asset_name);
        fs::copy(&found[0], &destination)
            .map_err(|error| GeneratorError::io("copy debian package", &found[0], &error))?;
        write_bare_digest_sidecar(&destination)?;
        println!("{}", destination.display());
        return Ok(());
    }
    for path in &found {
        let Some(name) = path.file_name() else {
            continue;
        };
        let destination = dist.join(name);
        fs::copy(path, &destination)
            .map_err(|error| GeneratorError::io("copy debian package", path, &error))?;
        write_bare_digest_sidecar(&destination)?;
        println!("{}", destination.display());
    }
    Ok(())
}

/// Collapse scan hits by file name, keeping the first root's copy: a
/// back-compat twin is the same package seen in both scan roots, not a
/// second package. Distinct file names still fail closed downstream.
fn dedup_debian_packages(found: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    found
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| seen.insert(name.to_owned()))
        })
        .collect()
}

/// The bare-digest sidecar a consumer lane cross-checks against its own
/// manifest: the digest alone, so both `sha256sum --check` corpora and
/// first-token readers accept it.
fn write_bare_digest_sidecar(artifact: &Path) -> Result<(), GeneratorError> {
    let digest = sha256_file(artifact)?;
    let Some(name) = artifact.file_name().and_then(|name| name.to_str()) else {
        return Err(GeneratorError::usage("deb asset name is not portable"));
    };
    let sidecar = artifact.with_file_name(format!("{name}.sha256"));
    fs::write(&sidecar, format!("{digest}\n"))
        .map_err(|error| GeneratorError::io("write deb checksum", &sidecar, &error))?;
    Ok(())
}

/// The canonical artifact-download flags: fail closed on HTTP errors,
/// resume partial files, retry transient failures with backoff inside a
/// bounded window, and bound connect and total time. Every artifact fetch
/// the generator emits or the runtime runs carries exactly this set, so one
/// audit covers all of them. Two deliberate exceptions keep their own
/// bounds: the producer-outcome fetch (`--retry 0` under a bounded Rust
/// loop, so retries stay countable) and the docs live-probe (a bounded
/// shell loop around a single-shot `--max-time` check).
pub(crate) const CURL_DOWNLOAD_FLAGS: &str = "--fail --show-error --silent --location --http1.1 --continue-at - --retry 20 --retry-all-errors --retry-delay 5 --retry-max-time 1800 --connect-timeout 30 --max-time 900";

/// The guest kernel download script: fetch the pinned tarball with the
/// canonical bounded-download flags and verify its digest before the build
/// consumes it. Extracted so tests pin the flags without network.
fn guest_kernel_download_script() -> String {
    format!(
        r#"
set -euo pipefail
url="$(jq -er '.kernel_tarball' microvm/pins.json)"
sha="$(jq -er '.kernel_tarball_sha256' microvm/pins.json)"
curl {CURL_DOWNLOAD_FLAGS} \
  -o linux.tar.xz "$url"
echo "$sha  linux.tar.xz" | sha256sum -c -
"#,
    )
}

fn package_guest(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["arch", "package", "bin", "agent"])?;
    let arch = required_option(&options, "arch")?;
    if !matches!(arch, "x86_64" | "aarch64") {
        return Err(GeneratorError::usage(
            "guest arch must be x86_64 or aarch64",
        ));
    }
    let pins = Path::new("microvm/pins.json");
    if pins.is_file() {
        let package = required_option(&options, "package")?;
        let bin = required_option(&options, "bin")?;
        let agent = required_option(&options, "agent")?;
        if !valid_package(package) || !valid_binary(bin) || !valid_binary(agent) {
            return Err(GeneratorError::usage(
                "invalid guest package, bin, or agent",
            ));
        }
        let status = Command::new("bash")
            .arg("-c")
            .arg(guest_kernel_download_script())
            .status()
            .map_err(|error| GeneratorError::usage(format!("download guest kernel: {error}")))?;
        if !status.success() {
            return Err(GeneratorError::usage("guest kernel download failed"));
        }
        fs::create_dir_all("dist/microvm").map_err(|error| {
            GeneratorError::io("create guest output", Path::new("dist/microvm"), &error)
        })?;
        let status = Command::new("cargo")
            .args([
                "run",
                "--locked",
                "--release",
                "--package",
                package,
                "--bin",
                bin,
                "--",
                "build",
                "--arch",
                arch,
                "--out",
                "dist/microvm",
                "--tarball",
                "linux.tar.xz",
                "--guest-agent",
                agent,
            ])
            .status()
            .map_err(|error| GeneratorError::usage(format!("build guest image: {error}")))?;
        if !status.success() {
            return Err(GeneratorError::usage("guest image build failed"));
        }
        return Ok(());
    }
    let script = Path::new("microvm/build.sh");
    if !script.is_file() {
        return Err(GeneratorError::usage(
            "guest image requires microvm/pins.json or a repository-owned microvm/build.sh",
        ));
    }
    let status = Command::new("bash")
        .args(["microvm/build.sh", arch])
        .status()
        .map_err(|error| GeneratorError::usage(format!("build guest image: {error}")))?;
    if !status.success() {
        return Err(GeneratorError::usage("guest image build failed"));
    }
    Ok(())
}

/// `release verify-digests --dir <dir> --archs <csv>`: validate the complete
/// platform digest set a manifest job assembles. Every declared arch must
/// carry exactly one `image-<arch>.digest` file holding a single `sha256:`
/// digest, and no extra digest file may ride along: a partial matrix can
/// never publish a partial tag. Prints `<arch> <digest>` per verified row.
fn verify_digests(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["dir", "archs"])?;
    let dir = required_option(&options, "dir")?;
    let archs = required_option(&options, "archs")?;
    let archs: Vec<&str> = archs
        .split(',')
        .map(str::trim)
        .filter(|arch| !arch.is_empty())
        .collect();
    if archs.is_empty() {
        return Err(GeneratorError::usage("--archs names no architecture"));
    }
    for arch in &archs {
        if !valid_arch(arch) {
            return Err(GeneratorError::usage(format!(
                "invalid digest architecture: {arch}"
            )));
        }
    }
    let dir = Path::new(dir);
    let mut found = BTreeSet::new();
    let entries = fs::read_dir(dir)
        .map_err(|error| GeneratorError::io("read digest directory", dir, &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| GeneratorError::io("read digest entry", dir, &error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".digest") {
            found.insert(name);
        }
    }
    for arch in &archs {
        let expected = format!("image-{arch}.digest");
        if !found.contains(&expected) {
            return Err(GeneratorError::usage(format!(
                "platform digest is missing: {expected}"
            )));
        }
    }
    for name in &found {
        let expected = archs
            .iter()
            .any(|arch| name == &format!("image-{arch}.digest"));
        if !expected {
            return Err(GeneratorError::usage(format!(
                "unexpected platform digest file: {name}"
            )));
        }
    }
    for arch in &archs {
        let path = dir.join(format!("image-{arch}.digest"));
        let digest = read_digest_file(&path)?;
        println!("{arch} {digest}");
    }
    Ok(())
}

/// Read one platform digest file: it must fit in 4 KiB and hold exactly one
/// `sha256:` digest with 64 lowercase hex digits.
fn read_digest_file(path: &Path) -> Result<String, GeneratorError> {
    let bytes =
        fs::read(path).map_err(|error| GeneratorError::io("read platform digest", path, &error))?;
    if bytes.len() > 4096 {
        return Err(GeneratorError::usage(format!(
            "platform digest exceeds 4096 bytes: {}",
            path.display()
        )));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        GeneratorError::usage(format!("platform digest is not UTF-8: {}", path.display()))
    })?;
    let mut tokens = text.split_whitespace();
    let (Some(token), None) = (tokens.next(), tokens.next()) else {
        return Err(GeneratorError::usage(format!(
            "platform digest must contain one token: {}",
            path.display()
        )));
    };
    let Some(hex) = token.strip_prefix("sha256:") else {
        return Err(GeneratorError::usage(format!(
            "platform digest is not a sha256 digest: {}",
            path.display()
        )));
    };
    let canonical = hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if !canonical {
        return Err(GeneratorError::usage(format!(
            "platform digest is not 64 lowercase hex digits: {}",
            path.display()
        )));
    }
    Ok(token.to_owned())
}

fn valid_arch(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn verify_feed(arguments: &[OsString]) -> Result<(), GeneratorError> {
    // `channel` is accepted and ignored: `update-feed` delegates its full
    // argument vector here, and rejecting the channel would fail every
    // channel update before it starts.
    let options = parse_options(arguments, &["kind", "package", "coordinate", "channel"])?;
    let kind = required_option(&options, "kind")?;
    let package = required_option(&options, "package")?;
    if !valid_package(package) {
        return Err(GeneratorError::usage("invalid feed package"));
    }
    match kind {
        "homebrew" => {
            let formula = Path::new("Formula").join(format!("{package}.rb"));
            if !formula.is_file() {
                return Err(GeneratorError::usage(format!(
                    "homebrew feed requires {}",
                    formula.display()
                )));
            }
        }
        other => {
            return Err(GeneratorError::usage(format!(
                "unsupported feed kind: {other}"
            )));
        }
    }
    Ok(())
}

fn update_feed(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["kind", "package", "coordinate", "channel"])?;
    verify_feed(arguments)?;
    let kind = required_option(&options, "kind")?;
    let channel = options.get("channel").map_or("stable", String::as_str);
    if !matches!(channel, "stable" | "preview") {
        return Err(GeneratorError::usage("channel must be stable or preview"));
    }
    match kind {
        "homebrew" => {
            println!("feed {kind} channel {channel} verified; mutation is GitHub-writer only");
            Ok(())
        }
        other => Err(GeneratorError::usage(format!(
            "unsupported feed kind: {other}"
        ))),
    }
}

/// Resolve the release mode for one event: the total event×mode matrix.
/// Prints the mode and refuses anything that would write externally from an
/// untrusted context. `publish` is reachable only from a version tag push
/// (stable) or an admitted producer run (rolling); `rehearse` finishes on
/// its feature branch and never waits for default-branch CI — the rendered
/// gate encodes that by resolving here, not by polling.
fn resolve_mode(arguments: &[OsString]) -> Result<(), GeneratorError> {
    println!("{}", resolve_mode_token(arguments)?);
    Ok(())
}

/// Map a validated `--input` to its static drill token: `publish` requests
/// reaching here were already refused, so only the three drill tokens remain.
fn input_token(input: &str) -> &'static str {
    match input {
        "build" => "build",
        "rehearse" => "rehearse",
        _ => "validate",
    }
}

/// The pure event→mode decision behind `resolve-mode`: exactly one
/// whitespace-free token (`publish`, `validate`, `build`, `rehearse`) the
/// caller prints to stdout for `GITHUB_OUTPUT`. Separated from the print
/// so tests assert the token itself, not just success.
fn resolve_mode_token(arguments: &[OsString]) -> Result<&'static str, GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "event",
            "ref",
            "input",
            "producer",
            "expected",
            "conclusion",
            "rolling",
            "branch",
        ],
    )?;
    let event = required_option(&options, "event")?;
    let reference = options.get("ref").map_or("", String::as_str);
    let input = options.get("input").map_or("validate", String::as_str);
    if !matches!(input, "validate" | "build" | "rehearse" | "publish") {
        return Err(GeneratorError::usage(format!(
            "unsupported release mode: {input}"
        )));
    }
    let rolling = match options.get("rolling").map(String::as_str) {
        None | Some("false") => false,
        Some("true") => true,
        Some(other) => {
            return Err(GeneratorError::usage(format!(
                "invalid --rolling value: {other}"
            )));
        }
    };
    match event {
        // Untrusted pull-request code never publishes, builds for release,
        // or rehearses with secrets: every PR resolves to secret-free
        // validation, and a publish request from a PR is a hard refusal.
        "pull_request" | "pull_request_target" => {
            if input == "publish" {
                return Err(GeneratorError::usage(
                    "release publish refused: pull requests resolve to validate only",
                ));
            }
            Ok("validate")
        }
        "schedule" => {
            if input == "publish" {
                return Err(GeneratorError::usage(
                    "release publish refused: scheduled runs resolve to validate only",
                ));
            }
            Ok("validate")
        }
        "push" => {
            if is_version_tag_ref(reference)
                || rolling && is_default_branch_ref(reference, &options)
            {
                Ok("publish")
            } else if input == "publish" {
                Err(GeneratorError::usage(
                    "release publish refused: branch pushes publish only on the rolling lane's default branch",
                ))
            } else {
                Ok(input_token(input))
            }
        }
        // Dispatch carries the declared mode, defaulting to validation.
        // Publication is tag-triggered (or admitted-producer) only: a
        // dispatch can rehearse the whole assembly but never publish it.
        "workflow_dispatch" => {
            if input == "publish" {
                return Err(GeneratorError::usage(
                    "release publish refused: publication is tag-triggered, never dispatched",
                ));
            }
            Ok(input_token(input))
        }
        // A producer run publishes only when its workflow name equals the
        // declared trusted producer at `success` — the same admission
        // `admit-producer` enforces, so no caller can publish off any
        // successful run by name alone.
        "workflow_run" => {
            let producer = options.get("producer").map_or("", String::as_str);
            let expected = options.get("expected").map_or("", String::as_str);
            let conclusion = options.get("conclusion").map_or("", String::as_str);
            if producer.is_empty() {
                return Err(GeneratorError::usage(
                    "release publish refused: workflow_run without an admitted producer",
                ));
            }
            if expected.is_empty() {
                return Err(GeneratorError::usage(
                    "release publish refused: workflow_run without a trusted producer to admit against",
                ));
            }
            if producer != expected {
                return Err(GeneratorError::usage(format!(
                    "release publish refused: producer `{producer}` is not the trusted `{expected}`"
                )));
            }
            if conclusion != "success" {
                return Err(GeneratorError::usage(format!(
                    "release publish refused: producer concluded {conclusion}, not success"
                )));
            }
            Ok("publish")
        }
        other => Err(GeneratorError::usage(format!(
            "unsupported release event: {other}"
        ))),
    }
}

/// Whether `reference` is a version tag push (`refs/tags/v[0-9]*`).
fn is_version_tag_ref(reference: &str) -> bool {
    reference.starts_with("refs/tags/v")
        && reference["refs/tags/v".len()..]
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit())
}

/// Whether `reference` is the default branch the lane rolls on. The branch
/// arrives as `--branch`; without it only the tag and rolling-default
/// shape can resolve, never an arbitrary branch.
fn is_default_branch_ref(reference: &str, options: &BTreeMap<String, String>) -> bool {
    let branch = options.get("branch").map_or("main", String::as_str);
    reference == format!("refs/heads/{branch}")
}

/// Resolve the source revision the lane builds: a `workflow_run` event
/// builds the producer run's head SHA, every other event builds its own
/// SHA. Both must be full 40-hex revisions; anything else fails closed
/// instead of building an unidentified tree.
fn resolve_source(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["event", "sha", "run-sha"])?;
    let event = required_option(&options, "event")?;
    let sha = if event == "workflow_run" {
        required_option(&options, "run-sha")?
    } else {
        required_option(&options, "sha")?
    };
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GeneratorError::usage(format!(
            "release source must be a 40-hex revision, found `{sha}`"
        )));
    }
    println!("{sha}");
    Ok(())
}

/// Admit a `workflow_run` producer: the run's workflow name must equal the
/// declared trusted producer and its conclusion must be `success`. A name
/// or conclusion mismatch is a hard refusal, never a warning.
fn admit_producer(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(arguments, &["producer", "expected", "conclusion"])?;
    let producer = required_option(&options, "producer")?;
    let expected = required_option(&options, "expected")?;
    let conclusion = required_option(&options, "conclusion")?;
    if producer != expected {
        return Err(GeneratorError::usage(format!(
            "release publish refused: producer `{producer}` is not the trusted `{expected}`"
        )));
    }
    if conclusion != "success" {
        return Err(GeneratorError::usage(format!(
            "release publish refused: producer `{producer}` concluded {conclusion}, not success"
        )));
    }
    println!("admitted");
    Ok(())
}

/// Assemble the consumer release manifest and the independent checksum
/// corpus from declared subjects: every `--subjects` entry must exist in
/// `--dir`, the corpus re-hashes each subject independently, and the
/// manifest binds the schema URN, source, version, and digests. The corpus
/// is verified strictly before anything prints success.
fn assemble_manifest(arguments: &[OsString]) -> Result<(), GeneratorError> {
    let options = parse_options(
        arguments,
        &[
            "dir",
            "subjects",
            "schema",
            "repository",
            "ref",
            "commit",
            "version",
        ],
    )?;
    let dir = Path::new(required_option(&options, "dir")?);
    let schema = required_option(&options, "schema")?;
    let repository = required_option(&options, "repository")?;
    let source_ref = required_option(&options, "ref")?;
    let commit = required_option(&options, "commit")?;
    let version = required_option(&options, "version")?;
    if schema.is_empty() || !schema.contains('/') {
        return Err(GeneratorError::usage(
            "release manifest needs a schema URN of the form <domain>/<name>",
        ));
    }
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GeneratorError::usage(
            "release manifest needs a 40-hex source commit",
        ));
    }
    if !is_artifact_version(version) {
        return Err(GeneratorError::usage(format!(
            "invalid release manifest version: {version}"
        )));
    }
    let subjects = required_option(&options, "subjects")?;
    let mut names: Vec<&str> = subjects.split(',').collect();
    if names.is_empty() || names.iter().any(|name| !valid_subject_name(name)) {
        return Err(GeneratorError::usage(
            "release manifest needs declared subject file names",
        ));
    }
    names.sort_unstable();
    let mut corpus = String::new();
    let mut assets = Vec::new();
    for name in &names {
        let path = dir.join(name);
        if !path.is_file() {
            return Err(GeneratorError::usage(format!(
                "declared manifest subject is missing: {name}"
            )));
        }
        let digest = sha256_file(&path)?;
        corpus.push_str(&digest);
        corpus.push_str("  ");
        corpus.push_str(name);
        corpus.push('\n');
        assets.push(serde_json::json!({"name": name, "sha256": digest}));
    }
    let document = serde_json::json!({
        "schema": schema,
        "source_repository": repository,
        "source_ref": source_ref,
        "source_commit": commit,
        "version": version,
        "assets": assets,
    });
    let Some(manifest) = serde_json::to_string_pretty(&document)
        .ok()
        .map(|mut text| {
            text.push('\n');
            text
        })
    else {
        return Err(GeneratorError::usage("release manifest is not encodable"));
    };
    let corpus_path = dir.join("SHA256SUMS");
    fs::write(&corpus_path, &corpus)
        .map_err(|error| GeneratorError::io("write checksum corpus", &corpus_path, &error))?;
    // The corpus is re-verified strictly from disk before the manifest is
    // written: a subject that changed mid-assembly fails here, not in a
    // consumer that trusted the manifest.
    verify_checksum_corpus(dir, &corpus)?;
    let manifest_path = dir.join("release-manifest.json");
    fs::write(&manifest_path, &manifest)
        .map_err(|error| GeneratorError::io("write release manifest", &manifest_path, &error))?;
    println!("{}", manifest_path.display());
    Ok(())
}

/// Re-verify a checksum corpus strictly: every line re-hashes its subject
/// from disk and any mismatch, miss, or malformed line fails.
fn verify_checksum_corpus(dir: &Path, corpus: &str) -> Result<(), GeneratorError> {
    for line in corpus.lines() {
        let (digest, name) = line.split_once("  ").ok_or_else(|| {
            GeneratorError::usage(format!("malformed checksum corpus line: {line}"))
        })?;
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(GeneratorError::usage(format!(
                "malformed checksum corpus digest: {line}"
            )));
        }
        let actual = sha256_file(&dir.join(name))?;
        if actual != digest {
            return Err(GeneratorError::usage(format!(
                "checksum corpus mismatch for {name}"
            )));
        }
    }
    Ok(())
}

/// Whether `name` is a portable manifest subject: a bare file name over the
/// portable asset alphabet, never a path.
fn valid_subject_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The deterministic archive flags are GNU tar's. Any other tar fails
/// closed here, before it writes a silently skewed archive.
fn require_gnu_tar() -> Result<(), GeneratorError> {
    let output = Command::new("tar")
        .arg("--version")
        .output()
        .map_err(|error| GeneratorError::usage(format!("probe tar: {error}")))?;
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout)
            .to_lowercase()
            .contains("gnu tar")
    {
        return Err(GeneratorError::usage(
            "deterministic archives need GNU tar; refusing to package on this runner",
        ));
    }
    Ok(())
}

fn required_option<'a>(
    options: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, GeneratorError> {
    options
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| GeneratorError::usage(format!("--{name} needs a value")))
}

fn git_revision(reference: &str) -> Result<String, GeneratorError> {
    let revision_reference = format!("{reference}^{{commit}}");
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &revision_reference])
        .output()
        .map_err(|error| GeneratorError::usage(format!("run git: {error}")))?;
    if !output.status.success() {
        return Err(GeneratorError::usage(format!(
            "git could not resolve release reference {reference}"
        )));
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(GeneratorError::usage(format!(
            "git returned an invalid revision for {reference}"
        )));
    }
    Ok(revision)
}

fn sha256_file(path: &Path) -> Result<String, GeneratorError> {
    let contents =
        fs::read(path).map_err(|error| GeneratorError::io("read release archive", path, &error))?;
    let digest = Sha256::digest(contents);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    Ok(output)
}

fn is_semver(value: &str) -> bool {
    let (core, suffix) = value
        .split_once('-')
        .map_or((value, None), |(core, suffix)| (core, Some(suffix)));
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && suffix.is_none_or(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        })
}

fn is_artifact_version(value: &str) -> bool {
    value == "preview" || is_semver(value) || is_preview_version(value)
}

/// A rolling-preview Debian version: the crate version, a `~preview.N`
/// Debian-sortable pre-release (older than the coming stable), and the
/// 7-hex source the lane built. Strict: the consumer lane matches this
/// exact shape before it binds a preview.
fn is_preview_version(value: &str) -> bool {
    let Some((core, preview)) = value.split_once('~') else {
        return false;
    };
    let parts = core.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    let Some(number) = preview.strip_prefix("preview.") else {
        return false;
    };
    let Some((run, commit)) = number.split_once('+') else {
        return false;
    };
    !run.is_empty()
        && run.bytes().all(|byte| byte.is_ascii_digit())
        && commit.len() == 7
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// A consumer-facing deb file name: a bare file name (no directories, no
/// traversal), ending in `.deb`, over the portable asset alphabet.
fn valid_deb_asset_name(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains("..")
        && value.strip_suffix(".deb").is_some_and(|stem| {
            !stem.is_empty()
                && stem.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~' | b'+')
                })
        })
}

pub(crate) fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'/' | b'-'))
}

pub(crate) fn valid_target(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(crate) fn valid_package(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) fn valid_binary(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether `value` is a portable archive member: a bare file name over the
/// portable asset alphabet, never a path or traversal.
fn valid_archive_member(value: &str) -> bool {
    valid_binary(value) && value != "." && value != ".."
}

/// Whether `value` is a usable tag-trigger filter: one non-empty line with
/// no whitespace or control bytes. GitHub interprets `*?[]!` as glob syntax
/// and `/` as a tag path separator, so both stay legal here; the renderer
/// quotes the filter for YAML.
pub(crate) fn valid_tag_pattern(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| !byte.is_ascii_whitespace() && !byte.is_ascii_control())
}

/// Whether `value` is a usable OCI registry host: lowercase dot-separated
/// labels with an optional `:port`. The renderer interpolates the host into
/// a login step and a step name, so anything outside the hostname alphabet
/// — uppercase, whitespace, slashes, credentials — fails here, never in YAML.
pub(crate) fn valid_registry_host(value: &str) -> bool {
    let host = value.split(':').next().unwrap_or_default();
    let port = value.split(':').nth(1);
    if value.split(':').nth(2).is_some() {
        return false;
    }
    if let Some(port) = port
        && (port.is_empty() || port.len() > 5 || !port.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    !host.is_empty()
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..")
        && host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::error::Error;

    use super::*;
    use crate::primitives::prepared_tools::{ProducerIdentity, ToolFile, ToolOutcome};

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}: missing value"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> GeneratorError {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error,
        }
    }

    /// A throwaway digest directory: the only way to feed `verify-digests`
    /// a real artifact set.
    pub(crate) fn digest_fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-digests-{name}-{}",
            crate::unique_suffix()
        ));
        must(std::fs::create_dir_all(&root), "create digest fixture");
        root
    }

    fn write_digest(dir: &Path, arch: &str, digest: &str) {
        must(
            std::fs::write(dir.join(format!("image-{arch}.digest")), digest),
            "write platform digest",
        );
    }

    #[test]
    fn verify_digests_accepts_the_complete_multi_arch_set() {
        let dir = digest_fixture("complete");
        let amd64 = format!("sha256:{}", "a".repeat(64));
        let arm64 = format!("sha256:{}", "b".repeat(64));
        write_digest(&dir, "amd64", &format!("{amd64}\n"));
        write_digest(&dir, "arm64", &format!("{arm64}\n"));
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        must(
            verify_digests(&args(&[
                "--dir",
                dir.to_str().unwrap_or_default(),
                "--archs",
                "amd64,arm64",
            ])),
            "a complete digest set must verify",
        );
        assert_eq!(
            must(
                read_digest_file(&dir.join("image-amd64.digest")),
                "read amd64 digest"
            ),
            amd64
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_digests_rejects_missing_extra_and_malformed_rows() {
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        // A missing arch fails: a partial matrix can never publish.
        let dir = digest_fixture("missing");
        write_digest(&dir, "amd64", &format!("sha256:{}\n", "a".repeat(64)));
        let error = must_fail(
            verify_digests(&args(&[
                "--dir",
                dir.to_str().unwrap_or_default(),
                "--archs",
                "amd64,arm64",
            ])),
            "a missing arch must fail",
        );
        assert!(
            error.to_string().contains("platform digest is missing"),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        // An extra digest file fails: only the declared set assembles.
        let dir = digest_fixture("extra");
        write_digest(&dir, "amd64", &format!("sha256:{}\n", "a".repeat(64)));
        write_digest(&dir, "arm64", &format!("sha256:{}\n", "b".repeat(64)));
        write_digest(&dir, "riscv64", &format!("sha256:{}\n", "c".repeat(64)));
        let error = must_fail(
            verify_digests(&args(&[
                "--dir",
                dir.to_str().unwrap_or_default(),
                "--archs",
                "amd64,arm64",
            ])),
            "an extra digest must fail",
        );
        assert!(
            error
                .to_string()
                .contains("unexpected platform digest file"),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        // Malformed rows fail: not a digest, two tokens, short hex.
        for (name, body, marker) in [
            ("scheme", "md5:abc\n".to_owned(), "not a sha256 digest"),
            (
                "tokens",
                "sha256:aaa sha256:bbb\n".to_owned(),
                "must contain one token",
            ),
            (
                "length",
                format!("sha256:{}\n", "a".repeat(63)),
                "not 64 lowercase hex digits",
            ),
            (
                "case",
                format!("sha256:{}\n", "A".repeat(64)),
                "not 64 lowercase hex digits",
            ),
        ] {
            let dir = digest_fixture(name);
            write_digest(&dir, "amd64", &body);
            let error = must_fail(
                verify_digests(&args(&[
                    "--dir",
                    dir.to_str().unwrap_or_default(),
                    "--archs",
                    "amd64",
                ])),
                "a malformed digest must fail",
            );
            assert!(
                error.to_string().contains(marker),
                "unexpected error for {name}: {error}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
        // Empty and hostile arch lists fail before any read.
        let error = must_fail(
            verify_digests(&args(&["--dir", "image-artifacts", "--archs", " , "])),
            "an empty arch list must fail",
        );
        assert!(
            error.to_string().contains("--archs names no architecture"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            verify_digests(&args(&["--dir", "image-artifacts", "--archs", "../x"])),
            "a hostile arch must fail",
        );
        assert!(
            error.to_string().contains("invalid digest architecture"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn package_deb_rejects_bad_inputs_before_any_cargo_call() {
        // Every case below fails on option validation, before package-deb
        // shells out: no cargo, no filesystem writes, no network.
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        let error = must_fail(
            package_deb(&args(&["--version", "1.2.3"])),
            "package-deb without --package",
        );
        assert!(
            error.to_string().contains("--package needs a value"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            package_deb(&args(&[
                "--package",
                "velnor-runner",
                "--version",
                "v1.2.3",
            ])),
            "package-deb with a v-prefixed version",
        );
        assert!(
            error.to_string().contains("invalid package or version"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            package_deb(&args(&[
                "--package",
                "velnor-runner",
                "--version",
                "1.2.3",
                "--guest",
                "/nonexistent-velnor-guest-payload",
            ])),
            "package-deb with a missing guest directory",
        );
        assert!(
            error
                .to_string()
                .contains("guest payload directory is missing"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            package_deb(&args(&[
                "--package",
                "velnor-runner",
                "--version",
                "1.2.3",
                "--target",
                "x86_64!",
            ])),
            "package-deb with an invalid target",
        );
        assert!(
            error.to_string().contains("invalid package target"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn preview_versions_are_strictly_shaped() {
        for version in [
            "0.1.274~preview.145+d3e441f",
            "1.0.0~preview.1+abcdef0",
            "10.20.30~preview.9999999+0123456",
        ] {
            assert!(is_preview_version(version), "{version} must be accepted");
            assert!(
                is_artifact_version(version),
                "{version} must package as an artifact version"
            );
        }
        for version in [
            "0.1.274",
            "preview",
            "v0.1.274",
            "0.1.274~preview.145",
            "0.1.274~preview.+d3e441f",
            "0.1.274~preview145+d3e441f",
            "0.1.274~preview.145+d3e441",
            "0.1.274~preview.145+d3e441fg",
            "0.1.274~preview.x+d3e441f",
            "0.1~preview.145+d3e441f",
            "0.1.274~stable.145+d3e441f",
            "0.1.274~~preview.145+d3e441f",
            "0.1.274~preview.145+D3E441F",
        ] {
            assert!(!is_preview_version(version), "{version} must be rejected");
        }
        // The tag gate stays strict semver: preview shapes never leak into it.
        assert!(!is_semver("0.1.274~preview.145+d3e441f"));
    }

    #[test]
    fn package_deb_validates_packaging_options_before_any_cargo_call() {
        // Every case below fails on option validation, before package-deb
        // shells out: no cargo, no filesystem writes, no network.
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        let error = must_fail(
            package_deb(&args(&[
                "--package",
                "widget",
                "--version",
                "1.2.3",
                "--no-build",
                "yes",
            ])),
            "package-deb with a non-true --no-build",
        );
        assert!(
            error.to_string().contains("invalid --no-build value"),
            "unexpected error: {error}"
        );
        for name in [
            "widget-1.2.3-amd64.deb/../escape.deb",
            "../escape.deb",
            "widget-1.2.3-amd64.tar.gz",
            "widget 1.2.3.deb",
            ".deb",
        ] {
            let error = must_fail(
                package_deb(&args(&[
                    "--package",
                    "widget",
                    "--version",
                    "1.2.3",
                    "--asset-name",
                    name,
                ])),
                "package-deb with a hostile asset name",
            );
            assert!(
                error.to_string().contains("invalid deb asset name"),
                "unexpected error for {name}: {error}"
            );
        }
        // A preview version passes version validation: the failure below
        // names the missing guest directory, never the version.
        let error = must_fail(
            package_deb(&args(&[
                "--package",
                "widget",
                "--version",
                "1.2.3~preview.7+abcdef0",
                "--guest",
                "/nonexistent-widget-guest-payload",
                "--no-build",
                "true",
                "--asset-name",
                "widget-preview-1.2.3~preview.7+abcdef0-amd64.deb",
            ])),
            "package-deb with a preview version and missing guest directory",
        );
        assert!(
            error
                .to_string()
                .contains("guest payload directory is missing"),
            "unexpected error: {error}"
        );
    }

    /// A throwaway cargo-deb output tree: the canonical dir plus the
    /// per-target dir cargo-deb drops its back-compat twin into.
    fn debian_output_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-debian-{name}-{}",
            crate::unique_suffix()
        ));
        let canonical = root.join("target/debian");
        let twin = root.join("target/x86_64-unknown-linux-gnu/debian");
        for dir in [&canonical, &twin] {
            must(std::fs::create_dir_all(dir), "create debian fixture dir");
        }
        (root, canonical, twin)
    }

    #[test]
    fn debian_collect_dedups_the_cargo_deb_back_compat_twin() {
        // cargo-deb hard-links the same file name into both scan roots;
        // the collector must see one package and rename it, not fail.
        let (root, canonical, twin) = debian_output_fixture("twin");
        let dist = root.join("dist");
        let body = b"fake deb bytes";
        must(
            std::fs::write(canonical.join("widget_1.2.3_amd64.deb"), body),
            "write canonical deb",
        );
        must(
            std::fs::hard_link(
                canonical.join("widget_1.2.3_amd64.deb"),
                twin.join("widget_1.2.3_amd64.deb"),
            ),
            "link back-compat twin",
        );
        must(
            collect_debian_packages_from(&[canonical, twin], &dist, Some("widget-1.2.3-amd64.deb")),
            "a back-compat twin must collect as one package",
        );
        let renamed = dist.join("widget-1.2.3-amd64.deb");
        assert_eq!(
            must(std::fs::read(&renamed), "read renamed deb"),
            body.to_vec(),
            "renamed deb must carry the built bytes"
        );
        assert!(
            dist.join("widget-1.2.3-amd64.deb.sha256").is_file(),
            "renamed deb must carry its digest sidecar"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn debian_collect_rejects_two_distinct_debs_with_their_paths() {
        // Two genuinely different file names are still an ambiguous pick:
        // fail closed, and name both paths so the log diagnoses itself.
        let (root, canonical, twin) = debian_output_fixture("distinct");
        let dist = root.join("dist");
        must(
            std::fs::write(canonical.join("widget_1.2.3_amd64.deb"), b"one"),
            "write first deb",
        );
        must(
            std::fs::write(twin.join("widget_1.2.4_amd64.deb"), b"two"),
            "write second deb",
        );
        let error = must_fail(
            collect_debian_packages_from(&[canonical, twin], &dist, Some("widget-1.2.3-amd64.deb")),
            "two distinct debs must fail closed",
        );
        let message = error.to_string();
        assert!(
            message.contains("expected exactly one .deb to rename, found 2"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("widget_1.2.3_amd64.deb")
                && message.contains("widget_1.2.4_amd64.deb"),
            "error must list both paths: {message}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cargo_deb_command_pins_the_canonical_output_dir() {
        // An explicit `--output` keeps cargo-deb's default location while
        // disabling the back-compat twin it emits for a single `--target`.
        let command = cargo_deb_command("widget", "1.2.3", true, Some("aarch64-unknown-linux-gnu"));
        assert_eq!(command.get_program(), "cargo");
        let argv: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            [
                "deb",
                "--no-strip",
                "--package",
                "widget",
                "--deb-version",
                "1.2.3",
                "--no-build",
                "--target",
                "aarch64-unknown-linux-gnu",
                "--output",
                "target/debian",
            ]
        );
        let bare = cargo_deb_command("widget", "1.2.3", false, None);
        let argv: Vec<String> = bare
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(
            !argv
                .iter()
                .any(|arg| arg == "--target" || arg == "--no-build"),
            "optional flags must stay conditional: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--output", "target/debian"]),
            "the output pin must be unconditional: {argv:?}"
        );
    }

    #[test]
    fn numeric_actions_cache_ids_become_lossless_internal_strings() {
        let record: CacheEntryRecord = must(
            serde_json::from_str(
                r#"{"id":7636963307,"key":"example-docker-seed-Linux-X64",
                "size_in_bytes":1,"created_at":"2026-09-01T00:00:00Z"}"#,
            ),
            "deserialize a numeric Actions cache id",
        );
        assert_eq!(record.id, "7636963307");
        assert_eq!(record.key, "example-docker-seed-Linux-X64");
    }

    fn selection_config() -> CiConfig {
        let unit = |id: &str, watch: &[&str], depends_on: &[&str]| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: "rust".to_owned(),
            root: ".".to_owned(),
            watch: watch.iter().map(|value| (*value).to_owned()).collect(),
            github_pr_commands: vec!["true".to_owned()],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["true".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: depends_on.iter().map(|value| (*value).to_owned()).collect(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        CiConfig {
            schema: 2,
            repository: "example/repository".to_owned(),
            profile: "rust-workspace".to_owned(),
            verified: true,
            default_branch: "main".to_owned(),
            runners: "github".to_owned(),
            automatic: "github".to_owned(),
            analysis: Analysis::default(),
            workflow: Workflow::default(),
            release: Release::default(),
            unit: vec![
                unit("base", &["crates/base/**"], &[]),
                unit("app", &["crates/app/**"], &["base"]),
                unit("consumer", &["crates/consumer/**"], &["app"]),
                unit("docs", &["docs/**"], &[]),
            ],
        }
    }

    /// A polyglot consumer without a docs unit: rust, bun, docker, and swift
    /// watches shaped like a real analyzed estate. A root-level contributor
    /// document matches none of them.
    fn polyglot_selection_config() -> CiConfig {
        let unit = |id: &str, kind: &str, watch: &[&str]| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: kind.to_owned(),
            root: ".".to_owned(),
            watch: watch.iter().map(|value| (*value).to_owned()).collect(),
            github_pr_commands: vec!["true".to_owned()],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["true".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        CiConfig {
            schema: 2,
            repository: "example/repository".to_owned(),
            profile: "polyglot-no-docs".to_owned(),
            verified: true,
            default_branch: "main".to_owned(),
            runners: "github".to_owned(),
            automatic: "github".to_owned(),
            analysis: Analysis::default(),
            workflow: Workflow::default(),
            release: Release::default(),
            unit: vec![
                unit(
                    "rust-alpha",
                    "rust",
                    &[
                        "Cargo.lock",
                        "Cargo.toml",
                        "crates/alpha/**/*.rs",
                        "crates/alpha/Cargo.toml",
                        "crates/alpha/src/**",
                        "rust-toolchain.toml",
                    ],
                ),
                unit(
                    "bun-web",
                    "bun",
                    &[
                        "web/**/*.ts",
                        "web/**/*.tsx",
                        "web/bun.lock",
                        "web/package.json",
                    ],
                ),
                unit("docker-construct", "docker", &["docker/construct/**"]),
                unit(
                    "swift-native",
                    "swift",
                    &[
                        "native/**/*.swift",
                        "native/Package.swift",
                        "native/Package.resolved",
                    ],
                ),
            ],
        }
    }

    fn selection_git_fixture(
        name: &str,
        changed: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        let id = crate::unique_suffix();
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-selection-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        let init = |args: &[&str]| -> Result<(), Box<dyn Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?;
            assert!(status.success(), "git command failed: {args:?}");
            Ok(())
        };
        init(&["init", "-q"])?;
        init(&["config", "user.email", "test@example.invalid"])?;
        init(&["config", "user.name", "Velnor test"])?;
        for path in [
            "crates/base/src/lib.rs",
            "crates/app/src/lib.rs",
            "crates/consumer/src/lib.rs",
            "docs/index.md",
        ] {
            let path = root.join(path);
            let parent = path.parent().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "fixture parent")
            })?;
            std::fs::create_dir_all(parent)?;
            std::fs::write(path, "initial\n")?;
        }
        init(&["add", "."])?;
        init(&["commit", "-qm", "base"])?;
        let base = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        let changed_path = root.join(changed);
        let parent = changed_path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "changed parent")
        })?;
        std::fs::create_dir_all(parent)?;
        std::fs::write(changed_path, "changed\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "change"])?;
        let head = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        Ok((root, base, head))
    }

    fn git_fixture_head(root: &std::path::Path) -> Result<String, Box<dyn Error>> {
        Ok(String::from_utf8(
            std::process::Command::new("git")
                .current_dir(root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned())
    }

    /// A two-commit fixture whose diff is a pure rename: `from` exists at
    /// base, `to` carries identical content at head, so `-M` reports `R100`
    /// with both sides.
    fn selection_git_rename_fixture(
        name: &str,
        from: &str,
        to: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        let id = crate::unique_suffix();
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-selection-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        let run = |args: &[&str]| -> Result<(), Box<dyn Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?;
            assert!(status.success(), "git command failed: {args:?}");
            Ok(())
        };
        run(&["init", "-q"])?;
        run(&["config", "user.email", "test@example.invalid"])?;
        run(&["config", "user.name", "Velnor test"])?;
        let from_path = root.join(from);
        let parent = from_path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "rename source parent")
        })?;
        std::fs::create_dir_all(parent)?;
        std::fs::write(&from_path, "line one\nline two\nline three\n")?;
        run(&["add", "."])?;
        run(&["commit", "-qm", "base"])?;
        let base = git_fixture_head(&root)?;
        if let Some(parent) = root.join(to).parent() {
            std::fs::create_dir_all(parent)?;
        }
        run(&["mv", from, to])?;
        run(&["commit", "-qm", "rename"])?;
        let head = git_fixture_head(&root)?;
        Ok((root, base, head))
    }

    /// A two-commit fixture whose diff deletes `deleted`: the path exists at
    /// base and is gone at head, so the diff reports `D` with the old path.
    fn selection_git_delete_fixture(
        name: &str,
        deleted: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        let id = crate::unique_suffix();
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-selection-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        let run = |args: &[&str]| -> Result<(), Box<dyn Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?;
            assert!(status.success(), "git command failed: {args:?}");
            Ok(())
        };
        run(&["init", "-q"])?;
        run(&["config", "user.email", "test@example.invalid"])?;
        run(&["config", "user.name", "Velnor test"])?;
        for path in [deleted, "keeper.txt"] {
            let path = root.join(path);
            let parent = path.parent().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "delete fixture parent")
            })?;
            std::fs::create_dir_all(parent)?;
            std::fs::write(path, "initial\n")?;
        }
        run(&["add", "."])?;
        run(&["commit", "-qm", "base"])?;
        let base = git_fixture_head(&root)?;
        run(&["rm", "-q", deleted])?;
        run(&["commit", "-qm", "delete"])?;
        let head = git_fixture_head(&root)?;
        Ok((root, base, head))
    }

    fn stale_base_selection_git_fixture(
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        let id = crate::unique_suffix();
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-stale-base-selection-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("crates/base/src"))?;
        std::fs::create_dir_all(root.join("docs"))?;
        let init = |args: &[&str]| -> Result<(), Box<dyn Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?;
            assert!(status.success(), "git command failed: {args:?}");
            Ok(())
        };
        init(&["init", "-q"])?;
        init(&["config", "user.email", "test@example.invalid"])?;
        init(&["config", "user.name", "Velnor test"])?;
        std::fs::write(root.join("crates/base/src/lib.rs"), "initial\n")?;
        std::fs::write(root.join("docs/index.md"), "initial\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "base"])?;
        let initial = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();

        init(&["checkout", "-q", "-b", "base-line"])?;
        std::fs::write(root.join("docs/index.md"), "base-only\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "base-only"])?;
        let base = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();

        init(&["checkout", "-q", "-b", "feature", &initial])?;
        std::fs::write(root.join("crates/base/src/lib.rs"), "feature-only\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "feature-only"])?;
        let head = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        Ok((root, base, head))
    }

    fn lockfile_version_selection_git_fixture(
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        let id = crate::unique_suffix();
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-version-selection-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("crates/runner"))?;
        std::fs::create_dir_all(root.join("crates/ctl"))?;
        let init = |args: &[&str]| -> Result<(), Box<dyn Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .status()?;
            assert!(status.success(), "git command failed: {args:?}");
            Ok(())
        };
        init(&["init", "-q"])?;
        init(&["config", "user.email", "test@example.invalid"])?;
        init(&["config", "user.name", "Velnor test"])?;
        std::fs::write(
            root.join("Cargo.lock"),
            "version = \"1\"\nchecksum = \"unchanged\"\n",
        )?;
        std::fs::write(
            root.join("crates/runner/Cargo.toml"),
            "version = \"0.1.0\"\n",
        )?;
        std::fs::write(root.join("crates/ctl/Cargo.toml"), "version = \"0.1.0\"\n")?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "base"])?;
        let base = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        std::fs::write(
            root.join("Cargo.lock"),
            "version = \"2\"\nchecksum = \"unchanged\"\n",
        )?;
        init(&["add", "."])?;
        init(&["commit", "-qm", "version bump"])?;
        let head = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()?
                .stdout,
        )?
        .trim()
        .to_owned();
        Ok((root, base, head))
    }

    /// A repo-owned project config written for these tests: one docker unit,
    /// a small rust crate graph, and a version-bump allowlist. The selection
    /// engine consumes whatever the file declares, so the fixture stays
    /// independent of any repository the workspace happens to live in.
    const SELECTION_PROJECT_CONFIG: &str = r#"schema = 2
repository = "example/selection"
profile = "generic"
verified = true
default_branch = "main"
runners = "github"

[analysis]
method = "static-filesystem-and-manifest-inspection"
detected = []
limitations = []

[workflow]
github_runner = "ubuntu-24.04"
files = ["ci-docker-docker.yml", "ci-rust-base.yml", "ci-rust-leaf.yml", "ci-pr.yml", "ci-main.yml", "ci-policy.yml", "maintenance.yml", "nightly.yml"]
version_bump_units = ["docker", "rust-bench", "rust-leaf"]

[[unit]]
id = "docker"
kind = "docker"
root = "."
watch = ["Dockerfile"]
github_pr_commands = ["docker build --file 'Dockerfile' --tag local-ci:dockerfile '.'"]
github_full_commands = ["docker build --file 'Dockerfile' --tag local-ci:dockerfile '.'"]
velnor_pr_commands = ["docker build --file 'Dockerfile' --tag local-ci:dockerfile '.'"]
velnor_full_commands = ["docker build --file 'Dockerfile' --tag local-ci:dockerfile '.'"]

[[unit]]
id = "rust-base"
kind = "rust"
root = "crates/base"
watch = ["crates/base/**", "Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'crates/base/Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'crates/base/Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'crates/base/Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'crates/base/Cargo.toml'"]

[[unit]]
id = "rust-leaf"
kind = "rust"
root = "crates/leaf"
depends_on = ["rust-base"]
watch = ["crates/leaf/**", "Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'crates/leaf/Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'crates/leaf/Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'crates/leaf/Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'crates/leaf/Cargo.toml'"]

[[unit]]
id = "rust-bench"
kind = "rust"
root = "crates/bench"
depends_on = ["rust-base"]
watch = ["crates/bench/**", "Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'crates/bench/Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'crates/bench/Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'crates/bench/Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'crates/bench/Cargo.toml'"]
"#;

    const ROOT_SCOPED_SELECTION_PROJECT_CONFIG: &str = r#"schema = 2
repository = "example/selection"
profile = "generic"
verified = true
default_branch = "main"
runners = "both"

[workflow]
version_bump_units = ["rust-contract"]

[[unit]]
id = "rust-root"
kind = "rust"
root = "crates/root"
watch = ["crates/root/**", "Cargo.lock"]
github_pr_commands = ["true"]
github_full_commands = ["true"]
velnor_pr_commands = ["true"]
velnor_full_commands = ["true"]

[[unit]]
id = "rust-contract"
kind = "rust"
root = "crates/velnor-workflow-contract"
watch = ["crates/velnor-workflow-contract/**", "Cargo.lock", "crates/velnor-workflow-contract/Cargo.lock"]
github_pr_commands = ["true"]
github_full_commands = ["true"]
velnor_pr_commands = ["true"]
velnor_full_commands = ["true"]

[[unit]]
id = "rust-root-workspace"
kind = "rust"
root = "."
watch = ["Cargo.toml"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true

[[unit]]
id = "rust-contract-workspace"
kind = "rust"
root = "crates/velnor-workflow-contract"
watch = ["crates/velnor-workflow-contract/Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true
"#;

    fn current_project_selection_git_fixture(
        name: &str,
        changed: &str,
        base_contents: &str,
        head_contents: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        current_project_selection_git_fixture_with_config(
            name,
            changed,
            base_contents,
            head_contents,
            SELECTION_PROJECT_CONFIG,
        )
    }

    fn current_project_selection_git_fixture_with_config(
        name: &str,
        changed: &str,
        base_contents: &str,
        head_contents: &str,
        config_text: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        current_project_selection_git_fixture_with_changes(
            name,
            &[(changed, base_contents, head_contents)],
            config_text,
        )
    }

    fn current_project_selection_git_fixture_with_changes(
        name: &str,
        changes: &[(&str, &str, &str)],
        config_text: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        let id = crate::unique_suffix();
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-current-project-selection-{name}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)?;
        let init = |args: &[&str]| -> Result<String, Box<dyn Error>> {
            let output = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()?;
            assert!(
                output.status.success(),
                "git command failed: {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(String::from_utf8(output.stdout)?.trim().to_owned())
        };
        init(&["init", "-q"])?;
        init(&["config", "user.email", "test@example.invalid"])?;
        init(&["config", "user.name", "Velnor test"])?;

        let config = root.join(".github/ci/project.toml");
        std::fs::create_dir_all(config.parent().ok_or("project config parent")?)?;
        std::fs::write(&config, config_text)?;

        for (changed, base_contents, _) in changes {
            let changed_path = root.join(changed);
            std::fs::create_dir_all(changed_path.parent().ok_or("changed file parent")?)?;
            std::fs::write(changed_path, base_contents)?;
        }
        init(&["add", "."])?;
        init(&["commit", "-qm", "base"])?;
        let base = init(&["rev-parse", "HEAD"])?;

        for (changed, _, head_contents) in changes {
            std::fs::write(root.join(changed), head_contents)?;
        }
        init(&["add", "."])?;
        init(&["commit", "-qm", "change"])?;
        let head = init(&["rev-parse", "HEAD"])?;
        Ok((root, base, head))
    }

    fn selected_ids(units: Vec<&CiUnit>) -> Vec<&str> {
        units.into_iter().map(|unit| unit.id.as_str()).collect()
    }

    fn selected_id_set(selection: &UnitSelection<'_>) -> BTreeSet<String> {
        selection.units.iter().map(|unit| unit.id.clone()).collect()
    }

    fn lanes_selection_config() -> CiConfig {
        let unit = |id: &str, kind: &str| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: kind.to_owned(),
            root: ".".to_owned(),
            watch: vec!["**".to_owned()],
            github_pr_commands: vec!["true".to_owned()],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["true".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        CiConfig {
            schema: 2,
            repository: String::new(),
            profile: String::new(),
            verified: true,
            default_branch: "main".to_owned(),
            runners: "both".to_owned(),
            automatic: "both".to_owned(),
            analysis: Analysis::default(),
            workflow: Workflow::default(),
            release: Release::default(),
            unit: vec![
                unit("rust-app", "rust"),
                unit("swift-app", "swift"),
                unit("future-app", "quantum"),
            ],
        }
    }

    #[test]
    fn plan_lanes_default_to_both_and_reject_unknown_lanes() {
        assert_eq!(
            must(plan_lanes_for_value(""), "empty lanes"),
            RunnerMode::Both
        );
        assert_eq!(
            must(plan_lanes_for_value("both"), "both lanes"),
            RunnerMode::Both
        );
        assert_eq!(
            must(plan_lanes_for_value("velnor"), "velnor lanes"),
            RunnerMode::Velnor
        );
        assert_eq!(
            must(plan_lanes_for_value("github"), "github lanes"),
            RunnerMode::Github
        );
        let result = plan_lanes_for_value("self-hosted");
        assert!(
            result.as_ref().is_err_and(|error| error
                .to_string()
                .contains("unsupported CI lanes `self-hosted`")),
            "unknown lanes must fail closed: {result:?}"
        );
    }

    #[test]
    fn velnor_only_selection_drops_swift_but_keeps_unknown_kinds() {
        let config = lanes_selection_config();
        let narrowed = selection_for_lanes(
            &config,
            must(full_selection(&config, None), "full selection"),
            RunnerMode::Velnor,
        );
        let ids = narrowed
            .units
            .iter()
            .map(|unit| unit.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["rust-app", "future-app"]);
        assert_eq!(
            narrowed.full_units,
            ["future-app", "rust-app"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        for lanes in [RunnerMode::Github, RunnerMode::Both] {
            let narrowed = selection_for_lanes(
                &config,
                must(full_selection(&config, None), "full selection"),
                lanes,
            );
            let ids = narrowed
                .units
                .iter()
                .map(|unit| unit.id.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                vec!["rust-app", "swift-app", "future-app"],
                "{lanes:?}"
            );
            assert_eq!(narrowed.full_units.len(), 3, "{lanes:?}");
        }
    }

    #[test]
    fn velnor_only_selection_prunes_full_marks_of_excluded_units() {
        let config = lanes_selection_config();
        let units = config.unit.iter().collect::<Vec<_>>();
        let selection = UnitSelection {
            units,
            full_units: ["swift-app"].into_iter().map(str::to_owned).collect(),
            fallback_reason: None,
            no_work_reason: None,
        };
        let narrowed = selection_for_lanes(&config, selection, RunnerMode::Velnor);
        assert!(narrowed.full_units.is_empty());
        assert!(narrowed.units.iter().all(|unit| unit.id != "swift-app"));
    }

    #[test]
    fn velnor_only_plan_output_excludes_swift_and_empties_its_matrix() -> Result<(), Box<dyn Error>>
    {
        let id = crate::unique_suffix();
        let path = std::env::temp_dir().join(format!(
            "velnor-workflow-runner-matrices-{}-{id}",
            std::process::id()
        ));
        let config = lanes_selection_config();
        let narrowed = selection_for_lanes(
            &config,
            full_selection(&config, None).map_err(|error| error.to_string())?,
            RunnerMode::Velnor,
        );
        let units = narrowed
            .units
            .iter()
            .map(|unit| unit.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        assert!(!units.contains("swift-app"), "units={units}");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        write_kind_matrices(&mut file, &config, &narrowed, &path)
            .map_err(|error| error.to_string())?;
        drop(file);
        let output = std::fs::read_to_string(&path)?;
        std::fs::remove_file(&path)?;
        assert!(
            !output.contains("swift_matrix=[{"),
            "swift matrix must be omitted when no swift units are selected: {output}"
        );
        assert!(
            output.contains("rust_matrix=[{")
                && output.contains("rust-app")
                && output.contains("quantum_matrix=[{"),
            "supported kinds keep their matrices: {output}"
        );
        Ok(())
    }

    #[test]
    fn selection_artifact_round_trips_scope_and_sha() -> Result<(), Box<dyn Error>> {
        let id = crate::unique_suffix();
        let path = std::env::temp_dir().join(format!(
            "velnor-workflow-selection-artifact-{}-{id}",
            std::process::id()
        ));
        write_selection_file(
            &path,
            "base-sha",
            "head-sha",
            Scope::Affected,
            "base,app",
            "app",
        )?;
        let selection = read_selection_file(&path)?;
        assert_eq!(selection.base_sha, "base-sha");
        assert_eq!(selection.head_sha, "head-sha");
        assert_eq!(selection.scope, Scope::Affected);
        assert_eq!(
            selection.units,
            ["app", "base"].into_iter().map(str::to_owned).collect()
        );
        assert_eq!(
            selection.full_units,
            ["app"].into_iter().map(str::to_owned).collect()
        );
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn unselected_unit_fails_closed_instead_of_succeeding_as_a_noop() {
        let config = selection_config();
        let selected = config.unit.iter().collect::<Vec<_>>();
        let result = super::select_units_for_job(selected, Some("not-selected"));
        assert!(
            result.as_ref().is_err_and(|error| error
                .to_string()
                .contains("does not include requested unit `not-selected`")),
            "an unselected unit must fail closed with a useful error: {result:?}"
        );
    }

    #[test]
    fn prerequisite_tier_selects_the_stored_check_phase() {
        let unit = CiUnit {
            id: "rust-app".to_owned(),
            label: "rust-app".to_owned(),
            kind: "rust".to_owned(),
            root: ".".to_owned(),
            watch: vec!["crates/app/**".to_owned()],
            // The selection never inspects these: the check below is the
            // contract even though no command here mentions clippy.
            github_pr_commands: vec![
                "cargo fmt --check".to_owned(),
                "cargo nextest run --locked".to_owned(),
            ],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["mbx nextest run --locked".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            phases: vec![ValidationPhase::Fmt, ValidationPhase::Test],
            check_commands: vec!["cargo check --locked --no-deps --all-targets".to_owned()],
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        assert_eq!(
            must(
                prerequisite_commands(&unit, RunnerLane::Github, Scope::Affected, None),
                "github prerequisite",
            ),
            vec!["cargo check --locked --no-deps --all-targets"]
        );
        assert_eq!(
            must(
                prerequisite_commands(&unit, RunnerLane::Velnor, Scope::Affected, None),
                "velnor prerequisite",
            ),
            // `mbx check` does not expose Cargo's `--no-deps` flag.
            vec!["cargo check --locked --all-targets"]
        );
        // The prerequisite tier compiles the unit in the first validation
        // step; later phase steps no-op.
        assert_eq!(
            must(
                prerequisite_commands(
                    &unit,
                    RunnerLane::Github,
                    Scope::Affected,
                    Some(ValidationPhase::Fmt),
                ),
                "first-step prerequisite",
            ),
            vec!["cargo check --locked --no-deps --all-targets"]
        );
        assert!(
            must(
                prerequisite_commands(
                    &unit,
                    RunnerLane::Github,
                    Scope::Affected,
                    Some(ValidationPhase::Test),
                ),
                "later-step prerequisite",
            )
            .is_empty(),
            "later phase steps no-op on the prerequisite tier"
        );
        assert!(
            must(
                prerequisite_commands(
                    &unit,
                    RunnerLane::Github,
                    Scope::Affected,
                    Some(ValidationPhase::Check),
                ),
                "check-phase prerequisite",
            ) == vec!["cargo check --locked --no-deps --all-targets"],
            "--phase check selects the stored check commands"
        );
    }

    #[test]
    fn prerequisite_tier_falls_back_to_clippy_rewrite_without_phases() {
        // Old-TOML shape: no `phases` or `check_commands` keys, so both
        // deserialize empty. The prerequisite tier must behave byte-identical
        // to the pre-phase clippy→check rewrite, never silently no-op.
        let unit = CiUnit {
            id: "rust-app".to_owned(),
            label: "rust-app".to_owned(),
            kind: "rust".to_owned(),
            root: ".".to_owned(),
            watch: vec!["crates/app/**".to_owned()],
            github_pr_commands: vec![
                "cargo fmt --check".to_owned(),
                "cargo clippy --locked --no-deps --all-targets -- -D warnings".to_owned(),
                "cargo nextest run --locked".to_owned(),
            ],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec![
                "mbx clippy --locked --no-deps --all-targets -- -D warnings".to_owned()
            ],
            velnor_full_commands: vec!["true".to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        assert_eq!(
            must(
                prerequisite_commands(&unit, RunnerLane::Github, Scope::Affected, None),
                "github prerequisite",
            ),
            vec!["cargo check --locked --no-deps --all-targets"]
        );
        assert_eq!(
            must(
                prerequisite_commands(&unit, RunnerLane::Velnor, Scope::Affected, None),
                "velnor prerequisite",
            ),
            vec!["mbx check --locked --all-targets"]
        );
        // The fallback also covers the first validation step and an explicit
        // `--phase check`; later phase steps still no-op.
        for phase in [
            None,
            Some(ValidationPhase::Fmt),
            Some(ValidationPhase::Check),
        ] {
            assert_eq!(
                must(
                    prerequisite_commands(&unit, RunnerLane::Github, Scope::Affected, phase),
                    "unphased fallback follows the phase",
                ),
                vec!["cargo check --locked --no-deps --all-targets"]
            );
        }
        assert!(
            must(
                prerequisite_commands(
                    &unit,
                    RunnerLane::Github,
                    Scope::Affected,
                    Some(ValidationPhase::Test),
                ),
                "later-step prerequisite",
            )
            .is_empty(),
            "later phase steps no-op on the prerequisite tier"
        );
        // Non-rust units have no prerequisite, phased or not.
        let mut foreign = unit.clone();
        foreign.kind = "bun".to_owned();
        assert!(
            must(
                prerequisite_commands(&foreign, RunnerLane::Github, Scope::Affected, None),
                "non-rust prerequisite",
            )
            .is_empty(),
            "unphased non-rust units no-op on the prerequisite tier"
        );
    }

    #[test]
    fn phase_selection_filters_positionally_and_fails_closed() {
        let unit = CiUnit {
            id: "rust-app".to_owned(),
            label: "rust-app".to_owned(),
            kind: "rust".to_owned(),
            root: ".".to_owned(),
            watch: vec!["crates/app/**".to_owned()],
            github_pr_commands: vec![
                "cargo fmt --check".to_owned(),
                "cargo clippy -- -D warnings".to_owned(),
                "cargo nextest run".to_owned(),
            ],
            github_full_commands: vec![
                "cargo fmt --check".to_owned(),
                "cargo clippy -- -D warnings".to_owned(),
                "cargo nextest run".to_owned(),
            ],
            velnor_pr_commands: vec![
                "mbx fmt --check".to_owned(),
                "mbx clippy -- -D warnings".to_owned(),
                "mbx nextest run".to_owned(),
            ],
            velnor_full_commands: vec![
                "mbx fmt --check".to_owned(),
                "mbx clippy -- -D warnings".to_owned(),
                "mbx nextest run".to_owned(),
            ],
            phases: vec![
                ValidationPhase::Fmt,
                ValidationPhase::Clippy,
                ValidationPhase::Test,
            ],
            check_commands: vec!["cargo check --locked --no-deps".to_owned()],
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        for (lane, tool) in [(RunnerLane::Github, "cargo"), (RunnerLane::Velnor, "mbx")] {
            for scope in [Scope::Affected, Scope::Full] {
                assert_eq!(
                    must(
                        unit.commands_for_phase(lane, scope, ValidationPhase::Fmt),
                        "fmt selection",
                    ),
                    vec![format!("{tool} fmt --check")],
                );
                assert_eq!(
                    must(
                        unit.commands_for_phase(lane, scope, ValidationPhase::Clippy),
                        "clippy selection",
                    ),
                    vec![format!("{tool} clippy -- -D warnings")],
                );
                assert_eq!(
                    must(
                        unit.commands_for_phase(lane, scope, ValidationPhase::Test),
                        "test selection",
                    ),
                    vec![format!("{tool} nextest run")],
                );
            }
        }
        let error = must_fail(
            unit.commands_for_phase(RunnerLane::Github, Scope::Full, ValidationPhase::Doctest),
            "a phase the unit does not carry",
        );
        assert!(
            error.to_string().contains("has no doctest phase"),
            "missing phases fail closed: {error}"
        );
        let mut bare = unit.clone();
        bare.phases.clear();
        bare.check_commands.clear();
        let error = must_fail(
            bare.commands_for_phase(RunnerLane::Github, Scope::Full, ValidationPhase::Fmt),
            "an unphased unit",
        );
        assert!(
            error.to_string().contains("has no validation phases"),
            "unphased units fail closed instead of silently skipping: {error}"
        );
        let error = must_fail(
            bare.commands_for_phase(RunnerLane::Github, Scope::Full, ValidationPhase::Check),
            "an unphased unit with --phase check",
        );
        assert!(
            error.to_string().contains("has no validation phases"),
            "--phase check on an unphased unit fails closed: {error}"
        );
        let mut skewed = unit.clone();
        skewed.github_pr_commands.push("extra".to_owned());
        let error = must_fail(
            skewed.commands_for_phase(RunnerLane::Github, Scope::Affected, ValidationPhase::Fmt),
            "misaligned tags",
        );
        assert!(
            error.to_string().contains("misaligned"),
            "tag/command misalignments fail closed: {error}"
        );
        let mut corrupt = unit.clone();
        corrupt.check_commands.clear();
        let error = must_fail(
            corrupt.commands_for_phase(RunnerLane::Github, Scope::Full, ValidationPhase::Check),
            "phases without a stored check",
        );
        assert!(
            error
                .to_string()
                .contains("without prerequisite check commands"),
            "a phased unit without a check fails closed: {error}"
        );
    }

    #[test]
    fn run_rejects_unknown_phases_before_touching_the_filesystem() {
        let args = ["run", "--phase", "fuzz"]
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        let error = must_fail(super::try_run(&args), "unknown phase");
        assert!(
            error
                .to_string()
                .contains("unsupported --phase: fuzz; use fmt, clippy, test, doctest, or check"),
            "the failure lists the valid phases: {error}"
        );
        // A valid phase parses through to execution: the missing config,
        // not the selector, fails.
        let args = [
            "run",
            "--phase",
            "fmt",
            "--config",
            "/nonexistent-project-toml-dir/project.toml",
        ]
        .iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        let error = must_fail(super::try_run(&args), "missing config");
        assert!(
            error.to_string().contains("project.toml"),
            "a valid phase reaches execution: {error}"
        );
    }

    #[test]
    fn affected_selection_expands_dependency_and_dependent_closure() -> Result<(), Box<dyn Error>> {
        let (root, base, head) = selection_git_fixture("closure", "crates/base/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            ["app", "base", "consumer"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        std::fs::remove_dir_all(root)?;

        let (root, base, head) = selection_git_fixture("prerequisite", "crates/app/src/lib.rs")?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            ["app", "consumer"].into_iter().map(str::to_owned).collect()
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn slice_c_selection_agrees_with_runtime_selection() -> Result<(), Box<dyn Error>> {
        // The auditable core and the runtime planner share one selection
        // model: the same change list selects the same required and full sets
        // on both paths. The fixture carries no workspace gates or version
        // bumps, so the planner's refinements stay out of the comparison.
        let config = selection_config();
        let watched: Vec<crate::reuse::WatchedUnit> = config
            .unit
            .iter()
            .map(|unit| crate::reuse::WatchedUnit {
                id: unit.id.clone(),
                watch: unit.watch.clone(),
                depends_on: unit.depends_on.clone(),
                kind: unit.kind.clone(),
                commands: unit
                    .github_pr_commands
                    .iter()
                    .chain(&unit.github_full_commands)
                    .chain(&unit.velnor_pr_commands)
                    .chain(&unit.velnor_full_commands)
                    .cloned()
                    .collect(),
            })
            .collect();
        for changed in [
            "crates/base/src/lib.rs",
            "crates/app/src/lib.rs",
            "crates/consumer/src/lib.rs",
            "docs/index.md",
            "README.md",
            ".github/workflows/ci.yml",
        ] {
            let (root, base, head) = selection_git_fixture("slice-c", changed)?;
            let runtime_selection =
                selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
            let raw = git_name_status_nul(&root, &base, &head)?.ok_or("the diff must resolve")?;
            let changed_files = parse_name_status_nul(&raw).ok_or("the diff must parse")?;
            let changes: Vec<crate::reuse::ChangedPath> = changed_files
                .into_iter()
                .map(|path| crate::reuse::ChangedPath {
                    path,
                    previous: None,
                    status: crate::reuse::ChangeKind::Modified,
                })
                .collect();
            let model = crate::reuse::select_affected(
                &watched,
                &changes,
                crate::reuse::FULL_SELECTION_PREFIXES,
            )?;
            assert_eq!(
                selected_id_set(&runtime_selection),
                model.required,
                "required set for {changed}"
            );
            assert_eq!(
                runtime_selection.full_units, model.full_units,
                "full set for {changed}"
            );
            std::fs::remove_dir_all(root)?;
        }
        let (root, base, _) = selection_git_fixture("slice-c-empty", "crates/base/src/lib.rs")?;
        let runtime_selection = selection_for_diff(&root, &config, Scope::Affected, &base, &base)?;
        let model =
            crate::reuse::select_affected(&watched, &[], crate::reuse::FULL_SELECTION_PREFIXES)?;
        assert!(selected_id_set(&runtime_selection).is_empty());
        assert!(model.required.is_empty() && !model.fallback_full);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_falls_back_to_full_for_unknown_contract_changes(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, head) = selection_git_fixture("github", ".github/workflows/ci.yml")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer", "docs"]
        );
        assert!(
            selection
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("classification table")),
            "the planner surfaces the fallback reason: {:?}",
            selection.fallback_reason
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_selects_nothing_for_an_unowned_transparent_change(
    ) -> Result<(), Box<dyn Error>> {
        // `README.md` matches no watch and every unit is transparent (rust
        // units running the no-op), so the planner selects nothing.
        let (root, base, head) = selection_git_fixture("unmatched", "README.md")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert!(selection.units.is_empty());
        assert!(selection.full_units.is_empty());
        assert!(selection.fallback_reason.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_narrows_an_unmatched_path_to_opaque_units() -> Result<(), Box<dyn Error>>
    {
        // `AGENTS.md` matches no watch; only `app` runs unprovable package
        // commands, so the planner selects it plus its prerequisite and
        // dependent — not the transparent units, not the full set.
        let (root, base, head) = selection_git_fixture("opaque", "AGENTS.md")?;
        let mut config = selection_config();
        let app = config
            .unit
            .iter_mut()
            .find(|unit| unit.id == "app")
            .ok_or("the fixture must carry the app unit")?;
        app.kind = "bun".to_owned();
        app.github_pr_commands = vec!["bun run build".to_owned()];
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            BTreeSet::from(["app".to_owned(), "consumer".to_owned()]),
            "the prerequisite joins the required set, not full"
        );
        assert!(
            selection
                .fallback_reason
                .as_deref()
                .is_some_and(
                    |reason| reason.contains("selects only opaque units") && reason.contains("app")
                ),
            "the planner surfaces the opaque-narrowing reason: {:?}",
            selection.fallback_reason
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_narrows_a_kind_reusable_to_that_kind() -> Result<(), Box<dyn Error>> {
        let (root, base, head) =
            selection_git_fixture("kind", ".github/workflows/ci-unit-rust.yml")?;
        let config = polyglot_selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(selected_ids(selection.units), vec!["rust-alpha"]);
        assert!(selection.fallback_reason.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_selects_nothing_for_release_lanes() -> Result<(), Box<dyn Error>> {
        for (name, changed) in [
            ("release", ".github/workflows/release.yml"),
            ("maintenance", ".github/workflows/maintenance.yml"),
        ] {
            let (root, base, head) = selection_git_fixture(name, changed)?;
            let config = polyglot_selection_config();
            let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
            assert!(
                selection.units.is_empty() && selection.full_units.is_empty(),
                "release lanes select nothing in affected scope: {changed}"
            );
            std::fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn affected_selection_narrows_to_the_opaque_package_unit() -> Result<(), Box<dyn Error>> {
        // The polyglot fixture's package unit runs the no-op (transparent);
        // real package commands make it opaque, so the same unmatched change
        // selects only it, with the consulted sources and the opaque unit in
        // the reason.
        let (root, base, head) = selection_git_fixture("opaque", "AGENTS.md")?;
        let mut config = polyglot_selection_config();
        let bun = config
            .unit
            .iter_mut()
            .find(|unit| unit.id == "bun-web")
            .ok_or("the polyglot fixture must carry bun-web")?;
        bun.github_pr_commands = vec!["bun run build".to_owned()];
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(selected_ids(selection.units), vec!["bun-web"]);
        assert!(
            selection
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("command read-globs")
                    && reason.contains("selects only opaque units")
                    && reason.contains("bun-web")
                    && reason.contains("missing contract")
                    && reason.contains("for unit `bun-web`")
                    && reason.contains("covering `AGENTS.md`")),
            "the fallback reason names the consulted sources, the opaque unit, and the missing contract: {:?}",
            selection.fallback_reason
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_is_empty_for_an_empty_diff() -> Result<(), Box<dyn Error>> {
        let (root, base, _) = selection_git_fixture("empty", "crates/base/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &base)?;
        assert!(selection.units.is_empty());
        assert!(selection.full_units.is_empty());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn unmatched_root_doc_selects_no_workload_units() -> Result<(), Box<dyn Error>> {
        // A change no watch owns selects nothing: the planner records
        // explicit no-work instead of running the estate.
        let (root, base, head) = selection_git_fixture("polyglot-unmatched", "AGENTS.md")?;
        let config = polyglot_selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert!(
            selection.units.is_empty() && selection.full_units.is_empty(),
            "an unmatched AGENTS.md must select no units, got {:?}",
            selection.full_units,
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_matches_both_rename_sides() -> Result<(), Box<dyn Error>> {
        // A rename invalidates the old owner (the file left) and the new
        // owner (the file arrived): both select, plus the dependent closure.
        let (root, base, head) = selection_git_rename_fixture(
            "rename-both",
            "crates/base/src/lib.rs",
            "crates/app/src/moved.rs",
        )?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            ["app", "base", "consumer"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert!(selection.fallback_reason.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_matches_the_rename_source_when_the_target_is_unowned(
    ) -> Result<(), Box<dyn Error>> {
        // The target matches nothing, but the source still selects its owner:
        // a rename out of a watched tree is not a disappearance.
        let (root, base, head) = selection_git_rename_fixture(
            "rename-source",
            "crates/base/src/lib.rs",
            "notes/moved.md",
        )?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            ["app", "base", "consumer"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert!(selection.fallback_reason.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_selects_the_owner_of_a_deleted_file() -> Result<(), Box<dyn Error>> {
        let (root, base, head) =
            selection_git_delete_fixture("delete-owned", "crates/app/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            ["app", "consumer"].into_iter().map(str::to_owned).collect()
        );
        assert!(selection.fallback_reason.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_matches_unusual_owned_filenames_exactly() -> Result<(), Box<dyn Error>> {
        // Quotes and glob metacharacters in an owned path match their owner
        // exactly instead of failing closed on the quoted form.
        let (root, base, head) = selection_git_fixture("unusual-owned", "crates/app/we\"ird?.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert_eq!(
            selection.full_units,
            ["app", "consumer"].into_iter().map(str::to_owned).collect()
        );
        assert!(selection.fallback_reason.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn unmatched_unusual_filename_selects_nothing() -> Result<(), Box<dyn Error>> {
        // Spaces, quotes, and newlines in an unowned path parse exactly and
        // select nothing instead of failing closed on the quoted form.
        let (root, base, head) =
            selection_git_fixture("unusual-unowned", "notes/with \"quotes\" and\nnewline.txt")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert!(selection.units.is_empty());
        assert!(selection.full_units.is_empty());
        assert!(selection.fallback_reason.is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn plan_selection_agrees_with_select_path_on_rename_and_delete() -> Result<(), Box<dyn Error>> {
        // The plan path and the select path share one rename/delete model: a
        // rename matches both owners, a delete matches its owner.
        let config = selection_config();
        let watched: Vec<crate::reuse::WatchedUnit> = config
            .unit
            .iter()
            .map(|unit| crate::reuse::WatchedUnit {
                id: unit.id.clone(),
                watch: unit.watch.clone(),
                depends_on: unit.depends_on.clone(),
                kind: unit.kind.clone(),
                commands: unit
                    .github_pr_commands
                    .iter()
                    .chain(&unit.github_full_commands)
                    .chain(&unit.velnor_pr_commands)
                    .chain(&unit.velnor_full_commands)
                    .cloned()
                    .collect(),
            })
            .collect();
        let (root, base, head) = selection_git_rename_fixture(
            "parity-rename",
            "crates/base/src/lib.rs",
            "crates/app/src/moved.rs",
        )?;
        let runtime_selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let model = crate::reuse::select_affected(
            &watched,
            &[crate::reuse::ChangedPath {
                path: "crates/app/src/moved.rs".to_owned(),
                previous: Some("crates/base/src/lib.rs".to_owned()),
                status: crate::reuse::ChangeKind::Renamed,
            }],
            crate::reuse::FULL_SELECTION_PREFIXES,
        )?;
        assert_eq!(
            selected_id_set(&runtime_selection),
            model.required,
            "required set for the rename"
        );
        assert_eq!(
            runtime_selection.full_units, model.full_units,
            "full set for the rename"
        );
        std::fs::remove_dir_all(root)?;
        let (root, base, head) =
            selection_git_delete_fixture("parity-delete", "crates/app/src/lib.rs")?;
        let runtime_selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let model = crate::reuse::select_affected(
            &watched,
            &[crate::reuse::ChangedPath {
                path: "crates/app/src/lib.rs".to_owned(),
                previous: None,
                status: crate::reuse::ChangeKind::Deleted,
            }],
            crate::reuse::FULL_SELECTION_PREFIXES,
        )?;
        assert_eq!(
            selected_id_set(&runtime_selection),
            model.required,
            "required set for the delete"
        );
        assert_eq!(
            runtime_selection.full_units, model.full_units,
            "full set for the delete"
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn name_status_nul_parses_exact_paths() {
        // Status entries arrive verbatim: spaces, quotes, unicode, newlines,
        // and glob metacharacters survive because NUL delimits records.
        // Renames contribute target then source; copies contribute the target
        // only; deletes keep their path.
        for (input, expected) in [
            ("".as_bytes(), Vec::new()),
            ("M\0src/lib.rs\0".as_bytes(), vec!["src/lib.rs".to_owned()]),
            (
                "A\0dir with space/f ile.txt\0".as_bytes(),
                vec!["dir with space/f ile.txt".to_owned()],
            ),
            (
                "M\0we\"ird?.rs\0".as_bytes(),
                vec!["we\"ird?.rs".to_owned()],
            ),
            (
                "M\0ünïcode/[bracket]*.rs\0".as_bytes(),
                vec!["ünïcode/[bracket]*.rs".to_owned()],
            ),
            (
                "M\0with\nnewline.txt\0".as_bytes(),
                vec!["with\nnewline.txt".to_owned()],
            ),
            ("D\0gone.rs\0".as_bytes(), vec!["gone.rs".to_owned()]),
            ("T\0link.rs\0".as_bytes(), vec!["link.rs".to_owned()]),
            (
                "R100\0old.rs\0new.rs\0".as_bytes(),
                vec!["new.rs".to_owned(), "old.rs".to_owned()],
            ),
            (
                "R050\0a.rs\0b.rs\0".as_bytes(),
                vec!["b.rs".to_owned(), "a.rs".to_owned()],
            ),
            ("C75\0a.rs\0b.rs\0".as_bytes(), vec!["b.rs".to_owned()]),
            (
                "M\0a.rs\0D\0b.rs\0R100\0c.rs\0d.rs\0".as_bytes(),
                vec![
                    "a.rs".to_owned(),
                    "b.rs".to_owned(),
                    "d.rs".to_owned(),
                    "c.rs".to_owned(),
                ],
            ),
        ] {
            assert_eq!(
                parse_name_status_nul(input),
                Some(expected),
                "input {input:?} must parse exactly"
            );
        }
    }

    #[test]
    fn name_status_nul_fails_closed_on_malformed_input() {
        // Unknown statuses, truncated records, empty paths, trailing
        // garbage, and non-UTF-8 bytes never parse: the caller falls back to
        // full instead of matching a mangled name.
        for input in [
            "X\0a.rs\0".as_bytes(),
            "U\0a.rs\0".as_bytes(),
            "m\0a.rs\0".as_bytes(),
            "R\0a.rs\0b.rs\0".as_bytes(),
            "Rx\0a.rs\0b.rs\0".as_bytes(),
            "C\0a.rs\0b.rs\0".as_bytes(),
            "M\0".as_bytes(),
            "M\0a.rs".as_bytes(),
            "R100\0a.rs\0".as_bytes(),
            "R100\0a.rs".as_bytes(),
            "M\0\0".as_bytes(),
            "R100\0\0b.rs\0".as_bytes(),
            "M\0a.rs\0junk".as_bytes(),
            "A\0a.rs\0\0".as_bytes(),
            "\0".as_bytes(),
            b"M\0\xff.rs\0".as_slice(),
        ] {
            assert_eq!(
                parse_name_status_nul(input),
                None,
                "input {input:?} must fail closed"
            );
        }
    }

    #[test]
    fn unit_jobs_require_full_scope_for_trusted_events() {
        // The unit-job guard mirrors the plan-time verdict: push, schedule,
        // and merge-queue validation always run full scope.
        for event in ["push", "schedule", "merge_group"] {
            assert!(
                event_requires_full_scope(event),
                "{event} is a trusted event"
            );
        }
        for event in ["", "pull_request", "workflow_dispatch"] {
            assert!(
                !event_requires_full_scope(event),
                "{event} admits affected scope"
            );
        }
    }

    #[test]
    fn affected_selection_falls_back_to_full_for_a_stale_base() -> Result<(), Box<dyn Error>> {
        let (root, _, head) = selection_git_fixture("stale-base", "crates/base/src/lib.rs")?;
        let config = selection_config();
        let units = selected_units_for_diff(
            &root,
            &config,
            Scope::Affected,
            "0000000000000000000000000000000000000001",
            &head,
        )?;
        assert_eq!(selected_ids(units), vec!["base", "app", "consumer", "docs"]);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_uses_merge_base_for_a_stale_branch_base() -> Result<(), Box<dyn Error>> {
        let (root, base, head) = stale_base_selection_git_fixture()?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["base", "app", "consumer"]
        );
        assert!(!selection.full_units.contains("docs"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn cargo_version_bump_uses_the_explicit_allowlist_ids() -> Result<(), Box<dyn Error>> {
        let (root, base, head) = lockfile_version_selection_git_fixture()?;
        let command_unit = |id: &str, watch: &[&str]| CiUnit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: "rust".to_owned(),
            root: ".".to_owned(),
            watch: watch.iter().map(|value| (*value).to_owned()).collect(),
            github_pr_commands: vec!["true".to_owned()],
            github_full_commands: vec!["true".to_owned()],
            velnor_pr_commands: vec!["true".to_owned()],
            velnor_full_commands: vec!["true".to_owned()],
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            tool_version: None,
            cache: None,
            workspace_check: false,
        };
        let mut config = selection_config();
        config.workflow.version_bump_units = vec![
            "rust-runner".to_owned(),
            "rust-ctl".to_owned(),
            "docker".to_owned(),
        ];
        config.unit = vec![
            command_unit("rust-runner", &["crates/runner/**"]),
            command_unit("rust-ctl", &["crates/ctl/**"]),
            command_unit("docker", &["Dockerfile"]),
            command_unit("unrelated", &["unrelated/**"]),
        ];
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units),
            vec!["rust-runner", "rust-ctl", "docker"]
        );
        assert_eq!(
            selection.full_units,
            ["docker", "rust-ctl", "rust-runner"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn current_project_cargo_lock_only_change_selects_exact_allowlist_units(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, head) = current_project_selection_git_fixture(
            "cargo-lock-only",
            "Cargo.lock",
            "[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
            "[[package]]\nname = \"fixture\"\nversion = \"0.1.1\"\n",
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected = ["docker", "rust-bench", "rust-leaf"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(selected_id_set(&selection), expected);
        assert_eq!(selection.full_units, expected);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn current_project_leaf_change_selects_exact_dependency_closure() -> Result<(), Box<dyn Error>>
    {
        let (root, base, head) = current_project_selection_git_fixture(
            "leaf-source",
            "crates/leaf/src/lib.rs",
            "pub fn fixture() {}\n",
            "pub fn fixture() { let _ = 1; }\n",
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected_selected = ["rust-base", "rust-leaf"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        // `full_units` carries the affected set (the changed unit and its
        // dependents); the dependency closure lives on the selected side.
        let expected_full = ["rust-leaf"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(selected_id_set(&selection), expected_selected);
        assert_eq!(selection.full_units, expected_full);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn workspace_checks_are_selected_only_for_matching_cargo_roots() -> Result<(), Box<dyn Error>> {
        let workspace_unit = r#"[[unit]]
id = "rust-workspace"
kind = "rust"
root = "."
watch = ["Cargo.toml", "Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true

[[unit]]
id = "rust-contract"
kind = "rust"
root = "crates/contract"
watch = ["crates/contract/**", "crates/contract/Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'Cargo.toml'"]
[unit.cache]
key_files = ["crates/contract/Cargo.lock"]
paths = ["~/.cargo/registry"]

[[unit]]
id = "rust-contract-workspace"
kind = "rust"
root = "crates/contract"
watch = ["crates/contract/Cargo.toml", "crates/contract/Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true

[[unit]]
id = "docs"
kind = "docs"
root = "."
watch = ["docs/**"]
github_pr_commands = ["markdownlint docs"]
github_full_commands = ["markdownlint docs"]
velnor_pr_commands = ["markdownlint docs"]
velnor_full_commands = ["markdownlint docs"]
"#;
        let config_text = format!("{SELECTION_PROJECT_CONFIG}\n{workspace_unit}");
        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "leaf-source-workspace-check",
            "crates/leaf/src/lib.rs",
            "pub fn fixture() {}\n",
            "pub fn fixture() { let _ = 1; }\n",
            &config_text,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_id_set(&selection),
            ["rust-base", "rust-leaf", "rust-workspace"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(
            selection.full_units,
            ["rust-leaf", "rust-workspace"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        std::fs::remove_dir_all(root)?;

        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "contract-source-workspace-check",
            "crates/contract/src/lib.rs",
            "initial\n",
            "changed\n",
            &config_text,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_id_set(&selection),
            ["rust-contract", "rust-contract-workspace"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(
            selection.full_units,
            ["rust-contract", "rust-contract-workspace"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert!(!selection.full_units.contains("rust-workspace"));
        std::fs::remove_dir_all(root)?;

        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "docs-source-workspace-check",
            "docs/index.md",
            "initial\n",
            "changed\n",
            &config_text,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_id_set(&selection),
            BTreeSet::from(["docs".to_owned()])
        );
        assert!(!selection.full_units.contains("rust-workspace"));
        assert!(!selection.full_units.contains("rust-contract-workspace"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn mixed_workspace_roots_select_only_the_matching_checks_and_keep_closure(
    ) -> Result<(), Box<dyn Error>> {
        let workspace_unit = r#"[[unit]]
id = "rust-workspace"
kind = "rust"
root = "."
watch = ["Cargo.toml", "Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true

[[unit]]
id = "rust-contract"
kind = "rust"
root = "crates/contract"
watch = ["crates/contract/**", "crates/contract/Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'Cargo.toml'"]
[unit.cache]
key_files = ["crates/contract/Cargo.lock"]
paths = ["~/.cargo/registry"]

[[unit]]
id = "rust-contract-workspace"
kind = "rust"
root = "crates/contract"
watch = ["crates/contract/Cargo.toml", "crates/contract/Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true
"#;
        let config_text = format!("{SELECTION_PROJECT_CONFIG}\n{workspace_unit}");
        let (root, base, head) = current_project_selection_git_fixture_with_changes(
            "mixed-workspace-roots",
            &[
                ("crates/leaf/src/lib.rs", "initial\n", "changed leaf\n"),
                (
                    "crates/contract/src/lib.rs",
                    "initial\n",
                    "changed contract\n",
                ),
            ],
            &config_text,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_id_set(&selection),
            [
                "rust-base",
                "rust-contract",
                "rust-contract-workspace",
                "rust-leaf",
                "rust-workspace"
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        assert_eq!(
            selection.full_units,
            [
                "rust-contract",
                "rust-contract-workspace",
                "rust-leaf",
                "rust-workspace"
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn version_bump_selects_workspace_check_for_the_allowlisted_root_only(
    ) -> Result<(), Box<dyn Error>> {
        let workspace_unit = r#"[[unit]]
id = "rust-workspace"
kind = "rust"
root = "."
watch = ["Cargo.toml", "Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true

[[unit]]
id = "rust-contract"
kind = "rust"
root = "crates/contract"
watch = ["crates/contract/**", "crates/contract/Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'Cargo.toml'"]
[unit.cache]
key_files = ["crates/contract/Cargo.lock"]
paths = ["~/.cargo/registry"]

[[unit]]
id = "rust-contract-workspace"
kind = "rust"
root = "crates/contract"
watch = ["crates/contract/Cargo.toml", "crates/contract/Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true
"#;
        let config_text = format!("{SELECTION_PROJECT_CONFIG}\n{workspace_unit}").replace(
            "version_bump_units = [\"docker\", \"rust-bench\", \"rust-leaf\"]",
            "version_bump_units = [\"rust-contract\"]",
        );
        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "contract-version-bump",
            "crates/contract/Cargo.toml",
            "version = \"0.1.0\"\n",
            "version = \"0.1.1\"\n",
            &config_text,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_id_set(&selection),
            ["rust-contract", "rust-contract-workspace"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(
            selection.full_units,
            ["rust-contract", "rust-contract-workspace"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert!(!selection.full_units.contains("rust-workspace"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn independent_manifest_and_lock_version_bump_selects_matching_workspace_check(
    ) -> Result<(), Box<dyn Error>> {
        let workspace_unit = r#"[[unit]]
id = "rust-contract-workspace"
kind = "rust"
root = "crates/contract"
watch = ["crates/contract/Cargo.toml", "crates/contract/Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true

[[unit]]
id = "rust-contract"
kind = "rust"
root = "crates/contract"
watch = ["crates/contract/**", "crates/contract/Cargo.lock"]
github_pr_commands = ["cargo test --manifest-path 'Cargo.toml'"]
github_full_commands = ["cargo test --manifest-path 'Cargo.toml'"]
velnor_pr_commands = ["cargo test --manifest-path 'Cargo.toml'"]
velnor_full_commands = ["cargo test --manifest-path 'Cargo.toml'"]
[unit.cache]
key_files = ["crates/contract/Cargo.lock"]
paths = ["~/.cargo/registry"]
"#;
        let config_text = format!("{SELECTION_PROJECT_CONFIG}\n{workspace_unit}").replace(
            "version_bump_units = [\"docker\", \"rust-bench\", \"rust-leaf\"]",
            "version_bump_units = [\"rust-contract\"]",
        );
        let (root, base, head) = current_project_selection_git_fixture_with_changes(
            "contract-version-bump-with-lock",
            &[
                (
                    "crates/contract/Cargo.toml",
                    "version = \"0.1.0\"\n",
                    "version = \"0.1.1\"\n",
                ),
                (
                    "crates/contract/Cargo.lock",
                    "version = \"1\"\n",
                    "version = \"2\"\n",
                ),
            ],
            &config_text,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_id_set(&selection),
            ["rust-contract", "rust-contract-workspace"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
        assert_eq!(selection.full_units, selected_id_set(&selection));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn hosted_selection_keeps_matching_workspace_checks() -> Result<(), Box<dyn Error>> {
        let workspace_unit = r#"[[unit]]
id = "rust-workspace"
kind = "rust"
root = "."
watch = ["Cargo.toml", "Cargo.lock"]
github_pr_commands = ["cargo check --workspace --all-targets --locked"]
github_full_commands = ["cargo check --workspace --all-targets --locked"]
velnor_pr_commands = ["cargo check --workspace --all-targets --locked"]
velnor_full_commands = ["cargo check --workspace --all-targets --locked"]
workspace_check = true
"#;
        let config_text = format!("{SELECTION_PROJECT_CONFIG}\n{workspace_unit}");
        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "hosted-workspace-check",
            "crates/leaf/src/lib.rs",
            "initial\n",
            "changed\n",
            &config_text,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let hosted = selection_for_lanes(&config, selection, RunnerMode::Github);
        assert!(hosted.units.iter().any(|unit| unit.id == "rust-workspace"));
        assert!(hosted.full_units.contains("rust-workspace"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_scopes_root_workspace_check_to_root_cargo_project(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "root-cargo-project",
            "crates/root/src/lib.rs",
            "initial\n",
            "changed\n",
            ROOT_SCOPED_SELECTION_PROJECT_CONFIG,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected = BTreeSet::from(["rust-root".to_owned(), "rust-root-workspace".to_owned()]);
        assert_eq!(selected_id_set(&selection), expected);
        assert_eq!(selection.full_units, expected);
        assert!(!selection
            .units
            .iter()
            .any(|unit| unit.id == "rust-contract"));
        assert!(!selection
            .units
            .iter()
            .any(|unit| unit.id == "rust-contract-workspace"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_scopes_contract_workspace_check_to_independent_cargo_project(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "independent-cargo-project",
            "crates/velnor-workflow-contract/src/lib.rs",
            "initial\n",
            "changed\n",
            ROOT_SCOPED_SELECTION_PROJECT_CONFIG,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected = BTreeSet::from([
            "rust-contract".to_owned(),
            "rust-contract-workspace".to_owned(),
        ]);
        assert_eq!(selected_id_set(&selection), expected);
        assert_eq!(selection.full_units, expected);
        assert!(!selection
            .units
            .iter()
            .any(|unit| unit.id == "rust-root-workspace"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn affected_selection_includes_workspace_checks_for_each_changed_cargo_root(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "mixed-cargo-roots",
            "Cargo.lock",
            "initial\n",
            "changed\n",
            ROOT_SCOPED_SELECTION_PROJECT_CONFIG,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected = BTreeSet::from([
            "rust-root".to_owned(),
            "rust-contract".to_owned(),
            "rust-root-workspace".to_owned(),
            "rust-contract-workspace".to_owned(),
        ]);
        assert_eq!(selected_id_set(&selection), expected);
        assert_eq!(selection.full_units, expected);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn apt_fetch_and_resolve_reject_bad_inputs_before_any_effect() {
        // Every case below fails on option validation, before any fetch
        // or network: no gh, no git, no filesystem writes.
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        let error = must_fail(
            apt_fetch(&args(&["--suite", "testing"])),
            "apt-fetch with an unknown suite",
        );
        assert!(error.to_string().contains("suite must be"), "{}", error);
        let error = must_fail(
            apt_fetch(&args(&[
                "--suite",
                "stable",
                "--source-repo",
                "not-a-slug",
                "--package",
                "example",
                "--version",
                "v1.2.3",
                "--dir",
                "incoming",
            ])),
            "apt-fetch with a bad slug",
        );
        assert!(error.to_string().contains("owner/name"), "{}", error);
        let error = must_fail(
            apt_fetch(&args(&[
                "--suite",
                "stable",
                "--source-repo",
                "example/app",
                "--package",
                "example",
                "--version",
                "1.2.3",
            ])),
            "apt-fetch without --dir",
        );
        assert!(
            error.to_string().contains("--dir needs a value"),
            "{}",
            error
        );
        let error = must_fail(
            apt_resolve_commit(&args(&[
                "--source-repo",
                "example/app",
                "--version",
                "1.2.3",
            ])),
            "apt-resolve-commit with an untagged version",
        );
        assert!(error.to_string().contains("vX.Y.Z"), "{}", error);
    }

    #[test]
    fn apt_verify_rejects_bad_inputs_before_any_effect() {
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        let error = must_fail(
            apt_verify(&args(&["--suite", "stable"])),
            "apt-verify without --source-repo",
        );
        assert!(
            error.to_string().contains("--source-repo needs a value"),
            "{}",
            error
        );
        let error = must_fail(
            apt_verify(&args(&[
                "--suite",
                "preview",
                "--source-repo",
                "example/app",
                "--package",
                "example",
                "--binary",
                "example",
                "--identity-dir",
                "app",
                "--manifest-schema",
                "example.test/apt-manifest-v1",
                "--version",
                "1.2.3~preview.41+0123456",
                "--incoming",
                "incoming",
                "--signer",
                "0123456789ABCDEF0123456789ABCDEF01234567",
                "--expect-signer",
                "0123456789ABCDEF0123456789ABCDEF01234567",
            ])),
            "apt-verify preview without --commit",
        );
        assert!(
            error.to_string().contains("--commit is required"),
            "{}",
            error
        );
        let error = must_fail(
            apt_verify(&args(&[
                "--suite",
                "preview",
                "--source-repo",
                "example/app",
                "--package",
                "example",
                "--binary",
                "example",
                "--identity-dir",
                "app",
                "--manifest-schema",
                "example.test/apt-manifest-v1",
                "--version",
                "1.2.3~preview.41+0123456",
                "--incoming",
                "incoming",
                "--commit",
                "short",
                "--signer",
                "0123456789ABCDEF0123456789ABCDEF01234567",
                "--expect-signer",
                "0123456789ABCDEF0123456789ABCDEF01234567",
            ])),
            "apt-verify preview with a malformed commit",
        );
        assert!(error.to_string().contains("40 lowercase hex"), "{}", error);
        let error = must_fail(
            apt_verify(&args(&[
                "--suite",
                "stable",
                "--source-repo",
                "example/app",
                "--package",
                "example",
                "--binary",
                "example",
                "--identity-dir",
                "app",
                "--manifest-schema",
                "example.test/apt-manifest-v1",
                "--version",
                "v1.2.3",
                "--incoming",
                "incoming",
                "--commit",
                "0123456789abcdef0123456789abcdef01234567",
                "--signer",
                "short",
                "--expect-signer",
                "0123456789ABCDEF0123456789ABCDEF01234567",
            ])),
            "apt-verify with a short signer",
        );
        assert!(error.to_string().contains("40-hex"), "{}", error);
    }

    #[test]
    fn apt_publish_pointer_channel_and_guard_reject_bad_inputs() {
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        let error = must_fail(
            apt_publish(&args(&["--suite", "stable"])),
            "apt-publish without --package",
        );
        assert!(
            error.to_string().contains("--package needs a value"),
            "{}",
            error
        );
        let error = must_fail(
            apt_previous_pointer(&args(&["--suite", "stable", "--bootstrap", "true"])),
            "stable previous pointer with --bootstrap",
        );
        assert!(
            error.to_string().contains("only to --suite preview"),
            "{}",
            error
        );
        let error = must_fail(
            apt_channel_update(&args(&["--suite", "testing"])),
            "apt-channel-update with an unknown suite",
        );
        assert!(error.to_string().contains("suite must be"), "{}", error);
        let error = must_fail(
            apt_deploy_guard(&args(&[
                "--suite",
                "testing",
                "--staged",
                "public",
                "--live-version",
                "unknown",
            ])),
            "apt-deploy-guard with an unknown suite",
        );
        assert!(error.to_string().contains("suite must be"), "{}", error);
        let error = must_fail(
            apt_deploy_guard(&args(&[
                "--suite",
                "stable",
                "--staged",
                "/nonexistent-velnor-staging",
                "--live-version",
                "unknown",
            ])),
            "apt-deploy-guard with a missing staged tree",
        );
        assert!(error.to_string().contains("last-publish"), "{}", error);
    }

    #[test]
    fn apt_publish_refuses_without_the_sentinel() {
        // A missing incoming directory carries no sentinel, so publication
        // refuses before tool checks, secrets, or any write.
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        let error = must_fail(
            apt_publish(&args(&[
                "--suite",
                "stable",
                "--source-repo",
                "example/app",
                "--package",
                "example",
                "--binary",
                "example",
                "--consumer-repo",
                "example/apt",
                "--manifest-schema",
                "example.test/apt-manifest-v1",
                "--signer",
                "0123456789ABCDEF0123456789ABCDEF01234567",
                "--passphrase-env",
                "APT_PASSPHRASE",
                "--key-env",
                "APT_SIGNING_KEY",
                "--keyring",
                "example.gpg",
                "--origin",
                "Example",
                "--identity-dir",
                "app",
                "--feed-url",
                "https://feed.example.test",
                "--description",
                "apt repository for example",
                "--version",
                "v1.2.3",
                "--incoming",
                "/nonexistent-velnor-incoming",
                "--previous-pointer",
                "previous-pointer.json",
                "--staging",
                "public",
            ])),
            "apt-publish without the sentinel",
        );
        assert!(error.to_string().contains("sentinel"), "{}", error);
    }

    #[test]
    fn legacy_feed_commands_no_longer_accept_apt() {
        // The `update-feed` stub is replaced: apt flows through the typed
        // `apt-*` commands, and the legacy feed pair is homebrew-only.
        let args = |options: &[&str]| options.iter().map(OsString::from).collect::<Vec<_>>();
        let error = must_fail(
            verify_feed(&args(&["--kind", "apt", "--package", "example"])),
            "verify-feed with apt",
        );
        assert!(
            error.to_string().contains("unsupported feed kind: apt"),
            "{}",
            error
        );
        let error = must_fail(
            update_feed(&args(&[
                "--kind",
                "apt",
                "--package",
                "example",
                "--channel",
                "stable",
            ])),
            "update-feed with apt",
        );
        assert!(
            error.to_string().contains("unsupported feed kind: apt"),
            "{}",
            error
        );
    }

    #[test]
    fn explicit_boolean_flags_parse_true_false_or_absent() {
        let options = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<String, String>>()
        };
        assert!(!must(
            flag_bool(&options(&[]), "bootstrap"),
            "absent is false"
        ));
        assert!(must(
            flag_bool(&options(&[("bootstrap", "true")]), "bootstrap"),
            "true"
        ));
        assert!(!must(
            flag_bool(&options(&[("bootstrap", "false")]), "bootstrap"),
            "false"
        ));
        let error = must_fail(
            flag_bool(&options(&[("bootstrap", "yes")]), "bootstrap"),
            "invalid boolean",
        );
        assert!(error.to_string().contains("`true` or `false`"), "{}", error);
    }

    #[test]
    fn version_bump_selection_scopes_workspace_check_to_matching_cargo_root(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, head) = current_project_selection_git_fixture_with_config(
            "independent-version-bump",
            "crates/velnor-workflow-contract/Cargo.toml",
            "version = \"0.1.0\"\n",
            "version = \"0.1.1\"\n",
            ROOT_SCOPED_SELECTION_PROJECT_CONFIG,
        )?;
        let config = read_config(&root.join(".github/ci/project.toml"))?;
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected = BTreeSet::from([
            "rust-contract".to_owned(),
            "rust-contract-workspace".to_owned(),
        ]);
        assert_eq!(selected_id_set(&selection), expected);
        assert_eq!(selection.full_units, expected);
        assert!(!selection
            .units
            .iter()
            .any(|unit| unit.id == "rust-root-workspace"));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    fn release_args(options: &[&str]) -> Vec<OsString> {
        options.iter().map(OsString::from).collect()
    }

    /// Every event resolves to exactly one mode token: tag pushes and
    /// admitted producer runs publish, dispatches drill, and everything else
    /// validates. The token is the whole point (callers print it for
    /// `GITHUB_OUTPUT`), so every case asserts the exact token, and every
    /// token must be a single whitespace-free word.
    #[test]
    fn resolve_mode_resolves_each_event() {
        let cases: Vec<(Vec<&str>, &str)> = vec![
            // Tag pushes publish.
            (
                vec!["--event", "push", "--ref", "refs/tags/v1.2.3"],
                "publish",
            ),
            // A rolling push to the default branch publishes; anywhere else
            // a branch push drills or validates.
            (
                vec![
                    "--event",
                    "push",
                    "--ref",
                    "refs/heads/main",
                    "--rolling",
                    "true",
                    "--branch",
                    "main",
                ],
                "publish",
            ),
            (
                vec!["--event", "push", "--ref", "refs/heads/main"],
                "validate",
            ),
            (
                vec![
                    "--event",
                    "push",
                    "--ref",
                    "refs/heads/feature",
                    "--input",
                    "rehearse",
                ],
                "rehearse",
            ),
            // Dispatch carries the declared drill mode, defaulting to
            // validation.
            (
                vec!["--event", "workflow_dispatch", "--input", "validate"],
                "validate",
            ),
            (
                vec!["--event", "workflow_dispatch", "--input", "build"],
                "build",
            ),
            (
                vec!["--event", "workflow_dispatch", "--input", "rehearse"],
                "rehearse",
            ),
            (vec!["--event", "workflow_dispatch"], "validate"),
            // Scheduled runs and pull requests validate.
            (vec!["--event", "schedule"], "validate"),
            (vec!["--event", "pull_request"], "validate"),
            (vec!["--event", "pull_request_target"], "validate"),
            // A producer run at success publishes only when its name equals
            // the declared trusted producer.
            (
                vec![
                    "--event",
                    "workflow_run",
                    "--producer",
                    "CI",
                    "--expected",
                    "CI",
                    "--conclusion",
                    "success",
                ],
                "publish",
            ),
        ];
        for (args, expected) in cases {
            let token = must(resolve_mode_token(&release_args(&args)), "mode resolves");
            assert_eq!(token, expected, "args: {args:?}");
            assert!(
                !token.chars().any(char::is_whitespace),
                "mode token must be a single word: {token:?}"
            );
        }
    }

    /// Anything that would write externally from an untrusted context is a
    /// hard refusal, and unknown events and modes fail closed.
    #[test]
    fn resolve_mode_refuses_untrusted_publish() {
        let error = must_fail(
            resolve_mode(&release_args(&[
                "--event",
                "push",
                "--ref",
                "refs/heads/feature",
                "--input",
                "publish",
            ])),
            "feature push must not publish",
        );
        assert!(
            error.to_string().contains("publish refused"),
            "unexpected error: {error}"
        );
        // Publication is tag-triggered, never dispatched.
        let error = must_fail(
            resolve_mode(&release_args(&[
                "--event",
                "workflow_dispatch",
                "--input",
                "publish",
            ])),
            "dispatch must not publish",
        );
        assert!(
            error
                .to_string()
                .contains("tag-triggered, never dispatched"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            resolve_mode(&release_args(&[
                "--event", "schedule", "--input", "publish",
            ])),
            "schedule must not publish",
        );
        assert!(
            error.to_string().contains("publish refused"),
            "unexpected error: {error}"
        );
        // Untrusted pull-request code validates only.
        for event in ["pull_request", "pull_request_target"] {
            let error = must_fail(
                resolve_mode(&release_args(&["--event", event, "--input", "publish"])),
                "pull request must not publish",
            );
            assert!(
                error
                    .to_string()
                    .contains("pull requests resolve to validate only"),
                "unexpected error: {error}"
            );
        }
        let error = must_fail(
            resolve_mode(&release_args(&[
                "--event",
                "workflow_run",
                "--conclusion",
                "success",
            ])),
            "producer-less run must not publish",
        );
        assert!(
            error.to_string().contains("without an admitted producer"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            resolve_mode(&release_args(&[
                "--event",
                "workflow_run",
                "--producer",
                "CI",
                "--expected",
                "CI",
                "--conclusion",
                "failure",
            ])),
            "failed producer must not publish",
        );
        assert!(
            error.to_string().contains("not success"),
            "unexpected error: {error}"
        );
        // Unknown events and modes fail closed.
        let error = must_fail(
            resolve_mode(&release_args(&["--event", "merge_group"])),
            "unknown event",
        );
        assert!(
            error.to_string().contains("unsupported release event"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            resolve_mode(&release_args(&["--event", "push", "--input", "ship"])),
            "unknown mode",
        );
        assert!(
            error.to_string().contains("unsupported release mode"),
            "unexpected error: {error}"
        );
    }

    /// A successful run under any other name is not the trusted producer,
    /// and without a name to admit against there is no admission: the
    /// `workflow_run` arm admits exactly like `admit-producer`.
    #[test]
    fn resolve_mode_workflow_run_admits_only_the_trusted_producer() {
        let error = must_fail(
            resolve_mode(&release_args(&[
                "--event",
                "workflow_run",
                "--producer",
                "EVIL",
                "--expected",
                "CI",
                "--conclusion",
                "success",
            ])),
            "wrong-name producer must not publish",
        );
        assert!(
            error.to_string().contains("is not the trusted `CI`"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            resolve_mode(&release_args(&[
                "--event",
                "workflow_run",
                "--producer",
                "CI",
                "--conclusion",
                "success",
            ])),
            "producer without a trusted name must not publish",
        );
        assert!(
            error
                .to_string()
                .contains("without a trusted producer to admit against"),
            "unexpected error: {error}"
        );
    }

    /// A `workflow_run` builds the producer run's head SHA; every other
    /// event builds its own SHA. Both must be full revisions.
    #[test]
    fn resolve_source_binds_the_producer_revision() {
        let run = "0123456789abcdef0123456789abcdef01234567";
        let own = "89abcdef0123456789abcdef0123456789abcdef";
        must(
            resolve_source(&release_args(&[
                "--event",
                "workflow_run",
                "--sha",
                own,
                "--run-sha",
                run,
            ])),
            "producer revision resolves",
        );
        must(
            resolve_source(&release_args(&["--event", "push", "--sha", own])),
            "own revision resolves",
        );
        let error = must_fail(
            resolve_source(&release_args(&["--event", "push", "--sha", "short"])),
            "short revision",
        );
        assert!(
            error.to_string().contains("40-hex revision"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            resolve_source(&release_args(&["--event", "workflow_run", "--sha", own])),
            "producer run without a run SHA",
        );
        assert!(
            error.to_string().contains("--run-sha needs a value"),
            "unexpected error: {error}"
        );
    }

    /// The producer name must equal the trusted producer and its conclusion
    /// must be `success`; a mismatch is a refusal, never a warning.
    #[test]
    fn admit_producer_refuses_name_and_conclusion_mismatch() {
        must(
            admit_producer(&release_args(&[
                "--producer",
                "CI",
                "--expected",
                "CI",
                "--conclusion",
                "success",
            ])),
            "trusted producer admits",
        );
        let error = must_fail(
            admit_producer(&release_args(&[
                "--producer",
                "Other",
                "--expected",
                "CI",
                "--conclusion",
                "success",
            ])),
            "untrusted producer",
        );
        assert!(
            error.to_string().contains("is not the trusted"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            admit_producer(&release_args(&[
                "--producer",
                "CI",
                "--expected",
                "CI",
                "--conclusion",
                "cancelled",
            ])),
            "cancelled producer",
        );
        assert!(
            error.to_string().contains("not success"),
            "unexpected error: {error}"
        );
    }

    /// The canonical download flags fail closed, resume, retry inside a
    /// bounded window, and bound connect and total time — no site may
    /// download with retries but no bounds, or bounds but no retries.
    #[test]
    fn canonical_download_flags_retry_inside_time_bounds() {
        for flag in [
            "--fail",
            "--continue-at -",
            "--retry 20",
            "--retry-all-errors",
            "--retry-delay 5",
            "--retry-max-time 1800",
            "--connect-timeout 30",
            "--max-time 900",
        ] {
            assert!(
                CURL_DOWNLOAD_FLAGS.contains(flag),
                "the canonical set must carry {flag}"
            );
        }
    }

    /// The guest kernel download carries the canonical flags and still
    /// verifies the pinned digest before the build consumes the tarball.
    #[test]
    fn guest_kernel_download_uses_the_canonical_bounded_flags() {
        let script = guest_kernel_download_script();
        assert!(
            script.contains(CURL_DOWNLOAD_FLAGS),
            "the kernel must download with the canonical flags: {script}"
        );
        assert!(
            script.contains("sha256sum -c -"),
            "the kernel must verify its digest: {script}"
        );
    }

    fn manifest_fixture(name: &str) -> std::path::PathBuf {
        let id = crate::unique_suffix();
        let dir = std::env::temp_dir().join(format!(
            "velnor-workflow-manifest-{name}-{pid}-{id}",
            pid = std::process::id()
        ));
        must(std::fs::create_dir_all(&dir), "create manifest fixture");
        must(
            std::fs::write(dir.join("b.tar.gz"), "second-subject"),
            "write second subject",
        );
        must(
            std::fs::write(dir.join("a.tar.gz"), "first-subject"),
            "write first subject",
        );
        dir
    }

    /// The manifest binds the declared subjects with independently
    /// re-hashed digests, and the corpus is strictly re-verified from disk
    /// before the manifest is written.
    #[test]
    fn assemble_manifest_writes_and_verifies_the_corpus() {
        let dir = manifest_fixture("corpus");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        must(
            assemble_manifest(&release_args(&[
                "--dir",
                dir.to_str().unwrap_or_default(),
                "--subjects",
                "b.tar.gz,a.tar.gz",
                "--schema",
                "example.test/release-manifest-v1",
                "--repository",
                "example/app",
                "--ref",
                "refs/tags/v1.2.3",
                "--commit",
                commit,
                "--version",
                "1.2.3",
            ])),
            "assemble manifest",
        );
        let corpus = must(
            std::fs::read_to_string(dir.join("SHA256SUMS")),
            "read checksum corpus",
        );
        let lines: Vec<&str> = corpus.lines().collect();
        assert_eq!(lines.len(), 2, "unexpected corpus: {corpus}");
        assert!(
            lines[0].ends_with("  a.tar.gz") && lines[1].ends_with("  b.tar.gz"),
            "subjects must be sorted: {corpus}"
        );
        let manifest = must(
            std::fs::read_to_string(dir.join("release-manifest.json")),
            "read release manifest",
        );
        let document: serde_json::Value =
            must(serde_json::from_str(&manifest), "parse release manifest");
        assert_eq!(document["schema"], "example.test/release-manifest-v1");
        assert_eq!(document["source_commit"], commit);
        assert_eq!(document["version"], "1.2.3");
        let assets = must(
            document["assets"].as_array().ok_or("manifest assets"),
            "manifest assets",
        );
        assert_eq!(assets.len(), 2);
        for (line, asset) in lines.iter().zip(assets.iter()) {
            let digest = line.split_once("  ").unwrap_or_default().0;
            assert_eq!(asset["sha256"], digest);
        }
        must(std::fs::remove_dir_all(&dir), "remove manifest fixture");
    }

    /// A corpus line that no longer matches its subject fails strictly:
    /// conflicting bytes are never papered over.
    #[test]
    fn checksum_corpus_mismatch_fails_strict_verification() {
        let dir = manifest_fixture("conflict");
        let digest = "0".repeat(64);
        let error = must_fail(
            verify_checksum_corpus(&dir, &format!("{digest}  a.tar.gz\n")),
            "conflicting corpus digest",
        );
        assert!(
            error.to_string().contains("corpus mismatch"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            verify_checksum_corpus(&dir, "not-a-corpus-line\n"),
            "malformed corpus line",
        );
        assert!(
            error.to_string().contains("malformed checksum corpus"),
            "unexpected error: {error}"
        );
        must(std::fs::remove_dir_all(&dir), "remove manifest fixture");
    }

    /// Missing and traversing subjects fail before anything is written.
    #[test]
    fn assemble_manifest_refuses_bad_subjects() {
        let dir = manifest_fixture("refuse");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let error = must_fail(
            assemble_manifest(&release_args(&[
                "--dir",
                dir.to_str().unwrap_or_default(),
                "--subjects",
                "missing.tar.gz",
                "--schema",
                "example.test/release-manifest-v1",
                "--repository",
                "example/app",
                "--ref",
                "refs/tags/v1.2.3",
                "--commit",
                commit,
                "--version",
                "1.2.3",
            ])),
            "missing subject",
        );
        assert!(
            error.to_string().contains("subject is missing"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            assemble_manifest(&release_args(&[
                "--dir",
                dir.to_str().unwrap_or_default(),
                "--subjects",
                "../escape.tar.gz",
                "--schema",
                "example.test/release-manifest-v1",
                "--repository",
                "example/app",
                "--ref",
                "refs/tags/v1.2.3",
                "--commit",
                commit,
                "--version",
                "1.2.3",
            ])),
            "traversal subject",
        );
        assert!(
            error.to_string().contains("declared subject file names"),
            "unexpected error: {error}"
        );
        must(std::fs::remove_dir_all(&dir), "remove manifest fixture");
    }

    /// Archive members validate before any tar call: no traversal, no
    /// duplicates of the binary, no bad deterministic flag.
    #[test]
    fn package_binary_validates_members_before_any_tar_call() {
        let error = must_fail(
            package_binary(&release_args(&[
                "--target",
                "x86_64-unknown-linux-gnu",
                "--version",
                "1.2.3",
                "--package",
                "example",
                "--binary",
                "example",
                "--members",
                "../escape",
            ])),
            "traversal member",
        );
        assert!(
            error.to_string().contains("invalid archive member"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            package_binary(&release_args(&[
                "--target",
                "x86_64-unknown-linux-gnu",
                "--version",
                "1.2.3",
                "--package",
                "example",
                "--binary",
                "example",
                "--members",
                "example",
            ])),
            "member duplicating the binary",
        );
        assert!(
            error.to_string().contains("invalid archive member"),
            "unexpected error: {error}"
        );
        let error = must_fail(
            package_binary(&release_args(&[
                "--target",
                "x86_64-unknown-linux-gnu",
                "--version",
                "1.2.3",
                "--package",
                "example",
                "--binary",
                "example",
                "--deterministic",
                "sometimes",
            ])),
            "bad deterministic flag",
        );
        assert!(
            error.to_string().contains("invalid --deterministic value"),
            "unexpected error: {error}"
        );
    }

    /// The GNU tar probe reports exactly what `tar --version` says: GNU
    /// tar admits deterministic packaging, anything else refuses it.
    #[test]
    fn deterministic_packaging_needs_gnu_tar() {
        let output = must(
            std::process::Command::new("tar").arg("--version").output(),
            "probe tar",
        );
        let gnu = output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .to_lowercase()
                .contains("gnu tar");
        assert_eq!(require_gnu_tar().is_ok(), gnu);
    }

    const INSTALL_TOOL: &str = "test-runner";
    const INSTALL_PRODUCER: &str = "producer-job";
    const INSTALL_ABI: &str = "Linux-X64";
    const INSTALL_INPUTS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn hex_bytes(data: &[u8]) -> String {
        use std::fmt::Write as _;
        let digest = Sha256::digest(data);
        let mut output = String::with_capacity(digest.len() * 2);
        for byte in digest {
            let _ = write!(output, "{byte:02x}");
        }
        output
    }

    fn install_manifest(run_id: &str) -> ToolManifest {
        let files = vec![
            ToolFile {
                path: "bin/test-runner".to_owned(),
                sha256: hex_bytes(b"runner-bytes"),
                executable: true,
            },
            ToolFile {
                path: "share/policy.json".to_owned(),
                sha256: hex_bytes(b"{\"deny\":[]}"),
                executable: false,
            },
        ];
        let mut manifest = ToolManifest {
            tool_id: INSTALL_TOOL.to_owned(),
            inputs_digest: INSTALL_INPUTS.to_owned(),
            platform_abi: INSTALL_ABI.to_owned(),
            producer: ProducerIdentity {
                producer: INSTALL_PRODUCER.to_owned(),
                run_id: run_id.to_owned(),
            },
            outcome: ToolOutcome::Success,
            files,
            manifest_sha256: String::new(),
        };
        manifest.manifest_sha256 = manifest.canonical_digest();
        manifest
    }

    /// A restored bundle on disk: `bundle/` holds the manifest and the
    /// files, `dest/` is the uncreated install target, `outputs` collects
    /// the step outputs.
    struct InstallFixture {
        root: PathBuf,
        manifest: PathBuf,
        dir: PathBuf,
        dest: PathBuf,
        outputs: PathBuf,
    }

    fn install_fixture(name: &str, manifest: &ToolManifest) -> InstallFixture {
        let id = crate::unique_suffix();
        let root = std::env::temp_dir().join(format!("velnor-prepared-tool-install-{name}-{id}"));
        let dir = root.join("bundle");
        must(
            fs::create_dir_all(dir.join("bin")),
            "create bundle binary directory",
        );
        must(
            fs::create_dir_all(dir.join("share")),
            "create bundle data directory",
        );
        let bytes = must(serde_json::to_vec(manifest), "serialize fixture manifest");
        must(
            fs::write(dir.join("manifest.json"), bytes),
            "write manifest",
        );
        must(
            fs::write(dir.join("bin/test-runner"), b"runner-bytes"),
            "write fixture binary",
        );
        must(
            fs::write(dir.join("share/policy.json"), b"{\"deny\":[]}"),
            "write fixture data",
        );
        InstallFixture {
            manifest: dir.join("manifest.json"),
            dir,
            dest: root.join("dest").join("tool"),
            outputs: root.join("outputs"),
            root,
        }
    }

    fn install_args(fixture: &InstallFixture, current_run: &str, extra: &[&str]) -> Vec<OsString> {
        let lossy = |path: &Path| path.to_string_lossy().into_owned();
        let mut args = vec![
            OsString::from("--manifest"),
            OsString::from(lossy(&fixture.manifest)),
            OsString::from("--dir"),
            OsString::from(lossy(&fixture.dir)),
            OsString::from("--dest"),
            OsString::from(lossy(&fixture.dest)),
            OsString::from("--tool"),
            OsString::from(INSTALL_TOOL),
            OsString::from("--inputs"),
            OsString::from(INSTALL_INPUTS),
            OsString::from("--abi"),
            OsString::from(INSTALL_ABI),
            OsString::from("--producers"),
            OsString::from(INSTALL_PRODUCER),
            OsString::from("--run-id"),
            OsString::from(current_run),
            OsString::from("--repo"),
            OsString::from("example/fixture"),
        ];
        args.extend(extra.iter().map(OsString::from));
        args
    }

    fn read_outputs(fixture: &InstallFixture) -> String {
        must(
            fs::read_to_string(&fixture.outputs),
            "read recorded step outputs",
        )
    }

    #[test]
    fn prepared_tool_install_verifies_and_installs_exact_hits() {
        let fixture = install_fixture("exact", &install_manifest("42"));
        let mut sleeps = Vec::new();
        must(
            prepared_tool_install_to(
                &install_args(&fixture, "42", &[]),
                Some(&fixture.outputs),
                &mut |wait| sleeps.push(wait),
            ),
            "exact install lands",
        );
        // An exact hit never touches the network: no sleeps, no executor.
        assert!(sleeps.is_empty());
        assert_eq!(
            must(
                fs::read(fixture.dest.join("bin/test-runner")),
                "read installed binary"
            ),
            b"runner-bytes"
        );
        let outputs = read_outputs(&fixture);
        assert!(outputs.contains("is-exact=true\n"), "{outputs}");
        assert!(outputs.contains("outcome=installed\n"), "{outputs}");
        let requested = outputs
            .lines()
            .find_map(|line| line.strip_prefix("requested-key="));
        let resolved = outputs
            .lines()
            .find_map(|line| line.strip_prefix("resolved-key="));
        let save = outputs
            .lines()
            .find_map(|line| line.strip_prefix("save-key="));
        assert_eq!(requested, resolved);
        assert_eq!(save, resolved);
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn prepared_tool_install_refuses_precise_verdicts() {
        // Each invalid bundle fails with its taxonomy outcome recorded: the
        // message names the break, the output names it for machines.
        let mut foreign = install_manifest("42");
        foreign.producer.producer = "intruder-job".to_owned();
        foreign.manifest_sha256 = foreign.canonical_digest();
        let mut abi = install_manifest("42");
        abi.platform_abi = "Linux-ARM64".to_owned();
        abi.manifest_sha256 = abi.canonical_digest();
        for (name, manifest, outcome) in [
            ("foreign-producer", foreign, "denied"),
            ("wrong-abi", abi, "corrupt"),
        ] {
            let fixture = install_fixture(name, &manifest);
            let error = must_fail(
                prepared_tool_install_to(
                    &install_args(&fixture, "42", &[]),
                    Some(&fixture.outputs),
                    &mut |_| {},
                ),
                "an invalid bundle never installs",
            );
            assert!(error.to_string().contains(outcome), "{error}");
            assert!(read_outputs(&fixture).contains(&format!("outcome={outcome}\n")));
            assert!(!fixture.dest.exists());
            let _ = fs::remove_dir_all(&fixture.root);
        }
        // Incomplete and tampered restores are corrupt, whatever the
        // manifest claims.
        let manifest = install_manifest("42");
        let fixture = install_fixture("incomplete", &manifest);
        must(
            fs::remove_file(fixture.dir.join("share/policy.json")),
            "drop a restored file",
        );
        let error = must_fail(
            prepared_tool_install_to(
                &install_args(&fixture, "42", &[]),
                Some(&fixture.outputs),
                &mut |_| {},
            ),
            "an incomplete restore never installs",
        );
        assert!(read_outputs(&fixture).contains("outcome=corrupt\n"));
        assert!(error.to_string().contains("corrupt"), "{error}");
        let _ = fs::remove_dir_all(&fixture.root);
        let fixture = install_fixture("tampered", &manifest);
        must(
            fs::write(fixture.dir.join("bin/test-runner"), b"forged-bytes"),
            "tamper with restored bytes",
        );
        let error = must_fail(
            prepared_tool_install_to(
                &install_args(&fixture, "42", &[]),
                Some(&fixture.outputs),
                &mut |_| {},
            ),
            "tampered bytes never install",
        );
        assert!(read_outputs(&fixture).contains("outcome=corrupt\n"));
        assert!(error.to_string().contains("corrupt"), "{error}");
        let _ = fs::remove_dir_all(&fixture.root);
        // No manifest at all is a miss: nothing to prove, the signal to
        // build.
        let fixture = install_fixture("miss", &manifest);
        must(
            fs::remove_file(&fixture.manifest),
            "drop the restored manifest",
        );
        let error = must_fail(
            prepared_tool_install_to(
                &install_args(&fixture, "42", &[]),
                Some(&fixture.outputs),
                &mut |_| {},
            ),
            "a missing manifest is a miss",
        );
        assert!(read_outputs(&fixture).contains("outcome=miss\n"));
        assert!(error.to_string().contains("miss"), "{error}");
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn prepared_tool_check_save_key_gates_fallback_saves() {
        use crate::primitives::prepared_tools::{requested_key, resolved_key, ToolRequest};
        let manifest = install_manifest("42");
        let request = ToolRequest {
            tool_id: INSTALL_TOOL.to_owned(),
            inputs_digest: INSTALL_INPUTS.to_owned(),
            platform_abi: INSTALL_ABI.to_owned(),
            authorized_producers: BTreeSet::from([INSTALL_PRODUCER.to_owned()]),
            run_id: "99".to_owned(),
        };
        let requested = requested_key(&request);
        let resolved = resolved_key(&manifest);
        assert_ne!(requested.as_str(), resolved.as_str());
        // The historical bug, gated at save time: fallback bytes under the
        // requested exact key are refused.
        let fixture = install_fixture("save-refused", &manifest);
        let error = must_fail(
            prepared_tool_install_to(
                &install_args(&fixture, "99", &["--check-save-key", requested.as_str()]),
                Some(&fixture.outputs),
                &mut |_| {},
            ),
            "fallback bytes under the requested key are refused",
        );
        assert!(error.to_string().contains("refusing to save"), "{error}");
        assert!(read_outputs(&fixture).contains("outcome=corrupt\n"));
        assert!(!fixture.dest.exists());
        let _ = fs::remove_dir_all(&fixture.root);
        // The resolved key — the bundle's own producer run — is allowed,
        // and the check records it for the save step to consume.
        let fixture = install_fixture("save-allowed", &manifest);
        must(
            prepared_tool_install_to(
                &install_args(&fixture, "99", &["--check-save-key", resolved.as_str()]),
                Some(&fixture.outputs),
                &mut |_| {},
            ),
            "the resolved key is allowed",
        );
        let outputs = read_outputs(&fixture);
        assert!(outputs.contains(&format!("save-key={}\n", resolved.as_str())));
        assert!(outputs.contains("outcome=save-allowed\n"));
        let _ = fs::remove_dir_all(&fixture.root);
    }

    #[test]
    fn historical_outcome_validates_success_and_refuses_the_rest() {
        let run = |body: &str, status: u16| ApiResponse {
            status,
            headers: String::new(),
            body: body.to_owned(),
        };
        // A proven success passes silently.
        let root = std::env::temp_dir().join(format!(
            "velnor-prepared-tool-outcome-{}",
            crate::unique_suffix()
        ));
        let outputs = root.join("outputs");
        must(fs::create_dir_all(&root), "create outcome directory");
        let mut executor =
            || -> Result<ApiResponse, String> { Ok(run(r#"{"conclusion":"success"}"#, 200)) };
        must(
            check_historical_outcome("42", &mut executor, &mut |_| {}, Some(&outputs)),
            "a successful producer run passes",
        );
        // Anything else fails closed with its taxonomy recorded.
        for (body, status, headers, outcome) in [
            (r#"{"conclusion":"failure"}"#, 200_u16, "", "denied"),
            (r#"{"conclusion":null}"#, 200_u16, "", "denied"),
            ("{}", 403_u16, "", "denied"),
            ("{}", 404_u16, "", "miss"),
        ] {
            let _ = fs::remove_file(&outputs);
            let mut executor = || -> Result<ApiResponse, String> {
                Ok(ApiResponse {
                    status,
                    headers: headers.to_owned(),
                    body: body.to_owned(),
                })
            };
            let error = must_fail(
                check_historical_outcome("42", &mut executor, &mut |_| {}, Some(&outputs)),
                "an unproven outcome never passes",
            );
            assert!(error.to_string().contains(outcome), "{error}");
            assert_eq!(
                must(fs::read_to_string(&outputs), "read recorded outcome"),
                format!("outcome={outcome}\n")
            );
        }
        // A flapping API exhausts its bounds as transient, fast: the fake
        // sleeper records instead of sleeping.
        let _ = fs::remove_file(&outputs);
        let mut sleeps = Vec::new();
        let mut executor = || -> Result<ApiResponse, String> { Ok(run("flaked", 500)) };
        let error = must_fail(
            check_historical_outcome(
                "42",
                &mut executor,
                &mut |wait| sleeps.push(wait),
                Some(&outputs),
            ),
            "a flapping api exhausts as transient",
        );
        assert!(error.to_string().contains("transient"), "{error}");
        assert_eq!(
            must(fs::read_to_string(&outputs), "read recorded outcome"),
            "outcome=transient\n"
        );
        assert_eq!(sleeps.len(), 2);
        // A broken executor is a usage error, never taxonomy.
        let mut executor = || -> Result<ApiResponse, String> { Err("curl is missing".to_owned()) };
        let error = must_fail(
            check_historical_outcome("42", &mut executor, &mut |_| {}, Some(&outputs)),
            "a broken executor is a usage error",
        );
        assert!(error.to_string().contains("curl is missing"), "{error}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn retention_policy_extends_only_for_declared_tools() {
        let parse = |config: &str| {
            must(
                crate::config::parse(Path::new("velnor-workflow.toml"), config.as_bytes()),
                "parse generation config",
            )
        };
        assert_eq!(
            retention_policy_with_declared_tools(RetentionPolicy::default_policy(), None),
            RetentionPolicy::default_policy()
        );
        let bare = parse("schema = 1\n");
        assert_eq!(
            retention_policy_with_declared_tools(RetentionPolicy::default_policy(), Some(&bare)),
            RetentionPolicy::default_policy()
        );
        let declared = parse(
            "schema = 1\n\n[[declare]]\nprimitive = \"prepared-tool\"\n\n[declare.args.tools]\ntest-runner = [\"producer-job\"]\n",
        );
        let policy = retention_policy_with_declared_tools(
            RetentionPolicy::default_policy(),
            Some(&declared),
        );
        assert!(
            policy
                .classes
                .iter()
                .any(|class| class.id == "prepared-tools"),
            "a declared repository carries the prepared-tools class"
        );
        assert_eq!(
            policy.classes.len(),
            RetentionPolicy::default_policy().classes.len() + 1
        );
    }

    #[test]
    fn curl_producer_run_reports_spawn_failures() {
        let error = must_some(
            curl_producer_run(
                "/nonexistent-velnor-curl-binary",
                "example/fixture",
                "42",
                "fixture-token",
            )
            .err(),
            "a missing http client must fail",
        );
        assert!(error.contains("nonexistent-velnor-curl-binary"), "{error}");
    }

    #[test]
    fn split_curl_response_splits_status_headers_and_body() {
        let response = must_some(
            split_curl_response(
                "HTTP/2 200\r\nx-ratelimit-remaining: 42\r\n\r\n{\"conclusion\":\"success\"}\n__PREPARED_TOOL_STATUS:200\n",
            ),
            "a header capture must split",
        );
        assert_eq!(response.status, 200);
        assert!(response.headers.contains("x-ratelimit-remaining: 42"));
        assert!(response.body.contains("success"));
        // LF-only captures split too, and a body containing the marker
        // cannot shift the parse: the trailer splits from the right.
        let response = must_some(
            split_curl_response(
                "HTTP/1.1 403\nx: y\n\n__PREPARED_TOOL_STATUS:200\n__PREPARED_TOOL_STATUS:403\n",
            ),
            "an lf capture must split",
        );
        assert_eq!(response.status, 403);
        assert!(split_curl_response("no trailer here").is_none());
        assert!(split_curl_response("body\n__PREPARED_TOOL_STATUS:banana\n").is_none());
    }

    // S4 aggregate wiring cut: the planner's expected work binds the
    // required-check aggregate, and a proven no-work plan passes via planner
    // + aggregate only, with an explicit machine-readable reason.

    /// A scratch directory for one S4 aggregate/run fixture.
    fn s4_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-workflow-s4-{name}-{}-{}",
            std::process::id(),
            crate::unique_suffix()
        ));
        must(std::fs::create_dir_all(&dir), "create s4 fixture");
        dir
    }

    /// The ambient checkout SHAs the aggregate command binds against, so
    /// writer fixtures pass under any outer environment (mirrors
    /// `s4_run_paths`).
    fn s4_ambient_shas() -> (String, String) {
        (
            std::env::var("BASE_SHA").unwrap_or_default(),
            std::env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned()),
        )
    }

    /// The expected-work JSON the planner writes for `selection`, bound to
    /// the ambient checkout SHAs.
    fn s4_expected_for_selection(selection: &UnitSelection<'_>, lanes: RunnerMode) -> String {
        let dir = s4_dir("expected");
        let path = dir.join("expected.json");
        let (base, head) = s4_ambient_shas();
        must(
            write_expected_work_file(&path, selection, lanes, &base, &head),
            "write expected work",
        );
        let text = must(std::fs::read_to_string(&path), "read expected work");
        must(std::fs::remove_dir_all(&dir), "remove s4 fixture");
        text
    }

    /// Score one expected/results pair against the ambient checkout SHAs —
    /// the same binding the aggregate command verifies.
    fn s4_score(
        expected_json: &str,
        results_json: &str,
    ) -> Result<crate::reuse::AggregateVerdict, String> {
        let (base, head) = s4_ambient_shas();
        crate::reuse::aggregate_files(expected_json, results_json, &base, &head)
    }

    /// All-success results JSON covering every (unit, lane) the expected-work
    /// JSON names. Tests mutate one entry to prove each fail-closed case.
    fn s4_success_results_for(expected_json: &str) -> String {
        let document: serde_json::Value =
            must(serde_json::from_str(expected_json), "parse expected work");
        let units = must_some(
            document.get("units").and_then(serde_json::Value::as_array),
            "expected units array",
        );
        let mut results = Vec::new();
        for unit in units {
            let id = must_some(
                unit.get("id").and_then(serde_json::Value::as_str),
                "expected unit id",
            );
            let lanes = must_some(
                unit.get("lanes").and_then(serde_json::Value::as_array),
                "expected unit lanes",
            );
            for lane in lanes {
                results.push(serde_json::json!({
                    "unit": id,
                    "lane": must_some(lane.as_str(), "expected lane name"),
                    "outcome": "success",
                }));
            }
        }
        must(
            serde_json::to_string(&serde_json::json!({ "results": results })),
            "serialize success results",
        )
    }

    /// Rewrite one (unit, lane) entry's outcome, attaching extra fields
    /// (`reason`, `reused_from`) beside it.
    fn s4_set_result(
        results_json: &str,
        unit: &str,
        lane: &str,
        outcome: &str,
        extra: &[(&str, &str)],
    ) -> String {
        let mut document: serde_json::Value =
            must(serde_json::from_str(results_json), "parse results");
        let results = must_some(
            document
                .get_mut("results")
                .and_then(serde_json::Value::as_array_mut),
            "results array",
        );
        let mut patched = false;
        for entry in results.iter_mut() {
            let same_unit = entry.get("unit").and_then(serde_json::Value::as_str) == Some(unit);
            let same_lane = entry.get("lane").and_then(serde_json::Value::as_str) == Some(lane);
            if same_unit && same_lane {
                entry["outcome"] = serde_json::Value::String(outcome.to_owned());
                for (key, value) in extra {
                    entry[*key] = serde_json::Value::String((*value).to_owned());
                }
                patched = true;
            }
        }
        assert!(patched, "missing result entry for {unit} {lane}");
        must(serde_json::to_string(&document), "serialize results")
    }

    /// Drop one (unit, lane) entry from a results JSON document.
    fn s4_drop_result(results_json: &str, unit: &str, lane: &str) -> String {
        let mut document: serde_json::Value =
            must(serde_json::from_str(results_json), "parse results");
        let results = must_some(
            document
                .get_mut("results")
                .and_then(serde_json::Value::as_array_mut),
            "results array",
        );
        let before = results.len();
        results.retain(|entry| {
            entry.get("unit").and_then(serde_json::Value::as_str) != Some(unit)
                || entry.get("lane").and_then(serde_json::Value::as_str) != Some(lane)
        });
        assert_eq!(
            results.len() + 1,
            before,
            "missing result entry for {unit} {lane}"
        );
        must(serde_json::to_string(&document), "serialize results")
    }

    /// Score one expected/results pair through the aggregate CLI wiring and
    /// return the core verdict beside the CLI exit, so tests pin both the
    /// exact failure lines and the pass/fail behavior.
    fn s4_verdict(
        dir: &Path,
        expected_json: &str,
        results_json: &str,
    ) -> (crate::reuse::AggregateVerdict, Result<(), GeneratorError>) {
        let expected_path = dir.join("expected.json");
        let results_path = dir.join("results.json");
        must(
            std::fs::write(&expected_path, expected_json),
            "write expected work",
        );
        must(
            std::fs::write(&results_path, results_json),
            "write reported results",
        );
        let verdict = must(s4_score(expected_json, results_json), "score expected work");
        let exit = aggregate_command(&expected_path, &results_path);
        (verdict, exit)
    }

    /// The real-work binding input: an owned change's planned selection plus
    /// the expected-work JSON the planner writes for it, on both lanes.
    fn s4_owned_binding(
        name: &str,
    ) -> Result<(std::path::PathBuf, String, String), Box<dyn Error>> {
        let (root, base, head) = selection_git_fixture(name, "crates/app/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected = s4_expected_for_selection(&selection, RunnerMode::Both);
        let results = s4_success_results_for(&expected);
        Ok((root, expected, results))
    }

    /// Plan one lane-scoped diff end to end through `plan_with`: the
    /// `VELNOR_LANES` value the render emits for `lanes` (a configured
    /// single lane or a dispatch-narrowed selection), over the
    /// `S4_PLAN_CONFIG_TOML` estate with `changed` as the diff. Returns the
    /// fixture root, the scratch dir, the written expected-work JSON, and
    /// the `GITHUB_OUTPUT` text.
    fn s4_lanes_plan(
        name: &str,
        lanes: &str,
        changed: &str,
    ) -> Result<(std::path::PathBuf, std::path::PathBuf, String, String), Box<dyn Error>> {
        let (root, base, head) = selection_git_fixture(name, changed)?;
        let dir = s4_dir(name);
        let config_path = dir.join("project.toml");
        must(
            std::fs::write(&config_path, S4_PLAN_CONFIG_TOML),
            "write s4 plan config",
        );
        let expected_path = dir.join("expected.json");
        let output_path = dir.join("github-output");
        must(
            plan_with(
                &config_path,
                &PlanInputs {
                    root: root.clone(),
                    event: PlanEvent::PullRequest,
                    scope_override: None,
                    base,
                    head,
                    lanes: lanes.to_owned(),
                    selection_file: None,
                    expected_file: Some(expected_path.clone()),
                    github_output: Some(output_path.clone()),
                },
            ),
            "plan the lane-scoped diff",
        );
        let expected = must(
            std::fs::read_to_string(&expected_path),
            "read expected work",
        );
        let outputs = must(std::fs::read_to_string(&output_path), "read github output");
        Ok((root, dir, expected, outputs))
    }

    const S4_RUN_CONFIG_TOML: &str = r#"schema = 2
repository = "example/s4"
profile = "s4-no-work"
verified = true
default_branch = "main"

[[unit]]
id = "base"
kind = "rust"
watch = ["crates/base/**"]
github_pr_commands = ["true"]
github_full_commands = ["true"]
velnor_pr_commands = ["true"]
velnor_full_commands = ["true"]
"#;

    /// A plan-level config mirroring `polyglot_selection_config`: a root
    /// contributor document matches no watch, so the plan is proven no-work.
    const S4_PLAN_CONFIG_TOML: &str = r#"schema = 2
repository = "example/s4-plan"
profile = "s4-plan-e2e"
verified = true
default_branch = "main"

[[unit]]
id = "rust-alpha"
kind = "rust"
watch = ["Cargo.lock", "Cargo.toml", "crates/alpha/**/*.rs", "crates/alpha/Cargo.toml", "crates/alpha/src/**", "rust-toolchain.toml"]
github_pr_commands = ["true"]
github_full_commands = ["true"]
velnor_pr_commands = ["true"]
velnor_full_commands = ["true"]

[[unit]]
id = "bun-web"
kind = "bun"
watch = ["web/**/*.ts", "web/**/*.tsx", "web/bun.lock", "web/package.json"]
github_pr_commands = ["true"]
github_full_commands = ["true"]
velnor_pr_commands = ["true"]
velnor_full_commands = ["true"]

[[unit]]
id = "docker-construct"
kind = "docker"
watch = ["docker/construct/**"]
github_pr_commands = ["true"]
github_full_commands = ["true"]
velnor_pr_commands = ["true"]
velnor_full_commands = ["true"]

[[unit]]
id = "swift-native"
kind = "swift"
watch = ["native/**/*.swift", "native/Package.swift", "native/Package.resolved"]
github_pr_commands = ["true"]
github_full_commands = ["true"]
velnor_pr_commands = ["true"]
velnor_full_commands = ["true"]
"#;

    /// The scope the ambient job environment demands: trusted events run
    /// full, everything else runs affected. The test adapts to the outer
    /// environment instead of mutating process-global env.
    fn s4_run_scope() -> Scope {
        if event_requires_full_scope(&std::env::var("EVENT_NAME").unwrap_or_default()) {
            Scope::Full
        } else {
            Scope::Affected
        }
    }

    /// A runnable config plus a selection file matching the ambient SHAs, so
    /// the SHA binding passes under any outer environment.
    fn s4_run_paths(dir: &Path, scope: Scope, units: &str, full_units: &str) -> (PathBuf, PathBuf) {
        let config_path = dir.join("project.toml");
        must(
            std::fs::write(&config_path, S4_RUN_CONFIG_TOML),
            "write s4 config",
        );
        let selection_path = dir.join("velnor-ci-selection");
        let base = std::env::var("BASE_SHA").unwrap_or_default();
        let head = std::env::var("HEAD_SHA").unwrap_or_else(|_| "HEAD".to_owned());
        must(
            write_selection_file(&selection_path, &base, &head, scope, units, full_units),
            "write s4 selection",
        );
        (config_path, selection_path)
    }

    #[test]
    fn no_work_plan_passes_aggregate_with_explicit_reason() -> Result<(), Box<dyn Error>> {
        // The S0 shape end to end: an unmatched root doc selects nothing,
        // the planner records why, and planner success plus zero workload
        // results passes the aggregate with a machine-readable reason.
        let (root, base, head) = selection_git_fixture("s4-unmatched", "AGENTS.md")?;
        let config = polyglot_selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert!(selection.units.is_empty() && selection.full_units.is_empty());
        assert_eq!(
            selection.no_work_reason.as_deref(),
            Some("no changed path selected a workload unit"),
        );
        assert_eq!(
            must(
                planned_no_work_reason(&selection),
                "record the no-work reason"
            )
            .as_deref(),
            Some("no changed path selected a workload unit"),
        );
        let expected = s4_expected_for_selection(&selection, RunnerMode::Both);
        let document: serde_json::Value = serde_json::from_str(&expected)?;
        assert_eq!(
            document
                .get("planned_no_work")
                .and_then(serde_json::Value::as_bool),
            Some(true),
        );
        assert_eq!(
            document
                .get("units")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0),
        );
        let dir = s4_dir("no-work");
        let (verdict, exit) = s4_verdict(&dir, &expected, r#"{"results": []}"#);
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(exit.is_ok(), "no-work must pass the aggregate: {exit:?}");
        assert_eq!(
            explicit_no_work_line(&expected, &verdict).as_deref(),
            Some(
                "no_work_reason=planner selected zero workload units and no workload results were reported"
            ),
        );
        // A stray report beside a no-work plan stays extra and ignored: the
        // pass stands, and the reason names the ignored report.
        let stray = r#"{"results": [{"unit": "ghost", "lane": "github", "outcome": "success"}]}"#;
        let (verdict, exit) = s4_verdict(&dir, &expected, stray);
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(
            exit.is_ok(),
            "extra results never red a no-work plan: {exit:?}"
        );
        assert_eq!(
            explicit_no_work_line(&expected, &verdict).as_deref(),
            Some(
                "no_work_reason=planner selected zero workload units; 1 reported result ignored as outside the plan"
            ),
        );
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn empty_diff_and_lane_narrowing_carry_reasons_while_unproven_empty_errors(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, _) = selection_git_fixture("s4-empty", "crates/base/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &base)?;
        assert!(selection.units.is_empty());
        assert_eq!(
            must(
                planned_no_work_reason(&selection),
                "record the empty-diff reason"
            )
            .as_deref(),
            Some("empty diff selects no workload units"),
        );
        std::fs::remove_dir_all(root)?;

        // Lane admission narrowing a real selection to zero is no-work for
        // its own reason: nothing runnable, not nothing affected.
        let config = lanes_selection_config();
        let swift = config.unit.iter().find(|unit| unit.id == "swift-app");
        let selection = UnitSelection {
            units: swift.into_iter().collect(),
            full_units: ["swift-app"].into_iter().map(str::to_owned).collect(),
            fallback_reason: None,
            no_work_reason: None,
        };
        let narrowed = selection_for_lanes(&config, selection, RunnerMode::Velnor);
        assert!(narrowed.units.is_empty());
        assert_eq!(
            must(
                planned_no_work_reason(&narrowed),
                "record the lane-narrowing reason"
            )
            .as_deref(),
            Some("no selected workload unit runs on the admitted lanes"),
        );

        // An empty selection that somehow carries no recorded reason is a
        // plan error, never a silent no-work pass: no future selection arm
        // can go empty without proving why. Reachable only by construction —
        // every selection arm that can go empty records its reason.
        let degenerate = UnitSelection {
            units: Vec::new(),
            full_units: BTreeSet::new(),
            fallback_reason: None,
            no_work_reason: None,
        };
        let error = must_fail(
            planned_no_work_reason(&degenerate),
            "an unproven empty selection must fail",
        );
        assert!(
            error
                .to_string()
                .contains("without a recorded no-work reason"),
            "{error}",
        );
        // A recorded reason survives lane narrowing untouched.
        let recorded = UnitSelection {
            units: Vec::new(),
            full_units: BTreeSet::new(),
            fallback_reason: None,
            no_work_reason: Some("empty diff selects no workload units".to_owned()),
        };
        let narrowed = selection_for_lanes(&config, recorded, RunnerMode::Velnor);
        assert_eq!(
            must(
                planned_no_work_reason(&narrowed),
                "a recorded reason survives narrowing"
            )
            .as_deref(),
            Some("empty diff selects no workload units"),
        );
        Ok(())
    }

    #[test]
    fn expected_work_writer_maps_units_lanes_and_in_plan_prerequisites(
    ) -> Result<(), Box<dyn Error>> {
        let (root, base, head) = selection_git_fixture("s4-owned", "crates/app/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert_eq!(
            selected_ids(selection.units.clone()),
            vec!["base", "app", "consumer"]
        );
        assert!(must(
            planned_no_work_reason(&selection),
            "real work carries no no-work reason"
        )
        .is_none());
        let expected = s4_expected_for_selection(&selection, RunnerMode::Both);
        let document: serde_json::Value = serde_json::from_str(&expected)?;
        assert_eq!(
            document
                .get("planned_no_work")
                .and_then(serde_json::Value::as_bool),
            Some(false),
        );
        let (ambient_base, ambient_head) = s4_ambient_shas();
        assert_eq!(
            document.get("base_sha").and_then(serde_json::Value::as_str),
            Some(ambient_base.as_str()),
            "the writer binds the plan's base SHA",
        );
        assert_eq!(
            document.get("head_sha").and_then(serde_json::Value::as_str),
            Some(ambient_head.as_str()),
            "the writer binds the plan's head SHA",
        );
        let units = document
            .get("units")
            .and_then(serde_json::Value::as_array)
            .ok_or("the writer must emit a units array")?;
        assert_eq!(units.len(), 3);
        for unit in units {
            assert_eq!(
                unit.get("lanes"),
                Some(&serde_json::json!(["github", "velnor"])),
                "every admitted lane supporting rust must hear a verdict",
            );
            assert_eq!(unit.get("matrix"), Some(&serde_json::json!([])));
            assert_eq!(
                unit.get("required").and_then(serde_json::Value::as_bool),
                Some(true),
            );
            assert!(
                unit.get("planned_skip").is_none(),
                "the planner removes instead of pre-skipping",
            );
        }
        assert_eq!(
            document.get("prerequisites"),
            Some(&serde_json::json!({
                "app": ["base"],
                "base": [],
                "consumer": ["app"],
            })),
        );
        std::fs::remove_dir_all(root)?;

        // Dependency skipping follows the executed set: a prerequisite the
        // plan did not select neither gates the runner (`run_layers` filters
        // out-of-plan edges) nor appears in the aggregate's graph, so the
        // dependent's own success holds.
        let config = selection_config();
        let app = config.unit.iter().find(|unit| unit.id == "app");
        let selection = UnitSelection {
            units: app.into_iter().collect(),
            full_units: ["app"].into_iter().map(str::to_owned).collect(),
            fallback_reason: None,
            no_work_reason: None,
        };
        let expected = s4_expected_for_selection(&selection, RunnerMode::Github);
        let document: serde_json::Value = serde_json::from_str(&expected)?;
        assert_eq!(
            document.get("prerequisites"),
            Some(&serde_json::json!({ "app": [] })),
            "out-of-plan prerequisites leave the expected graph",
        );
        let results = s4_success_results_for(&expected);
        let verdict = s4_score(&expected, &results)?;
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        Ok(())
    }

    #[test]
    fn expected_work_writer_creates_a_missing_nested_parent() -> Result<(), Box<dyn Error>> {
        // Live-CI shape: the plan job runs in a fresh checkout with no
        // `.velnor-ci-expected-work/` directory, so the writer must create
        // its own parent instead of relying on a pre-existing dir.
        let config = selection_config();
        let app = config.unit.iter().find(|unit| unit.id == "app");
        let selection = UnitSelection {
            units: app.into_iter().collect(),
            full_units: ["app"].into_iter().map(str::to_owned).collect(),
            fallback_reason: None,
            no_work_reason: None,
        };
        let dir = s4_dir("expected-nested-parent");
        let path = dir.join("does/not/exist/expected-work.json");
        let (base, head) = s4_ambient_shas();
        must(
            write_expected_work_file(&path, &selection, RunnerMode::Both, &base, &head),
            "write expected work through a missing nested parent",
        );
        let text = must(std::fs::read_to_string(&path), "read expected work");
        let document: serde_json::Value = serde_json::from_str(&text)?;
        assert_eq!(
            document
                .get("planned_no_work")
                .and_then(serde_json::Value::as_bool),
            Some(false),
        );
        assert_eq!(
            document.get("units"),
            Some(&serde_json::json!([{
                "id": "app",
                "lanes": ["github", "velnor"],
                "matrix": [],
                "required": true,
            }])),
        );
        assert_eq!(
            document.get("prerequisites"),
            Some(&serde_json::json!({ "app": [] })),
        );
        must(std::fs::remove_dir_all(&dir), "remove s4 fixture");
        Ok(())
    }

    #[test]
    fn selection_writer_creates_a_missing_nested_parent() {
        // The selection writer shares the expected-work writer's contract:
        // a nested `VELNOR_SELECTION_FILE` path must not need a
        // pre-existing directory.
        let dir = s4_dir("selection-nested-parent");
        let path = dir.join("does/not/exist/velnor-ci-selection");
        must(
            write_selection_file(
                &path,
                "base-sha",
                "head-sha",
                Scope::Affected,
                "app",
                "app,base",
            ),
            "write selection through a missing nested parent",
        );
        let text = must(std::fs::read_to_string(&path), "read selection");
        assert_eq!(
            text,
            format!(
                "version={SELECTION_FILE_VERSION}\nbase_sha=base-sha\nhead_sha=head-sha\nscope=affected\nunits=app\nfull_units=app,base\n",
            ),
        );
        must(std::fs::remove_dir_all(&dir), "remove s4 fixture");
    }

    #[test]
    fn selected_but_missing_result_fails_aggregate() -> Result<(), Box<dyn Error>> {
        let (root, expected, results) = s4_owned_binding("s4-missing")?;
        // Sanity: full coverage passes before one verdict goes missing.
        let clean = s4_score(&expected, &results)?;
        assert!(clean.passed, "failures: {:?}", clean.failures);
        let results = s4_drop_result(&results, "consumer", "velnor");
        let dir = s4_dir("missing");
        let (verdict, exit) = s4_verdict(&dir, &expected, &results);
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("missing result for consumer velnor")),
            "failures: {:?}",
            verdict.failures,
        );
        assert!(exit.is_err(), "a missing result must fail the aggregate");
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn lane_scoped_expected_work_passes_with_matching_lane_only_results(
    ) -> Result<(), Box<dyn Error>> {
        // A single-lane workflow plans only its lane and runs only its
        // lane: lane-scoped expected work plus the matching lane-only
        // results passes, with no phantom entries for the unscheduled lane.
        let (root, base, head) = selection_git_fixture("s4-lane-scope", "crates/app/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        assert!(
            !selection.units.is_empty(),
            "the fixture diff must select real work"
        );
        let expected = s4_expected_for_selection(&selection, RunnerMode::Github);
        let document: serde_json::Value = serde_json::from_str(&expected)?;
        let units = must_some(
            document.get("units").and_then(serde_json::Value::as_array),
            "expected units array",
        );
        assert!(!units.is_empty(), "a real-work plan expects units");
        for unit in units {
            assert_eq!(
                unit.get("lanes"),
                Some(&serde_json::json!(["github"])),
                "a github-scoped plan names no velnor lane: {expected}"
            );
        }
        let results = s4_success_results_for(&expected);
        let dir = s4_dir("lane-scope");
        let (verdict, exit) = s4_verdict(&dir, &expected, &results);
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(exit.is_ok(), "matching lane-only results pass: {exit:?}");
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn both_scoped_expected_work_fails_closed_with_lane_only_results() -> Result<(), Box<dyn Error>>
    {
        // The single-lane phantom failure, pinned at the aggregate: a
        // both-scoped plan scored against lane-only results fails on every
        // missing unscheduled-lane record, never silently.
        let (root, base, head) = selection_git_fixture("s4-lane-phantom", "crates/app/src/lib.rs")?;
        let config = selection_config();
        let selection = selection_for_diff(&root, &config, Scope::Affected, &base, &head)?;
        let expected = s4_expected_for_selection(&selection, RunnerMode::Both);
        let document: serde_json::Value = serde_json::from_str(&expected)?;
        let ids: Vec<String> = document
            .get("units")
            .and_then(serde_json::Value::as_array)
            .map(|units| {
                units
                    .iter()
                    .filter_map(|unit| unit.get("id").and_then(serde_json::Value::as_str))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        assert!(!ids.is_empty(), "a real-work plan expects units");
        let mut results = s4_success_results_for(&expected);
        for id in &ids {
            results = s4_drop_result(&results, id, "velnor");
        }
        let dir = s4_dir("lane-phantom");
        let (verdict, exit) = s4_verdict(&dir, &expected, &results);
        assert!(!verdict.passed);
        for id in &ids {
            assert!(
                verdict
                    .failures
                    .iter()
                    .any(|failure| failure.contains(&format!("missing result for {id} velnor"))),
                "failures: {:?}",
                verdict.failures,
            );
        }
        assert!(exit.is_err(), "phantom lanes must fail the aggregate");
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn unexpected_skip_fails_aggregate() -> Result<(), Box<dyn Error>> {
        let (root, expected, results) = s4_owned_binding("s4-skip")?;
        let results = s4_set_result(
            &results,
            "app",
            "github",
            "skipped",
            &[("reason", "runner drained the queue")],
        );
        let dir = s4_dir("skip");
        let (verdict, exit) = s4_verdict(&dir, &expected, &results);
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("unexpected skip of app github")),
            "failures: {:?}",
            verdict.failures,
        );
        assert!(exit.is_err(), "an unplanned skip must fail the aggregate");
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn partial_matrix_fails_while_empty_matrix_expects_one_verdict() -> Result<(), Box<dyn Error>> {
        // A matrixed unit must report every entry: one missing entry fails
        // the aggregate as an incomplete matrix.
        let (base, head) = s4_ambient_shas();
        let expected = must(
            serde_json::to_string(&serde_json::json!({
                "base_sha": base,
                "head_sha": head,
                "planned_no_work": false,
                "units": [{"id": "shard", "lanes": ["github"], "matrix": ["a", "b"], "required": true}],
                "prerequisites": {"shard": []},
            })),
            "serialize matrixed plan",
        );
        let results = r#"{"results": [{"unit": "shard", "lane": "github", "matrix": "a", "outcome": "success"}]}"#;
        let dir = s4_dir("matrix");
        let (verdict, exit) = s4_verdict(&dir, &expected, results);
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("missing result for shard github[b]")),
            "failures: {:?}",
            verdict.failures,
        );
        assert!(
            verdict
                .explanations
                .iter()
                .any(|explanation| explanation.detail.contains("incomplete matrix")),
            "the explanation must name the incomplete matrix",
        );
        assert!(exit.is_err(), "a partial matrix must fail the aggregate");

        // The empty-matrix rule: no matrix means exactly one unmatrixed
        // item — the unit's single verdict — never zero, never many.
        let config = selection_config();
        let base = config.unit.iter().find(|unit| unit.id == "base");
        let selection = UnitSelection {
            units: base.into_iter().collect(),
            full_units: ["base"].into_iter().map(str::to_owned).collect(),
            fallback_reason: None,
            no_work_reason: None,
        };
        let expected = s4_expected_for_selection(&selection, RunnerMode::Github);
        let results = r#"{"results": [{"unit": "base", "lane": "github", "outcome": "success"}]}"#;
        let (verdict, exit) = s4_verdict(&dir, &expected, results);
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(
            exit.is_ok(),
            "one verdict satisfies an empty matrix: {exit:?}"
        );
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn cancelled_result_fails_aggregate() -> Result<(), Box<dyn Error>> {
        let (root, expected, results) = s4_owned_binding("s4-cancelled")?;
        let results = s4_set_result(&results, "base", "github", "cancelled", &[]);
        let dir = s4_dir("cancelled");
        let (verdict, exit) = s4_verdict(&dir, &expected, &results);
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("cancelled required work base github")),
            "failures: {:?}",
            verdict.failures,
        );
        assert!(exit.is_err(), "a cancellation has no verdict and must fail");
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn failed_prerequisite_blocks_its_dependent() -> Result<(), Box<dyn Error>> {
        let (root, expected, results) = s4_owned_binding("s4-prereq")?;
        let results = s4_set_result(&results, "base", "github", "failure", &[]);
        let results = s4_set_result(&results, "base", "velnor", "failure", &[]);
        let dir = s4_dir("prereq");
        let (verdict, exit) = s4_verdict(&dir, &expected, &results);
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("prerequisite `base` of `app` did not pass")),
            "failures: {:?}",
            verdict.failures,
        );
        assert!(
            exit.is_err(),
            "a success behind a failed prerequisite proves nothing"
        );
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn unplanned_reports_never_green_or_red_planned_work() -> Result<(), Box<dyn Error>> {
        // Extra reports are noted and ignored beside real work too: they
        // green nothing and red nothing — not even an extra failure.
        let (root, expected, results) = s4_owned_binding("s4-extra")?;
        let mut document: serde_json::Value = serde_json::from_str(&results)?;
        document["results"]
            .as_array_mut()
            .ok_or("results array")?
            .push(serde_json::json!({"unit": "ghost", "lane": "github", "outcome": "failure"}));
        let results = must(serde_json::to_string(&document), "serialize results");
        let verdict = s4_score(&expected, &results)?;
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(
            verdict
                .explanations
                .iter()
                .any(|explanation| explanation.disposition == crate::reuse::Disposition::Extra),
            "the stray report must be noted as extra",
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn planner_failure_leaves_no_green_path() -> Result<(), Box<dyn Error>> {
        // No plan artifacts, no green: missing files are hard errors, never
        // an empty pass.
        let dir = s4_dir("planner-failure");
        let missing = dir.join("absent.json");
        let error = must_fail(
            aggregate_command(&missing, &missing),
            "a missing expected-work file must fail",
        );
        assert!(error.to_string().contains("read expected work"), "{error}");
        let expected_path = dir.join("expected.json");
        must(
            std::fs::write(&expected_path, r#"{"planned_no_work": true, "units": []}"#),
            "write expected",
        );
        let error = must_fail(
            aggregate_command(&expected_path, &missing),
            "missing results must fail",
        );
        assert!(
            error.to_string().contains("read reported results"),
            "{error}"
        );

        let scope = s4_run_scope();
        let (config_path, _) = s4_run_paths(&dir, scope, "", "");
        let error = must_fail(
            run_units_with_selection_file(&dir, &config_path, scope, None, None, &missing),
            "a missing selection artifact must fail",
        );
        assert!(error.to_string().contains("read CI selection"), "{error}");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn invalid_plan_fails_closed() -> Result<(), Box<dyn Error>> {
        // An unparseable plan is a usage error, never a pass.
        let error = must_fail(
            crate::reuse::aggregate_files("{not json", "{}", "", "").map_err(GeneratorError::usage),
            "an unparseable plan must fail",
        );
        assert!(error.to_string().contains("not valid JSON"), "{error}");
        let dir = s4_dir("invalid");
        let expected_path = dir.join("expected.json");
        let results_path = dir.join("results.json");
        must(
            std::fs::write(&expected_path, "{not json"),
            "write bad expected",
        );
        must(std::fs::write(&results_path, "{}"), "write results");
        let error = must_fail(
            aggregate_command(&expected_path, &results_path),
            "an unparseable plan must fail the command",
        );
        assert!(error.to_string().contains("not valid JSON"), "{error}");

        // A contradictory plan — the no-work marker beside listed units —
        // fails inside the verdict.
        let (base, head) = s4_ambient_shas();
        let expected = must(
            serde_json::to_string(&serde_json::json!({
                "base_sha": base,
                "head_sha": head,
                "planned_no_work": true,
                "units": [{"id": "base", "lanes": ["github"]}],
            })),
            "serialize contradictory plan",
        );
        let verdict = s4_score(&expected, r#"{"results": []}"#)?;
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("contradictory")),
            "failures: {:?}",
            verdict.failures,
        );

        // So does unmarked emptiness: zero units without the explicit
        // marker is a broken plan, not proven no-work.
        let expected = must(
            serde_json::to_string(&serde_json::json!({
                "base_sha": base,
                "head_sha": head,
                "units": [],
            })),
            "serialize unmarked plan",
        );
        let verdict = s4_score(&expected, r#"{"results": []}"#)?;
        assert!(!verdict.passed);
        assert!(
            verdict
                .failures
                .iter()
                .any(|failure| failure.contains("without the explicit no-work marker")),
            "failures: {:?}",
            verdict.failures,
        );

        // And a malformed selection artifact fails the runner before any
        // unit — or any no-work line — runs.
        let scope = s4_run_scope();
        let (config_path, _) = s4_run_paths(&dir, scope, "", "");
        let bad_selection = dir.join("bad-selection");
        must(
            std::fs::write(&bad_selection, "version=99\n"),
            "write bad selection",
        );
        let error = must_fail(
            run_units_with_selection_file(&dir, &config_path, scope, None, None, &bad_selection),
            "a malformed selection artifact must fail",
        );
        assert!(
            error
                .to_string()
                .contains("unsupported CI selection artifact version"),
            "{error}",
        );
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn reused_evidence_still_passes_aggregate() -> Result<(), Box<dyn Error>> {
        // A success backed by producing evidence holds exactly like an
        // executed one: the wiring must not regress reuse acceptance.
        let (root, expected, results) = s4_owned_binding("s4-reuse")?;
        let results = s4_set_result(
            &results,
            "app",
            "velnor",
            "success",
            &[("reused_from", "run-7f3a")],
        );
        let dir = s4_dir("reuse");
        let (verdict, exit) = s4_verdict(&dir, &expected, &results);
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(exit.is_ok(), "reused evidence must still pass: {exit:?}");
        assert!(
            verdict
                .explanations
                .iter()
                .any(|explanation| explanation.disposition == crate::reuse::Disposition::Reused),
            "the reused verdict must be recorded as reused",
        );
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn run_with_empty_selection_reports_explicit_no_work() -> Result<(), Box<dyn Error>> {
        // A validated empty selection succeeds explicitly: the machine-
        // readable reason, not a silent pass and not an error.
        assert_eq!(
            no_work_report_line(),
            "no_work_reason=CI selection artifact selects zero workload units; nothing to run",
        );
        let dir = s4_dir("run-no-work");
        let scope = s4_run_scope();
        let (config_path, selection_path) = s4_run_paths(&dir, scope, "", "");
        must(
            run_units_with_selection_file(&dir, &config_path, scope, None, None, &selection_path)
                .map_err(|error| error.to_string()),
            "an empty selection must succeed explicitly",
        );
        // Asking for a unit the plan did not select is never no-work.
        let error = must_fail(
            run_units_with_selection_file(
                &dir,
                &config_path,
                scope,
                Some("base"),
                None,
                &selection_path,
            ),
            "an unselected unit must fail closed",
        );
        assert!(
            error
                .to_string()
                .contains("does not include requested unit `base`"),
            "{error}",
        );
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn aggregate_no_work_line_stays_silent_for_real_work() -> Result<(), Box<dyn Error>> {
        // Real-work verdicts print the audit report only: the no-work line
        // never reshapes their bytes.
        let (root, expected, results) = s4_owned_binding("s4-silence")?;
        let verdict = s4_score(&expected, &results)?;
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert!(explicit_no_work_line(&expected, &verdict).is_none());
        // Failed no-work verdicts stay silent too: only a PASS carries the
        // machine-readable reason.
        let (base, head) = s4_ambient_shas();
        let unmarked = must(
            serde_json::to_string(&serde_json::json!({
                "base_sha": base,
                "head_sha": head,
                "units": [],
            })),
            "serialize unmarked plan",
        );
        let verdict = s4_score(&unmarked, r#"{"results": []}"#)?;
        assert!(!verdict.passed);
        assert!(explicit_no_work_line(&unmarked, &verdict).is_none());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn selected_but_runnable_nowhere_fails_the_plan_closed() {
        // A selected unit with no admitted lane is a contradictory plan, not
        // an empty one: the writer fails instead of emitting a unit the
        // aggregate could never hear from. Unreachable after lane narrowing
        // (which drops such units with its own reason), proven here so the
        // guard cannot silently rot.
        let config = lanes_selection_config();
        let swift = config.unit.iter().find(|unit| unit.id == "swift-app");
        let selection = UnitSelection {
            units: swift.into_iter().collect(),
            full_units: ["swift-app"].into_iter().map(str::to_owned).collect(),
            fallback_reason: None,
            no_work_reason: None,
        };
        let dir = s4_dir("nowhere");
        let (base, head) = s4_ambient_shas();
        let error = must_fail(
            write_expected_work_file(
                &dir.join("expected.json"),
                &selection,
                RunnerMode::Velnor,
                &base,
                &head,
            ),
            "a unit runnable nowhere must fail the plan",
        );
        assert!(
            error.to_string().contains("runnable on no admitted lane"),
            "{error}",
        );
        // Unknown kinds stay on every admitted lane: the planner keeps
        // units it does not recognize, so the aggregate hears all of them.
        let future = config.unit.iter().find(|unit| unit.id == "future-app");
        let selection = UnitSelection {
            units: future.into_iter().collect(),
            full_units: ["future-app"].into_iter().map(str::to_owned).collect(),
            fallback_reason: None,
            no_work_reason: None,
        };
        let expected = s4_expected_for_selection(&selection, RunnerMode::Both);
        let document: serde_json::Value = must(serde_json::from_str(&expected), "parse expected");
        assert_eq!(
            document
                .get("units")
                .and_then(|units| units.get(0))
                .and_then(|unit| unit.get("lanes")),
            Some(&serde_json::json!(["github", "velnor"])),
        );
        must(std::fs::remove_dir_all(&dir), "remove s4 fixture");
    }

    #[test]
    fn stale_expected_work_fails_the_aggregate_closed() -> Result<(), Box<dyn Error>> {
        // A no-work file from an earlier run paired with zero results must
        // FAIL, never PASS: the identity binding — not artifact threading —
        // proves the file is this plan's. The stale SHAs derive from the
        // ambient checkout so the mismatch holds under any outer env.
        let (ambient_base, ambient_head) = s4_ambient_shas();
        let stale = must(
            serde_json::to_string(&serde_json::json!({
                "planned_no_work": true,
                "units": [],
                "prerequisites": {},
                "base_sha": format!("{ambient_base}-earlier-run"),
                "head_sha": format!("{ambient_head}-earlier-run"),
            })),
            "serialize stale plan",
        );
        let error = must_fail(
            s4_score(&stale, r#"{"results": []}"#).map_err(GeneratorError::usage),
            "a stale no-work file must fail",
        );
        assert!(
            error
                .to_string()
                .contains("does not match this aggregate checkout"),
            "{error}",
        );
        // The command binds the same ambient SHAs: the stale file fails the
        // CLI exit too, as does a file with no identity at all.
        let dir = s4_dir("stale");
        let expected_path = dir.join("expected.json");
        let results_path = dir.join("results.json");
        must(
            std::fs::write(&expected_path, &stale),
            "write stale expected",
        );
        must(
            std::fs::write(&results_path, r#"{"results": []}"#),
            "write results",
        );
        let error = must_fail(
            aggregate_command(&expected_path, &results_path),
            "a stale no-work file must fail the command",
        );
        assert!(
            error
                .to_string()
                .contains("does not match this aggregate checkout"),
            "{error}",
        );
        must(
            std::fs::write(&expected_path, r#"{"planned_no_work": true, "units": []}"#),
            "write unbound expected",
        );
        let error = must_fail(
            aggregate_command(&expected_path, &results_path),
            "an unbound file must fail the command",
        );
        assert!(error.to_string().contains("missing base_sha"), "{error}");
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn plan_writes_expected_work_file_and_no_work_marker() -> Result<(), Box<dyn Error>> {
        // The planner end to end: a no-work diff writes the expected-work
        // file — identity, marker, and empty units — and emits the marker to
        // `GITHUB_OUTPUT`. Driven through injected env, never process-global
        // mutation (which parallel tests also read).
        let (root, base, head) = selection_git_fixture("s4-plan-e2e", "AGENTS.md")?;
        let dir = s4_dir("plan-e2e");
        let config_path = dir.join("project.toml");
        must(
            std::fs::write(&config_path, S4_PLAN_CONFIG_TOML),
            "write s4 plan config",
        );
        let expected_path = dir.join("expected.json");
        let output_path = dir.join("github-output");
        must(
            plan_with(
                &config_path,
                &PlanInputs {
                    root: root.clone(),
                    event: PlanEvent::PullRequest,
                    scope_override: None,
                    base: base.clone(),
                    head: head.clone(),
                    lanes: String::new(),
                    selection_file: None,
                    expected_file: Some(expected_path.clone()),
                    github_output: Some(output_path.clone()),
                },
            ),
            "plan the no-work diff",
        );
        let expected = must(
            std::fs::read_to_string(&expected_path),
            "read expected work",
        );
        let document: serde_json::Value = serde_json::from_str(&expected)?;
        assert_eq!(
            document.get("base_sha").and_then(serde_json::Value::as_str),
            Some(base.as_str()),
            "the file binds the plan's base SHA",
        );
        assert_eq!(
            document.get("head_sha").and_then(serde_json::Value::as_str),
            Some(head.as_str()),
            "the file binds the plan's head SHA",
        );
        assert_eq!(
            document
                .get("planned_no_work")
                .and_then(serde_json::Value::as_bool),
            Some(true),
        );
        assert_eq!(
            document
                .get("units")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0),
        );
        // The bound file scores against the same SHAs: proven no-work plus
        // zero results passes with the machine-readable reason.
        let verdict = crate::reuse::aggregate_files(&expected, r#"{"results": []}"#, &base, &head)?;
        assert!(verdict.passed, "failures: {:?}", verdict.failures);
        assert_eq!(
            explicit_no_work_line(&expected, &verdict).as_deref(),
            Some(
                "no_work_reason=planner selected zero workload units and no workload results were reported"
            ),
        );
        let outputs = must(std::fs::read_to_string(&output_path), "read github output");
        assert!(outputs.contains("planned_no_work=true"), "{outputs}");
        assert!(
            outputs.contains("no_work_reason=no changed path selected a workload unit"),
            "{outputs}",
        );
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn lane_scoped_plan_writes_only_scheduled_lanes() -> Result<(), Box<dyn Error>> {
        // The planner honors the `VELNOR_LANES` value the render emits —
        // the configured single lane, or the dispatch-narrowed selection:
        // real work plans exactly the scheduled lanes, never the other.
        for (lanes, other) in [("github", "velnor"), ("velnor", "github")] {
            let (root, dir, expected, outputs) =
                s4_lanes_plan("s4-lanes-plan", lanes, "crates/alpha/src/lib.rs")?;
            assert!(
                !outputs.contains("planned_no_work"),
                "{lanes}: real work carries no no-work marker: {outputs}"
            );
            let document: serde_json::Value = serde_json::from_str(&expected)?;
            assert_eq!(
                document
                    .get("planned_no_work")
                    .and_then(serde_json::Value::as_bool),
                Some(false),
                "{lanes}: the fixture diff is real work"
            );
            let units = must_some(
                document.get("units").and_then(serde_json::Value::as_array),
                "expected units array",
            );
            assert_eq!(units.len(), 1, "{lanes}: one unit selected: {expected}");
            assert_eq!(
                units[0].get("lanes"),
                Some(&serde_json::json!([lanes])),
                "{lanes}: a lane-scoped plan names only its lane: {expected}"
            );
            assert!(
                !expected.contains(&format!("\"{other}\"")),
                "{lanes}: the unscheduled lane appears nowhere: {expected}"
            );
            std::fs::remove_dir_all(dir)?;
            std::fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[test]
    fn velnor_narrowed_plan_drops_swift_as_proven_no_work() -> Result<(), Box<dyn Error>> {
        // Dispatch narrowing to a lane that cannot run the selected units
        // is proven no-work with its own reason — not a phantom velnor
        // entry for a Swift unit no velnor job can report.
        let (root, dir, expected, outputs) =
            s4_lanes_plan("s4-lanes-swift", "velnor", "native/app.swift")?;
        let document: serde_json::Value = serde_json::from_str(&expected)?;
        assert_eq!(
            document
                .get("planned_no_work")
                .and_then(serde_json::Value::as_bool),
            Some(true),
        );
        assert_eq!(
            document
                .get("units")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(0),
        );
        assert!(outputs.contains("planned_no_work=true"), "{outputs}");
        assert!(
            outputs.contains("no_work_reason=no selected workload unit runs on the admitted lanes"),
            "{outputs}"
        );
        std::fs::remove_dir_all(dir)?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}

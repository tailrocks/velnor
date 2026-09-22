//! Execution platform requirements, prerequisite products, and placement.
//!
//! Providers are deployment detail: a unit never names a runner label. It
//! declares what it needs — an operating system, an architecture, and a set of
//! SDK capabilities — and generation maps each need onto an eligible executor
//! of the lane that runs it. A need no enabled lane can serve is a generation
//! error that names the unit, the need, and the remedy, never a silently
//! skipped job.
//!
//! The same module carries the prerequisite contract: a unit produces named
//! products through named tasks, and a consumer declares which producer
//! products it needs. Generation compiles each edge into the selection graph
//! (`depends_on`, so producer changes select the consumer transitively) and
//! into prepare commands (so the consumer rebuilds the product locally before
//! its own checks), with environment flowing from task inputs to job outputs.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::{GeneratorError, ProjectConfig, RunnerMode, Unit, UnitKind};

/// The SDK capability an Xcode scheme build needs. Only a macOS executor
/// offers it, so ordinary `SwiftPM` units must never declare it: they verify
/// anywhere their toolchain provisions.
pub(crate) const CAP_XCODE: &str = "xcode";
/// The SDK capability a unit that assembles or consumes an `XCFramework`
/// needs. Like [`CAP_XCODE`], it resolves to a macOS executor.
pub(crate) const CAP_XCFRAMEWORK: &str = "xcframework";

/// The operating system a unit needs. `Any` is portable: the unit verifies on
/// whatever the lane offers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Os {
    #[default]
    Any,
    Linux,
    Macos,
}

/// The architecture a unit needs. `Any` runs on either executor word size.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) enum Arch {
    #[default]
    Any,
    #[serde(rename = "x86_64")]
    X86_64,
    #[serde(rename = "aarch64")]
    Aarch64,
}

/// What a unit needs from its executor: an OS, an architecture, and a set of
/// SDK capabilities. Provider-independent by construction: no runner label,
/// pool name, or backend appears here.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct PlatformRequirement {
    pub(crate) os: Os,
    pub(crate) arch: Arch,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) capabilities: BTreeSet<String>,
}

impl PlatformRequirement {
    /// A unit with no needs: it verifies on any executor of any lane.
    pub(crate) fn portable() -> Self {
        Self::default()
    }

    /// A `SwiftPM` package: portable, with no Apple-only need. It must resolve
    /// to the lane's default executor, never to macOS by kind alone.
    pub(crate) fn swift_package() -> Self {
        Self::default()
    }

    /// An Xcode scheme build: macOS with the Xcode SDK capability.
    pub(crate) fn apple_xcode() -> Self {
        Self {
            os: Os::Macos,
            arch: Arch::Any,
            capabilities: BTreeSet::from([CAP_XCODE.to_owned()]),
        }
    }

    /// A `SwiftPM` package consuming an `XCFramework` binary target: macOS with
    /// the `XCFramework` capability. The bundle only resolves where the Apple
    /// SDK exists, so the unit is Apple-bound even without an Xcode project.
    pub(crate) fn apple_xcframework() -> Self {
        Self {
            os: Os::Macos,
            arch: Arch::Any,
            capabilities: BTreeSet::from([CAP_XCFRAMEWORK.to_owned()]),
        }
    }

    /// Whether the requirement can only be served by a macOS executor: an
    /// explicit macOS need or an Apple-only SDK capability.
    pub(crate) fn requires_apple(&self) -> bool {
        self.os == Os::Macos
            || self
                .capabilities
                .iter()
                .any(|capability| capability == CAP_XCODE || capability == CAP_XCFRAMEWORK)
    }

    /// Parse a `[[units]]` `os` value.
    ///
    /// # Errors
    /// Returns a usage error for anything but `any`, `linux`, or `macos`.
    pub(crate) fn parse_os(value: &str) -> Result<Os, GeneratorError> {
        match value {
            "any" => Ok(Os::Any),
            "linux" => Ok(Os::Linux),
            "macos" => Ok(Os::Macos),
            other => Err(GeneratorError::usage(format!(
                "unit `os` must be one of: any, linux, macos; found `{other}`"
            ))),
        }
    }

    /// Parse a `[[units]]` `arch` value.
    ///
    /// # Errors
    /// Returns a usage error for anything but `any`, `x86_64`, or `aarch64`.
    pub(crate) fn parse_arch(value: &str) -> Result<Arch, GeneratorError> {
        match value {
            "any" => Ok(Arch::Any),
            "x86_64" => Ok(Arch::X86_64),
            "aarch64" => Ok(Arch::Aarch64),
            other => Err(GeneratorError::usage(format!(
                "unit `arch` must be one of: any, x86_64, aarch64; found `{other}`"
            ))),
        }
    }

    /// A capability name: lowercase, short, and free of shell metacharacters,
    /// so it survives rendering into keys, labels, and diagnostics verbatim.
    pub(crate) fn valid_capability(value: &str) -> bool {
        valid_product_name(value)
    }
}

/// One class of executor a lane offers, described in the same vocabulary as
/// the requirement it serves. The id names the lane plus the OS family — the
/// generator's own routing vocabulary — never a provider's pool or label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Executor {
    pub(crate) id: &'static str,
    pub(crate) lane: RunnerMode,
    pub(crate) os: Os,
    pub(crate) arch: Arch,
    pub(crate) capabilities: BTreeSet<String>,
}

impl Executor {
    fn github_linux() -> Self {
        Self {
            id: "github-linux",
            lane: RunnerMode::Github,
            os: Os::Linux,
            arch: Arch::Any,
            capabilities: BTreeSet::new(),
        }
    }

    fn github_macos() -> Self {
        Self {
            id: "github-macos",
            lane: RunnerMode::Github,
            os: Os::Macos,
            arch: Arch::Any,
            capabilities: BTreeSet::from([CAP_XCODE.to_owned(), CAP_XCFRAMEWORK.to_owned()]),
        }
    }

    fn velnor() -> Self {
        Self {
            id: "velnor",
            lane: RunnerMode::Velnor,
            os: Os::Linux,
            arch: Arch::Any,
            capabilities: BTreeSet::new(),
        }
    }

    /// Whether this executor serves `requirement`: the OS and architecture
    /// agree (either side may be `Any`) and every needed capability is
    /// offered.
    pub(crate) fn satisfies(&self, requirement: &PlatformRequirement) -> bool {
        os_satisfies(self.os, requirement.os)
            && arch_satisfies(self.arch, requirement.arch)
            && requirement
                .capabilities
                .iter()
                .all(|capability| self.capabilities.contains(capability))
    }
}

fn os_satisfies(executor: Os, requirement: Os) -> bool {
    matches!(
        (executor, requirement),
        (Os::Any, _) | (_, Os::Any) | (Os::Linux, Os::Linux) | (Os::Macos, Os::Macos)
    )
}

fn arch_satisfies(executor: Arch, requirement: Arch) -> bool {
    matches!(
        (executor, requirement),
        (Arch::Any, _)
            | (_, Arch::Any)
            | (Arch::X86_64, Arch::X86_64)
            | (Arch::Aarch64, Arch::Aarch64)
    )
}

/// Every executor the enabled lanes offer, in preference order: the lane's
/// default executor first, the Apple executor where the lane has one.
pub(crate) fn executors_for(runners: RunnerMode) -> Vec<Executor> {
    match runners {
        RunnerMode::Github => vec![Executor::github_linux(), Executor::github_macos()],
        RunnerMode::Velnor => vec![Executor::velnor()],
        RunnerMode::Both => vec![
            Executor::github_linux(),
            Executor::github_macos(),
            Executor::velnor(),
        ],
    }
}

/// Whether `lane` offers an executor for `requirement`.
pub(crate) fn lane_supports_platform(lane: RunnerMode, requirement: &PlatformRequirement) -> bool {
    executors_for(RunnerMode::Both)
        .iter()
        .filter(|executor| lane == RunnerMode::Both || executor.lane == lane)
        .any(|executor| executor.satisfies(requirement))
}

/// The GitHub-hosted runner label for `unit`: the Apple executor's label when
/// the unit needs macOS, the default executor's label otherwise. Ordinary
/// `SwiftPM` support carries no Apple need, so it stays on Linux.
pub(crate) fn github_runner_for_unit<'a>(
    github_runner: &'a str,
    macos_runner: &'a str,
    unit: &Unit,
) -> &'a str {
    if unit.platform.requires_apple() {
        macos_runner
    } else {
        github_runner
    }
}

/// A named build product one unit produces for others: an `XCFramework`
/// bundle, a generated header set, a packed archive. `task` is the repository
/// task that rebuilds it (run through the task runner, never a shell string),
/// `env` carries the task's outputs — the paths and flags consumers need once
/// the product exists — and `inputs` names the product's semantic input
/// closure. Input paths are selection ownership: changing one selects this
/// producer, then the prerequisite edge selects its consumers. Unknown input
/// gaps stay conservative as well as disabling exact reuse.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct NamedProduct {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) inputs_unknown: Vec<String>,
}

/// One prerequisite edge: `producer` builds `product` for this consumer.
/// `task` overrides the product's own task for this consumer, and `env`
/// carries the task inputs the consumer's prepare step exports.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct Prerequisite {
    pub(crate) producer: String,
    pub(crate) product: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
}

impl Prerequisite {
    /// The task the consumer's prepare step runs: the edge override when the
    /// consumer declares one, the product's own task otherwise.
    pub(crate) fn effective_task<'a>(&'a self, product: &'a NamedProduct) -> Option<&'a str> {
        self.task.as_deref().or(product.task.as_deref())
    }
}

/// A product or capability name: lowercase, short, shell-safe.
pub(crate) fn valid_product_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// A declared product input: a repo-relative path or `hashFiles`-style glob.
/// Keep the schema-1 contract identical to schema 2 so a product cannot have
/// one selection identity merely because an older renderer consumed it.
pub(crate) fn valid_product_input(value: &str) -> bool {
    if value.is_empty() || value.len() > 500 || value.starts_with('/') || value.starts_with('!') {
        return false;
    }
    if value.bytes().any(|byte| byte == b'\\') || value.chars().any(char::is_control) {
        return false;
    }
    if value.split('/').any(|segment| {
        segment.is_empty() || segment == "." || segment == ".." || segment.contains('!')
    }) {
        return false;
    }
    value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'*' | b'?' | b'[' | b']')
    })
}

/// A repository task name, as the task runner resolves it: no whitespace, no
/// shell metacharacters, so the prepare step can invoke it without quoting.
pub(crate) fn valid_task_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'/' | b'-')
        })
}

/// An environment variable name for unit and task env.
pub(crate) fn valid_env_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// An environment value: anything without control characters, so it renders
/// into YAML and shell prefixes verbatim.
pub(crate) fn valid_env_value(value: &str) -> bool {
    value.len() <= 4096 && !value.chars().any(char::is_control)
}

/// Validate one env map.
///
/// # Errors
/// Returns a usage error naming the offending entry.
pub(crate) fn validate_env(
    env: &BTreeMap<String, String>,
    context: &str,
) -> Result<(), GeneratorError> {
    for (name, value) in env {
        if !valid_env_name(name) {
            return Err(GeneratorError::usage(format!(
                "{context} declares env `{name}`, which is not an environment variable name; use letters, digits, and underscores starting with a letter or underscore"
            )));
        }
        if !valid_env_value(value) {
            return Err(GeneratorError::usage(format!(
                "{context} declares env `{name}` with control characters; keep values to printable text"
            )));
        }
    }
    Ok(())
}

/// Whether a Cargo `[lib] crate-type` list marks an FFI crate: it builds a
/// native static library others link against.
pub(crate) fn is_ffi_crate_type(crate_types: &[String]) -> bool {
    crate_types
        .iter()
        .any(|crate_type| crate_type == "staticlib" || crate_type == "cdylib")
}

/// The task-runner invocation that rebuilds a prerequisite product, with the
/// edge's task inputs exported ahead of it.
pub(crate) fn prepare_command(task: &str, env: &BTreeMap<String, String>) -> String {
    let mut command = String::new();
    for (name, value) in env {
        command.push_str(name);
        command.push('=');
        command.push_str(&crate::shell_quote(value));
        command.push(' ');
    }
    command.push_str("mise run ");
    command.push_str(task);
    command
}

/// Resolve the platform surface over `config`: validate every prerequisite
/// edge and object-transport toggle, compile edges into the selection graph
/// and prepare commands, merge product outputs into consumer env, and reject
/// any placement no enabled lane can serve.
///
/// # Errors
/// Returns a usage error for an edge that names an unknown producer or a
/// product the producer does not declare, for an object-transport toggle on a
/// unit that cannot use it, and for a unit no enabled lane can execute.
pub(crate) fn resolve(config: &mut ProjectConfig) -> Result<(), GeneratorError> {
    validate_mbx_toggles(config)?;
    validate_product_inputs(config)?;
    materialize_prerequisites(config)?;
    validate_placement(config)?;
    Ok(())
}

fn validate_product_inputs(config: &ProjectConfig) -> Result<(), GeneratorError> {
    for unit in &config.units {
        for product in &unit.products {
            let mut seen = BTreeSet::new();
            for input in &product.inputs {
                if !valid_product_input(input) {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` with input `{input}`, which is not a repo-relative path or glob in normal form; use forward slashes without leading `/`, `.`, `..`, or empty segments",
                        unit.id, product.name
                    )));
                }
                if !seen.insert(input.as_str()) {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` input `{input}` twice; one entry per pattern",
                        unit.id, product.name
                    )));
                }
            }
            for gap in &product.inputs_unknown {
                if !valid_env_value(gap) {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` with an unprintable closure gap; keep gap entries to printable text",
                        unit.id, product.name
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_mbx_toggles(config: &ProjectConfig) -> Result<(), GeneratorError> {
    for unit in &config.units {
        if unit.kind != UnitKind::Rust && unit.mbx == Some(true) {
            return Err(GeneratorError::usage(format!(
                "unit `{}` is a {} unit, which never runs under the object transport; `mbx` applies to Rust units only",
                unit.id,
                unit.kind.label(),
            )));
        }
    }
    Ok(())
}

fn find_product<'a>(
    config: &'a ProjectConfig,
    unit_id: &str,
    name: &str,
) -> Option<&'a NamedProduct> {
    config
        .units
        .iter()
        .find(|unit| unit.id == unit_id)
        .and_then(|unit| unit.products.iter().find(|product| product.name == name))
}

/// Compile prerequisite edges into `depends_on` (so producer changes select
/// the consumer through the existing transitive closure), materialize every
/// product's input closure into its producer watch set, prepare commands (so
/// the consumer rebuilds each product before its own checks on every lane),
/// and merge consumer env (so product outputs reach the checks).
fn materialize_prerequisites(config: &mut ProjectConfig) -> Result<(), GeneratorError> {
    for unit in &config.units {
        for prerequisite in &unit.prerequisites {
            let Some(producer) = config
                .units
                .iter()
                .find(|unit| unit.id == prerequisite.producer)
            else {
                return Err(GeneratorError::usage(format!(
                    "unit `{}` requires product `{}` from `{}`, a unit the repository does not declare; known units: {}",
                    unit.id,
                    prerequisite.product,
                    prerequisite.producer,
                    config
                        .units
                        .iter()
                        .map(|unit| unit.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            };
            if !producer
                .products
                .iter()
                .any(|product| product.name == prerequisite.product)
            {
                let offered = producer
                    .products
                    .iter()
                    .map(|product| product.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let offered = if offered.is_empty() {
                    "it declares no products".to_owned()
                } else {
                    format!("it declares: {offered}")
                };
                return Err(GeneratorError::usage(format!(
                    "unit `{}` requires product `{}` from `{}`, which does not produce it; {}",
                    unit.id, prerequisite.product, prerequisite.producer, offered
                )));
            }
        }
    }
    materialize_product_input_watches(config);
    let mut prepared: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut inherited_env: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for unit in &config.units {
        for prerequisite in &unit.prerequisites {
            let Some(product) = find_product(config, &prerequisite.producer, &prerequisite.product)
            else {
                return Err(GeneratorError::usage(format!(
                    "unit `{}` requires product `{}` from `{}`, which does not produce it",
                    unit.id, prerequisite.product, prerequisite.producer
                )));
            };
            edges
                .entry(unit.id.clone())
                .or_default()
                .push(prerequisite.producer.clone());
            for (name, value) in &product.env {
                inherited_env
                    .entry(unit.id.clone())
                    .or_default()
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
            if let Some(task) = prerequisite.effective_task(product) {
                prepared
                    .entry(unit.id.clone())
                    .or_default()
                    .push(prepare_command(task, &prerequisite.env));
            }
        }
    }
    for unit in &mut config.units {
        if let Some(producers) = edges.remove(&unit.id) {
            for producer in producers {
                if !unit.depends_on.contains(&producer) {
                    unit.depends_on.push(producer);
                }
            }
        }
        if let Some(env) = inherited_env.remove(&unit.id) {
            for (name, value) in env {
                unit.env.entry(name).or_insert(value);
            }
        }
        if let Some(commands) = prepared.remove(&unit.id) {
            prepend_prepare_commands(unit, &commands)?;
        }
    }
    Ok(())
}

/// Add every product input to its producer's canonical watch/read closure.
/// Unknown closure gaps widen ownership to the whole tree: exact reuse and
/// affected selection both fail closed instead of silently skipping a read.
fn materialize_product_input_watches(config: &mut ProjectConfig) {
    for unit in &mut config.units {
        let inputs = unit
            .products
            .iter()
            .flat_map(|product| product.inputs.iter().cloned())
            .chain(
                unit.products
                    .iter()
                    .filter(|product| !product.inputs_unknown.is_empty())
                    .map(|_| "**".to_owned()),
            )
            .collect::<BTreeSet<_>>();
        unit.watch.extend(inputs);
        unit.watch.sort();
        unit.watch.dedup();
    }
}

/// Prepend prepare commands ahead of every command vector the unit runs, so
/// the product rebuilds before the unit's own checks on every lane and in
/// local runs, which read the same serialized vectors.
fn prepend_prepare_commands(unit: &mut Unit, commands: &[String]) -> Result<(), GeneratorError> {
    unit.prepend_precondition_commands(commands)?;
    unit.watch.sort();
    unit.watch.dedup();
    Ok(())
}

/// Describe a requirement for a placement diagnostic: the OS, the
/// architecture, and the capabilities, naming only what the unit constrains.
fn describe_requirement(requirement: &PlatformRequirement) -> String {
    let mut parts = Vec::new();
    match requirement.os {
        Os::Any => {}
        Os::Linux => parts.push("Linux".to_owned()),
        Os::Macos => parts.push("macOS".to_owned()),
    }
    match requirement.arch {
        Arch::Any => {}
        Arch::X86_64 => parts.push("x86_64".to_owned()),
        Arch::Aarch64 => parts.push("aarch64".to_owned()),
    }
    let mut capabilities = requirement.capabilities.iter().cloned().collect::<Vec<_>>();
    capabilities.sort();
    for capability in capabilities {
        parts.push(format!("capability `{capability}`"));
    }
    if parts.is_empty() {
        return "a portable unit".to_owned();
    }
    parts.join(" + ")
}

/// Fail closed when an enabled lane cannot execute a unit: every unit must be
/// servable by at least one enabled lane, so a repository that enables only
/// the self-hosted lane learns at generation time that its Apple work has
/// nowhere to run.
fn validate_placement(config: &ProjectConfig) -> Result<(), GeneratorError> {
    let lanes = match config.runners {
        RunnerMode::Github => vec![RunnerMode::Github],
        RunnerMode::Velnor => vec![RunnerMode::Velnor],
        RunnerMode::Both => vec![RunnerMode::Github, RunnerMode::Velnor],
    };
    for unit in &config.units {
        if lanes
            .iter()
            .any(|lane| lane_supports_platform(*lane, &unit.platform))
        {
            continue;
        }
        let lane = lanes[0];
        return Err(GeneratorError::usage(format!(
            "unit `{}` requires {} but the {} lane offers no matching executor; enable the github lane or drop the requirement",
            unit.id,
            describe_requirement(&unit.platform),
            lane.as_str(),
        )));
    }
    Ok(())
}

/// The job-level env a collapsed kind workflow agrees on: every member's env
/// merged, failing closed when two members export different values for one
/// name, since the shared job can carry only one.
///
/// # Errors
/// Returns a usage error when members of one kind disagree on a value.
pub(crate) fn agreed_env(
    members: &[&Unit],
    kind: UnitKind,
) -> Result<BTreeMap<String, String>, GeneratorError> {
    let mut agreed = BTreeMap::new();
    for member in members {
        for (name, value) in &member.env {
            match agreed.get(name) {
                None => {
                    agreed.insert(name.clone(), value.clone());
                }
                Some(current) if current == value => {}
                Some(_) => {
                    return Err(GeneratorError::usage(format!(
                        "collapsed {} job cannot render one env block: members disagree on `{name}`; keep per-unit env identical within a kind or split the kind",
                        kind.label(),
                    )));
                }
            }
        }
    }
    Ok(agreed)
}

#[cfg(test)]
mod tests {
    use super::{
        agreed_env, describe_requirement, executors_for, is_ffi_crate_type, lane_supports_platform,
        prepare_command, resolve, valid_env_name, valid_env_value, valid_product_input,
        valid_product_name, valid_task_name, Arch, Executor, NamedProduct, Os, PlatformRequirement,
        Prerequisite, CAP_XCFRAMEWORK, CAP_XCODE,
    };
    use crate::{
        AnalysisSummary, MaintenanceSpec, RunnerMode, Unit, UnitKind, ValidationPhase,
        VelnorRustNeeds,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_err<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error,
        }
    }

    fn unit(id: &str, kind: UnitKind) -> Unit {
        Unit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind,
            root: ".".to_owned(),
            pinned_lockfile: false,
            watch: Vec::new(),
            pr_commands: Vec::new(),
            full_commands: Vec::new(),
            github_pr_commands: None,
            github_full_commands: None,
            velnor_pr_commands: None,
            velnor_full_commands: None,
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: None,
            services: Vec::new(),
            requires_trusted: false,
            workspace_check: false,
            reads_closed: false,
            platform: PlatformRequirement::portable(),
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        }
    }

    fn product(name: &str, inputs: &[&str]) -> NamedProduct {
        NamedProduct {
            name: name.to_owned(),
            task: Some(format!("build-{name}")),
            env: BTreeMap::new(),
            inputs: inputs.iter().map(ToString::to_string).collect(),
            inputs_unknown: Vec::new(),
        }
    }

    fn requires(producer: &str, product: &str) -> Prerequisite {
        Prerequisite {
            producer: producer.to_owned(),
            product: product.to_owned(),
            task: None,
            env: BTreeMap::new(),
        }
    }

    fn project_config(units: Vec<Unit>) -> crate::ProjectConfig {
        crate::ProjectConfig {
            repository: String::new(),
            workflow_revision: "test-revision".to_owned(),
            profile: "generic".to_owned(),
            analysis: AnalysisSummary {
                method: "test".to_owned(),
                detected: Vec::new(),
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: Vec::new(),
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            runners: RunnerMode::Both,
            automatic: RunnerMode::Both,
            github_runner: "ubuntu-latest".to_owned(),
            macos_runner: "macos-latest".to_owned(),
            velnor_labels: vec!["self-hosted".to_owned()],
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: false,
            renovate_reason: String::new(),
            renovate: None,
            docs_enabled: false,
            docs_reason: String::new(),
            docs: None,
            check_profiles: Vec::new(),
            maintenance: MaintenanceSpec::default(),
            units,
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: true,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            velnor_runner_group: None,
            velnor_trusted_label: None,
            velnor_trusted_runner_available: None,
            pull_request_on_velnor: false,
            default_dispatch_runner: "github".to_owned(),
            automatic_lanes: "github".to_owned(),
            velnor_rust_needs: VelnorRustNeeds::Parallel,
            velnor_concurrency_group: None,
            velnor_serial_stack_groups: false,
            static_files: Vec::new(),
            reviewers: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            github_cache: crate::config::CacheGithubSection::default(),
            velnor_host_cache: crate::config::CacheVelnorSection::default(),
        }
    }

    fn selected_change(
        config: &crate::ProjectConfig,
        change: crate::reuse::ChangedPath,
    ) -> BTreeSet<String> {
        let units = config
            .units
            .iter()
            .map(|unit| crate::reuse::WatchedUnit {
                id: unit.id.clone(),
                watch: unit.watch.clone(),
                depends_on: unit.depends_on.clone(),
                kind: unit.kind.id_prefix().to_owned(),
                commands: unit
                    .pr_commands
                    .iter()
                    .chain(unit.full_commands.iter())
                    .cloned()
                    .collect(),
                reads_closed: unit.reads_closed,
            })
            .collect::<Vec<_>>();
        must_ok(
            crate::reuse::select_affected(&units, &[change], &[]),
            "product input selection",
        )
        .required
    }

    fn selected(config: &crate::ProjectConfig, path: &str) -> BTreeSet<String> {
        selected_change(
            config,
            crate::reuse::ChangedPath {
                path: path.to_owned(),
                previous: None,
                status: crate::reuse::ChangeKind::Modified,
            },
        )
    }

    #[test]
    fn portable_units_match_every_executor() {
        let requirement = PlatformRequirement::swift_package();
        assert!(!requirement.requires_apple());
        for executor in executors_for(RunnerMode::Both) {
            assert!(
                executor.satisfies(&requirement),
                "{} must serve a portable unit",
                executor.id
            );
        }
    }

    #[test]
    fn apple_work_matches_only_the_apple_executor() {
        for requirement in [
            PlatformRequirement::apple_xcode(),
            PlatformRequirement::apple_xcframework(),
        ] {
            assert!(requirement.requires_apple());
            for executor in executors_for(RunnerMode::Both) {
                assert_eq!(
                    executor.satisfies(&requirement),
                    executor.id == "github-macos",
                    "{}",
                    executor.id
                );
            }
        }
        let xcframework = PlatformRequirement {
            os: Os::Any,
            arch: Arch::Any,
            capabilities: std::collections::BTreeSet::from([CAP_XCFRAMEWORK.to_owned()]),
        };
        assert!(xcframework.requires_apple());
        assert!(!lane_supports_platform(RunnerMode::Velnor, &xcframework));
        assert!(lane_supports_platform(RunnerMode::Github, &xcframework));
    }

    #[test]
    fn arch_constrains_placement() {
        let requirement = PlatformRequirement {
            os: Os::Any,
            arch: Arch::Aarch64,
            capabilities: std::collections::BTreeSet::new(),
        };
        let x86 = Executor {
            id: "x86-only",
            lane: RunnerMode::Github,
            os: Os::Any,
            arch: Arch::X86_64,
            capabilities: std::collections::BTreeSet::new(),
        };
        assert!(!x86.satisfies(&requirement));
        assert!(Executor::github_linux().satisfies(&requirement));
    }

    #[test]
    fn names_reject_shell_metacharacters() {
        assert!(valid_product_name("xcframework"));
        assert!(valid_product_name("sys-headers_v2"));
        assert!(!valid_product_name(""));
        assert!(!valid_product_name("XCFramework"));
        assert!(!valid_product_name("xcode build"));
        assert!(!valid_product_name("x;rm"));
        assert!(valid_task_name("build-xcframework"));
        assert!(valid_task_name("ffi:headers"));
        assert!(!valid_task_name("build xcframework"));
        assert!(!valid_task_name("build;test"));
        assert!(valid_env_name("XCFRAMEWORK_PATH"));
        assert!(!valid_env_name("2FAST"));
        assert!(!valid_env_name("HAS-DASH"));
        assert!(valid_env_value("-C link-arg=-fuse-ld=mold"));
        assert!(!valid_env_value("line\nbreak"));
    }

    #[test]
    fn ffi_detection_follows_crate_types() {
        assert!(is_ffi_crate_type(&["staticlib".to_owned()]));
        assert!(is_ffi_crate_type(&["rlib".to_owned(), "cdylib".to_owned()]));
        assert!(!is_ffi_crate_type(&["rlib".to_owned()]));
        assert!(!is_ffi_crate_type(&[]));
    }

    #[test]
    fn prepare_exports_task_inputs() {
        let env = std::collections::BTreeMap::from([
            ("B_KEY".to_owned(), "b".to_owned()),
            ("A_KEY".to_owned(), "a b".to_owned()),
        ]);
        assert_eq!(
            prepare_command("build-xcframework", &env),
            "A_KEY='a b' B_KEY='b' mise run build-xcframework"
        );
        assert_eq!(
            prepare_command("build-xcframework", &std::collections::BTreeMap::new()),
            "mise run build-xcframework"
        );
    }

    #[test]
    fn phased_rust_prepare_retains_validation_phases() {
        let mut consumer = unit("rust-app", UnitKind::Rust);
        consumer.pr_commands = vec![
            "cargo fmt --check".to_owned(),
            "cargo clippy --all-targets".to_owned(),
            "cargo test --all-targets".to_owned(),
        ];
        consumer.full_commands = consumer.pr_commands.clone();
        consumer.phases = vec![
            ValidationPhase::Fmt,
            ValidationPhase::Clippy,
            ValidationPhase::Test,
        ];
        consumer.check_commands = vec!["cargo check --all-targets".to_owned()];
        let prepare = "mise run build-xcframework".to_owned();

        must_ok(
            super::prepend_prepare_commands(&mut consumer, std::slice::from_ref(&prepare)),
            "Rust prepare insertion",
        );

        assert_eq!(
            consumer.pr_commands,
            vec![
                prepare.clone(),
                "cargo fmt --check".to_owned(),
                "cargo clippy --all-targets".to_owned(),
                "cargo test --all-targets".to_owned(),
            ]
        );
        assert_eq!(consumer.full_commands, consumer.pr_commands);
        assert_eq!(
            consumer.phases,
            vec![
                ValidationPhase::Precondition,
                ValidationPhase::Fmt,
                ValidationPhase::Clippy,
                ValidationPhase::Test,
            ]
        );
        assert_eq!(
            consumer.check_commands,
            vec!["cargo check --all-targets".to_owned()]
        );
    }

    #[test]
    fn prerequisite_prefers_its_own_task() {
        let product = NamedProduct {
            name: "xcframework".to_owned(),
            task: Some("build-xcframework".to_owned()),
            env: std::collections::BTreeMap::new(),
            inputs: Vec::new(),
            inputs_unknown: Vec::new(),
        };
        let plain = Prerequisite {
            producer: "rust-ffi".to_owned(),
            product: "xcframework".to_owned(),
            task: None,
            env: std::collections::BTreeMap::new(),
        };
        assert_eq!(plain.effective_task(&product), Some("build-xcframework"));
        let overridden = Prerequisite {
            task: Some("build-xcframework-device".to_owned()),
            ..plain
        };
        assert_eq!(
            overridden.effective_task(&product),
            Some("build-xcframework-device")
        );
    }

    #[test]
    fn product_inputs_select_schema_one_producer_and_dependents() {
        for input in [
            "libs/bridge-ffi/boltffi.toml",
            "native/Sources/BridgeCore/FfiBoltFFI.swift",
            "libs/sibling/**/*.rs",
        ] {
            assert!(
                valid_product_input(input),
                "fixture input is valid: {input}"
            );
        }
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        producer.products = vec![product(
            "xcframework",
            &[
                "libs/bridge-ffi/boltffi.toml",
                "native/Sources/BridgeCore/FfiBoltFFI.swift",
                "libs/sibling/**/*.rs",
            ],
        )];
        let mut consumer = unit("swift-app", UnitKind::Swift);
        consumer.prerequisites = vec![requires("rust-ffi", "xcframework")];
        let mut unrelated = unit("unrelated", UnitKind::Rust);
        unrelated.watch = vec!["tools/unrelated/**".to_owned()];
        let mut config = project_config(vec![producer, consumer, unrelated]);
        must_ok(resolve(&mut config), "schema-one product graph resolves");
        let expected = BTreeSet::from(["rust-ffi".to_owned(), "swift-app".to_owned()]);
        for input in [
            "libs/bridge-ffi/boltffi.toml",
            "native/Sources/BridgeCore/FfiBoltFFI.swift",
            "libs/sibling/src/lib.rs",
        ] {
            assert_eq!(selected(&config, input), expected, "selection for {input}");
        }
        assert_eq!(
            selected(&config, "tools/unrelated/src/lib.rs"),
            BTreeSet::from(["unrelated".to_owned()]),
            "an unrelated input selects only its owner"
        );
        assert_eq!(
            selected_change(
                &config,
                crate::reuse::ChangedPath {
                    path: "libs/bridge-ffi/boltffi.toml.moved".to_owned(),
                    previous: Some("libs/bridge-ffi/boltffi.toml".to_owned()),
                    status: crate::reuse::ChangeKind::Renamed,
                },
            ),
            expected,
            "renaming a product input keeps old ownership"
        );
        assert_eq!(
            selected_change(
                &config,
                crate::reuse::ChangedPath {
                    path: "libs/bridge-ffi/boltffi.toml".to_owned(),
                    previous: None,
                    status: crate::reuse::ChangeKind::Deleted,
                },
            ),
            expected,
            "deleting a product input selects its producer and dependent"
        );
        assert_eq!(
            config.units[0].watch,
            vec![
                "libs/bridge-ffi/boltffi.toml".to_owned(),
                "libs/sibling/**/*.rs".to_owned(),
                "native/Sources/BridgeCore/FfiBoltFFI.swift".to_owned(),
            ]
        );
    }

    #[test]
    fn unknown_schema_one_product_inputs_fail_closed_to_whole_tree() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["libs/bridge-ffi/boltffi.toml"]);
        ffi.inputs_unknown = vec!["build script reads an untracked path".to_owned()];
        producer.products = vec![ffi];
        let mut consumer = unit("swift-app", UnitKind::Swift);
        consumer.prerequisites = vec![requires("rust-ffi", "xcframework")];
        let mut unrelated = unit("unrelated", UnitKind::Rust);
        unrelated.watch = vec!["tools/unrelated/**".to_owned()];
        let mut config = project_config(vec![producer, consumer, unrelated]);
        must_ok(resolve(&mut config), "unknown schema-one graph resolves");
        assert!(config.units[0].watch.contains(&"**".to_owned()));
        assert_eq!(
            selected(&config, "unmodeled/input.txt"),
            BTreeSet::from(["rust-ffi".to_owned(), "swift-app".to_owned()]),
            "unknown closure cannot silently narrow selection"
        );
    }

    #[test]
    fn agreed_env_fails_closed_on_conflict() {
        let mut left = unit("left", UnitKind::Swift);
        left.env.insert("KEY".to_owned(), "one".to_owned());
        let mut right = unit("right", UnitKind::Swift);
        right.env.insert("KEY".to_owned(), "two".to_owned());
        let error = must_err(
            agreed_env(&[&left, &right], UnitKind::Swift),
            "conflicting env fails closed",
        );
        assert!(
            error.to_string().contains("`KEY`"),
            "unexpected error: {error}"
        );
        right.env.insert("KEY".to_owned(), "one".to_owned());
        let agreed = must_ok(
            agreed_env(&[&left, &right], UnitKind::Swift),
            "agreeing env merges",
        );
        assert_eq!(agreed.get("KEY").map(String::as_str), Some("one"));
    }

    #[test]
    fn requirement_description_names_only_constraints() {
        assert_eq!(
            describe_requirement(&PlatformRequirement::portable()),
            "a portable unit"
        );
        assert_eq!(
            describe_requirement(&PlatformRequirement::apple_xcode()),
            "macOS + capability `xcode`"
        );
        assert!(describe_requirement(&PlatformRequirement {
            os: Os::Linux,
            arch: Arch::Aarch64,
            capabilities: std::collections::BTreeSet::from([CAP_XCODE.to_owned()]),
        })
        .contains("aarch64"));
    }
}

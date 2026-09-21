//! Unit execution needs, prerequisite products, and the agreed job environment.
//!
//! Providers are deployment detail: a unit never names a runner label. It
//! declares a typed platform, a trust tier, and a capability set, and
//! generation maps each need onto the eligible providers. A need no enabled
//! provider can serve is a generation error that names the unit, the need,
//! and the remedy, never a silently skipped job.
//!
//! The same module carries the prerequisite contract: a unit produces named
//! products through named tasks, and a consumer declares which producer
//! products it needs. Generation compiles each edge into the selection graph
//! (`depends_on`, so producer changes select the consumer transitively) and
//! into prepare commands (so the consumer rebuilds the product locally before
//! its own checks), with environment flowing from task inputs to job outputs.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::s2::{GeneratorError, ProjectConfig, Unit, UnitKind};

/// A named build product one unit produces for others: an `XCFramework`
/// bundle, a generated header set, a packed archive. `task` is the repository
/// task that rebuilds it (run through the task runner, never a shell string),
/// `env` carries the task's outputs — the paths and flags consumers need
/// once the product exists — and `outputs` declares the repo-relative artifact
/// paths the rebuild materializes, so later stages can validate, cache, and
/// transport the product instead of re-running blind. `inputs` declares the
/// product's semantic input closure as repo-relative paths and `hashFiles`
/// globs: manifests, sources, build scripts, and configuration whose bytes
/// feed the rebuild. `inputs_unknown` names closure gaps the scanner could
/// not resolve; an empty list means the closure is complete, and exact reuse
/// must stay disabled while any gap remains. `inputs_digest` is the
/// generator-computed SHA-256 over the expanded closure bytes (the
/// prepared-tool inputs digest with closure sources); `Some` only on a
/// complete closure, `None` otherwise. `output_files` lists the expected
/// structural files under the claimed `outputs` roots; per-file digests
/// are build-time facts the producer manifest records, not plan facts.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct NamedProduct {
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) output_files: Vec<String>,
    /// Scanner-derived Swift bindings directory for a native product, empty
    /// when the product has no binding contract.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) bindings_dir: String,
    /// Scanner-derived expected binding file under `bindings_dir`, empty
    /// when the product has no binding contract.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) bindings_file: String,
    /// Scanner-derived Apple deployment target, empty when the product has
    /// no Apple contract.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) deployment_target: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) inputs_unknown: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) inputs_digest: Option<String>,
    /// Shell commands that rebuild the product from a clean checkout, in
    /// order. The scanner records the producer recipe here so a consumer
    /// whose producer did not run in this workflow can still materialize
    /// the product locally; the transport guard skips the rebuild once the
    /// verified artifact is installed. Empty when the product has no
    /// local rebuild (a task-carrying product rebuilds through its task).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) rebuild: Vec<String>,
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

/// A declared product output: a repo-relative path in normal form. Absolute
/// paths, escapes, `.` segments, backslashes, and empty segments are all
/// refused, so two equal strings always name the same file and output
/// identity needs no further normalization.
pub(crate) fn valid_product_output(value: &str) -> bool {
    if value.is_empty() || value.len() > 500 || value.starts_with('/') {
        return false;
    }
    if value.bytes().any(|byte| byte == b'\\') || value.chars().any(char::is_control) {
        return false;
    }
    !value
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

/// A declared product input: a repo-relative path in normal form, with the
/// `hashFiles` glob characters `*?[]` allowed inside segments. Absolute
/// paths, escapes, `.` segments, backslashes, exclusions, and empty segments
/// are all refused, so identity hashing never silently widens or narrows the
/// closure.
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
        command.push_str(&crate::s2::shell_quote(value));
        command.push(' ');
    }
    command.push_str("mise run ");
    command.push_str(task);
    command
}

/// The ready-marker variable one transported edge exports: set only by the
/// consumer's verify step after the artifact's manifest and digests check
/// out. Sanitized to the env-name alphabet so any producer/product pair
/// maps to a valid variable.
pub(crate) fn transport_marker(producer: &str, product: &str) -> String {
    fn side(value: &str) -> String {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect()
    }
    format!("VELNOR_PRODUCT_{}__{}_READY", side(producer), side(product))
}

/// One prepare command that rebuilds a task-less product from `commands`
/// unless the transport already installed it: the marker is set only in a
/// consumer job whose verify step accepted the artifact, so local runs and
/// transport misses always rebuild. The compound runs under the runtime's
/// `bash -euo pipefail -c`; a failing rebuild command aborts the compound.
pub(crate) fn guarded_rebuild_command(marker: &str, commands: &[String]) -> String {
    let mut command = format!("if [[ -z \"${{{marker}:-}}\" ]]; then\n");
    let mut first = true;
    for rebuild in commands {
        if first {
            first = false;
        } else {
            command.push_str(" &&\n");
        }
        command.push_str("  ");
        command.push_str(rebuild);
    }
    command.push_str("\nfi");
    command
}

/// Resolve the platform surface over `config`: validate every prerequisite
/// edge and object-transport toggle, compile edges into the selection graph
/// and prepare commands, merge product outputs into consumer env, and reject
/// any placement no enabled lane can serve.
///
/// # Errors
/// Returns a usage error for an edge that names an unknown producer or a
/// product the producer does not declare, for an invalid, duplicated, or
/// conflicting product output, for a self-edge or dependency cycle, for an
/// object-transport toggle on a unit that cannot use it, and for a unit no
/// enabled lane can execute.
pub(crate) fn resolve(config: &mut ProjectConfig) -> Result<(), GeneratorError> {
    validate_mbx_toggles(config)?;
    validate_product_graph(config)?;
    materialize_prerequisites(config)?;
    Ok(())
}

/// Validate one product's expected output files: normal-form, duplicate-free,
/// and strictly under a claimed output root. A file outside every root
/// names bytes the product never claimed to produce.
fn validate_output_files(unit: &Unit, product: &NamedProduct) -> Result<(), GeneratorError> {
    let mut seen_files = BTreeSet::new();
    for file in &product.output_files {
        if !valid_product_output(file) {
            return Err(GeneratorError::usage(format!(
                "unit `{}` declares product `{}` with output file `{file}`, which is not a repo-relative path in normal form; use forward slashes without leading `/`, `.`, `..`, or empty segments",
                unit.id, product.name,
            )));
        }
        if !seen_files.insert(file.as_str()) {
            return Err(GeneratorError::usage(format!(
                "unit `{}` declares product `{}` output file `{file}` twice; one entry per path",
                unit.id, product.name,
            )));
        }
        let under_root = product.outputs.iter().any(|root| {
            file.len() > root.len()
                && file.starts_with(root.as_str())
                && file.as_bytes().get(root.len()) == Some(&b'/')
        });
        if !under_root {
            return Err(GeneratorError::usage(format!(
                "unit `{}` declares product `{}` with output file `{file}` outside its claimed outputs; every expected file must sit under a claimed output root",
                unit.id, product.name,
            )));
        }
    }
    Ok(())
}

/// Validate one product's binding contract: the directory and file are
/// normal-form repo-relative paths, the file sits strictly under the
/// directory, and the deployment target is printable text. All three are
/// empty together when the product has no binding contract; a lone file or
/// target without a directory names a contract the scanner never derived.
fn validate_bindings(unit: &Unit, product: &NamedProduct) -> Result<(), GeneratorError> {
    let dir = product.bindings_dir.as_str();
    let file = product.bindings_file.as_str();
    let target = product.deployment_target.as_str();
    if dir.is_empty() {
        if file.is_empty() && target.is_empty() {
            return Ok(());
        }
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares product `{}` with a binding file or deployment target but no bindings directory; binding facts arrive together from the scanner",
            unit.id, product.name,
        )));
    }
    if !valid_product_output(dir) {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares product `{}` with bindings directory `{dir}`, which is not a repo-relative path in normal form; use forward slashes without leading `/`, `.`, `..`, or empty segments",
            unit.id, product.name,
        )));
    }
    if !valid_product_output(file) {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares product `{}` with binding file `{file}`, which is not a repo-relative path in normal form; use forward slashes without leading `/`, `.`, `..`, or empty segments",
            unit.id, product.name,
        )));
    }
    let under_dir = file.len() > dir.len()
        && file.starts_with(dir)
        && file.as_bytes().get(dir.len()) == Some(&b'/');
    if !under_dir {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares product `{}` with binding file `{file}` outside its bindings directory `{dir}`; the expected file must sit under the directory the adapter writes",
            unit.id, product.name,
        )));
    }
    if !valid_env_value(target) || target.is_empty() {
        return Err(GeneratorError::usage(format!(
            "unit `{}` declares product `{}` with bindings directory `{dir}` but no printable deployment target; binding facts arrive together from the scanner",
            unit.id, product.name,
        )));
    }
    Ok(())
}

/// Validate one product's local rebuild: every command is non-empty
/// printable shell without NUL bytes, so the guarded compound the consumer
/// prepends cannot silently collapse or inject a second command.
fn validate_rebuild(unit: &Unit, product: &NamedProduct) -> Result<(), GeneratorError> {
    for command in &product.rebuild {
        if command.is_empty()
            || command.bytes().any(|byte| byte == 0)
            || command
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
        {
            return Err(GeneratorError::usage(format!(
                "unit `{}` declares product `{}` with an empty or non-printable rebuild command; rebuild commands are shell lines without NUL or control characters",
                unit.id, product.name,
            )));
        }
    }
    Ok(())
}

/// Validate the product graph before compilation: every declared output is a
/// normal-form repo-relative path claimed by exactly one product, every
/// expected output file is a duplicate-free normal-form path under a claimed
/// root, every binding contract carries a normal-form directory with its
/// expected file beneath it plus a printable deployment target, every
/// declared input is a normal-form path or glob without duplicates, closure
/// gaps stay printable diagnostics, a claimed inputs digest is a hex SHA-256
/// on a gap-free closure, no unit requires its own product, and the
/// consumer-to-producer edges are acyclic. Unknown producers and products
/// stay `materialize_prerequisites` errors, which already name the known
/// units and offered products.
fn validate_product_graph(config: &ProjectConfig) -> Result<(), GeneratorError> {
    let mut owners: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for unit in &config.units {
        for product in &unit.products {
            let mut seen_inputs = BTreeSet::new();
            for input in &product.inputs {
                if !valid_product_input(input) {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` with input `{input}`, which is not a repo-relative path or glob in normal form; use forward slashes without leading `/`, `.`, `..`, or empty segments",
                        unit.id, product.name,
                    )));
                }
                if !seen_inputs.insert(input.as_str()) {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` input `{input}` twice; one entry per pattern",
                        unit.id, product.name,
                    )));
                }
            }
            for gap in &product.inputs_unknown {
                if !valid_env_value(gap) {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` with an unprintable closure gap; keep gap entries to printable text",
                        unit.id, product.name,
                    )));
                }
            }
            if let Some(digest) = product.inputs_digest.as_deref() {
                if !crate::s2::primitives::prepared_tools::is_digest(digest) {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` with inputs digest `{digest}`, which is not a lowercase hex SHA-256; digests are generator-computed and cannot be declared by hand",
                        unit.id, product.name,
                    )));
                }
                if !product.inputs_unknown.is_empty() {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` with both an inputs digest and closure gaps; a digest over an incomplete closure would be a false identity",
                        unit.id, product.name,
                    )));
                }
            }
            for output in &product.outputs {
                if !valid_product_output(output) {
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` declares product `{}` with output `{output}`, which is not a repo-relative path in normal form; use forward slashes without leading `/`, `.`, `..`, or empty segments",
                        unit.id, product.name,
                    )));
                }
                if let Some((owner_unit, owner_product)) =
                    owners.insert(output.as_str(), (unit.id.as_str(), product.name.as_str()))
                {
                    if owner_unit == unit.id && owner_product == product.name {
                        return Err(GeneratorError::usage(format!(
                            "unit `{}` declares product `{}` output `{output}` twice; one entry per path",
                            unit.id, product.name,
                        )));
                    }
                    return Err(GeneratorError::usage(format!(
                        "unit `{}` product `{}` claims output `{output}`, already claimed by unit `{owner_unit}` product `{owner_product}`; one producer per path",
                        unit.id, product.name,
                    )));
                }
            }
            validate_output_files(unit, product)?;
            validate_bindings(unit, product)?;
            validate_rebuild(unit, product)?;
        }
        for prerequisite in &unit.prerequisites {
            if prerequisite.producer == unit.id {
                return Err(GeneratorError::usage(format!(
                    "unit `{}` requires its own product `{}`; a unit cannot consume what it produces",
                    unit.id, prerequisite.product,
                )));
            }
        }
    }
    if let Some(cycle) = find_product_cycle(config) {
        return Err(GeneratorError::usage(format!(
            "prerequisite edges contain a cycle: {}; break the cycle so producers build before consumers",
            cycle.join(" -> "),
        )));
    }
    Ok(())
}

/// The first consumer-to-producer cycle, as a closed id path, if one exists.
/// Edges that name unknown units contribute no outgoing edges here; the
/// unknown-producer error reports them with the known units instead.
fn find_product_cycle(config: &ProjectConfig) -> Option<Vec<String>> {
    fn visit(
        config: &ProjectConfig,
        id: &str,
        stack: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        if done.contains(id) {
            return None;
        }
        if let Some(start) = stack.iter().position(|each| each == id) {
            let mut cycle = stack[start..].to_vec();
            cycle.push(id.to_owned());
            return Some(cycle);
        }
        stack.push(id.to_owned());
        if let Some(unit) = config.units.iter().find(|unit| unit.id == id) {
            for prerequisite in &unit.prerequisites {
                if let Some(cycle) = visit(config, &prerequisite.producer, stack, done) {
                    return Some(cycle);
                }
            }
        }
        stack.pop();
        done.insert(id.to_owned());
        None
    }

    let mut done = BTreeSet::new();
    for unit in &config.units {
        let mut stack = Vec::new();
        if let Some(cycle) = visit(config, &unit.id, &mut stack, &mut done) {
            return Some(cycle);
        }
    }
    None
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
/// the consumer through the existing transitive closure), prepare commands
/// (so the consumer rebuilds each product before its own checks on every
/// provider), and consumer env (so product outputs reach the checks).
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
            } else if !product.rebuild.is_empty() {
                // A task-less product with a recorded recipe: the consumer
                // rebuilds locally unless the verified artifact already
                // landed — the transport-miss and local-run fallback.
                let marker = transport_marker(&prerequisite.producer, &prerequisite.product);
                prepared
                    .entry(unit.id.clone())
                    .or_default()
                    .push(guarded_rebuild_command(&marker, &product.rebuild));
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
            prepend_prepare_commands(unit, &commands);
        }
    }
    Ok(())
}

/// Prepend prepare commands ahead of every command vector the unit runs, so
/// the product rebuilds before the unit's own checks on every provider and in
/// local runs, which read the same serialized vectors.
fn prepend_prepare_commands(unit: &mut Unit, commands: &[String]) {
    let mut pr_commands = commands.to_vec();
    pr_commands.extend(unit.pr_commands.iter().cloned());
    unit.pr_commands = pr_commands;
    let mut full_commands = commands.to_vec();
    full_commands.extend(unit.full_commands.iter().cloned());
    unit.full_commands = full_commands;
    // Prepare commands carry no phase tags and shift every position: the unit
    // keeps the product rebuild ahead of its checks and verifies through the
    // single legacy step.
    unit.clear_phases();
    unit.watch.sort();
    unit.watch.dedup();
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
        agreed_env, guarded_rebuild_command, is_ffi_crate_type, prepare_command, resolve,
        transport_marker, valid_env_name, valid_env_value, valid_product_input, valid_product_name,
        valid_product_output, valid_task_name, NamedProduct, Prerequisite,
    };
    use crate::s2::provider::{Capabilities, Platform, ProviderId, TrustReq};
    use crate::s2::scan::default_selectors;
    use crate::s2::{AnalysisSummary, MaintenanceSpec, ProjectConfig, RustNeeds, Unit, UnitKind};
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

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_find<'a>(units: &'a [Unit], id: &str) -> &'a Unit {
        match units.iter().find(|unit| unit.id == id) {
            Some(unit) => unit,
            None => panic!("unit `{id}` survives resolve"),
        }
    }

    fn unit(id: &str, kind: UnitKind) -> Unit {
        Unit {
            xcode: None,
            id: id.to_owned(),
            label: id.to_owned(),
            kind,
            root: ".".to_owned(),
            pinned_lockfile: false,
            watch: Vec::new(),
            pr_commands: Vec::new(),
            full_commands: Vec::new(),
            phases: Vec::new(),
            check_commands: Vec::new(),
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: None,
            services: Vec::new(),
            trust: TrustReq::UntrustedOk,
            platform: Platform::LinuxX64,
            capabilities: Capabilities::default(),
            workspace_check: false,
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        }
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
    fn prerequisite_prefers_its_own_task() {
        let product = NamedProduct {
            name: "xcframework".to_owned(),
            task: Some("build-xcframework".to_owned()),
            env: std::collections::BTreeMap::new(),
            outputs: Vec::new(),
            output_files: Vec::new(),
            bindings_dir: String::new(),
            bindings_file: String::new(),
            deployment_target: String::new(),
            inputs: Vec::new(),
            inputs_unknown: Vec::new(),
            inputs_digest: None,
            rebuild: Vec::new(),
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

    fn product(name: &str, outputs: &[&str]) -> NamedProduct {
        NamedProduct {
            inputs: Vec::new(),
            inputs_unknown: Vec::new(),
            inputs_digest: None,
            name: name.to_owned(),
            task: Some(format!("build-{name}")),
            env: BTreeMap::new(),
            outputs: outputs.iter().map(ToString::to_string).collect(),
            output_files: Vec::new(),
            bindings_dir: String::new(),
            bindings_file: String::new(),
            deployment_target: String::new(),
            rebuild: Vec::new(),
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

    fn project_config(units: Vec<Unit>) -> ProjectConfig {
        ProjectConfig {
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
            providers: ProviderId::ALL.into_iter().collect(),
            automatic_providers: ProviderId::ALL.into_iter().collect(),
            selectors: default_selectors(),
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
            rust_needs: RustNeeds::Parallel,
            concurrency_group: None,
            serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            github_cache: crate::s2::config::CacheGithubSection::default(),
            velnor_host_cache: crate::s2::config::CacheVelnorSection::default(),
        }
    }

    fn clean_graph() -> Vec<Unit> {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        producer.products = vec![product(
            "xcframework",
            &["native/out/BridgeFFI.xcframework", "native/out/BridgeFFI.h"],
        )];
        let mut consumer = unit("swift-app", UnitKind::Swift);
        consumer.prerequisites = vec![requires("rust-ffi", "xcframework")];
        vec![producer, consumer]
    }

    #[test]
    fn product_output_accepts_normal_repo_relative_paths() {
        for accepted in [
            "out/lib.a",
            "native/out/BridgeFFI.xcframework",
            "a",
            "a/b/c",
            "with-dash/under_score/file.tar.gz",
        ] {
            assert!(valid_product_output(accepted), "should accept `{accepted}`");
        }
    }

    #[test]
    fn product_output_rejects_non_normal_paths() {
        let long = "x".repeat(501);
        let rejected = vec![
            "",
            "/absolute/path",
            "trailing/slash/",
            "double//slash",
            "dot/./segment",
            "up/../segment",
            "..",
            ".",
            "../escape",
            "back\\slash",
            "tab\tchar",
            long.as_str(),
        ];
        for value in rejected {
            assert!(!valid_product_output(value), "should reject `{value}`");
        }
    }

    #[test]
    fn product_inputs_accept_globs_and_reject_escapes() {
        for accepted in [
            "libs/ffi/Cargo.toml",
            "libs/ffi/**/*.rs",
            "libs/ffi/src/**",
            "**/*.rs",
            ".cargo/**",
            "Cargo.lock",
        ] {
            assert!(valid_product_input(accepted), "should accept `{accepted}`");
        }
        let long = "x".repeat(501);
        let rejected = vec![
            "",
            "/absolute/path",
            "trailing/slash/",
            "double//slash",
            "dot/./segment",
            "up/../segment",
            "..",
            "../escape",
            "back\\slash",
            "!excluded/path",
            "libs/ffi/!negated.rs",
            long.as_str(),
        ];
        for value in rejected {
            assert!(!valid_product_input(value), "should reject `{value}`");
        }
    }

    #[test]
    fn resolve_rejects_duplicate_input_within_one_product() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.inputs = vec!["libs/ffi/**/*.rs".to_owned(), "libs/ffi/**/*.rs".to_owned()];
        producer.products = vec![ffi];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "duplicate input fails closed",
        );
        assert!(
            error.to_string().contains("twice"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_unprintable_closure_gap() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.inputs_unknown = vec!["gap with\nnewline".to_owned()];
        producer.products = vec![ffi];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "unprintable gap fails closed",
        );
        assert!(
            error.to_string().contains("unprintable closure gap"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_malformed_inputs_digest() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.inputs_digest = Some("not-a-digest".to_owned());
        producer.products = vec![ffi];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "malformed digest fails closed",
        );
        assert!(
            error.to_string().contains("not-a-digest"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_digest_with_closure_gaps() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.inputs_unknown = vec!["build script reads the network".to_owned()];
        ffi.inputs_digest = Some("e".repeat(64));
        producer.products = vec![ffi];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "digest over gaps fails closed",
        );
        assert!(
            error.to_string().contains("false identity"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_accepts_digest_on_gap_free_product() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.inputs = vec!["libs/ffi/**/*.rs".to_owned()];
        ffi.inputs_digest = Some("e".repeat(64));
        producer.products = vec![ffi];
        must_ok(
            resolve(&mut project_config(vec![producer])),
            "digest on complete closure resolves",
        );
    }

    #[test]
    fn resolve_rejects_output_file_outside_roots() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.output_files = vec!["elsewhere/Info.plist".to_owned()];
        producer.products = vec![ffi];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "stray file fails closed",
        );
        assert!(
            error.to_string().contains("outside its claimed outputs"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_duplicate_output_file() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.output_files = vec![
            "native/out/lib.xcframework/Info.plist".to_owned(),
            "native/out/lib.xcframework/Info.plist".to_owned(),
        ];
        producer.products = vec![ffi];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "duplicate file fails closed",
        );
        assert!(
            error.to_string().contains("twice"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_accepts_output_files_under_roots() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.output_files = vec![
            "native/out/lib.xcframework/Info.plist".to_owned(),
            "native/out/lib.xcframework/macos-arm64/Headers/module.modulemap".to_owned(),
        ];
        producer.products = vec![ffi];
        must_ok(
            resolve(&mut project_config(vec![producer])),
            "files under roots resolve",
        );
    }

    #[test]
    fn resolve_accepts_binding_contract_together() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.bindings_dir = "app/Sources/Bindings/BoltFFI".to_owned();
        ffi.bindings_file = "app/Sources/Bindings/BoltFFI/BridgeCoreFfiBoltFFI.swift".to_owned();
        ffi.deployment_target = "15.0".to_owned();
        producer.products = vec![ffi];
        must_ok(
            resolve(&mut project_config(vec![producer])),
            "binding facts together resolve",
        );
    }

    #[test]
    fn resolve_rejects_binding_file_outside_dir() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.bindings_dir = "app/Sources/Bindings".to_owned();
        ffi.bindings_file = "elsewhere/Bindings.swift".to_owned();
        ffi.deployment_target = "15.0".to_owned();
        producer.products = vec![ffi];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "stray binding file fails closed",
        );
        assert!(
            error.to_string().contains("outside its bindings directory"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_lone_binding_file() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.bindings_file = "app/Sources/Bindings.swift".to_owned();
        producer.products = vec![ffi];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "lone binding file fails closed",
        );
        assert!(
            error.to_string().contains("no bindings directory"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_accepts_clean_graph_and_materializes_edges() {
        let mut config = project_config(clean_graph());
        must_ok(resolve(&mut config), "clean product graph resolves");
        let consumer = must_find(&config.units, "swift-app");
        assert_eq!(consumer.depends_on, vec!["rust-ffi".to_owned()]);
        assert!(
            consumer
                .pr_commands
                .iter()
                .any(|command| command.contains("build-xcframework")),
            "consumer rebuilds the product first: {:?}",
            consumer.pr_commands
        );
    }

    #[test]
    fn resolve_is_deterministic_for_identical_graphs() {
        let mut left = project_config(clean_graph());
        let mut right = project_config(clean_graph());
        must_ok(resolve(&mut left), "left graph resolves");
        must_ok(resolve(&mut right), "right graph resolves");
        assert_eq!(left.units, right.units);
    }

    #[test]
    fn resolve_rejects_duplicate_output_within_one_product() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        producer.products = vec![product(
            "xcframework",
            &["native/out/lib.a", "native/out/lib.a"],
        )];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "duplicate output fails closed",
        );
        assert!(
            error.to_string().contains("twice"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_output_claimed_by_two_products() {
        let mut first = unit("rust-ffi", UnitKind::Rust);
        first.products = vec![product("xcframework", &["native/out/shared.a"])];
        let mut second = unit("other-ffi", UnitKind::Rust);
        second.products = vec![product("staticlib", &["native/out/shared.a"])];
        let error = must_err(
            resolve(&mut project_config(vec![first, second])),
            "conflicting output fails closed",
        );
        assert!(
            error.to_string().contains("already claimed"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_non_normal_output() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        producer.products = vec![product("xcframework", &["../escape/lib.a"])];
        let error = must_err(
            resolve(&mut project_config(vec![producer])),
            "escaping output fails closed",
        );
        assert!(
            error.to_string().contains("normal form"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_self_edge() {
        let mut consumer = unit("swift-app", UnitKind::Swift);
        consumer.products = vec![product("app", &["out/app.zip"])];
        consumer.prerequisites = vec![requires("swift-app", "app")];
        let error = must_err(
            resolve(&mut project_config(vec![consumer])),
            "self edge fails closed",
        );
        assert!(
            error
                .to_string()
                .contains("cannot consume what it produces"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_dependency_cycle_with_closed_path() {
        let mut left = unit("unit-a", UnitKind::Swift);
        left.products = vec![product("product-a", &["out/a.zip"])];
        left.prerequisites = vec![requires("unit-b", "product-b")];
        let mut right = unit("unit-b", UnitKind::Swift);
        right.products = vec![product("product-b", &["out/b.zip"])];
        right.prerequisites = vec![requires("unit-a", "product-a")];
        let error = must_err(
            resolve(&mut project_config(vec![left, right])),
            "dependency cycle fails closed",
        );
        let message = error.to_string();
        assert!(message.contains("cycle"), "unexpected error: {message}");
        assert!(
            message.contains("unit-a -> unit-b -> unit-a"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn resolve_rejects_unknown_producer_naming_known_units() {
        let mut consumer = unit("swift-app", UnitKind::Swift);
        consumer.prerequisites = vec![requires("ghost", "xcframework")];
        let error = must_err(
            resolve(&mut project_config(vec![consumer])),
            "unknown producer fails closed",
        );
        assert!(
            error.to_string().contains("known units: swift-app"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_rejects_unknown_product_naming_offered_products() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        producer.products = vec![product("xcframework", &["native/out/lib.a"])];
        let mut consumer = unit("swift-app", UnitKind::Swift);
        consumer.prerequisites = vec![requires("rust-ffi", "ghost")];
        let error = must_err(
            resolve(&mut project_config(vec![producer, consumer])),
            "unknown product fails closed",
        );
        assert!(
            error.to_string().contains("it declares: xcframework"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn transport_marker_is_a_valid_env_name() {
        assert_eq!(
            transport_marker("rust-ffi", "xcframework"),
            "VELNOR_PRODUCT_RUST_FFI__XCFRAMEWORK_READY"
        );
        let marker = transport_marker("rust-ffi", "sys-headers_v2");
        assert!(valid_env_name(&marker), "{marker}");
    }

    #[test]
    fn resolve_prepends_guarded_rebuild_for_taskless_products() {
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.task = None;
        ffi.rebuild = vec!["make pack".to_owned(), "make verify".to_owned()];
        producer.products = vec![ffi];
        let mut consumer = unit("swift-app", UnitKind::Swift);
        consumer.pr_commands = vec!["swift test".to_owned()];
        consumer.prerequisites = vec![requires("rust-ffi", "xcframework")];
        let mut config = project_config(vec![producer, consumer]);
        must_ok(resolve(&mut config), "task-less rebuild resolves");
        let prepared = &config.units[1].pr_commands;
        assert_eq!(2, prepared.len(), "{prepared:?}");
        assert!(
            prepared[0].contains("VELNOR_PRODUCT_RUST_FFI__XCFRAMEWORK_READY"),
            "{}",
            prepared[0]
        );
        assert!(prepared[0].contains("make pack"), "{}", prepared[0]);
        assert!(prepared[0].contains("make verify"), "{}", prepared[0]);
        assert_eq!("swift test", prepared[1]);
    }

    #[test]
    fn resolve_keeps_task_prepare_ahead_of_guarded_rebuild() {
        // A product with a task rebuilds through mise; its recorded recipe,
        // if any, stays dormant.
        let mut producer = unit("rust-ffi", UnitKind::Rust);
        let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
        ffi.rebuild = vec!["make pack".to_owned()];
        producer.products = vec![ffi];
        let mut consumer = unit("swift-app", UnitKind::Swift);
        consumer.prerequisites = vec![requires("rust-ffi", "xcframework")];
        let mut config = project_config(vec![producer, consumer]);
        must_ok(resolve(&mut config), "task product resolves");
        let prepared = &config.units[1].pr_commands;
        assert_eq!(1, prepared.len(), "{prepared:?}");
        assert!(
            prepared[0].starts_with("mise run build-xcframework"),
            "{}",
            prepared[0]
        );
    }

    #[test]
    fn resolve_rejects_empty_and_nonprintable_rebuild() {
        for rebuild in [vec![String::new()], vec!["make pack\0".to_owned()]] {
            let mut producer = unit("rust-ffi", UnitKind::Rust);
            let mut ffi = product("xcframework", &["native/out/lib.xcframework"]);
            ffi.task = None;
            ffi.rebuild = rebuild;
            producer.products = vec![ffi];
            let error = must_err(
                resolve(&mut project_config(vec![producer])),
                "bad rebuild fails closed",
            );
            assert!(
                error.to_string().contains("rebuild command"),
                "unexpected error: {error}"
            );
        }
    }

    /// Run one prepare command exactly as the runtime would: `bash -euo
    /// pipefail -c` with an optional ready marker in the environment.
    fn run_prepare(command: &str, dir: &std::path::Path, marker: Option<(&str, &str)>) -> bool {
        let mut spawned = std::process::Command::new("bash");
        spawned
            .args(["-euo", "pipefail", "-c", command])
            .current_dir(dir);
        if let Some((name, value)) = marker {
            spawned.env(name, value);
        }
        must_ok(spawned.output(), "run prepare command")
            .status
            .success()
    }

    fn prepare_scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-prepare-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        must_ok(std::fs::create_dir_all(&root), "create scratch directory");
        root
    }

    #[test]
    fn guarded_rebuild_runs_without_marker_and_skips_with_it() {
        let marker = transport_marker("rust-ffi", "xcframework");
        let command = guarded_rebuild_command(
            &marker,
            &["touch rebuilt".to_owned(), "touch verified".to_owned()],
        );
        let cold = prepare_scratch("cold");
        assert!(run_prepare(&command, &cold, None), "cold rebuild runs");
        assert!(cold.join("rebuilt").exists(), "first command ran");
        assert!(cold.join("verified").exists(), "chained command ran");

        let warm = prepare_scratch("warm");
        assert!(
            run_prepare(&command, &warm, Some((&marker, "1"))),
            "marked rebuild skips"
        );
        assert!(!warm.join("rebuilt").exists(), "nothing runs when marked");
    }

    #[test]
    fn guarded_rebuild_propagates_failure() {
        let marker = transport_marker("rust-ffi", "xcframework");
        let command =
            guarded_rebuild_command(&marker, &["touch first".to_owned(), "exit 3".to_owned()]);
        let dir = prepare_scratch("failing");
        assert!(!run_prepare(&command, &dir, None), "failure propagates");
        assert!(dir.join("first").exists(), "earlier commands ran");
    }
}

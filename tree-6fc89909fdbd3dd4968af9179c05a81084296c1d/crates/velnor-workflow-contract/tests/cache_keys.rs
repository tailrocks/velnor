//! D9 of `plans/2026-09-16-ci-workflow-and-cache-plan.md`: parameterizing the
//! kind reusables by `workflow_call` inputs must not change one cache key.
//!
//! The kind reusables (`ci-unit-<kind>.yml`) render every unit-specific value
//! as an `inputs.*` reference and the aggregate callers supply the values in
//! `with:`. This test resolves those references for every hosted caller of
//! `ci-pr.yml` — `${{ inputs.x }}` from the caller's value, and
//! `hashFiles(inputs.x)` to the literal `hashFiles('a', 'b')` form, which is
//! equivalent at runtime because `actions/runner` joins the arguments with a
//! newline and `@actions/glob` splits patterns on newlines — and asserts the
//! resolved primary keys, restore keys, and cache paths equal the literal
//! forms the generator rendered before the parameterization.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "fixtures/pre_parameterization_cache_keys.rs"]
mod fixture;

use fixture::{CacheKey, PRE_PARAMETERIZATION_KEYS};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repository root resolves")
}

fn read_workflow(name: &str) -> String {
    let path = repository_root().join(".github/workflows").join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The lines of the top-level job `job_id` (two-space indented key under
/// `jobs:`), excluding the header line.
fn job_block<'a>(workflow: &'a str, job_id: &str) -> Vec<&'a str> {
    let header = format!("  {job_id}:");
    let mut lines = workflow.lines();
    lines
        .by_ref()
        .find(|line| *line == header)
        .unwrap_or_else(|| panic!("job `{job_id}` is missing"));
    lines
        .take_while(|line| line.is_empty() || line.starts_with("   ") || !line.starts_with("  "))
        .filter(|line| !line.is_empty())
        .collect()
}

/// Undo the generator's YAML scalar quoting for a `with:` value.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return inner.replace("''", "'");
    }
    value.to_owned()
}

/// The `uses:` target and `with:` values of one aggregate caller job.
fn caller_inputs(aggregate: &str, job_id: &str) -> (String, BTreeMap<String, String>) {
    let block = job_block(aggregate, job_id);
    let uses = block
        .iter()
        .find_map(|line| line.trim_start().strip_prefix("uses: ./.github/workflows/"))
        .unwrap_or_else(|| panic!("caller `{job_id}` has no reusable `uses:`"))
        .to_owned();
    let mut inputs = BTreeMap::new();
    let mut lines = block
        .iter()
        .skip_while(|line| **line != "    with:")
        .skip(1)
        .peekable();
    while let Some(line) = lines.next() {
        let Some(entry) = line.strip_prefix("      ") else {
            break;
        };
        let (key, value) = entry
            .split_once(':')
            .unwrap_or_else(|| panic!("caller `{job_id}` has a malformed with entry: {line}"));
        if value.trim() == "|" {
            let mut block_lines = Vec::new();
            while let Some(next) = lines.peek() {
                match next.strip_prefix("        ") {
                    Some(text) => {
                        block_lines.push(text.to_owned());
                        lines.next();
                    }
                    None => break,
                }
            }
            inputs.insert(key.to_owned(), block_lines.join("\n"));
        } else {
            inputs.insert(key.to_owned(), unquote(value));
        }
    }
    (uses, inputs)
}

/// One rendered cache step of the callee, as the raw (unresolved) YAML text.
struct CacheStep {
    gate: Option<String>,
    paths: Vec<String>,
    primary: String,
    restore_keys: Vec<String>,
}

fn step_with_id(job: &[&str], step_id: &str) -> Option<CacheStep> {
    let id_line = format!("        id: {step_id}");
    let mut steps: Vec<Vec<&str>> = Vec::new();
    for line in job.iter().skip_while(|line| **line != "    steps:").skip(1) {
        if line.starts_with("      - ") {
            steps.push(vec![line]);
        } else if let Some(current) = steps.last_mut() {
            current.push(line);
        }
    }
    let step = steps
        .into_iter()
        .find(|step| step.contains(&id_line.as_str()))?;
    let gate = step
        .iter()
        .find_map(|line| line.strip_prefix("        if: "))
        .map(str::to_owned);
    let field = |name: &str| -> Option<String> {
        let prefix = format!("          {name}: ");
        step.iter()
            .find_map(|line| line.strip_prefix(prefix.as_str()))
            .map(str::to_owned)
    };
    let block = |name: &str| -> Vec<String> {
        let header = format!("          {name}: |");
        step.iter()
            .skip_while(|line| **line != header)
            .skip(1)
            .map_while(|line| line.strip_prefix("            "))
            .map(str::to_owned)
            .collect()
    };
    let primary = field("key")
        .or_else(|| field("cache-key"))
        .unwrap_or_else(|| panic!("step `{step_id}` has no cache key"));
    let paths = match field("path") {
        Some(path) if path != "|" => vec![path],
        _ => block("path"),
    };
    Some(CacheStep {
        gate,
        paths,
        primary,
        restore_keys: block("restore-keys"),
    })
}

/// Resolve every `inputs.*` reference in `text` from the caller's values.
fn resolve(text: &str, inputs: &BTreeMap<String, String>) -> String {
    let mut resolved = text.to_owned();
    for (name, value) in inputs {
        let hash_files = format!("hashFiles(inputs.{name})");
        if resolved.contains(&hash_files) {
            let literal = value
                .lines()
                .map(|path| format!("'{}'", path.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ");
            resolved = resolved.replace(&hash_files, &format!("hashFiles({literal})"));
        }
        resolved = resolved.replace(&format!("${{{{ inputs.{name} }}}}"), value);
    }
    assert!(
        !resolved.contains("inputs."),
        "unresolved input reference in `{resolved}`"
    );
    resolved
}

fn resolve_paths(paths: &[String], inputs: &BTreeMap<String, String>) -> Vec<String> {
    paths
        .iter()
        .flat_map(|path| {
            resolve(path, inputs)
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn step_id_for_layer(layer: &str) -> &'static str {
    match layer {
        "rustup" => "rustup-toolchain",
        "mold" => "mold-cache",
        "cargo_bin" => "cargo-bin-toolchain",
        "mbx" => "mbx-cache",
        "bundle" | "docker_seed" => "cache",
        other => panic!("unknown cache layer `{other}` in the fixture"),
    }
}

/// A step gated on an input's presence must have that input supplied by the
/// caller, or the step is skipped at runtime and the key is moot.
fn assert_gate_open(gate: Option<&str>, inputs: &BTreeMap<String, String>, context: &str) {
    let Some(gate) = gate else {
        return;
    };
    for token in gate.split(|character: char| {
        !(character.is_alphanumeric() || character == '_' || character == '.')
    }) {
        if let Some(name) = token.strip_prefix("inputs.")
            && gate.contains(&format!("inputs.{name} != ''"))
        {
            assert!(
                inputs.get(name).is_some_and(|value| !value.is_empty()),
                "{context}: the step is gated on `{name}` but the caller does not pass it"
            );
        }
    }
}

#[test]
fn parameterized_callees_resolve_to_the_pre_parameterization_cache_keys() {
    let aggregate = read_workflow("ci-pr.yml");
    let mut callees: BTreeMap<String, String> = BTreeMap::new();
    for expected in PRE_PARAMETERIZATION_KEYS {
        let (uses, inputs) = caller_inputs(&aggregate, expected.caller_job);
        assert_eq!(
            uses, expected.callee,
            "{} calls a different reusable than before",
            expected.caller_job
        );
        let callee = callees
            .entry(uses.clone())
            .or_insert_with(|| read_workflow(&uses));
        let job = job_block(callee, expected.job);
        for CacheKey {
            layer,
            paths,
            primary,
            restore_keys,
        } in expected.keys
        {
            let context = format!(
                "{} → {}#{} [{layer}]",
                expected.caller_job, uses, expected.job
            );
            let step = step_with_id(&job, step_id_for_layer(layer))
                .unwrap_or_else(|| panic!("{context}: the cache step is missing"));
            assert_gate_open(step.gate.as_deref(), &inputs, &context);
            assert_eq!(
                resolve(&step.primary, &inputs),
                *primary,
                "{context}: primary key changed"
            );
            let resolved_restore = step
                .restore_keys
                .iter()
                .map(|key| resolve(key, &inputs))
                .collect::<Vec<_>>();
            assert_eq!(
                resolved_restore, *restore_keys,
                "{context}: restore keys changed"
            );
            if !paths.is_empty() {
                assert_eq!(
                    resolve_paths(&step.paths, &inputs),
                    *paths,
                    "{context}: cache paths changed"
                );
            }
        }
    }
}

/// The kind reusables hold exactly one step block per lane job: no step is
/// guarded by a unit identity, and the callee's size is independent of how
/// many units the kind has.
#[test]
fn kind_reusables_hold_no_per_unit_step_blocks() {
    for entry in fs::read_dir(repository_root().join(".github/workflows")).expect("workflows") {
        let path = entry.expect("entry").path();
        let name = path.file_name().unwrap().to_str().unwrap();
        if !name.starts_with("ci-unit-") {
            continue;
        }
        let content = fs::read_to_string(&path).expect("read kind reusable");
        assert!(
            !content.contains("inputs.unit == '"),
            "{name} guards steps by unit identity"
        );
        assert!(
            content.matches("- name: Run unit checks").count() <= 3,
            "{name} renders more than one checks step per lane job"
        );
    }
}

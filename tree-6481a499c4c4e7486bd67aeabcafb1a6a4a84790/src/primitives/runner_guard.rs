//! Lossless insertion of per-job GitHub Actions runner-environment guards.
//!
//! This edits only targeted jobs' `if` scalars and the insertion point before
//! their first step. The YAML CST preserves comments, indentation, and all
//! unrelated workflow bytes.

use std::collections::BTreeMap;

use serde_yaml::cst::{parse_document, Document};
use serde_yaml::Value;

const GUARD_ID: &str = "runner_provenance";
const GUARD_NAME: &str = "Verify runner environment";
const GUARD_SHELL: &str = "bash --noprofile --norc -p -e -o pipefail {0}";
pub(crate) const GUARD_WORKING_DIRECTORY: &str = "${{ runner.temp }}";

/// Variables that can influence Bash startup, dynamic-loader lookup, or
/// Windows DLL resolution before the guard body runs. The guard overrides
/// every one at step scope; an empty `PATH` is safe because Actions Runner
/// resolves the shell executable before starting the child process and this
/// guard does not invoke external commands.
pub(crate) const GUARD_SANITIZED_ENVIRONMENT: &[&str] = &[
    "BASH_ENV",
    "BASHOPTS",
    "SHELLOPTS",
    "PATH",
    "NODE_OPTIONS",
    "GCONV_PATH",
    "GLIBC_TUNABLES",
    "LD_ASSUME_KERNEL",
    "LD_AUDIT",
    "LD_BIND_NOT",
    "LD_BIND_NOW",
    "LD_DEBUG",
    "LD_DEBUG_OUTPUT",
    "LD_DYNAMIC_WEAK",
    "LD_HWCAP_MASK",
    "LD_LIBRARY_PATH",
    "LD_ORIGIN_PATH",
    "LD_PREFER_MAP_32BIT_EXEC",
    "LD_PRELOAD",
    "LD_PROFILE",
    "LD_PROFILE_OUTPUT",
    "LD_SHOW_AUXV",
    "LD_TRACE_LOADED_OBJECTS",
    "LD_USE_LOAD_BIAS",
    "LD_VERBOSE",
    "LD_WARN",
    "DYLD_DISABLE_DOFS",
    "DYLD_FALLBACK_FRAMEWORK_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "DYLD_IMAGE_SUFFIX",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_PATHS_ROOT",
    "DYLD_PRINT_TO_FILE",
    "DYLD_ROOT_PATH",
    "DYLD_SHARED_CACHE_DIR",
    "DYLD_SHARED_REGION",
    "DYLD_VERSIONED_FRAMEWORK_PATH",
    "DYLD_VERSIONED_LIBRARY_PATH",
];

pub(crate) fn guard_environment_is_sanitized(value: Option<&Value>) -> bool {
    let Some(environment) = value.and_then(Value::as_mapping) else {
        return false;
    };
    environment.len() == GUARD_SANITIZED_ENVIRONMENT.len()
        && GUARD_SANITIZED_ENVIRONMENT.iter().all(|key| {
            environment
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(str::is_empty)
        })
}

/// Add a first-step environment guard to each targeted job and gate every
/// original step on its expected runner environment.
///
/// `expected_match_by_job` maps job IDs to raw GitHub Actions boolean
/// expressions, such as `runner.environment == 'github-hosted'`. Expressions
/// must be single-line and omit the outer `${{ }}` delimiters.
///
/// The first step fails when its predicate is false. Every original step is
/// additionally conditioned on the same predicate, so later `always()` steps
/// cannot proceed after a failed guard. Reapplying the same transformation is
/// byte-identical. Unsupported YAML layouts fail closed with an error.
pub(crate) fn transform_workflow(
    content: &str,
    expected_match_by_job: &BTreeMap<String, String>,
) -> Result<String, String> {
    if expected_match_by_job.is_empty() {
        return Ok(content.to_owned());
    }

    let mut document = parse_document(content)
        .map_err(|error| format!("runner guard cannot parse workflow YAML: {error}"))?;

    // A path through an alias resolves to its anchor's bytes. Editing it could
    // silently alter another job, so refuse alias-bearing documents.
    if !document.aliases().is_empty() {
        return Err("runner guard does not support YAML aliases".to_owned());
    }

    for (job_id, raw_match_expression) in expected_match_by_job {
        validate_job_id(job_id)?;
        let match_expression = validate_match_expression(job_id, raw_match_expression)?;
        let snapshot = read_job_steps(&document, job_id)?;
        let has_guard = validate_existing_guard(job_id, &snapshot, match_expression)?;

        for (index, step) in snapshot
            .steps
            .iter()
            .enumerate()
            .skip(usize::from(has_guard))
        {
            let step_path = format!("jobs.{job_id}.steps[{index}]");
            if let Some(condition) = &step.condition {
                let body = expression_body(job_id, index, condition)?;
                if is_gated(body, match_expression) {
                    continue;
                }
                let combined = format!("${{{{ ({match_expression}) && ({body}) }}}}");
                set_step_condition(&mut document, &step_path, &combined, job_id, index)?;
            } else {
                let combined = format!("${{{{ ({match_expression}) }}}}");
                insert_step_condition(&mut document, &step_path, &combined, job_id, index)?;
            }
        }

        if !has_guard {
            insert_guard(&mut document, job_id, match_expression)?;
        }
    }

    document
        .validate()
        .map_err(|error| format!("runner guard produced invalid workflow YAML: {error}"))?;
    verify_transformed_workflow(&document, expected_match_by_job)?;
    Ok(document.source().to_owned())
}

fn validate_job_id(job_id: &str) -> Result<(), String> {
    let mut bytes = job_id.bytes();
    let Some(first) = bytes.next() else {
        return Err("runner guard job id cannot be empty".to_owned());
    };
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(format!(
            "runner guard job id `{job_id}` has unsupported path syntax"
        ));
    }
    Ok(())
}

struct JobSnapshot {
    steps: Vec<StepSnapshot>,
}

struct StepSnapshot {
    id: Option<String>,
    name: Option<String>,
    condition: Option<String>,
    shell: Option<String>,
    working_directory: Option<String>,
    environment_is_sanitized: bool,
    run: Option<String>,
    continues_on_error: bool,
}

fn read_job_steps(document: &Document, job_id: &str) -> Result<JobSnapshot, String> {
    let root = document.as_value();
    let jobs = root
        .as_mapping()
        .and_then(|mapping| mapping.get("jobs"))
        .and_then(Value::as_mapping)
        .ok_or_else(|| format!("runner guard target job `{job_id}` requires a block `jobs` map"))?;
    let job = jobs
        .get(job_id)
        .ok_or_else(|| format!("runner guard target job `{job_id}` was not found"))?;
    let job_map = job
        .as_mapping()
        .ok_or_else(|| format!("runner guard target job `{job_id}` must be a block mapping"))?;
    if job_map.contains_key("container") || job_map.contains_key("services") {
        return Err(format!(
            "runner guard target job `{job_id}` cannot use `container` or `services`"
        ));
    }
    if value_enables_continue_on_error(job_map.get("continue-on-error"))
        || job_map
            .get("strategy")
            .is_some_and(|strategy| contains_yaml_key(strategy, "continue-on-error"))
    {
        return Err(format!(
            "runner guard target job `{job_id}` cannot use job or matrix `continue-on-error`"
        ));
    }
    let steps_value = job_map
        .get("steps")
        .ok_or_else(|| format!("runner guard target job `{job_id}` has no `steps` block"))?;
    let steps = steps_value.as_sequence().ok_or_else(|| {
        format!("runner guard target job `{job_id}` requires a block `steps` sequence")
    })?;
    if steps.is_empty() {
        return Err(format!(
            "runner guard target job `{job_id}` has an empty `steps` sequence"
        ));
    }

    let steps_path = format!("jobs.{job_id}.steps");
    let Some((steps_start, steps_end)) = document.span_at(&steps_path) else {
        return Err(format!(
            "runner guard cannot locate the `steps` block for job `{job_id}`"
        ));
    };
    let step_source = &document.source()[steps_start..steps_end];
    if !step_source.trim_start().starts_with('-') {
        return Err(format!(
            "runner guard target job `{job_id}` requires block-style `steps`"
        ));
    }

    let mut snapshots = Vec::with_capacity(steps.len());
    for (index, step) in steps.iter().enumerate() {
        let mapping = step.as_mapping().ok_or_else(|| {
            format!("runner guard target job `{job_id}` step {index} must be a mapping")
        })?;
        let string_field = |field: &str| -> Result<Option<String>, String> {
            let Some(value) = mapping.get(field) else {
                return Ok(None);
            };
            value
                .as_str()
                .map(|text| Some(text.to_owned()))
                .ok_or_else(|| {
                    format!(
                        "runner guard target job `{job_id}` step {index} field `{field}` must be a scalar string"
                    )
                })
        };
        let condition = string_field("if")?;
        if condition.is_some() {
            require_single_line_if(document, job_id, index)?;
        }
        snapshots.push(StepSnapshot {
            id: string_field("id")?,
            name: string_field("name")?,
            condition,
            shell: string_field("shell")?,
            working_directory: string_field("working-directory")?,
            environment_is_sanitized: guard_environment_is_sanitized(mapping.get("env")),
            run: string_field("run")?,
            continues_on_error: mapping
                .get("continue-on-error")
                .is_some_and(|value| !matches!(value, Value::Bool(false))),
        });
    }
    Ok(JobSnapshot { steps: snapshots })
}

fn value_enables_continue_on_error(value: Option<&Value>) -> bool {
    value.is_some_and(|value| value.as_bool() != Some(false))
}

fn contains_yaml_key(value: &Value, key: &str) -> bool {
    match value {
        Value::Mapping(mapping) => {
            mapping.contains_key(key) || mapping.values().any(|value| contains_yaml_key(value, key))
        }
        Value::Sequence(sequence) => sequence.iter().any(|value| contains_yaml_key(value, key)),
        _ => false,
    }
}

fn validate_match_expression<'a>(job_id: &str, expression: &'a str) -> Result<&'a str, String> {
    let expression = expression.trim();
    if expression.is_empty()
        || expression.contains('\n')
        || expression.contains('\r')
        || expression.contains("${{")
        || expression.contains("}}")
        || !has_balanced_expression_groups(expression)
    {
        return Err(format!(
            "runner guard target job `{job_id}` has an unsupported expected-environment expression"
        ));
    }
    Ok(expression)
}

fn validate_existing_guard(
    job_id: &str,
    snapshot: &JobSnapshot,
    match_expression: &str,
) -> Result<bool, String> {
    let guard_positions = snapshot
        .steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| (step.id.as_deref() == Some(GUARD_ID)).then_some(index))
        .collect::<Vec<_>>();
    if guard_positions.is_empty() {
        return Ok(false);
    }
    if guard_positions.as_slice() != [0] {
        return Err(format!(
            "runner guard target job `{job_id}` has `{GUARD_ID}` outside the first step"
        ));
    }

    let guard = &snapshot.steps[0];
    let expected_if = mismatch_expression(match_expression);
    if guard.name.as_deref() != Some(GUARD_NAME)
        || guard.condition.as_deref() != Some(expected_if.as_str())
        || guard.shell.as_deref() != Some(GUARD_SHELL)
        || guard.working_directory.as_deref() != Some(GUARD_WORKING_DIRECTORY)
        || !guard.environment_is_sanitized
        || guard.run.as_deref() != Some("((0))")
        || guard.continues_on_error
    {
        return Err(format!(
            "runner guard target job `{job_id}` has an incompatible `{GUARD_ID}` step"
        ));
    }
    Ok(true)
}

fn expression_body<'a>(
    job_id: &str,
    step_index: usize,
    condition: &'a str,
) -> Result<&'a str, String> {
    let trimmed = condition.trim();
    let body = if let Some(body) = trimmed
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
    {
        body.trim()
    } else if trimmed.contains("${{") || trimmed.contains("}}") {
        return Err(format!(
            "runner guard target job `{job_id}` step {step_index} has an unsupported mixed expression"
        ));
    } else {
        trimmed
    };

    if body.is_empty()
        || body.contains('\n')
        || body.contains('\r')
        || !has_balanced_expression_groups(body)
    {
        return Err(format!(
            "runner guard target job `{job_id}` step {step_index} has an empty or multiline `if`"
        ));
    }
    Ok(body)
}

fn has_balanced_expression_groups(expression: &str) -> bool {
    let bytes = expression.as_bytes();
    let mut depth = 0_usize;
    let mut in_single_quote = false;
    let mut index = 0_usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_single_quote {
            if byte == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                }
                in_single_quote = false;
            }
        } else {
            match byte {
                b'\'' => in_single_quote = true,
                b'(' => depth += 1,
                b')' => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                byte if byte.is_ascii_control() => return false,
                _ => {}
            }
        }
        index += 1;
    }
    depth == 0 && !in_single_quote
}

fn is_gated(condition: &str, match_expression: &str) -> bool {
    let condition = condition.trim();
    let direct_match = format!("({match_expression})");
    if condition == direct_match {
        return true;
    }

    // Parse the only compound form emitted by this module: an explicit
    // parenthesized expected predicate as the left operand of a top-level
    // conjunction, followed by one parenthesized operand that consumes the
    // entire remainder. Requiring this structure rejects top-level `||`
    // bypasses even when they follow an apparently valid conjunction.
    let Some(expected_group_end) = matching_group_end(condition, 0) else {
        return false;
    };
    if &condition[1..expected_group_end - 1] != match_expression {
        return false;
    }
    condition[expected_group_end..]
        .strip_prefix(" && ")
        .is_some_and(is_one_outer_group)
}

fn is_one_outer_group(expression: &str) -> bool {
    let expression = expression.trim();
    matching_group_end(expression, 0) == Some(expression.len())
}

fn matching_group_end(expression: &str, start: usize) -> Option<usize> {
    let mut depth = 0_usize;
    let mut in_single_quote = false;
    let bytes = expression.as_bytes();
    if bytes.get(start) != Some(&b'(') {
        return None;
    }
    let mut index = start;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_single_quote {
            if byte == b'\'' {
                if bytes.get(index + 1) == Some(&b'\'') {
                    index += 2;
                    continue;
                }
                in_single_quote = false;
            }
        } else {
            match byte {
                b'\'' => in_single_quote = true,
                b'(' => depth += 1,
                b')' => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                    if depth == 0 {
                        return Some(index + 1);
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

fn set_step_condition(
    document: &mut Document,
    step_path: &str,
    condition: &str,
    job_id: &str,
    step_index: usize,
) -> Result<(), String> {
    document
        .set(&format!("{step_path}.if"), &yaml_single_quote(condition))
        .map_err(|error| {
            format!("runner guard cannot update job `{job_id}` step {step_index} `if`: {error}")
        })
}

fn insert_step_condition(
    document: &mut Document,
    step_path: &str,
    condition: &str,
    job_id: &str,
    step_index: usize,
) -> Result<(), String> {
    let (step_start, step_end) = document
        .span_at(step_path)
        .ok_or_else(|| format!("runner guard cannot locate job `{job_id}` step {step_index}"))?;
    let source = document.source();
    let line_start = source[..step_start]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let line_end = source[step_start..]
        .find('\n')
        .map_or(source.len(), |offset| step_start + offset);
    let first_line = &source[line_start..line_end];
    let dash_indent = first_line.bytes().take_while(|byte| *byte == b' ').count();
    if !first_line[dash_indent..].starts_with("- ") {
        return Err(format!(
            "runner guard target job `{job_id}` step {step_index} requires a block mapping item"
        ));
    }
    let property_indent = " ".repeat(dash_indent + 2);
    let insert_at = source[step_end..]
        .find('\n')
        .map_or(source.len(), |offset| step_end + offset + 1);
    let line_ending = line_ending_near(source, insert_at);
    let needs_leading_break = insert_at == source.len() && !source[..insert_at].ends_with('\n');
    let fragment = format!(
        "{}{property_indent}if: {}{}",
        if needs_leading_break { line_ending } else { "" },
        yaml_single_quote(condition),
        if insert_at < source.len() || source.ends_with('\n') {
            line_ending
        } else {
            ""
        },
    );
    document
        .replace_span(insert_at, insert_at, &fragment)
        .map_err(|error| {
            format!("runner guard cannot add job `{job_id}` step {step_index} `if`: {error}")
        })
}

fn require_single_line_if(
    document: &Document,
    job_id: &str,
    step_index: usize,
) -> Result<(), String> {
    let path = format!("jobs.{job_id}.steps[{step_index}].if");
    let Some((key_start, key_end)) = document.key_span(&path) else {
        return Err(format!(
            "runner guard cannot locate job `{job_id}` step {step_index} `if` key"
        ));
    };
    if &document.source()[key_start..key_end] != "if" {
        return Err(format!(
            "runner guard target job `{job_id}` step {step_index} uses an unsupported `if` key"
        ));
    }
    let line_start = document.source()[..key_start]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let line_end = document.source()[key_end..]
        .find('\n')
        .map_or(document.source().len(), |offset| key_end + offset);
    let key_line = &document.source()[line_start..line_end];
    let Some(colon_offset) = key_line.find(':') else {
        return Err(format!(
            "runner guard target job `{job_id}` step {step_index} has an unsupported `if` entry"
        ));
    };
    let scalar = key_line[colon_offset + 1..].trim_start();
    if scalar.starts_with('|') || scalar.starts_with('>') {
        return Err(format!(
            "runner guard target job `{job_id}` step {step_index} has a multiline `if`"
        ));
    }
    let value_path = format!("jobs.{job_id}.steps[{step_index}].if");
    let Some((value_start, value_end)) = document.span_at(&value_path) else {
        return Err(format!(
            "runner guard cannot locate job `{job_id}` step {step_index} `if` value"
        ));
    };
    if document.source()[value_start..value_end].contains(['\n', '\r']) || value_end > line_end {
        return Err(format!(
            "runner guard target job `{job_id}` step {step_index} has a multiline `if`"
        ));
    }
    Ok(())
}

fn insert_guard(
    document: &mut Document,
    job_id: &str,
    match_expression: &str,
) -> Result<(), String> {
    let first_step_path = format!("jobs.{job_id}.steps[0]");
    let (step_start, _) = document
        .span_at(&first_step_path)
        .ok_or_else(|| format!("runner guard cannot locate first step for job `{job_id}`"))?;
    let source = document.source();
    let line_start = source[..step_start]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let line_end = source[step_start..]
        .find('\n')
        .map_or(source.len(), |offset| step_start + offset);
    let line = &source[line_start..line_end];
    let indent_end = line.bytes().take_while(|byte| *byte == b' ').count();
    if line[indent_end..].starts_with('\t') || !line[indent_end..].starts_with('-') {
        return Err(format!(
            "runner guard target job `{job_id}` requires a block-style first step"
        ));
    }
    if !line[indent_end + 1..]
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
    {
        return Err(format!(
            "runner guard target job `{job_id}` has an unsupported first-step sequence item"
        ));
    }
    let line_ending = line_ending_near(document.source(), line_start);
    let indent = &line[..indent_end];
    let guard = format!(
        "{indent}- name: {GUARD_NAME}{line_ending}\
         {indent}  id: {GUARD_ID}{line_ending}\
         {indent}  if: {}{line_ending}\
         {indent}  run: ((0)){line_ending}\
         {indent}  shell: {}{line_ending}\
         {indent}  working-directory: {GUARD_WORKING_DIRECTORY}{line_ending}\
         {indent}  env:{line_ending}\
         {environment_lines}",
        yaml_single_quote(&mismatch_expression(match_expression)),
        yaml_single_quote(GUARD_SHELL),
        environment_lines = GUARD_SANITIZED_ENVIRONMENT
            .iter()
            .map(|key| format!("{indent}    {key}: ''{line_ending}"))
            .collect::<String>(),
    );
    document
        .replace_span(line_start, line_start, &guard)
        .map_err(|error| format!("runner guard cannot insert into job `{job_id}`: {error}"))
}

fn mismatch_expression(match_expression: &str) -> String {
    format!("${{{{ !({match_expression}) }}}}")
}

fn yaml_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn line_ending_near(source: &str, position: usize) -> &'static str {
    let after = &source[position..];
    let before = &source[..position];
    if after.starts_with("\r\n") || before.ends_with("\r\n") {
        "\r\n"
    } else if after.starts_with('\n') || before.ends_with('\n') {
        "\n"
    } else if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

fn verify_transformed_workflow(
    document: &Document,
    expected_match_by_job: &BTreeMap<String, String>,
) -> Result<(), String> {
    let root = document.as_value();
    let jobs = root
        .as_mapping()
        .and_then(|mapping| mapping.get("jobs"))
        .and_then(Value::as_mapping)
        .ok_or_else(|| "runner guard output has no `jobs` map".to_owned())?;
    for (job_id, expected) in expected_match_by_job {
        validate_job_id(job_id)?;
        let expected = validate_match_expression(job_id, expected)?;
        let job = jobs
            .get(job_id)
            .and_then(Value::as_mapping)
            .ok_or_else(|| format!("runner guard output lost target job `{job_id}`"))?;
        let steps = job
            .get("steps")
            .and_then(Value::as_sequence)
            .ok_or_else(|| format!("runner guard output lost steps for job `{job_id}`"))?;
        let guard = steps
            .first()
            .and_then(Value::as_mapping)
            .ok_or_else(|| format!("runner guard output lost first step for job `{job_id}`"))?;
        let expected_guard_if = mismatch_expression(expected);
        if guard.get("id").and_then(Value::as_str) != Some(GUARD_ID)
            || guard.get("name").and_then(Value::as_str) != Some(GUARD_NAME)
            || guard.get("if").and_then(Value::as_str) != Some(expected_guard_if.as_str())
            || guard.get("run").and_then(Value::as_str) != Some("((0))")
            || guard.get("shell").and_then(Value::as_str) != Some(GUARD_SHELL)
            || guard.get("working-directory").and_then(Value::as_str)
                != Some(GUARD_WORKING_DIRECTORY)
            || !guard_environment_is_sanitized(guard.get("env"))
            || guard
                .get("continue-on-error")
                .is_some_and(|value| !matches!(value, Value::Bool(false)))
        {
            return Err(format!(
                "runner guard output failed the first-step contract for job `{job_id}`"
            ));
        }
        for (index, step) in steps.iter().enumerate().skip(1) {
            let condition = step
                .as_mapping()
                .and_then(|mapping| mapping.get("if"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!("runner guard output left job `{job_id}` step {index} without an `if`")
                })?;
            let body = expression_body(job_id, index, condition)?;
            if !is_gated(body, expected) {
                return Err(format!(
                    "runner guard output left job `{job_id}` step {index} ungated"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::transform_workflow;
    use super::{GUARD_SANITIZED_ENVIRONMENT, GUARD_WORKING_DIRECTORY};
    use std::collections::BTreeMap;

    fn expected(job_id: &str, expression: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(job_id.to_owned(), expression.to_owned())])
    }

    #[test]
    fn inserts_guard_and_conjoins_existing_and_missing_conditions() {
        let source = "name: CI\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n        if: always()\n      - run: echo ok\n";
        let output = transform_workflow(
            source,
            &expected("build", "runner.environment == 'github-hosted'"),
        )
        .expect("generated workflow should be supported");

        assert!(output.starts_with(
            "name: CI\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    steps:\n      - name: Verify runner environment\n        id: runner_provenance\n        if: '${{ !(runner.environment == ''github-hosted'') }}'\n        run: ((0))\n        shell: 'bash --noprofile --norc -p -e -o pipefail {0}'\n        working-directory: ${{ runner.temp }}\n        env:\n"
        ));
        for key in GUARD_SANITIZED_ENVIRONMENT {
            assert!(output.contains(&format!("          {key}: ''\n")));
        }
        assert!(output.ends_with(
            "      - uses: actions/checkout@v4\n        if: '${{ (runner.environment == ''github-hosted'') && (always()) }}'\n      - run: echo ok\n        if: '${{ (runner.environment == ''github-hosted'') }}'\n"
        ));
    }

    #[test]
    fn accepts_wrapped_and_unwrapped_conditions_and_is_idempotent() {
        let source = "jobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo one\n        if: ${{ always() }}\n      - run: echo two\n        if: needs.prepare.result == 'success'\n";
        let expected = expected("test", "runner.environment == 'github-hosted'");
        let once = transform_workflow(source, &expected).expect("first transform");
        let twice = transform_workflow(&once, &expected).expect("second transform");
        assert_eq!(twice, once);
        assert!(once.contains("(runner.environment == ''github-hosted'') && (always())"));
        assert!(once.contains(
            "(runner.environment == ''github-hosted'') && (needs.prepare.result == ''success'')"
        ));
    }

    #[test]
    fn explicit_guard_shell_and_environment_override_workflow_defaults() {
        let source = "defaults:\n  run:\n    shell: 'bash {0} || true'\n    working-directory: /tmp/untrusted-cwd\nenv:\n  BASH_ENV: /tmp/untrusted-startup\n  SHELLOPTS: noexec\n  LD_PRELOAD: /tmp/workflow-payload.so\njobs:\n  test:\n    defaults:\n      run:\n        shell: 'bash {0} || true'\n        working-directory: /tmp/job-cwd\n    env:\n      BASH_ENV: /tmp/job-startup\n      BASHOPTS: expand_aliases\n      DYLD_INSERT_LIBRARIES: /tmp/job-payload.dylib\n      PATH: /tmp/job-bin\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n";
        let output = transform_workflow(
            source,
            &expected("test", "runner.environment == 'github-hosted'"),
        )
        .expect("step-local shell and environment must override workflow defaults");
        assert!(output.contains(&format!(
            "        shell: 'bash --noprofile --norc -p -e -o pipefail {{0}}'\n        working-directory: {GUARD_WORKING_DIRECTORY}\n        env:\n"
        )));
        for key in GUARD_SANITIZED_ENVIRONMENT {
            assert!(output.contains(&format!("          {key}: ''\n")));
        }
        assert!(output.contains("shell: 'bash {0} || true'"));
        assert!(output.contains("BASH_ENV: /tmp/untrusted-startup"));
        assert!(output.contains("BASH_ENV: /tmp/job-startup"));
        assert!(output.contains("LD_PRELOAD: /tmp/workflow-payload.so"));
        assert!(output.contains("DYLD_INSERT_LIBRARIES: /tmp/job-payload.dylib"));
        assert!(output.contains("PATH: /tmp/job-bin"));
        assert!(output.contains("working-directory: /tmp/untrusted-cwd"));
        assert!(output.contains("working-directory: /tmp/job-cwd"));
    }

    #[test]
    fn preserves_unrelated_bytes_and_crlf() {
        let source = "# workflow header\r\njobs:\r\n  build:\r\n    runs-on: ubuntu-24.04\r\n    steps:\r\n      - run: echo ok # keep this comment\r\n  other:\r\n    runs-on: ubuntu-latest\r\n    steps:\r\n      - run: echo untouched\r\n";
        let output = transform_workflow(
            source,
            &expected("build", "runner.environment == 'github-hosted'"),
        )
        .expect("CRLF workflow should be supported");
        assert!(output.starts_with("# workflow header\r\njobs:\r\n"));
        assert!(output.contains("      - run: echo ok # keep this comment\r\n"));
        assert!(output.ends_with(
            "  other:\r\n    runs-on: ubuntu-latest\r\n    steps:\r\n      - run: echo untouched\r\n"
        ));
        assert!(output.replace("\r\n", "").find('\n').is_none());
    }

    #[test]
    fn fails_closed_for_flow_sequences_multiline_conditions_and_unknown_jobs() {
        let flow_steps = "jobs:\n  build:\n    steps: [{run: echo ok}]\n";
        assert!(transform_workflow(flow_steps, &expected("build", "true")).is_err());

        let multiline_if = "jobs:\n  build:\n    steps:\n      - run: echo ok\n        if: >\n          always()\n";
        assert!(transform_workflow(multiline_if, &expected("build", "true")).is_err());

        let absent = "jobs:\n  other:\n    steps:\n      - run: echo ok\n";
        assert!(transform_workflow(absent, &expected("build", "true")).is_err());
    }

    #[test]
    fn rejects_conflicting_existing_guard_and_expression_injection() {
        let wrong_guard = "jobs:\n  build:\n    steps:\n      - name: Verify runner environment\n        id: runner_provenance\n        if: '${{ !(false) }}'\n        run: exit 1\n      - run: echo ok\n        if: true\n";
        assert!(transform_workflow(
            wrong_guard,
            &expected("build", "runner.environment == 'github-hosted'"),
        )
        .is_err());

        let guard_environment = GUARD_SANITIZED_ENVIRONMENT
            .iter()
            .map(|key| format!("          {key}: ''\n"))
            .collect::<String>();
        let valid_guard = format!(
            "jobs:\n  build:\n    steps:\n      - name: Verify runner environment\n        id: runner_provenance\n        if: '${{{{ !(runner.environment == ''github-hosted'') }}}}'\n        run: ((0))\n        shell: 'bash --noprofile --norc -p -e -o pipefail {{0}}'\n        working-directory: {GUARD_WORKING_DIRECTORY}\n        env:\n{guard_environment}      - run: echo ok\n        if: '${{{{ (runner.environment == ''github-hosted'') }}}}'\n"
        );
        let shell_template_guard =
            valid_guard.replace("bash --noprofile --norc -p -e -o pipefail {0}", "true {0}");
        assert!(transform_workflow(
            &shell_template_guard,
            &expected("build", "runner.environment == 'github-hosted'"),
        )
        .is_err());

        for bad_env in [
            valid_guard.replace("BASH_ENV: ''", "BASH_ENV: /tmp/runner-startup"),
            valid_guard.replace("SHELLOPTS: ''", "SHELLOPTS: noexec"),
            valid_guard.replace("BASHOPTS: ''", "BASHOPTS: expand_aliases"),
            valid_guard.replace("LD_PRELOAD: ''", "LD_PRELOAD: /tmp/payload.so"),
            valid_guard.replace(
                "DYLD_INSERT_LIBRARIES: ''",
                "DYLD_INSERT_LIBRARIES: /tmp/payload.dylib",
            ),
            valid_guard.replace("PATH: ''", "PATH: /tmp/attacker-bin"),
            valid_guard.replace("LD_PRELOAD: ''\n", ""),
            valid_guard.replace("DYLD_INSERT_LIBRARIES: ''\n", ""),
            valid_guard.replace(
                "        working-directory: ${{ runner.temp }}",
                "        working-directory: ${{ github.workspace }}",
            ),
        ] {
            assert!(transform_workflow(
                &bad_env,
                &expected("build", "runner.environment == 'github-hosted'"),
            )
            .is_err());
        }

        let tolerated_guard =
            valid_guard.replace("env:\n", "continue-on-error: true\n        env:\n");
        assert!(transform_workflow(
            &tolerated_guard,
            &expected("build", "runner.environment == 'github-hosted'"),
        )
        .is_err());

        assert!(transform_workflow(
            "jobs:\n  build:\n    steps:\n      - run: echo ok\n",
            &expected("build", "true\n    - run: unsafe"),
        )
        .is_err());
    }

    #[test]
    fn rejects_jobs_that_execute_containers_before_the_guard() {
        for field in ["container", "services"] {
            let source = format!(
                "jobs:\n  build:\n    runs-on: ubuntu-24.04\n    {field}: alpine:latest\n    steps:\n      - run: echo ok\n"
            );
            let error = transform_workflow(
                &source,
                &expected("build", "runner.environment == 'github-hosted'"),
            )
            .expect_err("pre-step container execution must be rejected");
            assert!(error.contains(field), "{error}");
        }
    }

    #[test]
    fn rejects_job_or_matrix_continue_on_error() {
        for extra in [
            "    continue-on-error: true\n",
            "    strategy:\n      matrix:\n        continue-on-error: true\n",
        ] {
            let source = format!(
                "jobs:\n  build:\n    runs-on: ubuntu-24.04\n{extra}    steps:\n      - run: echo ok\n"
            );
            assert!(transform_workflow(
                &source,
                &expected("build", "runner.environment == 'github-hosted'"),
            )
            .is_err());
        }
    }

    #[test]
    fn wraps_or_conditions_and_expected_expressions_without_broadening_the_gate() {
        let source = "jobs:\n  build:\n    steps:\n      - run: echo unsafe\n        if: runner.environment == 'github-hosted' && (false) || always()\n";
        let output = transform_workflow(
            source,
            &expected("build", "runner.environment == 'github-hosted' || true"),
        )
        .expect("complex expressions should remain correctly grouped");
        assert!(output.contains(
            "if: '${{ (runner.environment == ''github-hosted'' || true) && (runner.environment == ''github-hosted'' && (false) || always()) }}'"
        ));
        let twice = transform_workflow(
            &output,
            &expected("build", "runner.environment == 'github-hosted' || true"),
        )
        .expect("canonical gate should be recognized on the second pass");
        assert_eq!(twice, output);
    }

    #[test]
    fn gate_recognition_rejects_a_top_level_or_bypass() {
        let expected = "runner.environment == 'github-hosted'";
        assert!(!super::is_gated(
            "runner.environment == 'github-hosted' && false || always()",
            expected,
        ));
        assert!(!super::is_gated(
            "runner.environment == 'github-hosted' && (false) || always()",
            expected,
        ));
        assert!(!super::is_gated(
            "(runner.environment == 'github-hosted') && (false) || always()",
            expected,
        ));
        assert!(super::is_gated(
            "(runner.environment == 'github-hosted') && (false || always())",
            expected,
        ));
    }

    #[test]
    fn adds_missing_condition_after_a_multiline_run_body() {
        let source = "jobs:\n  build:\n    steps:\n      - name: Script\n        run: |\n          echo first\n          echo second\n      - run: echo next\n";
        let output = transform_workflow(
            source,
            &expected("build", "runner.environment == 'github-hosted'"),
        )
        .expect("multiline run body is not a multiline `if`");
        assert!(output.contains(
            "          echo second\n        if: '${{ (runner.environment == ''github-hosted'') }}'\n      - run: echo next"
        ));
    }
}

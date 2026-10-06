//! Fail-closed estimate of the GitHub Actions template memory a workflow run
//! consumes at startup.
//!
//! `actions/runner` loads every workflow template into one `TemplateMemory`
//! budget (`ObjectTemplating/TemplateMemory.cs`, 10 MiB). A reusable workflow
//! called from N jobs is loaded N times, so the expanded cost of an aggregate
//! is its own template plus every callee counted once per caller. Exceeding
//! the budget fails the run at startup with `Maximum object size exceeded`
//! before any job runs, so the generator refuses to emit a surface whose
//! expanded cost passes a hard ceiling with headroom under GitHub's limit.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::GeneratorError;

/// `TemplateMemory.MinObjectSize`: every token (mapping, sequence, scalar)
/// costs this much before its string payload.
const MIN_OBJECT_SIZE: usize = 24;
/// `TemplateMemory.StringBaseOverhead`: a .NET string costs this plus two
/// bytes per UTF-16 code unit.
const STRING_BASE_OVERHEAD: usize = 26;

/// The hard ceiling on one aggregate's expanded template cost. GitHub's limit
/// is 10 MiB; the generator keeps at least two times headroom so a surface
/// cannot creep up to the platform budget.
pub(crate) const TEMPLATE_MEMORY_CEILING: usize = 5 * 1024 * 1024;

/// The directory the aggregates call reusable workflows from.
const WORKFLOWS_DIR: &str = ".github/workflows";

/// One aggregate's expanded template-memory account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TemplateMemoryReport {
    pub(crate) workflow: PathBuf,
    /// The aggregate's own template cost.
    pub(crate) own: usize,
    /// Callee file → (callers, cost per load), for every `uses:` job.
    pub(crate) callees: BTreeMap<String, (usize, usize)>,
}

impl TemplateMemoryReport {
    /// Own cost plus every callee counted once per caller.
    pub(crate) fn total(&self) -> usize {
        self.own
            + self
                .callees
                .values()
                .map(|(callers, cost)| callers * cost)
                .sum::<usize>()
    }

    /// Contributors ordered by their share, largest first: the aggregate's
    /// own template and every callee's `callers × cost`.
    fn contributors(&self) -> Vec<(String, usize)> {
        let mut rows = vec![(format!("{} (own)", self.workflow.display()), self.own)];
        rows.extend(self.callees.iter().map(|(file, (callers, cost))| {
            (
                format!("{WORKFLOWS_DIR}/{file} × {callers} callers"),
                callers * cost,
            )
        }));
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        rows
    }
}

/// The template-memory cost of one workflow document, mirroring
/// `TemplateMemory.CalculateBytes` over the parsed token tree: every mapping,
/// sequence, and scalar costs [`MIN_OBJECT_SIZE`]; every string — mapping
/// keys, values, and expressions alike — adds [`STRING_BASE_OVERHEAD`] plus
/// two bytes per UTF-16 code unit.
///
/// # Errors
/// Returns a usage error when the document is not valid YAML.
pub(crate) fn template_memory_estimate(yaml: &str) -> Result<usize, GeneratorError> {
    let value = serde_yaml::from_str::<serde_yaml::Value>(yaml).map_err(|error| {
        GeneratorError::usage(format!("template memory estimate: parse YAML: {error}"))
    })?;
    Ok(value_cost(&value))
}

fn value_cost(value: &serde_yaml::Value) -> usize {
    match value {
        serde_yaml::Value::Null | serde_yaml::Value::Bool(_) | serde_yaml::Value::Number(_) => {
            MIN_OBJECT_SIZE
        }
        serde_yaml::Value::String(text) => string_cost(text),
        serde_yaml::Value::Sequence(items) => {
            MIN_OBJECT_SIZE + items.iter().map(value_cost).sum::<usize>()
        }
        serde_yaml::Value::Mapping(mapping) => {
            MIN_OBJECT_SIZE
                + mapping
                    .iter()
                    .map(|(key, value)| string_cost(key) + value_cost(value))
                    .sum::<usize>()
        }
        serde_yaml::Value::Tagged(tagged) => value_cost(tagged.value()),
    }
}

fn string_cost(text: &str) -> usize {
    MIN_OBJECT_SIZE + STRING_BASE_OVERHEAD + 2 * text.encode_utf16().count()
}

/// The local reusable workflow files each job of `yaml` calls, one entry per
/// calling job (`jobs.<id>.uses: ./.github/workflows/<file>`).
fn local_callees(yaml: &str) -> Result<Vec<String>, GeneratorError> {
    let value = serde_yaml::from_str::<serde_yaml::Value>(yaml).map_err(|error| {
        GeneratorError::usage(format!("template memory estimate: parse YAML: {error}"))
    })?;
    let Some(jobs) = value.get("jobs").and_then(serde_yaml::Value::as_mapping) else {
        return Ok(Vec::new());
    };
    let prefix = format!("./{WORKFLOWS_DIR}/");
    Ok(jobs
        .values()
        .filter_map(|job| job.get("uses").and_then(serde_yaml::Value::as_str))
        .filter_map(|uses| uses.strip_prefix(prefix.as_str()))
        .map(str::to_owned)
        .collect())
}

/// Account every generated workflow that calls a local reusable workflow.
/// Callee costs come from the generated set, so the account reflects exactly
/// the tree about to be written.
///
/// # Errors
/// Returns a usage error when a workflow is not valid YAML or a job calls a
/// local reusable workflow the generated set does not contain.
pub(crate) fn template_memory_reports(
    files: &BTreeMap<PathBuf, String>,
) -> Result<Vec<TemplateMemoryReport>, GeneratorError> {
    let workflows_dir = Path::new(WORKFLOWS_DIR);
    let mut callee_costs = BTreeMap::<String, usize>::new();
    let mut reports = Vec::new();
    for (path, content) in files {
        if path.parent() != Some(workflows_dir)
            || path.extension().is_none_or(|extension| extension != "yml")
        {
            continue;
        }
        let callees = local_callees(content)?;
        if callees.is_empty() {
            continue;
        }
        let mut report = TemplateMemoryReport {
            workflow: path.clone(),
            own: template_memory_estimate(content)?,
            callees: BTreeMap::new(),
        };
        for callee in callees {
            let cost = if let Some(cost) = callee_costs.get(&callee) {
                *cost
            } else {
                let callee_path = workflows_dir.join(&callee);
                let Some(content) = files.get(&callee_path) else {
                    return Err(GeneratorError::usage(format!(
                        "{} calls ./{} which the generated surface does not contain",
                        path.display(),
                        callee_path.display()
                    )));
                };
                let cost = template_memory_estimate(content)?;
                callee_costs.insert(callee.clone(), cost);
                cost
            };
            let entry = report.callees.entry(callee).or_insert((0, cost));
            entry.0 += 1;
        }
        reports.push(report);
    }
    Ok(reports)
}

/// Refuse a generated surface whose expanded template cost passes
/// [`TEMPLATE_MEMORY_CEILING`] for any aggregate. The error names the
/// aggregate, its total, and the largest contributors so the offending callee
/// is visible without re-deriving the account.
///
/// # Errors
/// Returns a usage error for the first aggregate over the ceiling, or when the
/// account itself cannot be computed.
pub(crate) fn validate_template_memory(
    files: &BTreeMap<PathBuf, String>,
) -> Result<Vec<TemplateMemoryReport>, GeneratorError> {
    let reports = template_memory_reports(files)?;
    if let Some(report) = reports
        .iter()
        .find(|report| report.total() > TEMPLATE_MEMORY_CEILING)
    {
        let mut message = format!(
            "{} would expand to {} of GitHub template memory, over the generator ceiling of {} (GitHub fails the run at 10 MiB with `Maximum object size exceeded`); largest contributors:",
            report.workflow.display(),
            format_bytes(report.total()),
            format_bytes(TEMPLATE_MEMORY_CEILING),
        );
        for (label, cost) in report.contributors().into_iter().take(5) {
            let _ = write!(message, "\n  {label}: {}", format_bytes(cost));
        }
        return Err(GeneratorError::usage(message));
    }
    Ok(reports)
}

/// `1.23 MiB` / `456.7 KiB` for report lines.
pub(crate) fn format_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "byte counts far below 2^53 render as human-readable sizes"
    )]
    let value = bytes as f64;
    if value >= MIB {
        format!("{:.2} MiB", value / MIB)
    } else {
        format!("{:.1} KiB", value / KIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn estimate_mirrors_runner_token_costs() {
        // `{a: b}`: mapping 24 + key (24 + 26 + 2) + value (24 + 26 + 2).
        assert_eq!(
            must(template_memory_estimate("a: b\n"), "estimate"),
            24 + 52 + 52
        );
        // Scalars that parse as non-strings cost the bare object size.
        assert_eq!(
            must(
                template_memory_estimate("- 1\n- true\n- null\n"),
                "estimate"
            ),
            24 + 3 * 24
        );
        // UTF-16 code units, not bytes: `é` is one unit, `😀` is two.
        assert_eq!(
            must(template_memory_estimate("k: é😀\n"), "estimate"),
            24 + (24 + 26 + 2) + (24 + 26 + 2 * 3)
        );
        // An expression is a string like any other.
        assert_eq!(
            must(template_memory_estimate("k: ${{ inputs.x }}\n"), "estimate"),
            24 + (24 + 26 + 2) + (24 + 26 + 2 * "${{ inputs.x }}".len())
        );
    }

    #[test]
    fn expanded_cost_counts_a_callee_once_per_caller() {
        let callee = format!(
            "name: callee\non:\n  workflow_call: {{}}\njobs:\n  verify:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: |\n          {}\n",
            "x".repeat(200_000)
        );
        let mut aggregate = String::from("name: aggregate\non: push\njobs:\n");
        for index in 0..30 {
            let _ = writeln!(
                aggregate,
                "  call-{index}:\n    uses: ./.github/workflows/callee.yml\n    with:\n      unit: u{index}"
            );
        }
        let mut files = BTreeMap::new();
        files.insert(
            PathBuf::from(".github/workflows/callee.yml"),
            callee.clone(),
        );
        files.insert(
            PathBuf::from(".github/workflows/aggregate.yml"),
            aggregate.clone(),
        );

        let reports = must(template_memory_reports(&files), "reports");
        assert_eq!(reports.len(), 1, "only the aggregate calls a reusable");
        let report = &reports[0];
        let callee_cost = must(template_memory_estimate(&callee), "callee cost");
        assert_eq!(report.callees.get("callee.yml"), Some(&(30, callee_cost)));
        assert_eq!(
            report.total(),
            must(template_memory_estimate(&aggregate), "aggregate cost") + 30 * callee_cost
        );
        assert!(
            report.total() > TEMPLATE_MEMORY_CEILING,
            "{}",
            report.total()
        );

        let error = must_fail(
            validate_template_memory(&files),
            "a 30 × 400 KiB expansion must be refused",
        );
        assert!(
            error.contains(".github/workflows/aggregate.yml would expand to"),
            "{error}"
        );
        assert!(
            error.contains("over the generator ceiling of 5.00 MiB"),
            "{error}"
        );
        assert!(
            error.contains(".github/workflows/callee.yml × 30 callers:"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_local_callee_is_an_error() {
        let mut files = BTreeMap::new();
        files.insert(
            PathBuf::from(".github/workflows/aggregate.yml"),
            "name: a\non: push\njobs:\n  call:\n    uses: ./.github/workflows/missing.yml\n"
                .to_owned(),
        );
        let error = must_fail(
            template_memory_reports(&files),
            "a missing callee must be refused",
        );
        assert!(error.contains("missing.yml"), "{error}");
    }

    #[test]
    fn a_small_surface_passes_and_reports_each_aggregate() {
        let callee = "name: c\non:\n  workflow_call: {}\njobs:\n  verify:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n";
        let aggregate = "name: a\non: push\njobs:\n  one:\n    uses: ./.github/workflows/c.yml\n  two:\n    uses: ./.github/workflows/c.yml\n";
        let mut files = BTreeMap::new();
        files.insert(PathBuf::from(".github/workflows/c.yml"), callee.to_owned());
        files.insert(
            PathBuf::from(".github/workflows/a.yml"),
            aggregate.to_owned(),
        );
        files.insert(
            PathBuf::from(".github/actionlint.yaml"),
            "self-hosted-runner: {}\n".to_owned(),
        );
        let reports = must(validate_template_memory(&files), "small surface passes");
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].workflow,
            PathBuf::from(".github/workflows/a.yml")
        );
        assert_eq!(
            reports[0].callees.get("c.yml").map(|(callers, _)| *callers),
            Some(2)
        );
    }
}

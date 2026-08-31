//! Fleet audit.
//!
//! Regenerates every composite building-block action and every template (and, once
//! the block SHA is bound, every callable workflow) in memory, compares against the
//! committed bytes, materializes all 28 repositories to prove each equals its class
//! template, and enforces the closure, owner-fan-out, aggregation, gate, and routing
//! invariants. Any one-byte hand edit to a generated file — including a neutered
//! composite run-script body — fails the audit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cache::CacheContract;
use crate::composite;
use crate::forks::ForkTable;
use crate::model::{FleetManifest, RepositoryKind, is_sha40};
use crate::package::{self, PackagePolicy};
use crate::releases::ReleaseTable;
use crate::render::{self, ACTIONS_REPO, CALVER_PLACEHOLDER, FLEET_SHA_PLACEHOLDER};
use crate::tools::{self, ToolRegistry};
use crate::{ALL_CLASSES, RepositoryClass};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REMOTE_CLOSURE_SCHEMA: u32 = 1;
const DEFERRED_MAX_AGE_DAYS: u64 = 180;
const ZIZMOR_FIRST_FINDINGS_EXIT_CODE: i32 = 11;
const ZIZMOR_LAST_FINDINGS_EXIT_CODE: i32 = 14;

#[derive(Debug, Deserialize)]
struct RemoteClosure {
    schema_version: u32,
    action: Vec<RemoteAction>,
}

#[derive(Debug, Deserialize)]
struct RemoteAction {
    root: String,
    sha: String,
    files: Vec<RemoteFile>,
}

#[derive(Debug, Deserialize)]
struct RemoteFile {
    path: String,
    kind: String,
    sha256: String,
}

/// A deterministic disposable release identity used only to exercise
/// materialization during the audit (never written anywhere).
const AUDIT_RELEASE_SHA: &str = "0000000000000000000000000000000000000000";
const AUDIT_CALVER: &str = "2026.7.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdoptionState {
    Adopted,
    Unadopted,
}

struct ScratchGuard(Option<PathBuf>);

impl ScratchGuard {
    fn new() -> Result<Self, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system clock before Unix epoch: {e}"))?;
        let path = std::env::temp_dir().join(format!(
            "velnor-actions-fleet-audit-{}-{}",
            std::process::id(),
            now.as_nanos()
        ));
        std::fs::create_dir(&path)
            .map_err(|e| format!("creating audit scratch {}: {e}", path.display()))?;
        Ok(Self(Some(path)))
    }

    fn path(&self) -> &Path {
        self.0.as_deref().expect("scratch guard has a path")
    }
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// Run the scheduled census audit. `offline` retains deterministic policy and
/// aggregate checks for local gates; the generated workflow always uses the
/// online path, which shallow-clones every registered repository.
pub fn fleet_audit(root: &Path, write_deferred: bool, offline: bool) -> Result<String, String> {
    // `audit` is the canonical regen-into-memory-and-diff proof. Keeping it in
    // this scheduled path prevents the fleet audit from checking remote repos
    // against stale local generator outputs.
    audit(root)?;
    let manifest = FleetManifest::load(root)?;
    let forks = ForkTable::load(root)?;
    let mut deferred = Vec::new();
    let scratch = (!offline).then(ScratchGuard::new).transpose()?;
    for repository in manifest.repositories() {
        let checkout = scratch
            .as_ref()
            .map(|scratch| scratch.path().join(repository.slug.replace('/', "--")));
        if !offline {
            let checkout = checkout.as_deref().expect("online audit has scratch");
            let status = Command::new("git")
                .args([
                    "clone",
                    "--depth=1",
                    "--no-tags",
                    &format!("https://github.com/{}", repository.slug),
                ])
                .arg(checkout)
                .status()
                .map_err(|e| format!("cloning {}: {e}", repository.slug))?;
            if !status.success() {
                return Err(format!("shallow clone failed for {}", repository.slug));
            }
            if repository.census.kind == RepositoryKind::OutOfScope {
                eprintln!(
                    "fleet audit: skipping adoption and tool invariants for out-of-scope {}",
                    repository.slug
                );
                continue;
            }
            let adoption = cross_check_local_repo(repository, checkout)?;
            if let Err(error) = audit_cloned_consumer(repository, checkout, &forks) {
                if adoption == AdoptionState::Adopted {
                    return Err(error);
                }
                eprintln!("fleet audit warning: {error}");
            }
            run_fleet_tool(
                repository,
                checkout,
                adoption,
                "repolint",
                &["check"],
                false,
            )?;
            run_fleet_tool(
                repository,
                checkout,
                adoption,
                "alint",
                if adoption == AdoptionState::Adopted {
                    &["check", "--fail-on-warning"]
                } else {
                    &["check"]
                },
                true,
            )?;
            run_fleet_tool(
                repository,
                checkout,
                adoption,
                "zizmor",
                &[".github/workflows"],
                false,
            )?;
            if adoption == AdoptionState::Unadopted {
                enforce_first_seen(repository)?;
            }
            deferred.extend(read_deferred(repository, checkout)?);
        } else {
            if repository.census.kind != RepositoryKind::OutOfScope {
                enforce_first_seen(repository)?;
            }
        }
    }
    if !offline {
        // ReleaseTable is the source of the current CalVer and signer slots;
        // release_check verifies the same table against every owner mirror's
        // immutable tag/tree before the aggregate is accepted.
        release_check(root, None)?;
    }
    let aggregate = render_deferred_aggregate(&deferred);
    let aggregate_path = root.join("fleet").join("deferred.toml");
    if write_deferred {
        std::fs::write(&aggregate_path, &aggregate)
            .map_err(|e| format!("writing {}: {e}", aggregate_path.display()))?;
    } else {
        let committed = std::fs::read_to_string(&aggregate_path)
            .map_err(|e| format!("reading {}: {e}", aggregate_path.display()))?;
        if committed != aggregate {
            return Err(format!(
                "{}: generated deferred aggregate is stale; rerun fleet-audit --write-deferred",
                aggregate_path.display()
            ));
        }
    }
    Ok(format!(
        "fleet audit valid: {} repositories, {} deferred items",
        manifest.repositories().len(),
        deferred.len()
    ))
}

fn audit_cloned_consumer(
    repository: &crate::model::Repository,
    checkout: &Path,
    forks: &ForkTable,
) -> Result<(), String> {
    let path = checkout.join(".github").join("workflows").join("ci.yml");
    let actual = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "{}: reading cloned consumer {}: {e}",
            repository.slug,
            path.display()
        )
    })?;
    let (shas, calver) = materialized_binding(&actual, repository.class, forks, &repository.slug)?;
    let template = render::consumer_template_for(repository.class, forks);
    let refs: Vec<&str> = shas.iter().map(String::as_str).collect();
    let expected = render::render_consumer_for(&template, forks, &refs, &calver)?;
    require_equal(
        &actual,
        &expected,
        &format!("{}:.github/workflows/ci.yml", repository.slug),
    )
}

fn materialized_binding(
    actual: &str,
    class: RepositoryClass,
    forks: &ForkTable,
    repository: &str,
) -> Result<(Vec<String>, String), String> {
    let file = render::callable_file_name(class);
    let mut shas = Vec::with_capacity(forks.len());
    let mut calver = None;
    for fork in forks.forks() {
        let prefix = format!(
            "uses: {}/{}/.github/workflows/{file}@",
            fork.owner(),
            ACTIONS_REPO
        );
        let rows: Vec<_> = actual
            .lines()
            .map(str::trim_start)
            .filter_map(|line| line.strip_prefix(&prefix))
            .collect();
        if rows.len() != 1 {
            return Err(format!(
                "{repository}: cloned ci.yml must contain exactly one {prefix}<sha> # <calver> call"
            ));
        }
        let (sha, version) = rows[0].split_once(" # ").ok_or_else(|| {
            format!("{repository}: cloned {prefix} call must bind SHA and CalVer together")
        })?;
        if !is_sha40(sha) || version.is_empty() || version.contains('#') {
            return Err(format!(
                "{repository}: cloned {prefix} call has invalid SHA/CalVer binding"
            ));
        }
        if let Some(previous) = &calver {
            if previous != version {
                return Err(format!(
                    "{repository}: cloned ci.yml uses mixed CalVer release bindings"
                ));
            }
        } else {
            calver = Some(version.to_owned());
        }
        shas.push(sha.to_owned());
    }
    Ok((
        shas,
        calver.ok_or_else(|| format!("{repository}: cloned ci.yml has no release binding"))?,
    ))
}

fn cross_check_local_repo(
    repository: &crate::model::Repository,
    checkout: &Path,
) -> Result<AdoptionState, String> {
    let config_path = checkout.join("repolint.toml");
    if !config_path.is_file() {
        return Ok(AdoptionState::Unadopted);
    }
    let body = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("reading {}: {e}", config_path.display()))?;
    let document: toml::Value =
        toml::from_str(&body).map_err(|e| format!("parsing {}: {e}", config_path.display()))?;
    let Some(repo) = document.get("repo").and_then(toml::Value::as_table) else {
        return Ok(AdoptionState::Unadopted);
    };
    let expected = [
        ("tier", repository.census.tier.token().to_ascii_lowercase()),
        ("kind", repository.census.kind.token().to_ascii_lowercase()),
        (
            "visibility",
            repository.census.visibility.token().to_owned(),
        ),
    ];
    for (field, expected) in expected {
        let actual = repo
            .get(field)
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("{}: adopted [repo] is missing {field}", repository.slug))?;
        if actual != expected {
            return Err(format!(
                "{}: [repo].{field}={actual:?} disagrees with census {expected:?}",
                repository.slug
            ));
        }
    }
    let actual_research = repo
        .get("research")
        .and_then(toml::Value::as_bool)
        .ok_or_else(|| format!("{}: adopted [repo] is missing research", repository.slug))?;
    if actual_research != repository.census.research {
        return Err(format!(
            "{}: [repo].research disagrees with census",
            repository.slug
        ));
    }
    Ok(AdoptionState::Adopted)
}

fn run_fleet_tool(
    repository: &crate::model::Repository,
    checkout: &Path,
    adoption: AdoptionState,
    tool: &str,
    args: &[&str],
    alint_allows_missing_config: bool,
) -> Result<(), String> {
    let status = Command::new(tool)
        .args(args)
        .current_dir(checkout)
        .status()
        .map_err(|e| format!("running {tool} for {}: {e}", repository.slug))?;
    if status.success() {
        return Ok(());
    }
    let code = status.code();
    let missing_alint_config = tool == "alint"
        && alint_allows_missing_config
        && !checkout.join(".alint.yml").is_file()
        && code == Some(2);
    let pre_adoption_warning = is_pre_adoption_warning(adoption, tool, code, missing_alint_config);
    if pre_adoption_warning {
        eprintln!(
            "fleet audit warning: {tool} reported for {}",
            repository.slug
        );
        return Ok(());
    }
    Err(format!(
        "{tool} failed for {} with exit status {code:?}",
        repository.slug
    ))
}

fn is_pre_adoption_warning(
    adoption: AdoptionState,
    tool: &str,
    code: Option<i32>,
    missing_alint_config: bool,
) -> bool {
    adoption == AdoptionState::Unadopted
        && ((tool != "zizmor" && code == Some(1))
            || (tool == "zizmor"
                && code.is_some_and(|code| {
                    (ZIZMOR_FIRST_FINDINGS_EXIT_CODE..=ZIZMOR_LAST_FINDINGS_EXIT_CODE)
                        .contains(&code)
                }))
            || missing_alint_config)
}

fn read_deferred(
    repository: &crate::model::Repository,
    checkout: &Path,
) -> Result<Vec<toml::Value>, String> {
    let path = checkout.join("repolint.toml");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let body =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let document: toml::Value =
        toml::from_str(&body).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    let rows = document
        .get("deferred")
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("item"))
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let today = today_days()?;
    rows.into_iter()
        .map(|mut row| {
            validate_deferred_row(repository, checkout, &row, today)?;
            let table = row
                .as_table_mut()
                .ok_or_else(|| format!("{}: deferred item is not a table", repository.slug))?;
            table.insert(
                "repository".to_owned(),
                toml::Value::String(repository.slug.clone()),
            );
            Ok(row)
        })
        .collect()
}

fn validate_deferred_row(
    repository: &crate::model::Repository,
    checkout: &Path,
    row: &toml::Value,
    today: u64,
) -> Result<(), String> {
    let table = row
        .as_table()
        .ok_or_else(|| format!("{}: deferred item is not a table", repository.slug))?;
    const ALLOWED_FIELDS: [&str; 8] = [
        "item",
        "registered",
        "last_deferred",
        "reason",
        "effort",
        "trigger",
        "blocking_gate",
        "steps",
    ];
    if let Some(field) = table
        .keys()
        .find(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
    {
        return Err(format!(
            "{}: deferred item has unknown field {field:?}",
            repository.slug
        ));
    }
    let text = |field: &str| {
        table
            .get(field)
            .and_then(toml::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("{}: deferred item missing {field}", repository.slug))
    };
    let item = text("item")?;
    let registered = text("registered")?;
    let last_deferred = text("last_deferred")?;
    let _reason = text("reason")?;
    let effort = text("effort")?;
    let trigger = text("trigger")?;
    let registered_days = date_days(registered)?;
    let last_deferred_days = date_days(last_deferred)?;
    if registered_days > today || last_deferred_days > today {
        return Err(format!(
            "{}: deferred item {item:?} has a future date",
            repository.slug
        ));
    }
    if last_deferred_days < registered_days {
        return Err(format!(
            "{}: deferred item {item:?} last_deferred predates registered",
            repository.slug
        ));
    }
    if let Some(blocking_gate) = table.get("blocking_gate")
        && (!blocking_gate.is_str()
            || blocking_gate
                .as_str()
                .is_some_and(|value| value.trim().is_empty()))
    {
        return Err(format!(
            "{}: deferred item {item:?} has an invalid blocking_gate",
            repository.slug
        ));
    }
    match effort {
        "S" | "M" => {
            if today.saturating_sub(registered_days) > DEFERRED_MAX_AGE_DAYS {
                return Err(format!(
                    "{}: deferred item {item:?} is older than {DEFERRED_MAX_AGE_DAYS} days",
                    repository.slug
                ));
            }
            if table.contains_key("steps") {
                return Err(format!(
                    "{}: S/M deferred item {item:?} cannot declare steps",
                    repository.slug
                ));
            }
            let trigger = validate_trigger(trigger, repository, item)?;
            if !checkout_contains_trigger(checkout, trigger)? {
                return Err(format!(
                    "{}: deferred item {item:?} trigger {trigger:?} is stale",
                    repository.slug
                ));
            }
        }
        "L" => {
            let steps = table
                .get("steps")
                .and_then(toml::Value::as_array)
                .filter(|steps| !steps.is_empty())
                .ok_or_else(|| {
                    format!(
                        "{}: L deferred item {item:?} requires non-empty steps",
                        repository.slug
                    )
                })?;
            for (index, step) in steps.iter().enumerate() {
                let step = step.as_table().ok_or_else(|| {
                    format!(
                        "{}: deferred item {item:?} step {index} is not a table",
                        repository.slug
                    )
                })?;
                if let Some(field) = step
                    .keys()
                    .find(|field| !["step", "date"].contains(&field.as_str()))
                {
                    return Err(format!(
                        "{}: deferred item {item:?} step {index} has unknown field {field:?}",
                        repository.slug
                    ));
                }
                let step_name = step
                    .get("step")
                    .and_then(toml::Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        format!(
                            "{}: deferred item {item:?} step {index} missing step",
                            repository.slug
                        )
                    })?;
                let date = step
                    .get("date")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(|| {
                        format!(
                            "{}: deferred item {item:?} step {index} missing date",
                            repository.slug
                        )
                    })?;
                let date = date_days(date)?;
                if date <= today {
                    return Err(format!(
                        "{}: deferred item {item:?} step {step_name:?} is overdue",
                        repository.slug
                    ));
                }
            }
        }
        other => {
            return Err(format!(
                "{}: deferred item {item:?} has invalid effort {other:?}",
                repository.slug
            ));
        }
    }
    Ok(())
}

fn validate_trigger<'a>(
    trigger: &'a str,
    repository: &crate::model::Repository,
    item: &str,
) -> Result<&'a str, String> {
    if trigger.starts_with('/')
        || trigger.contains('\n')
        || trigger
            .split('/')
            .any(|part| part.is_empty() || part == "..")
    {
        return Err(format!(
            "{}: deferred item {item:?} has unsafe trigger {trigger:?}",
            repository.slug
        ));
    }
    Ok(trigger
        .strip_prefix("./")
        .unwrap_or(trigger)
        .trim_end_matches('/'))
}

fn checkout_contains_trigger(checkout: &Path, trigger: &str) -> Result<bool, String> {
    if trigger.is_empty() || trigger == "." {
        return Ok(true);
    }
    let mut paths = Vec::new();
    collect_checkout_paths(checkout, checkout, &mut paths)?;
    Ok(paths.iter().any(|path| glob_matches(trigger, path)))
}

fn collect_checkout_paths(
    root: &Path,
    current: &Path,
    paths: &mut Vec<String>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(current).map_err(|e| {
        format!(
            "reading deferred target directory {}: {e}",
            current.display()
        )
    })? {
        let entry = entry.map_err(|e| format!("reading deferred target entry: {e}"))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|e| format!("relative deferred target path: {e}"))?;
        if relative.components().next() == Some(Component::Normal(std::ffi::OsStr::new(".git"))) {
            continue;
        }
        let relative = relative
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        paths.push(relative);
        if path.is_dir() && !path.is_symlink() {
            collect_checkout_paths(root, &path, paths)?;
        }
    }
    Ok(())
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    fn matches(pattern: &[u8], value: &[u8], p: usize, v: usize) -> bool {
        if p == pattern.len() {
            return v == value.len();
        }
        if pattern[p] == b'*' {
            let double = p + 1 < pattern.len() && pattern[p + 1] == b'*';
            if double {
                let next = p + 2;
                if next < pattern.len()
                    && pattern[next] == b'/'
                    && matches(pattern, value, next + 1, v)
                {
                    return true;
                }
                return matches(pattern, value, next, v)
                    || (v < value.len() && matches(pattern, value, p, v + 1));
            }
            return matches(pattern, value, p + 1, v)
                || (v < value.len() && value[v] != b'/' && matches(pattern, value, p, v + 1));
        }
        if pattern[p] == b'?' {
            return v < value.len() && value[v] != b'/' && matches(pattern, value, p + 1, v + 1);
        }
        v < value.len() && pattern[p] == value[v] && matches(pattern, value, p + 1, v + 1)
    }
    matches(pattern.as_bytes(), value.as_bytes(), 0, 0)
}

fn render_deferred_aggregate(rows: &[toml::Value]) -> String {
    let mut output = String::from(
        "# Generated by velnor-actions-generator fleet-audit. DO NOT EDIT.\n\nschema = 1\n",
    );
    for row in rows {
        output.push('\n');
        output.push_str("[[item]]\n");
        if let Some(table) = row.as_table() {
            for (key, value) in table {
                output.push_str(&format!("{key} = {}\n", toml_value(value)));
            }
        }
        output.push('\n');
    }
    output
}

fn toml_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(value) => format!("{value:?}"),
        toml::Value::Integer(value) => value.to_string(),
        toml::Value::Float(value) => value.to_string(),
        toml::Value::Boolean(value) => value.to_string(),
        toml::Value::Datetime(value) => value.to_string(),
        toml::Value::Array(values) => format!(
            "[{}]",
            values.iter().map(toml_value).collect::<Vec<_>>().join(", ")
        ),
        toml::Value::Table(values) => format!(
            "{{ {} }}",
            values
                .iter()
                .map(|(key, value)| format!("{key} = {}", toml_value(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn enforce_first_seen(repository: &crate::model::Repository) -> Result<(), String> {
    let today = today_days()?;
    let seen = date_days(&repository.census.first_seen)?;
    if today.saturating_sub(seen) > DEFERRED_MAX_AGE_DAYS {
        return Err(format!(
            "{}: un-adopted census row is older than 180 days",
            repository.slug
        ));
    }
    Ok(())
}

fn date_days(value: &str) -> Result<u64, String> {
    if value.len() != 10
        || value.as_bytes().get(4) != Some(&b'-')
        || value.as_bytes().get(7) != Some(&b'-')
        || !value
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return Err(format!("invalid date {value:?}"));
    }
    let year: i64 = value[0..4]
        .parse()
        .map_err(|_| format!("invalid date {value:?}"))?;
    let month: i64 = value[5..7]
        .parse()
        .map_err(|_| format!("invalid date {value:?}"))?;
    let day: i64 = value[8..10]
        .parse()
        .map_err(|_| format!("invalid date {value:?}"))?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if !(1..=12).contains(&month) || !(1..=days_in_month).contains(&day) {
        return Err(format!("invalid date {value:?}"));
    }
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Ok((era * 146_097 + day_of_era - 719_468) as u64)
}

fn today_days() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock before Unix epoch: {e}"))
        .map(|duration| duration.as_secs() / 86_400)
}

/// Run the full fleet audit under `root`. Returns the exact summary line on
/// success.
///
/// # Errors
///
/// Returns `Err` on any contract, byte, closure, owner-fan-out, aggregation,
/// gate, or routing violation.
pub fn audit(root: &Path) -> Result<String, String> {
    let forks = ForkTable::load(root)?;
    let manifest = FleetManifest::load(root)?;
    let caches = CacheContract::load(&root.join("fleet").join("caches.toml"))?;
    let packages = PackagePolicy::load(root, &forks)?;
    let registry = ToolRegistry::load(&tools::registry_path(root))?;
    let remote_closure = load_remote_closure(root)?;
    let releases = ReleaseTable::load(root)?;
    audit_release_table_bindings(&packages, &releases, forks.len())?;

    // Composite building blocks must exist and match their canonical bytes exactly
    // (body included), so a neutered run-script fails the audit.
    for name in composite::COMPOSITE_NAMES {
        check_composite(root, name)?;
    }

    // Every class template: regenerate and compare committed bytes.
    for class in ALL_CLASSES {
        let rendered = render::consumer_template_for(class, &forks);
        let committed = read_committed(&template_path(root, class))?;
        require_equal(&committed, &rendered, &template_path_display(class))?;
        audit_consumer_structure(class, &rendered, &forks)?;
        let alint_path = root.join("templates").join(class.code()).join(".alint.yml");
        let alint = read_committed(&alint_path)?;
        require_equal(
            &alint,
            &crate::policy::alint_config(class),
            &alint_path.display().to_string(),
        )?;
    }

    let fleet_audit_workflow = root.join(".github/workflows/fleet-audit.yml");
    let committed = read_committed(&fleet_audit_workflow)?;
    require_equal(
        &committed,
        crate::policy::FLEET_AUDIT_WORKFLOW,
        &fleet_audit_workflow.display().to_string(),
    )?;

    let tools_template = root.join("templates").join("tools").join("mise.toml");
    let tool_names = registry.entries().keys().map(String::as_str);
    let expected_tools = registry.render_tools_block(tool_names)?;
    let committed_tools = read_committed(&tools_template)?;
    require_equal(
        &committed_tools,
        &expected_tools,
        &tools_template.display().to_string(),
    )?;
    let mise_path = root.join("mise.toml");
    let lock_path = root.join("mise.lock");
    if mise_path.is_file() || lock_path.is_file() {
        if !(mise_path.is_file() && lock_path.is_file()) {
            return Err("mise.toml and mise.lock must appear together".into());
        }
        registry.check_generator_files(&mise_path, &lock_path)?;
    }

    // Materialize all 28 repositories and prove each equals its class template.
    audit_materialization(&manifest, &forks)?;

    // If the block SHA is bound, audit the full callable-workflow closure.
    let block_sha_path = root.join("fleet").join("block-sha");
    if block_sha_path.exists() {
        let block_sha = read_block_sha(&block_sha_path)?;
        for class in ALL_CLASSES {
            let contract = manifest.class(class);
            let rendered = render::callable_workflow_for(contract, &caches, &block_sha, &forks);
            let committed = read_committed(&callable_path(root, class))?;
            require_equal(&committed, &rendered, &callable_path_display(class))?;
            audit_callable_structure(class, &rendered, &block_sha, &remote_closure, &forks)?;
        }
        let updater = packages.render_updater();
        for (path, expected) in [
            (
                root.join(".github/workflows/package-signer.yml"),
                package::SIGNER_WORKFLOW,
            ),
            (
                root.join(".github/workflows/package-updater.yml"),
                updater.as_str(),
            ),
            (
                root.join("templates/tap/package-update.yml"),
                package::TAP_TEMPLATE,
            ),
            (
                root.join("templates/apt/package-update.yml"),
                package::APT_TEMPLATE,
            ),
        ] {
            let committed = read_committed(&path)?;
            require_equal(&committed, expected, &path.display().to_string())?;
            if path.starts_with(root.join(".github/workflows")) {
                audit_immutable_uses(expected, &path)?;
                audit_admitted_closure(
                    expected,
                    &block_sha,
                    &path.display().to_string(),
                    &remote_closure,
                    forks.canonical_owner(),
                )?;
            }
        }
    }

    Ok(format!(
        "fleet valid: {} repositories, {} classes, {} templates",
        manifest.repositories().len(),
        manifest.classes().len(),
        ALL_CLASSES.len(),
    ))
}

fn audit_release_table_bindings(
    packages: &PackagePolicy,
    releases: &ReleaseTable,
    fork_count: usize,
) -> Result<(), String> {
    let release = releases.current();
    let release_shas = [AUDIT_RELEASE_SHA; 3];
    if fork_count != release_shas.len() {
        return Err(format!(
            "release table binding fixture has {} forks, expected {}",
            fork_count,
            release_shas.len()
        ));
    }
    for slot in release.signers() {
        let rendered = packages.render_consumer(slot.consumer(), release_shas, release.calver())?;
        if !rendered.contains(slot.current_digest()) {
            return Err(format!(
                "release {} current signer digest for {} is not rendered",
                release.calver(),
                slot.consumer()
            ));
        }
        if let Some(old_digest) = slot.old_digest()
            && !rendered.contains(old_digest)
        {
            return Err(format!(
                "release {} old signer digest for {} is not rendered",
                release.calver(),
                slot.consumer()
            ));
        }
    }
    Ok(())
}

fn audit_immutable_uses(workflow: &str, path: &Path) -> Result<(), String> {
    for reference in uses_refs(workflow) {
        if !is_sha40(&reference) {
            return Err(format!(
                "{}: non-40-hex or mutable ref {reference:?}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn load_remote_closure(root: &Path) -> Result<RemoteClosure, String> {
    let path = root.join("fleet").join("remote-actions.toml");
    let bytes = std::fs::read_to_string(&path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let closure: RemoteClosure =
        toml::from_str(&bytes).map_err(|error| format!("parsing {}: {error}", path.display()))?;
    if closure.schema_version != REMOTE_CLOSURE_SCHEMA || closure.action.is_empty() {
        return Err("remote action closure has unsupported schema or no actions".to_string());
    }
    let mut roots = BTreeSet::new();
    for action in &closure.action {
        if action.root.split('/').count() != 2 || !is_sha40(&action.sha) || action.files.is_empty()
        {
            return Err(format!(
                "invalid remote action identity {}@{}",
                action.root, action.sha
            ));
        }
        if !roots.insert((&action.root, &action.sha)) {
            return Err(format!(
                "duplicate remote action {}@{}",
                action.root, action.sha
            ));
        }
        let mut paths = BTreeSet::new();
        let mut manifests = 0;
        for file in &action.files {
            validate_relative_path(&file.path)?;
            if !paths.insert(&file.path) {
                return Err(format!(
                    "duplicate remote closure path {}/{}",
                    action.root, file.path
                ));
            }
            match file.kind.as_str() {
                "manifest" => manifests += 1,
                "behavior" => {}
                other => return Err(format!("unknown remote closure kind {other:?}")),
            }
            if file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(format!(
                    "malformed SHA-256 for {}/{}",
                    action.root, file.path
                ));
            }
        }
        if manifests == 0 {
            return Err(format!("{} has no manifest", action.root));
        }
    }
    Ok(closure)
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains('\n')
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe remote closure path {path:?}"));
    }
    Ok(())
}

/// Fetch every admitted file from its immutable commit, verify SHA-256, parse
/// each action manifest, and prove its exact Node `main`/`pre`/`post` closure.
pub fn verify_remote_closure(root: &Path) -> Result<String, String> {
    let closure = load_remote_closure(root)?;
    let mut handles = Vec::new();
    for action in &closure.action {
        for file in &action.files {
            let action_root = action.root.clone();
            let sha = action.sha.clone();
            let path = file.path.clone();
            let expected = file.sha256.clone();
            handles.push(std::thread::spawn(move || {
                let endpoint = format!("repos/{action_root}/contents/{path}?ref={sha}");
                let child = Command::new("gh")
                    .args([
                        "api",
                        "-H",
                        "Accept: application/vnd.github.raw+json",
                        &endpoint,
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|error| format!("executing gh for {endpoint}: {error}"))?;
                let output = child
                    .wait_with_output()
                    .map_err(|error| format!("waiting for gh for {endpoint}: {error}"))?;
                if !output.status.success() {
                    return Err(format!(
                        "fetching {endpoint} failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }
                let observed = hex::encode(Sha256::digest(&output.stdout));
                if observed != expected {
                    return Err(format!(
                        "remote closure hash mismatch for {action_root}/{path}: expected {expected}, observed {observed}"
                    ));
                }
                Ok(((action_root, path), output.stdout))
            }));
        }
    }
    let mut fetched = BTreeMap::new();
    for handle in handles {
        let (identity, bytes) = handle
            .join()
            .map_err(|_| "remote closure fetch worker panicked".to_string())??;
        if fetched.insert(identity.clone(), bytes).is_some() {
            return Err(format!("duplicate fetched remote file {identity:?}"));
        }
    }
    for action in &closure.action {
        let declared_behaviors = action
            .files
            .iter()
            .filter(|file| file.kind == "behavior")
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        let mut derived_behaviors = BTreeSet::new();
        let mut derived_actions = BTreeSet::new();
        for manifest in action.files.iter().filter(|file| file.kind == "manifest") {
            let raw = fetched
                .get(&(action.root.clone(), manifest.path.clone()))
                .ok_or_else(|| {
                    format!("missing fetched manifest {}/{}", action.root, manifest.path)
                })?;
            derive_manifest_closure(
                &manifest.path,
                raw,
                &mut derived_behaviors,
                &mut derived_actions,
            )?;
        }
        if derived_behaviors != declared_behaviors {
            return Err(format!(
                "remote executable closure mismatch for {}: derived {derived_behaviors:?}, declared {declared_behaviors:?}",
                action.root
            ));
        }
        for (root, sha) in derived_actions {
            let mut segments = root.split('/');
            let repository = format!(
                "{}/{}",
                segments.next().unwrap_or_default(),
                segments.next().unwrap_or_default()
            );
            let subpath = segments.collect::<Vec<_>>().join("/");
            let manifest = if subpath.is_empty() {
                "action.yml".to_string()
            } else {
                format!("{subpath}/action.yml")
            };
            if !closure.action.iter().any(|candidate| {
                candidate.root == repository
                    && candidate.sha == sha
                    && candidate
                        .files
                        .iter()
                        .any(|file| file.kind == "manifest" && file.path == manifest)
            }) {
                return Err(format!(
                    "remote composite closure omits nested action {root}@{sha}"
                ));
            }
        }
    }
    Ok(format!(
        "remote closure valid: {} actions",
        closure.action.len()
    ))
}

/// Verify that every declared owner-local fork has the same release tree as the
/// canonical owner at an exact CalVer tag.
pub fn release_check(root: &Path, requested: Option<&str>) -> Result<String, String> {
    let forks = ForkTable::load(root)?;
    let releases = ReleaseTable::load(root)?;
    let calver = requested.unwrap_or_else(|| releases.current().calver());
    if calver.is_empty() {
        return Err("release label must be non-empty".into());
    }
    if releases.by_calver(calver).is_none() {
        return Err(format!("release table has no row for {calver}"));
    }

    let mut trees = BTreeMap::new();
    for fork in forks.forks() {
        let endpoint = format!(
            "repos/{}/{}/git/ref/tags/{calver}",
            fork.owner(),
            forks.repository()
        );
        let tag_row = gh_api_jq(&endpoint, "[.object.type,.object.sha]|@tsv")?;
        let commit_sha = resolve_release_commit(fork.owner(), forks.repository(), &tag_row)?;
        let commit_endpoint = format!(
            "repos/{}/{}/git/commits/{commit_sha}",
            fork.owner(),
            forks.repository()
        );
        let tree_sha = gh_api_jq(&commit_endpoint, ".tree.sha")?;
        if !is_sha40(&tree_sha) {
            return Err(format!(
                "release {calver} returned an invalid tree SHA for {}",
                fork.owner()
            ));
        }
        trees.insert(fork.owner().to_string(), tree_sha);
    }
    let canonical_tree = trees
        .get(forks.canonical_owner())
        .ok_or_else(|| "canonical fork tree was not collected".to_string())?;
    let mismatches = trees
        .iter()
        .filter(|(owner, tree)| {
            owner.as_str() != forks.canonical_owner() && *tree != canonical_tree
        })
        .map(|(owner, tree)| format!("{owner}={tree}"))
        .collect::<Vec<_>>();
    if !mismatches.is_empty() {
        return Err(format!(
            "release {calver} fork trees differ from {}: {}",
            forks.canonical_owner(),
            mismatches.join(", ")
        ));
    }
    Ok(format!(
        "fork release valid: {calver} ({} owner trees byte-equal)",
        trees.len()
    ))
}

fn gh_api_jq(endpoint: &str, expression: &str) -> Result<String, String> {
    let output = Command::new("gh")
        .args(["api", endpoint, "--jq", expression])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("executing gh for {endpoint}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "fetching {endpoint} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value = String::from_utf8(output.stdout)
        .map_err(|error| format!("gh returned non-UTF-8 data for {endpoint}: {error}"))?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        return Err(format!("gh returned an empty value for {endpoint}"));
    }
    Ok(value.to_string())
}

fn resolve_release_commit(owner: &str, repository: &str, tag_row: &str) -> Result<String, String> {
    let mut fields = tag_row.split('\t');
    let mut object_type = fields.next().unwrap_or_default().to_string();
    let mut object_sha = fields.next().unwrap_or_default().to_string();
    if fields.next().is_some() || !is_sha40(&object_sha) {
        return Err(format!(
            "invalid release tag object for {owner}/{repository}"
        ));
    }
    for _ in 0..=2 {
        match object_type.as_str() {
            "commit" => return Ok(object_sha),
            "tag" => {
                let endpoint = format!("repos/{owner}/{repository}/git/tags/{object_sha}");
                let row = gh_api_jq(&endpoint, "[.object.type,.object.sha]|@tsv")?;
                let mut next = row.split('\t');
                object_type = next.next().unwrap_or_default().to_string();
                object_sha = next.next().unwrap_or_default().to_string();
                if next.next().is_some() || !is_sha40(&object_sha) {
                    return Err(format!(
                        "invalid annotated release tag for {owner}/{repository}"
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "release tag for {owner}/{repository} does not resolve to a commit"
                ));
            }
        }
    }
    Err(format!(
        "release tag for {owner}/{repository} is nested too deeply"
    ))
}

fn derive_manifest_closure(
    manifest_path: &str,
    raw: &[u8],
    output: &mut BTreeSet<String>,
    actions: &mut BTreeSet<(String, String)>,
) -> Result<(), String> {
    let text = std::str::from_utf8(raw)
        .map_err(|error| format!("manifest {manifest_path} is not UTF-8: {error}"))?;
    let documents = yaml_rust2::YamlLoader::load_from_str(text)
        .map_err(|error| format!("parsing remote manifest {manifest_path}: {error}"))?;
    let document = documents
        .first()
        .ok_or_else(|| format!("empty remote manifest {manifest_path}"))?;
    let using = document["runs"]["using"]
        .as_str()
        .ok_or_else(|| format!("manifest {manifest_path} has no string runs.using"))?;
    if using == "composite" {
        let steps = document["runs"]["steps"]
            .as_vec()
            .ok_or_else(|| format!("composite manifest {manifest_path} has no runs.steps"))?;
        for step in steps {
            let uses = step["uses"].as_str().ok_or_else(|| {
                format!("composite manifest {manifest_path} has a step without uses")
            })?;
            let (root, sha) = uses
                .split_once('@')
                .ok_or_else(|| format!("nested action {uses:?} is not immutable"))?;
            if root.split('/').count() < 2 || !is_sha40(sha) {
                return Err(format!(
                    "nested action {uses:?} is not an immutable remote action"
                ));
            }
            actions.insert((root.to_string(), sha.to_string()));
        }
        return Ok(());
    }
    if !matches!(using, "node20" | "node24") {
        return Err(format!(
            "manifest {manifest_path} uses unsupported runtime {using:?}; closure must fail closed"
        ));
    }
    let parent = Path::new(manifest_path).parent().unwrap_or(Path::new(""));
    for (field, required) in [("main", true), ("pre", false), ("post", false)] {
        let value = document["runs"][field].as_str();
        if required && value.is_none() {
            return Err(format!("manifest {manifest_path} has no runs.{field}"));
        }
        if let Some(value) = value {
            let joined = normalize_join(parent, Path::new(value))?;
            output.insert(joined);
        }
    }
    Ok(())
}

fn normalize_join(parent: &Path, child: &Path) -> Result<String, String> {
    let mut segments = Vec::new();
    for component in parent.components().chain(child.components()) {
        match component {
            Component::Normal(segment) => segments.push(
                segment
                    .to_str()
                    .ok_or_else(|| "non-UTF-8 remote behavior path".to_string())?,
            ),
            Component::ParentDir => {
                segments
                    .pop()
                    .ok_or_else(|| "remote behavior path escapes action root".to_string())?;
            }
            Component::CurDir => {}
            _ => return Err("absolute remote behavior path".to_string()),
        }
    }
    if segments.is_empty() {
        return Err("empty remote behavior path".to_string());
    }
    Ok(segments.join("/"))
}

fn template_path(root: &Path, class: RepositoryClass) -> std::path::PathBuf {
    root.join("templates").join(class.code()).join("ci.yml")
}

fn template_path_display(class: RepositoryClass) -> String {
    format!("templates/{}/ci.yml", class.code())
}

fn callable_path(root: &Path, class: RepositoryClass) -> std::path::PathBuf {
    root.join(".github")
        .join("workflows")
        .join(render::callable_file_name(class))
}

fn callable_path_display(class: RepositoryClass) -> String {
    format!(".github/workflows/{}", render::callable_file_name(class))
}

fn read_committed(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| {
        format!(
            "reading generated file {}: {e} (run `mise run generate`)",
            path.display()
        )
    })
}

fn require_equal(committed: &str, rendered: &str, what: &str) -> Result<(), String> {
    if committed == rendered {
        Ok(())
    } else {
        Err(format!(
            "generated file {what} does not match regeneration (a hand edit or stale render); run `mise run generate`"
        ))
    }
}

fn read_block_sha(path: &Path) -> Result<String, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let sha = raw.trim();
    if !is_sha40(sha) {
        return Err(format!(
            "fleet/block-sha {sha:?} is not a 40 lowercase hex commit SHA (mutable or malformed refs are forbidden)"
        ));
    }
    Ok(sha.to_string())
}

fn check_composite(root: &Path, name: &str) -> Result<(), String> {
    let canonical = composite::canonical(name)
        .ok_or_else(|| format!("no canonical bytes for composite {name:?}"))?;

    // The embedded canonical bytes must themselves be a valid composite: this guards
    // the constant in `composite` against a bad future edit (a non-composite, or a
    // mutable/non-40-hex nested action ref).
    if !canonical.contains("using: composite") {
        return Err(format!(
            "canonical composite {name:?} is not a composite action"
        ));
    }
    for reference in uses_refs(canonical) {
        if !is_sha40(&reference) {
            return Err(format!(
                "canonical composite {name:?} references non-40-hex ref {reference:?}"
            ));
        }
    }

    // The committed action body must match the canonical bytes exactly. This is the
    // full body — env, run script, and exec line — not merely the refs, so tampering
    // (e.g. replacing the gate exec with `echo skipped`) fails the audit.
    let path = composite_path(root, name);
    let committed = read_committed(&path)?;
    require_equal(&committed, canonical, &composite_path_display(name))?;
    Ok(())
}

fn composite_path(root: &Path, name: &str) -> std::path::PathBuf {
    root.join("actions").join(name).join("action.yml")
}

fn composite_path_display(name: &str) -> String {
    format!("actions/{name}/action.yml")
}

/// Extract the git ref of every `uses: OWNER/REPO...@REF` line (ignoring local
/// `./` references, which have none). A trailing ` # comment` is stripped.
fn uses_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start().trim_start_matches("- ").trim_start();
        let Some(rest) = trimmed.strip_prefix("uses:") else {
            continue;
        };
        let value = rest.trim();
        if value.starts_with("./") || value.starts_with('.') {
            continue;
        }
        // Drop a trailing version comment.
        let value = value.split_whitespace().next().unwrap_or(value);
        if let Some((_, r)) = value.rsplit_once('@') {
            refs.push(r.to_string());
        }
    }
    refs
}

fn uses_identities(text: &str) -> Vec<String> {
    let mut identities = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start().trim_start_matches("- ").trim_start();
        let Some(rest) = trimmed.strip_prefix("uses:") else {
            continue;
        };
        let value = rest.split_whitespace().next().unwrap_or_default();
        if !value.starts_with("./") && !value.starts_with('.') {
            identities.push(value.to_string());
        }
    }
    identities
}

fn audit_consumer_structure(
    class: RepositoryClass,
    rendered: &str,
    forks: &ForkTable,
) -> Result<(), String> {
    let what = template_path_display(class);
    let file = render::callable_file_name(class);

    // Exactly one static owner-local reusable call per recognized owner,
    // selected only by exact github.repository_owner. No dynamic `uses`.
    for fork in forks.forks() {
        let owner = fork.owner();
        let call = format!(
            "uses: {owner}/{ACTIONS_REPO}/.github/workflows/{file}{} # {CALVER_PLACEHOLDER}",
            fork.placeholder()
        );
        if !rendered.contains(&call) {
            return Err(format!("{what}: missing owner-local call for {owner}"));
        }
        let guard = format!("if: ${{{{ github.repository_owner == '{owner}' }}}}");
        if !rendered.contains(&guard) {
            return Err(format!("{what}: missing exact owner guard for {owner}"));
        }
    }
    if rendered.matches("uses:").count() != forks.len() {
        return Err(format!(
            "{what}: expected exactly {} reusable-workflow calls",
            forks.len()
        ));
    }
    // A dynamic `uses:` (expression in the ref) is forbidden.
    for line in rendered.lines() {
        if line.contains("uses:") && line.contains("${{") {
            return Err(format!("{what}: dynamic `uses:` reference is forbidden"));
        }
    }

    // ci-required is fail-closed: always() plus the positive truth table.
    require_contains(
        rendered,
        "if: ${{ always() }}",
        &what,
        "ci-required always()",
    )?;
    require_contains(rendered, "case \"${OWNER}\" in", &what, "owner truth table")?;
    require_contains(
        rendered,
        "unrecognized owner",
        &what,
        "unknown-owner rejection",
    )?;
    require_contains(
        rendered,
        "expected both 'skipped'",
        &what,
        "non-selected skipped requirement",
    )?;
    require_contains(
        rendered,
        "expected empty",
        &what,
        "non-selected empty-output requirement",
    )?;
    require_contains(
        rendered,
        "expected 'success'",
        &what,
        "selected success requirement",
    )?;
    // Forbid "no failure found" acceptance logic.
    if rendered.contains("!= \"failure\"") || rendered.contains("!= 'failure'") {
        return Err(format!(
            "{what}: forbidden negative (no-failure) aggregation logic"
        ));
    }
    Ok(())
}

fn audit_callable_structure(
    class: RepositoryClass,
    rendered: &str,
    block_sha: &str,
    remote_closure: &RemoteClosure,
    forks: &ForkTable,
) -> Result<(), String> {
    let what = callable_path_display(class);

    // workflow_call plus exactly the canonical fleet `lanes` dispatch escape
    // hatch — no other trigger.
    require_contains(rendered, "workflow_call:", &what, "workflow_call trigger")?;
    const CANONICAL_LANES_DISPATCH: &str = "\n  workflow_dispatch:\n    inputs:\n      lanes:\n        description: velnor (default) | github | both\n        type: choice\n        default: velnor\n        options: [velnor, github, both]\n";
    for forbidden in ["pull_request:", "push:", "schedule:"] {
        if rendered.contains(&format!("\n  {forbidden}")) {
            return Err(format!(
                "{what}: callable workflow must be workflow_call only, found {forbidden}"
            ));
        }
    }
    let dispatch_count = rendered.matches("\n  workflow_dispatch:").count();
    if dispatch_count != 1 || !rendered.contains(CANONICAL_LANES_DISPATCH) {
        return Err(format!(
            "{what}: workflow_dispatch must be exactly the canonical lanes escape hatch"
        ));
    }
    // No secret or environment inheritance.
    if rendered.contains("secrets:") || rendered.contains("environment:") {
        return Err(format!(
            "{what}: callable workflow must not inherit secrets or environments"
        ));
    }

    // Internal composite closure is pinned to the block SHA.
    let run_gate = format!(
        "{}/{ACTIONS_REPO}/actions/run-gate@{block_sha}",
        forks.canonical_owner()
    );
    let aggregate = format!(
        "{}/{ACTIONS_REPO}/actions/aggregate@{block_sha}",
        forks.canonical_owner()
    );
    require_contains(rendered, &run_gate, &what, "run-gate pinned to block SHA")?;
    require_contains(rendered, &aggregate, &what, "aggregate pinned to block SHA")?;

    // Every executable `uses:` ref must be a full 40-hex SHA (no mutable refs).
    for reference in uses_refs(rendered) {
        if !is_sha40(&reference) {
            return Err(format!("{what}: non-40-hex or mutable ref {reference:?}"));
        }
    }
    audit_admitted_closure(
        rendered,
        block_sha,
        &what,
        remote_closure,
        forks.canonical_owner(),
    )?;

    require_contains(
        rendered,
        "caller owner pin cardinality",
        &what,
        "generated caller cardinality check",
    )?;
    if rendered.contains("@OWNER_") || rendered.contains("@FORK_COUNT@") {
        return Err(format!(
            "{what}: generated caller verifier left a fork-table placeholder"
        ));
    }
    for fork in forks.forks() {
        require_contains(
            rendered,
            &format!("  {})", fork.owner()),
            &what,
            "generated caller owner truth table",
        )?;
    }

    // A selected Velnor lane always means a real Velnor job. Event-dependent
    // substitution would make the lane selector and its evidence dishonest.
    require_contains(
        rendered,
        "velnor-lane:\n    name: velnor lane",
        &what,
        "Velnor lane job",
    )?;
    require_contains(
        rendered,
        "runs-on: ${{ 'velnor-trusted' }}",
        &what,
        "real Velnor route",
    )?;
    if rendered.contains(
        "(github.event_name == 'pull_request' || github.event_name == 'merge_group') && 'ubuntu-26.04' || 'velnor-trusted'",
    ) {
        return Err(format!(
            "{what}: selected Velnor lane must never substitute GitHub-hosted execution"
        ));
    }

    // Lane verdict is fail-closed and positive.
    require_contains(rendered, "if: ${{ always() }}", &what, "verdict always()")?;
    require_contains(
        rendered,
        "one lane never substitutes",
        &what,
        "no-substitution note",
    )?;
    require_contains(rendered, "contract=success", &what, "contract emission")?;

    // Every gate command rendered into the workflow is non-empty.
    for line in rendered.lines() {
        if let Some(cmd) = line.trim_start().strip_prefix("command:")
            && cmd.trim().is_empty()
        {
            return Err(format!("{what}: rendered gate has an empty command"));
        }
    }
    if class == RepositoryClass::Native {
        require_contains(
            rendered,
            "name: native-usage-menu-bar",
            &what,
            "native platform check name",
        )?;
        require_contains(
            rendered,
            "runs-on: macos-26",
            &what,
            "pinned native macOS runner",
        )?;
        require_contains(
            rendered,
            "command: mise run desktop-ci",
            &what,
            "repository-owned native gate",
        )?;
    } else if rendered.contains("mise run desktop-ci") {
        return Err(format!(
            "{what}: non-native class contains the native desktop gate"
        ));
    }
    Ok(())
}

fn audit_admitted_closure(
    rendered: &str,
    block_sha: &str,
    what: &str,
    closure: &RemoteClosure,
    canonical_owner: &str,
) -> Result<(), String> {
    for identity in uses_identities(rendered) {
        let (target, reference) = identity
            .rsplit_once('@')
            .ok_or_else(|| format!("{what}: action identity has no ref {identity:?}"))?;
        let mut segments = target.split('/');
        let owner = segments.next().unwrap_or_default();
        let repository = segments.next().unwrap_or_default();
        let root = format!("{owner}/{repository}");
        if root == format!("{canonical_owner}/{ACTIONS_REPO}") {
            if reference != block_sha {
                return Err(format!(
                    "{what}: internal action is not block-bound: {identity}"
                ));
            }
            continue;
        }
        let Some(action) = closure
            .action
            .iter()
            .find(|action| action.root == root && action.sha == reference)
        else {
            return Err(format!("{what}: unadmitted remote action {identity}"));
        };
        let subpath = target.split('/').skip(2).collect::<Vec<_>>().join("/");
        let manifest = if subpath.is_empty() {
            "action.yml".to_string()
        } else {
            format!("{subpath}/action.yml")
        };
        if !action
            .files
            .iter()
            .any(|file| file.kind == "manifest" && file.path == manifest)
        {
            return Err(format!(
                "{what}: action identity {identity} has no bound manifest {manifest}"
            ));
        }
    }
    Ok(())
}

fn audit_materialization(manifest: &FleetManifest, forks: &ForkTable) -> Result<(), String> {
    for class in ALL_CLASSES {
        let template = render::consumer_template_for(class, forks);
        let release_shas = vec![AUDIT_RELEASE_SHA; forks.len()];
        let class_bytes =
            render::render_consumer_for(&template, forks, &release_shas, AUDIT_CALVER)?;

        // Three coherent owner references sharing one CalVer. Each SHA is
        // owner-local; the audit fixture intentionally uses the same bytes.
        let want_ref = format!("@{AUDIT_RELEASE_SHA} # {AUDIT_CALVER}");
        if class_bytes.matches(&want_ref).count() != forks.len() {
            return Err(format!(
                "class {} materialization does not bind all {} owner calls to the release",
                class.code(),
                forks.len()
            ));
        }
        // No placeholder survives; a second substitution is refused.
        if class_bytes.contains(FLEET_SHA_PLACEHOLDER) || class_bytes.contains(CALVER_PLACEHOLDER) {
            return Err(format!(
                "class {} materialization left a placeholder",
                class.code()
            ));
        }
        if render::render_consumer_for(&class_bytes, forks, &release_shas, AUDIT_CALVER).is_ok() {
            return Err(format!(
                "class {} accepted a second repository-specific substitution",
                class.code()
            ));
        }

        // Every member of the class materializes to the identical bytes: no
        // per-repository fork or slug-specific substitution.
        for repo in manifest.members_of(class) {
            let repo_bytes =
                render::render_consumer_for(&template, forks, &release_shas, AUDIT_CALVER)?;
            if repo_bytes != class_bytes {
                return Err(format!(
                    "repository {} does not materialize to its class {} template bytes",
                    repo.slug,
                    class.code()
                ));
            }
        }
    }
    Ok(())
}

fn require_contains(text: &str, needle: &str, what: &str, label: &str) -> Result<(), String> {
    if text.contains(needle) {
        Ok(())
    } else {
        Err(format!("{what}: missing {label}"))
    }
}

#[cfg(test)]
mod remote_closure_tests {
    use super::*;

    #[test]
    fn node_manifest_derives_exact_main_pre_post_set() {
        let raw = br#"runs:
  using: node24
  pre: ../dist/pre.js
  main: ../dist/main.js
  post: ../dist/post.js
"#;
        let mut output = BTreeSet::new();
        derive_manifest_closure("sub/action.yml", raw, &mut output, &mut BTreeSet::new()).unwrap();
        assert_eq!(
            output,
            ["dist/main.js", "dist/post.js", "dist/pre.js"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }

    #[test]
    fn unknown_runtime_missing_main_and_escape_fail_closed() {
        for raw in [
            "runs:\n  using: nodeevil\n  main: dist/main.js\n",
            "runs:\n  using: node24\n",
            "runs:\n  using: node24\n  main: ../../escape.js\n",
        ] {
            assert!(
                derive_manifest_closure(
                    "action.yml",
                    raw.as_bytes(),
                    &mut BTreeSet::new(),
                    &mut BTreeSet::new(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn obsolete_node_runtimes_fail_closed() {
        for runtime in ["node12", "node16"] {
            let raw = format!("runs:\n  using: {runtime}\n  main: dist/main.js\n");
            assert!(
                derive_manifest_closure(
                    "action.yml",
                    raw.as_bytes(),
                    &mut BTreeSet::new(),
                    &mut BTreeSet::new(),
                )
                .is_err()
            );
        }
    }
}

#[cfg(test)]
mod fleet_audit_tests {
    use super::*;
    use crate::model::{CensusMetadata, RepositoryKind, RepositoryTier, Visibility};

    fn repository() -> crate::model::Repository {
        crate::model::Repository {
            slug: "tailrocks/example".to_owned(),
            organization: "tailrocks".to_owned(),
            class: RepositoryClass::Code,
            baseline_sha: "0000000000000000000000000000000000000000".to_owned(),
            census: CensusMetadata {
                tier: RepositoryTier::Leaf,
                kind: RepositoryKind::App,
                visibility: Visibility::Public,
                research: false,
                first_seen: "2026-08-30".to_owned(),
            },
        }
    }

    #[test]
    fn deferred_glob_requires_a_live_target() {
        assert!(glob_matches("backend*/**", "backend-rust/src/lib.rs"));
        assert!(glob_matches("**/*.rs", "src/lib.rs"));
        assert!(!glob_matches("backend*/**", "services/rust/src/lib.rs"));
    }

    #[test]
    fn deferred_rows_validate_age_and_staleness() {
        let root = std::env::temp_dir().join(format!(
            "velnor-actions-deferred-test-{}-{}",
            std::process::id(),
            today_days().unwrap()
        ));
        std::fs::create_dir_all(root.join("backend-rust")).unwrap();
        std::fs::write(root.join("backend-rust").join("README.md"), "legacy\n").unwrap();
        let today = today_days().unwrap();
        let valid = toml::Value::Table(toml::toml! {
            item = "legacy backend layout"
            registered = "2026-08-30"
            last_deferred = "2026-08-30"
            reason = "migration requires a coordinated release"
            effort = "M"
            trigger = "backend-rust/**"
        });
        validate_deferred_row(&repository(), &root, &valid, today).unwrap();

        let stale = toml::Value::Table(toml::toml! {
            item = "removed layout"
            registered = "2026-08-30"
            last_deferred = "2026-08-30"
            reason = "old"
            effort = "S"
            trigger = "missing/**"
        });
        let error = validate_deferred_row(&repository(), &root, &stale, today).unwrap_err();
        assert!(error.contains("stale"), "got: {error}");

        let old = toml::Value::Table(toml::toml! {
            item = "ancient layout"
            registered = "2020-01-01"
            last_deferred = "2020-01-01"
            reason = "old"
            effort = "S"
            trigger = "backend-rust/**"
        });
        let error = validate_deferred_row(&repository(), &root, &old, today).unwrap_err();
        assert!(error.contains("older than"), "got: {error}");
        let unknown = toml::Value::Table(toml::toml! {
            item = "unknown field"
            registered = "2026-08-30"
            last_deferred = "2026-08-30"
            reason = "old"
            effort = "S"
            trigger = "backend-rust/**"
            owner = "not allowed"
        });
        let error = validate_deferred_row(&repository(), &root, &unknown, today).unwrap_err();
        assert!(error.contains("unknown field"), "got: {error}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn adopted_repo_requires_all_census_cache_fields() {
        let root = std::env::temp_dir().join(format!(
            "velnor-actions-adoption-test-{}-{}",
            std::process::id(),
            today_days().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("repolint.toml"),
            "[repo]\ntier = \"leaf\"\nkind = \"app\"\nvisibility = \"public\"\n",
        )
        .unwrap();
        let error = cross_check_local_repo(&repository(), &root).unwrap_err();
        assert!(error.contains("research"), "got: {error}");
        std::fs::write(
            root.join("repolint.toml"),
            "[repo]\ntier = \"leaf\"\nkind = \"app\"\nvisibility = \"public\"\nresearch = false\n",
        )
        .unwrap();
        assert_eq!(
            cross_check_local_repo(&repository(), &root).unwrap(),
            AdoptionState::Adopted
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cloned_consumer_comparison_reads_actual_workflow_bytes() {
        let root = std::env::temp_dir().join(format!(
            "velnor-actions-cloned-consumer-test-{}-{}",
            std::process::id(),
            today_days().unwrap()
        ));
        let workflow = root.join(".github/workflows");
        std::fs::create_dir_all(&workflow).unwrap();
        let forks = ForkTable::canonical();
        let template = render::consumer_template_for(RepositoryClass::Code, &forks);
        let expected =
            render::render_consumer_for(&template, &forks, &[AUDIT_RELEASE_SHA; 3], AUDIT_CALVER)
                .unwrap();
        std::fs::write(workflow.join("ci.yml"), expected).unwrap();
        audit_cloned_consumer(&repository(), &root, &forks).unwrap();

        std::fs::write(workflow.join("ci.yml"), "name: tampered\n").unwrap();
        let error = audit_cloned_consumer(&repository(), &root, &forks).unwrap_err();
        assert!(
            error.contains("exactly one") || error.contains("diverges"),
            "got: {error}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zizmor_findings_are_warn_only_before_adoption() {
        assert!(is_pre_adoption_warning(
            AdoptionState::Unadopted,
            "zizmor",
            Some(ZIZMOR_LAST_FINDINGS_EXIT_CODE),
            false
        ));
        assert!(!is_pre_adoption_warning(
            AdoptionState::Adopted,
            "zizmor",
            Some(ZIZMOR_LAST_FINDINGS_EXIT_CODE),
            false
        ));
        assert!(!is_pre_adoption_warning(
            AdoptionState::Unadopted,
            "repolint",
            Some(ZIZMOR_LAST_FINDINGS_EXIT_CODE),
            false
        ));
        assert!(is_pre_adoption_warning(
            AdoptionState::Unadopted,
            "zizmor",
            Some(ZIZMOR_FIRST_FINDINGS_EXIT_CODE),
            false
        ));
        assert!(!is_pre_adoption_warning(
            AdoptionState::Unadopted,
            "zizmor",
            Some(2),
            false
        ));
    }
}

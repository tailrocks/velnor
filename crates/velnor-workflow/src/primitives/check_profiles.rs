//! Composable scheduled-check profiles: one `scheduled-checks` row renders one
//! scheduled workflow file from the `[[check_profile]]` rows it names.
//!
//! A profile declares its own cadence, platform, named tasks, dependencies,
//! timeout, artifacts, thresholds, and status. The primitive renders the
//! schedule trigger and the job shell; the named tasks own every product
//! assertion and threshold, which travel to the job as environment the
//! generator never interprets. Profiles that share a file share one cadence:
//! mixing cadences in one scheduled file would run every job on every cron, so
//! the renderer refuses the mix instead of silently over-executing.
//!
//! A row may also declare file-level `events` (`push`, `pull_request`,
//! `workflow_dispatch`): the file then renders those triggers alongside the
//! shared cron, and profiles in an evented file may omit `schedule` entirely
//! for a cron-less evented file. One file carries one trigger set — scheduled
//! and schedule-less profiles never mix in one file — and lanes stay
//! per-profile: each job runs on its own profile's lane exactly as in a
//! cron-only file, because triggers change when a job runs, never where.
//!
//! Why a file, not a CI unit: a scheduled check is a whole-repo compliance
//! probe that needs its own required status context plus main-branch runs
//! independent of affected-unit selection. A CI unit renders only when the
//! scan selects it and reports under unit lanes, so folding a repo-wide gate
//! into a unit would make compliance conditional on selection and lose the
//! standalone required signal. Event triggers therefore live on the
//! scheduled-checks file, not on a unit.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use super::{lanes_dispatch_inputs, lanes_runs_on, Args, Primitive, RenderCtx, Rendered};
use crate::{
    velnor_runner, velnor_runner_group, yaml_scalar, ActionPin, CheckProfileSpec, GeneratorError,
    ProjectConfig, RunnerMode, GENERATED_HEADER,
};

/// Default `timeout-minutes` for a scheduled-check job.
pub(crate) const DEFAULT_CHECK_PROFILE_TIMEOUT_MINUTES: u32 = 30;

/// Whether the primitive renders a scheduled-checks workflow file.
pub(crate) fn is_scheduled_checks_side(primitive: &str) -> bool {
    primitive == super::SCHEDULED_CHECKS
}

/// The declared `scheduled-*.yml` workflow family.
pub(crate) struct ScheduledChecks;

impl Primitive for ScheduledChecks {
    fn id(&self) -> &'static str {
        super::SCHEDULED_CHECKS
    }

    fn schema(&self) -> &'static [&'static str] {
        &["name", "profiles", "events", "branches", "lanes_input"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let file = ctx.file.unwrap_or_default();
        let label = if file.is_empty() { ctx.family } else { file };
        let profiles = select_profiles(&ctx.config.check_profiles, args, label)?;
        let events = select_events(args, label)?;
        let branches = select_branches(args, label, &events)?;
        let lanes_input = args.flag("lanes_input")?;
        let content = render_checks_file(
            ctx.config,
            file,
            args.string("name")?,
            &profiles,
            &events,
            &branches,
            lanes_input,
        )?;
        render_file(ctx, content)
    }
}

/// The profiles one `scheduled-checks` row renders, in configuration order.
///
/// An absent `profiles` argument selects every configured profile. Selection
/// is coherent or it fails: every selected profile shares one trigger set —
/// one cadence, or, only in a file whose row declares `events`, no cron at
/// all — every `needs` entry names another selected profile, and the
/// dependency graph is acyclic.
///
/// # Errors
/// Returns a usage error for an empty selection, an unknown or repeated
/// profile name, mixed cadences, a schedule-less profile in a file without
/// `events`, a dependency outside the file, or a dependency cycle.
pub(crate) fn select_profiles<'a>(
    all: &'a [CheckProfileSpec],
    args: &Args<'_>,
    family: &str,
) -> Result<Vec<&'a CheckProfileSpec>, GeneratorError> {
    let selected = match args.strings("profiles")? {
        None => all.iter().collect::<Vec<_>>(),
        Some(names) => {
            if names.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "`{family}` declares an empty `profiles`; name the check profiles the file renders"
                )));
            }
            let mut selected = Vec::new();
            for name in &names {
                let profile = all.iter().find(|profile| &profile.id == name).ok_or_else(|| {
                    let known = all
                        .iter()
                        .map(|profile| profile.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    GeneratorError::usage(format!(
                        "`{family}` names check profile `{name}`, which no [[check_profile]] row declares; declared profiles: {known}"
                    ))
                })?;
                if selected.contains(&profile) {
                    return Err(GeneratorError::usage(format!(
                        "`{family}` names check profile `{name}` twice; declare each profile once per file"
                    )));
                }
                selected.push(profile);
            }
            selected
        }
    };
    if selected.is_empty() {
        return Err(GeneratorError::usage(format!(
            "`{family}` renders only for a repository with [[check_profile]] rows; declare the profiles the file renders"
        )));
    }
    let evented = args
        .strings("events")?
        .is_some_and(|events| !events.is_empty());
    check_shared_cadence(&selected, family, evented)?;
    check_needs_within_file(&selected, family)?;
    check_acyclic(&selected, family)?;
    Ok(selected)
}

/// One scheduled file carries one trigger set: either one shared `schedule`
/// trigger, or — only in a file whose row declares `events` — no cron at
/// all. Profiles that share a file therefore share one cadence, and a file
/// never mixes scheduled and schedule-less profiles: the mix would run some
/// jobs on a cron the file's other jobs do not share.
fn check_shared_cadence(
    selected: &[&CheckProfileSpec],
    family: &str,
    evented: bool,
) -> Result<(), GeneratorError> {
    let Some(first) = selected.first() else {
        return Ok(());
    };
    for profile in &selected[1..] {
        if profile.schedule != first.schedule {
            if first.schedule.is_empty() || profile.schedule.is_empty() {
                let (scheduled, unscheduled) = if first.schedule.is_empty() {
                    (profile, first)
                } else {
                    (first, profile)
                };
                return Err(GeneratorError::usage(format!(
                    "`{family}` renders one trigger set per file, so a file never mixes scheduled and schedule-less profiles; `{}` declares `{}` but `{}` declares no `schedule`",
                    scheduled.id, scheduled.schedule, unscheduled.id
                )));
            }
            return Err(GeneratorError::usage(format!(
                "`{family}` renders one scheduled workflow per file, so every profile it renders shares one cadence; `{}` declares `{}` but `{}` declares `{}`",
                first.id, first.schedule, profile.id, profile.schedule
            )));
        }
    }
    if first.schedule.is_empty() && !evented {
        let ids = selected
            .iter()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>()
            .join("`, `");
        return Err(GeneratorError::usage(format!(
            "`{family}` cannot render check profile `{ids}` with no trigger: the profile declares no `schedule` and the file declares no `events`; declare `events` on the file's row or `schedule` on the profile"
        )));
    }
    Ok(())
}

/// The event names a `scheduled-checks` row may declare in `events`.
const ACCEPTED_EVENTS: [&str; 4] = ["push", "pull_request", "merge_group", "workflow_dispatch"];

/// The declared events that render trigger keys, in render order.
/// `workflow_dispatch` validates but renders nothing extra: every
/// scheduled-checks file already carries that trigger unconditionally.
const RENDERED_EVENT_ORDER: [&str; 3] = ["push", "pull_request", "merge_group"];

/// The file-level event triggers one `scheduled-checks` row declares, in
/// canonical render order. An absent `events` argument renders today's
/// cron-only file byte for byte.
///
/// Name validation lives here, on the render path where the file is known,
/// rather than in `select_profiles`: the coverage pre-check calls selection
/// with the primitive id, so validating names there would label the error
/// with the primitive instead of the file.
///
/// # Errors
/// Returns a usage error naming the file for a mistyped `events` value, an
/// empty list, an unknown event name, or a repeated event name.
fn select_events(args: &Args<'_>, file: &str) -> Result<Vec<String>, GeneratorError> {
    let Some(declared) = args.strings("events")? else {
        return Ok(Vec::new());
    };
    if declared.is_empty() {
        return Err(GeneratorError::usage(format!(
            "`{file}` declares an empty `events`; name the GitHub events the file runs on, or remove `events` for a cron-only file"
        )));
    }
    let mut seen = BTreeSet::new();
    for event in &declared {
        if !ACCEPTED_EVENTS.contains(&event.as_str()) {
            return Err(GeneratorError::usage(format!(
                "`{file}` declares unknown event `{event}` in `events`; accepted events: {}",
                ACCEPTED_EVENTS.join(", ")
            )));
        }
        if !seen.insert(event.as_str()) {
            return Err(GeneratorError::usage(format!(
                "`{file}` names event `{event}` twice in `events`; declare each event once per file"
            )));
        }
    }
    Ok(RENDERED_EVENT_ORDER
        .iter()
        .filter(|event| seen.contains(**event))
        .map(|event| (*event).to_owned())
        .collect())
}

/// The push branches one `scheduled-checks` row declares, in configuration
/// order. Absent renders the bare `push:` trigger byte for byte, so
/// `branches` is purely additive. It scopes the push trigger only: branches
/// without a `push` event fail closed instead of silently scoping nothing.
///
/// # Errors
/// Returns a usage error naming the file for an empty list, a `branches`
/// declaration without `push` in `events`, or a branch that is not one
/// non-empty line.
fn select_branches(
    args: &Args<'_>,
    file: &str,
    events: &[String],
) -> Result<Vec<String>, GeneratorError> {
    let Some(declared) = args.strings("branches")? else {
        return Ok(Vec::new());
    };
    if declared.is_empty() {
        return Err(GeneratorError::usage(format!(
            "`{file}` declares an empty `branches`; name the push branches the file runs on, or remove `branches` for an unscoped push trigger"
        )));
    }
    if !events.iter().any(|event| event == "push") {
        return Err(GeneratorError::usage(format!(
            "`{file}` declares `branches` but no `push` event; `branches` scopes the push trigger only, so declare `push` in `events` or remove `branches`"
        )));
    }
    for branch in &declared {
        if branch.is_empty() || branch.contains(['\n', '\r']) {
            return Err(GeneratorError::usage(format!(
                "`{file}` `branches` must be one non-empty line per branch"
            )));
        }
    }
    Ok(declared)
}

/// A `needs` edge cannot span workflow files: GitHub resolves it within one
/// file only, so a dependency outside the rendered set is unrenderable.
fn check_needs_within_file(
    selected: &[&CheckProfileSpec],
    family: &str,
) -> Result<(), GeneratorError> {
    let ids = selected
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<BTreeSet<_>>();
    for profile in selected {
        for dependency in &profile.needs {
            if !ids.contains(dependency.as_str()) {
                return Err(GeneratorError::usage(format!(
                    "`{family}` cannot render check profile `{}` with a dependency on `{dependency}`; `needs` spans one scheduled workflow file only, so render both profiles from the same row",
                    profile.id
                )));
            }
        }
    }
    Ok(())
}

/// A dependency cycle would deadlock every scheduled run at startup.
fn check_acyclic(selected: &[&CheckProfileSpec], family: &str) -> Result<(), GeneratorError> {
    let mut done = BTreeSet::new();
    for profile in selected {
        let mut visiting = Vec::new();
        visit(profile, selected, &mut visiting, &mut done, family)?;
    }
    Ok(())
}

fn visit<'a>(
    profile: &'a CheckProfileSpec,
    selected: &[&'a CheckProfileSpec],
    visiting: &mut Vec<&'a str>,
    done: &mut BTreeSet<&'a str>,
    family: &str,
) -> Result<(), GeneratorError> {
    if done.contains(profile.id.as_str()) {
        return Ok(());
    }
    if let Some(start) = visiting.iter().position(|id| *id == profile.id) {
        let mut cycle = visiting[start..].to_vec();
        cycle.push(profile.id.as_str());
        return Err(GeneratorError::usage(format!(
            "`{family}` cannot render check profiles with a dependency cycle: {}",
            cycle.join(" -> ")
        )));
    }
    visiting.push(profile.id.as_str());
    for dependency in &profile.needs {
        if let Some(next) = selected.iter().find(|profile| &profile.id == dependency) {
            visit(next, selected, visiting, done, family)?;
        }
    }
    visiting.pop();
    done.insert(profile.id.as_str());
    Ok(())
}

fn render_file(ctx: &RenderCtx<'_>, content: String) -> Result<Rendered, GeneratorError> {
    let file = ctx
        .file
        .filter(|file| !file.is_empty())
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{}` renders a scheduled workflow file and needs `file`",
                ctx.family
            ))
        })?
        .to_owned();
    Ok(Rendered {
        files: std::iter::once((PathBuf::from(".github/workflows").join(file), content)).collect(),
        ..Rendered::default()
    })
}

/// The scheduled workflow content for one row: the file-level event triggers,
/// the shared cadence trigger (absent in a cron-less evented file), and one
/// job per selected profile. An empty event set renders the cron-only file
/// exactly as before, so `events` is purely additive.
fn render_checks_file(
    config: &ProjectConfig,
    file: &str,
    name: Option<String>,
    profiles: &[&CheckProfileSpec],
    events: &[String],
    branches: &[String],
    lanes_input: bool,
) -> Result<String, GeneratorError> {
    let stem = file.strip_suffix(".yml").unwrap_or(file);
    let name = name.unwrap_or_else(|| stem.to_owned());
    let schedule = profiles
        .first()
        .map(|profile| profile.schedule.as_str())
        .unwrap_or_default();
    let label = if file.is_empty() {
        super::SCHEDULED_CHECKS
    } else {
        file
    };
    let lanes = lanes_override(config, label, profiles, lanes_input)?;
    let mut output = String::from(GENERATED_HEADER);
    let _ = writeln!(output, "name: {}", yaml_scalar(&name));
    let _ = writeln!(
        output,
        "run-name: {}",
        yaml_scalar(&format!("{name} · ${{{{ github.event_name }}}}"))
    );
    output.push_str("\non:\n");
    for event in events {
        if event == "push" && !branches.is_empty() {
            let list = branches
                .iter()
                .map(|branch| yaml_scalar(branch))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(output, "  {event}:\n    branches: [{list}]");
        } else {
            let _ = writeln!(output, "  {event}:");
        }
    }
    if !schedule.is_empty() {
        output.push_str("  schedule:\n");
        let _ = writeln!(output, "    - cron: {}", yaml_scalar(schedule));
    }
    output.push_str("  workflow_dispatch:");
    if let Some((default, _)) = &lanes {
        output.push_str(lanes_dispatch_inputs(*default));
    }
    output.push_str("\n\npermissions:\n  contents: read\n\nconcurrency:\n");
    let _ = writeln!(
        output,
        "  group: {stem}-${{{{ github.repository }}}}-${{{{ github.ref }}}}"
    );
    if events.iter().any(|event| event == "pull_request") {
        // PR-only cancel, like the docs-site and Renovate files:
        // pull-request runs supersede each other for fast feedback, while
        // push, schedule, and dispatch runs — the compliance signal on the
        // default branch — always run to completion instead of cancelling a
        // prior signal.
        output.push_str(
            "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n\njobs:\n",
        );
    } else if events
        .iter()
        .any(|event| matches!(event.as_str(), "push" | "merge_group"))
    {
        // A push or merge-group run is evidence for a committed/candidate
        // tree. Queue the same-ref runs so a later event cannot erase the
        // predecessor's verdict. Only pull_request attempts are
        // supersedable; that branch is handled above.
        output.push_str("  cancel-in-progress: false\n\njobs:\n");
    } else {
        // A cron-only or dispatch-only file has no candidate/main event to
        // preserve, so retain the historical supersession behavior.
        output.push_str("  cancel-in-progress: true\n\njobs:\n");
    }
    for profile in profiles {
        render_profile_job(
            &mut output,
            config,
            profile,
            lanes.as_ref().map(|(_, runs_on)| runs_on.as_str()),
        )?;
    }
    Ok(output)
}

/// The resolved lanes override for a file: the shared dispatch default plus
/// the conditional `runs-on` every dispatchable job renders. `None` without
/// the flag, so the file renders exactly as before.
fn lanes_override(
    config: &ProjectConfig,
    file: &str,
    profiles: &[&CheckProfileSpec],
    lanes_input: bool,
) -> Result<Option<(RunnerMode, String)>, GeneratorError> {
    if !lanes_input {
        return Ok(None);
    }
    let default = lanes_default_lane(profiles, file)?;
    let runs_on = lanes_runs_on(config, file, default)?;
    Ok(Some((default, runs_on)))
}

/// The shared dispatch lane for a `lanes_input` file: every non-macos profile
/// must declare the same lane, which becomes the input default and the
/// conditional's static leg. macos profiles always render static runs-on, so
/// they never constrain the default. Like the other coherence rules, a mixed
/// file fails closed naming the file instead of dispatching half its jobs.
fn lanes_default_lane(
    profiles: &[&CheckProfileSpec],
    file: &str,
) -> Result<RunnerMode, GeneratorError> {
    let mut default: Option<(RunnerMode, &str, &str)> = None;
    for profile in profiles {
        let lane = match profile.runner.as_str() {
            "github" => RunnerMode::Github,
            "velnor" => RunnerMode::Velnor,
            "macos" => continue,
            runner => {
                return Err(GeneratorError::usage(format!(
                    "check profile `{}` runs on `{runner}`, which is not a lane; use github, macos, or velnor",
                    profile.id
                )));
            }
        };
        match default {
            None => default = Some((lane, profile.id.as_str(), profile.runner.as_str())),
            Some((first, _, _)) if first == lane => {}
            Some((_, first_id, first_runner)) => {
                return Err(GeneratorError::usage(format!(
                    "`{file}` declares `lanes_input` but mixes dispatch lanes: `{first_id}` runs on {first_runner} while `{}` runs on {}; one file selects one lane",
                    profile.id, profile.runner
                )));
            }
        }
    }
    default.map(|(lane, _, _)| lane).ok_or_else(|| {
        GeneratorError::usage(format!(
            "`{file}` declares `lanes_input` but every profile runs on macos, which never dispatches across lanes; declare a github or velnor profile or drop `lanes_input`"
        ))
    })
}

/// The cron-only workflow content for one row, without file-level events.
/// Test-only: production rendering goes through [`render_checks_file`].
#[cfg(test)]
fn render_scheduled_checks(
    config: &ProjectConfig,
    file: &str,
    name: Option<String>,
    profiles: &[&CheckProfileSpec],
) -> Result<String, GeneratorError> {
    render_checks_file(config, file, name, profiles, &[], &[], false)
}

/// One profile job: the lane it runs on, the timeout it holds, the threshold
/// environment its tasks read, and the steps that check out, provision tools,
/// run the named tasks, and upload the declared artifacts.
fn render_profile_job(
    output: &mut String,
    config: &ProjectConfig,
    profile: &CheckProfileSpec,
    dispatch_runs_on: Option<&str>,
) -> Result<(), GeneratorError> {
    let _ = writeln!(output, "  {}:", profile.id);
    let _ = writeln!(output, "    name: {}", yaml_scalar(&profile.name));
    if !profile.needs.is_empty() {
        let _ = writeln!(output, "    needs: [{}]", profile.needs.join(", "));
    }
    // A dispatch selects the github/velnor lane only: macos profiles always
    // render their static label, never the conditional.
    let runs_on = match (dispatch_runs_on, profile.runner.as_str()) {
        (Some(conditional), "github" | "velnor") => conditional.to_owned(),
        _ => profile_runs_on(config, profile)?,
    };
    let _ = writeln!(output, "    runs-on: {runs_on}");
    let _ = writeln!(output, "    timeout-minutes: {}", profile.timeout_minutes);
    if profile.advisory {
        output.push_str("    continue-on-error: true\n");
    }
    if !profile.env.is_empty() {
        output.push_str("    env:\n");
        for (key, value) in &profile.env {
            let _ = writeln!(output, "      {key}: {}", yaml_scalar(value));
        }
    }
    output.push_str("    steps:\n");
    render_checkout_step(output);
    render_tool_steps(output, config, profile);
    for task in &profile.tasks {
        let _ = writeln!(
            output,
            "      - name: Run {task}\n        run: mise run {task}"
        );
    }
    if !profile.artifacts.is_empty() {
        render_artifact_step(output, profile);
    }
    Ok(())
}

/// The lane selector for one profile: the hosted Linux label, the hosted
/// Apple label, or the repository's own Velnor labels.
fn profile_runs_on(
    config: &ProjectConfig,
    profile: &CheckProfileSpec,
) -> Result<String, GeneratorError> {
    match profile.runner.as_str() {
        "github" => Ok(yaml_scalar(&config.github_runner)),
        "macos" => Ok(yaml_scalar(&config.macos_runner)),
        "velnor" => {
            if config.velnor_labels.is_empty() {
                return Err(GeneratorError::usage(format!(
                    "check profile `{}` runs on velnor, but [workflow] velnor_labels names no runner labels",
                    profile.id
                )));
            }
            Ok(velnor_runner(
                &config.velnor_labels,
                velnor_runner_group(config),
            ))
        }
        runner => Err(GeneratorError::usage(format!(
            "check profile `{}` runs on `{runner}`, which is not a lane; use github, macos, or velnor",
            profile.id
        ))),
    }
}

fn render_checkout_step(output: &mut String) {
    let checkout = ActionPin::Checkout.reference();
    let _ = writeln!(
        output,
        "      - name: Checkout repository\n        uses: {checkout}\n        with:\n          persist-credentials: false"
    );
}

/// Tool provisioning in lane order. Hosted lanes install through the pinned
/// `mise` action, which the Velnor lane cannot admit, so Velnor installs with
/// the preinstalled `mise` binary instead. Every profile runs named tasks, so
/// hosted lanes always set up the runner even when no tool needs installing.
fn render_tool_steps(output: &mut String, config: &ProjectConfig, profile: &CheckProfileSpec) {
    let mise = ActionPin::Mise.reference();
    if profile.runner == "velnor" {
        if !profile.tools.is_empty() {
            let _ = writeln!(
                output,
                "      - name: Install declared Mise tools\n        env:\n          MISE_TOOLS: {}\n        run: |\n          set -euo pipefail\n          read -ra tools <<<\"$MISE_TOOLS\"\n          mise --yes --locked install \"${{tools[@]}}\"",
                yaml_scalar(&profile.tools.join(" "))
            );
        }
        return;
    }
    if profile.tools.is_empty() {
        let _ = writeln!(
            output,
            "      - name: Set up Mise\n        uses: {mise}\n        with:\n          install: false"
        );
    } else {
        let trusted = super::trusted_cache_save_expression(&config.default_branch);
        let _ = writeln!(
            output,
            "      - name: Set up Mise\n        uses: {mise}\n        with:\n          install_args: {}\n          cache: true\n          cache_save: ${{{{ {trusted} }}}}",
            yaml_scalar(&profile.tools.join(" "))
        );
    }
}

fn render_artifact_step(output: &mut String, profile: &CheckProfileSpec) {
    let upload = ActionPin::UploadArtifact.reference();
    let _ = writeln!(
        output,
        "      - name: Upload {} artifacts\n        if: always()\n        uses: {upload}\n        with:\n          name: {}\n          path: |",
        profile.id,
        yaml_scalar(&profile.id)
    );
    for artifact in &profile.artifacts {
        let _ = writeln!(output, "            {artifact}");
    }
    output.push_str("          if-no-files-found: warn\n");
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use std::collections::BTreeMap;

    use super::*;
    use crate::config;

    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn must_fail<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}: expected a usage error"),
            Err(error) => error,
        }
    }

    fn profile(id: &str) -> CheckProfileSpec {
        CheckProfileSpec {
            id: id.to_owned(),
            name: format!("{id} check"),
            schedule: "23 2 * * *".to_owned(),
            runner: "github".to_owned(),
            tools: Vec::new(),
            tasks: vec![format!("check-{id}")],
            needs: Vec::new(),
            timeout_minutes: DEFAULT_CHECK_PROFILE_TIMEOUT_MINUTES,
            artifacts: Vec::new(),
            advisory: false,
            env: BTreeMap::new(),
        }
    }

    fn profile_config(profiles: Vec<CheckProfileSpec>) -> ProjectConfig {
        ProjectConfig {
            repository: String::new(),
            workflow_revision: crate::SOURCE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::AnalysisSummary {
                method: "test".to_owned(),
                detected: Vec::new(),
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: vec!["scheduled-daily.yml".to_owned()],
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            runners: crate::RunnerMode::Both,
            automatic: crate::RunnerMode::Both,
            github_runner: "ubuntu-24.04".to_owned(),
            macos_runner: "macos-15".to_owned(),
            velnor_labels: vec!["self-hosted".to_owned(), "example-lane".to_owned()],
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: false,
            renovate_reason: String::new(),
            renovate: None,
            docs_enabled: false,
            docs_reason: String::new(),
            docs: None,
            check_profiles: profiles,
            maintenance: crate::MaintenanceSpec::default(),
            units: Vec::new(),
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            velnor_runner_group: None,
            velnor_trusted_label: None,
            velnor_trusted_runner_available: None,
            pull_request_on_velnor: false,
            default_dispatch_runner: crate::DEFAULT_DISPATCH_RUNNER.to_owned(),
            automatic_lanes: crate::DEFAULT_AUTOMATIC_LANES.to_owned(),
            velnor_rust_needs: crate::VelnorRustNeeds::Parallel,
            velnor_concurrency_group: None,
            velnor_serial_stack_groups: false,
            static_files: Vec::new(),
            reviewers: Vec::new(),
            declared_surface: true,
            mise_lock_keys: std::collections::BTreeSet::new(),
            github_cache: config::CacheGithubSection::default(),
            velnor_host_cache: config::CacheVelnorSection::default(),
        }
    }

    fn args_for(text: &str) -> BTreeMap<String, toml::Value> {
        must(toml::from_str(text), "parse test args")
    }

    fn render(
        config: &ProjectConfig,
        name: Option<String>,
        profiles: &[&CheckProfileSpec],
    ) -> String {
        must(
            render_scheduled_checks(config, "scheduled-daily.yml", name, profiles),
            "render scheduled checks",
        )
    }

    #[test]
    fn required_and_advisory_jobs_split_continue_on_error() {
        let smoke = profile("smoke");
        let mut load = profile("load");
        load.advisory = true;
        let config = profile_config(vec![smoke, load]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-checks"),
            "select every profile",
        );
        let workflow = render(&config, None, &selected);
        assert_eq!(
            workflow.matches("continue-on-error: true").count(),
            1,
            "{workflow}"
        );
        let mut required = String::new();
        must(
            render_profile_job(&mut required, &config, &config.check_profiles[0], None),
            "render the required job",
        );
        assert!(
            !required.contains("continue-on-error"),
            "a required job gates the workflow: {required}"
        );
        let mut advisory = String::new();
        must(
            render_profile_job(&mut advisory, &config, &config.check_profiles[1], None),
            "render the advisory job",
        );
        assert!(
            advisory.contains("continue-on-error: true"),
            "an advisory job reports without gating: {advisory}"
        );
    }

    #[test]
    fn cadence_renders_as_schedule_trigger_and_default_name() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-checks"),
            "select every profile",
        );
        let workflow = render(&config, None, &selected);
        assert!(workflow.contains("- cron: \"23 2 * * *\""), "{workflow}");
        assert!(workflow.contains("workflow_dispatch:"), "{workflow}");
        assert!(workflow.contains("name: scheduled-daily"), "{workflow}");
        assert!(
            workflow.contains("group: scheduled-daily-${{ github.repository }}-${{ github.ref }}"),
            "{workflow}"
        );
    }

    #[test]
    fn mixed_cadences_are_refused() {
        let smoke = profile("smoke");
        let mut weekly = profile("weekly");
        weekly.schedule = "41 4 * * 1".to_owned();
        let all = vec![smoke, weekly];
        let map = args_for("");
        let error = must_fail(
            select_profiles(&all, &Args(&map), "scheduled-checks"),
            "mixed cadences must fail selection",
        );
        assert!(error.to_string().contains("one cadence"), "{error}");
        assert!(error.to_string().contains("weekly"), "{error}");
    }

    #[test]
    fn timeout_env_and_artifacts_pass_through_verbatim() {
        let mut load = profile("load");
        load.timeout_minutes = 90;
        load.env.insert("MAX_SECONDS".to_owned(), "300".to_owned());
        load.env
            .insert("COVERAGE_FLOOR".to_owned(), ">= 99.5".to_owned());
        load.artifacts = vec!["load-results/".to_owned(), "traces.ndjson".to_owned()];
        let config = profile_config(vec![load]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-checks"),
            "select every profile",
        );
        let workflow = render(&config, None, &selected);
        assert!(workflow.contains("timeout-minutes: 90"), "{workflow}");
        assert!(workflow.contains("MAX_SECONDS: \"300\""), "{workflow}");
        assert!(
            workflow.contains("COVERAGE_FLOOR: \">= 99.5\""),
            "{workflow}"
        );
        assert!(workflow.contains("Upload load artifacts"), "{workflow}");
        assert!(workflow.contains("load-results/"), "{workflow}");
        assert!(workflow.contains("traces.ndjson"), "{workflow}");
        assert!(
            workflow.contains(crate::ActionPin::UploadArtifact.reference()),
            "{workflow}"
        );
    }

    #[test]
    fn tools_render_per_lane() {
        let mut hosted = profile("hosted");
        hosted.tools = vec!["ripgrep".to_owned(), "cargo:example-tool".to_owned()];
        let mut fleet = profile("fleet");
        fleet.runner = "velnor".to_owned();
        fleet.tools = vec!["ripgrep".to_owned()];
        let mut bare = profile("bare");
        bare.runner = "velnor".to_owned();
        let config = profile_config(vec![hosted, fleet, bare]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-checks"),
            "select every profile",
        );
        let workflow = render(&config, None, &selected);
        assert!(
            workflow.contains("install_args: \"ripgrep cargo:example-tool\""),
            "{workflow}"
        );
        assert_eq!(
            workflow.matches(crate::ActionPin::Mise.reference()).count(),
            1,
            "one hosted profile owns one Mise bootstrap action: {workflow}"
        );
        assert!(workflow.contains("MISE_TOOLS: ripgrep"), "{workflow}");
        assert!(
            workflow.contains("mise --yes --locked install \"${tools[@]}\""),
            "{workflow}"
        );
        let mut bare_job = String::new();
        must(
            render_profile_job(&mut bare_job, &config, &config.check_profiles[2], None),
            "render the tool-less Velnor job",
        );
        assert!(
            !bare_job.contains("Install declared Mise tools"),
            "a tool-less Velnor job installs nothing: {bare_job}"
        );
        assert!(
            workflow.contains(crate::ActionPin::Mise.reference()),
            "{workflow}"
        );
    }

    #[test]
    fn needs_render_within_one_file_and_cross_file_is_refused() {
        let smoke = profile("smoke");
        let mut load = profile("load");
        load.needs = vec!["smoke".to_owned()];
        let all = vec![smoke, load];
        let map = args_for("");
        let selected = must(
            select_profiles(&all, &Args(&map), "scheduled-checks"),
            "select every profile",
        );
        let config = profile_config(Vec::new());
        let workflow = render(&config, None, &selected);
        assert!(workflow.contains("needs: [smoke]"), "{workflow}");

        let subset = args_for(r#"profiles = ["load"]"#);
        let error = must_fail(
            select_profiles(&all, &Args(&subset), "scheduled-checks"),
            "a dependency outside the file must fail selection",
        );
        assert!(
            error.to_string().contains("one scheduled workflow file"),
            "{error}"
        );
    }

    #[test]
    fn dependency_cycles_are_refused() {
        let mut first = profile("first");
        first.needs = vec!["second".to_owned()];
        let mut second = profile("second");
        second.needs = vec!["first".to_owned()];
        let all = vec![first, second];
        let map = args_for("");
        let error = must_fail(
            select_profiles(&all, &Args(&map), "scheduled-checks"),
            "a dependency cycle must fail selection",
        );
        assert!(error.to_string().contains("cycle"), "{error}");
        assert!(
            error.to_string().contains("first -> second -> first"),
            "{error}"
        );
    }

    #[test]
    fn unknown_repeated_and_empty_selections_are_refused() {
        let smoke = profile("smoke");
        let all = vec![smoke];
        let unknown = args_for(r#"profiles = ["nope"]"#);
        let error = must_fail(
            select_profiles(&all, &Args(&unknown), "scheduled-checks"),
            "an unknown profile must fail selection",
        );
        assert!(error.to_string().contains("nope"), "{error}");

        let repeated = args_for(r#"profiles = ["smoke", "smoke"]"#);
        let error = must_fail(
            select_profiles(&all, &Args(&repeated), "scheduled-checks"),
            "a repeated profile must fail selection",
        );
        assert!(error.to_string().contains("twice"), "{error}");

        let empty = args_for(r"profiles = []");
        let error = must_fail(
            select_profiles(&all, &Args(&empty), "scheduled-checks"),
            "an empty profiles list must fail selection",
        );
        assert!(error.to_string().contains("empty"), "{error}");

        let configured: Vec<CheckProfileSpec> = Vec::new();
        let map = args_for("");
        let error = must_fail(
            select_profiles(&configured, &Args(&map), "scheduled-checks"),
            "no configured profiles must fail selection",
        );
        assert!(error.to_string().contains("[[check_profile]]"), "{error}");
    }

    #[test]
    fn velnor_lane_needs_labels_and_renders_them() {
        let mut fleet = profile("fleet");
        fleet.runner = "velnor".to_owned();
        let config = profile_config(vec![fleet.clone()]);
        let runs_on = must(
            profile_runs_on(&config, &fleet),
            "render the Velnor lane selector",
        );
        assert_eq!(runs_on, "[self-hosted, example-lane]");

        let mut unlabeled = profile_config(Vec::new());
        unlabeled.velnor_labels = Vec::new();
        let error = must_fail(
            profile_runs_on(&unlabeled, &fleet),
            "a Velnor profile without labels must fail",
        );
        assert!(error.to_string().contains("velnor_labels"), "{error}");
    }

    #[test]
    fn runner_selection_covers_hosted_lanes_and_refuses_unknown() {
        let config = profile_config(Vec::new());
        let hosted = profile("hosted");
        assert_eq!(
            must(profile_runs_on(&config, &hosted), "render the hosted lane"),
            "ubuntu-24.04"
        );
        let mut apple = profile("apple");
        apple.runner = "macos".to_owned();
        assert_eq!(
            must(profile_runs_on(&config, &apple), "render the Apple lane"),
            "macos-15"
        );
        let mut unknown = profile("unknown");
        unknown.runner = "planetary".to_owned();
        let error = must_fail(
            profile_runs_on(&config, &unknown),
            "an unknown lane must fail",
        );
        assert!(error.to_string().contains("planetary"), "{error}");
    }

    #[test]
    fn custom_workflow_name_overrides_stem() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-checks"),
            "select every profile",
        );
        let workflow = render(&config, Some("Daily probes".to_owned()), &selected);
        assert!(workflow.contains("name: \"Daily probes\""), "{workflow}");
    }

    #[test]
    fn multi_word_name_renders_one_valid_run_name_scalar() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-checks"),
            "select every profile",
        );
        let workflow = render(&config, Some("Daily probes".to_owned()), &selected);
        assert!(
            workflow.contains("run-name: \"Daily probes · ${{ github.event_name }}\""),
            "the composed value must be quoted as one scalar: {workflow}"
        );
        assert!(
            !workflow.contains("\"Daily probes\" · "),
            "a quoted part followed by more content is not valid YAML: {workflow}"
        );
    }

    fn unscheduled(id: &str) -> CheckProfileSpec {
        let mut spec = profile(id);
        spec.schedule = String::new();
        spec
    }

    fn render_with_events(
        config: &ProjectConfig,
        name: Option<String>,
        profiles: &[&CheckProfileSpec],
        events: &[String],
    ) -> String {
        render_with_events_and_branches(config, name, profiles, events, &[])
    }

    fn render_with_events_and_branches(
        config: &ProjectConfig,
        name: Option<String>,
        profiles: &[&CheckProfileSpec],
        events: &[String],
        branches: &[String],
    ) -> String {
        must(
            render_checks_file(
                config,
                "scheduled-daily.yml",
                name,
                profiles,
                events,
                branches,
                false,
            ),
            "render scheduled checks",
        )
    }

    #[test]
    fn evented_file_renders_push_pr_triggers_plus_shared_cron() {
        let smoke = profile("smoke");
        let load = profile("load");
        let config = profile_config(vec![smoke, load]);
        let map = args_for(r#"events = ["push", "pull_request", "merge_group"]"#);
        let args = Args(&map);
        let selected = must(
            select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
            "select every profile",
        );
        let events = must(
            select_events(&args, "scheduled-daily.yml"),
            "select the declared events",
        );
        assert_eq!(events, vec!["push", "pull_request", "merge_group"]);
        let workflow = render_with_events(&config, None, &selected, &events);
        assert!(workflow.contains("  push:\n"), "{workflow}");
        assert!(workflow.contains("  pull_request:\n"), "{workflow}");
        assert!(workflow.contains("  merge_group:\n"), "{workflow}");
        assert!(workflow.contains("- cron: \"23 2 * * *\""), "{workflow}");
        assert!(workflow.contains("workflow_dispatch:"), "{workflow}");
        let push = workflow.find("  push:\n").unwrap_or(usize::MAX);
        let pull = workflow.find("  pull_request:\n").unwrap_or(usize::MAX);
        let cron = workflow.find("- cron:").unwrap_or(usize::MAX);
        let dispatch = workflow.find("workflow_dispatch:").unwrap_or(usize::MAX);
        assert!(
            push < pull && pull < cron && cron < dispatch,
            "events render before the shared cron, dispatch last: {workflow}"
        );
    }

    #[test]
    fn declared_events_canonicalize_to_push_then_pull_request() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for(r#"events = ["merge_group", "pull_request", "push"]"#);
        let args = Args(&map);
        let events = must(
            select_events(&args, "scheduled-daily.yml"),
            "select the declared events",
        );
        assert_eq!(events, vec!["push", "pull_request", "merge_group"]);
        let selected = must(
            select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
            "select every profile",
        );
        let workflow = render_with_events(&config, None, &selected, &events);
        let push = workflow.find("  push:\n").unwrap_or(usize::MAX);
        let pull = workflow.find("  pull_request:\n").unwrap_or(usize::MAX);
        let merge = workflow.find("  merge_group:\n").unwrap_or(usize::MAX);
        assert!(push < pull && pull < merge, "{workflow}");
    }

    #[test]
    fn declared_workflow_dispatch_validates_without_rendering_twice() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map =
            args_for(r#"events = ["push", "pull_request", "merge_group", "workflow_dispatch"]"#);
        let args = Args(&map);
        let events = must(
            select_events(&args, "scheduled-daily.yml"),
            "dispatch is accepted",
        );
        assert_eq!(events, vec!["push", "pull_request", "merge_group"]);
        let selected = must(
            select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
            "select every profile",
        );
        let workflow = render_with_events(&config, None, &selected, &events);
        assert_eq!(
            workflow.matches("workflow_dispatch:").count(),
            1,
            "{workflow}"
        );
    }

    #[test]
    fn declared_branches_scope_the_push_trigger_only() {
        let smoke = unscheduled("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for(
            r#"events = ["push", "pull_request"]
branches = ["main", "release/*"]"#,
        );
        let args = Args(&map);
        let events = must(
            select_events(&args, "scheduled-daily.yml"),
            "select the declared events",
        );
        let branches = must(
            select_branches(&args, "scheduled-daily.yml", &events),
            "select the declared branches",
        );
        assert_eq!(branches, vec!["main", "release/*"]);
        let selected = must(
            select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
            "select every profile",
        );
        let workflow =
            render_with_events_and_branches(&config, None, &selected, &events, &branches);
        assert!(
            workflow.contains("  push:\n    branches: [main, \"release/*\"]\n"),
            "glob branches quote (`*` is a YAML alias indicator): {workflow}"
        );
        assert!(
            workflow.contains("  pull_request:\n"),
            "branches never scope pull_request: {workflow}"
        );
        assert!(
            !workflow.contains("  pull_request:\n    branches:"),
            "{workflow}"
        );
    }

    #[test]
    fn absent_branches_render_the_bare_push_trigger() {
        let smoke = unscheduled("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for(r#"events = ["push"]"#);
        let args = Args(&map);
        let events = must(
            select_events(&args, "scheduled-daily.yml"),
            "select the declared events",
        );
        let branches = must(
            select_branches(&args, "scheduled-daily.yml", &events),
            "absent branches default to empty",
        );
        assert!(branches.is_empty());
        let selected = must(
            select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
            "select every profile",
        );
        let workflow =
            render_with_events_and_branches(&config, None, &selected, &events, &branches);
        assert!(workflow.contains("  push:\n"), "{workflow}");
        assert!(!workflow.contains("branches:"), "{workflow}");
    }

    #[test]
    fn branches_without_push_empty_and_multiline_are_refused() {
        let no_push = args_for(
            r#"events = ["pull_request"]
branches = ["main"]"#,
        );
        let error = must_fail(
            select_branches(
                &Args(&no_push),
                "scheduled-daily.yml",
                &["pull_request".to_owned()],
            ),
            "branches without push must fail",
        );
        assert!(error.to_string().contains("scheduled-daily.yml"), "{error}");
        assert!(error.to_string().contains("`branches`"), "{error}");
        assert!(error.to_string().contains("`push`"), "{error}");

        let empty = args_for(
            r#"events = ["push"]
branches = []"#,
        );
        let error = must_fail(
            select_branches(&Args(&empty), "scheduled-daily.yml", &["push".to_owned()]),
            "empty branches must fail",
        );
        assert!(error.to_string().contains("empty `branches`"), "{error}");

        let multiline = args_for("events = [\"push\"]\nbranches = [\"main\\nx\"]");
        let error = must_fail(
            select_branches(
                &Args(&multiline),
                "scheduled-daily.yml",
                &["push".to_owned()],
            ),
            "multiline branches must fail",
        );
        assert!(error.to_string().contains("one non-empty line"), "{error}");
    }

    #[test]
    fn cron_less_evented_file_renders_no_schedule_trigger() {
        let smoke = unscheduled("smoke");
        let load = unscheduled("load");
        let config = profile_config(vec![smoke, load]);
        let map = args_for(r#"events = ["push", "pull_request", "merge_group"]"#);
        let args = Args(&map);
        let selected = must(
            select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
            "schedule-less profiles select into an evented file",
        );
        let events = must(
            select_events(&args, "scheduled-daily.yml"),
            "select the declared events",
        );
        let workflow = render_with_events(&config, None, &selected, &events);
        assert!(workflow.contains("  push:\n"), "{workflow}");
        assert!(workflow.contains("  pull_request:\n"), "{workflow}");
        assert!(workflow.contains("  merge_group:\n"), "{workflow}");
        assert!(workflow.contains("workflow_dispatch:"), "{workflow}");
        assert!(!workflow.contains("schedule:"), "{workflow}");
        assert!(!workflow.contains("cron:"), "{workflow}");
        assert!(workflow.contains("  smoke:\n"), "{workflow}");
        assert!(workflow.contains("  load:\n"), "{workflow}");
    }

    #[test]
    fn schedule_less_profile_in_cron_file_is_refused() {
        let smoke = unscheduled("smoke");
        let all = vec![smoke];
        let map = args_for("");
        let error = must_fail(
            select_profiles(&all, &Args(&map), "scheduled-daily.yml"),
            "a schedule-less profile without file events must fail selection",
        );
        assert!(error.to_string().contains("scheduled-daily.yml"), "{error}");
        assert!(error.to_string().contains("smoke"), "{error}");
        assert!(error.to_string().contains("`events`"), "{error}");
    }

    #[test]
    fn mixed_scheduled_and_schedule_less_profiles_are_refused() {
        let smoke = profile("smoke");
        let load = unscheduled("load");
        let all = vec![smoke, load];
        let map = args_for(r#"events = ["push"]"#);
        let error = must_fail(
            select_profiles(&all, &Args(&map), "scheduled-daily.yml"),
            "events do not excuse a mixed trigger set",
        );
        assert!(error.to_string().contains("scheduled-daily.yml"), "{error}");
        assert!(error.to_string().contains("one trigger set"), "{error}");
        assert!(error.to_string().contains("smoke"), "{error}");
        assert!(error.to_string().contains("load"), "{error}");
    }

    #[test]
    fn unknown_empty_repeated_and_mistyped_events_are_refused() {
        let unknown = args_for(r#"events = ["release"]"#);
        let error = must_fail(
            select_events(&Args(&unknown), "scheduled-daily.yml"),
            "an unknown event must fail selection",
        );
        assert!(error.to_string().contains("scheduled-daily.yml"), "{error}");
        assert!(error.to_string().contains("release"), "{error}");
        assert!(error.to_string().contains("accepted events"), "{error}");

        let empty = args_for(r"events = []");
        let error = must_fail(
            select_events(&Args(&empty), "scheduled-daily.yml"),
            "an empty events list must fail selection",
        );
        assert!(error.to_string().contains("empty"), "{error}");

        let repeated = args_for(r#"events = ["push", "push"]"#);
        let error = must_fail(
            select_events(&Args(&repeated), "scheduled-daily.yml"),
            "a repeated event must fail selection",
        );
        assert!(error.to_string().contains("twice"), "{error}");

        let mistyped = args_for(r#"events = "push""#);
        let error = must_fail(
            select_events(&Args(&mistyped), "scheduled-daily.yml"),
            "a non-array events value must fail selection",
        );
        assert!(error.to_string().contains("an array of strings"), "{error}");
    }

    #[test]
    fn evented_files_cancel_pull_requests_only() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-daily.yml"),
            "select every profile",
        );
        let cron_only = render(&config, None, &selected);
        assert!(
            cron_only.contains("cancel-in-progress: true"),
            "{cron_only}"
        );
        assert!(
            !cron_only.contains("cancel-in-progress: ${{"),
            "{cron_only}"
        );

        let map = args_for(r#"events = ["push", "pull_request"]"#);
        let args = Args(&map);
        let selected = must(
            select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
            "select every profile",
        );
        let events = must(
            select_events(&args, "scheduled-daily.yml"),
            "select the declared events",
        );
        let evented = render_with_events(&config, None, &selected, &events);
        assert!(
            evented.contains("cancel-in-progress: ${{ github.event_name == 'pull_request' }}"),
            "{evented}"
        );
        assert!(
            evented.contains("group: scheduled-daily-${{ github.repository }}-${{ github.ref }}"),
            "{evented}"
        );
    }

    #[test]
    fn push_and_merge_group_runs_are_lossless() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        for events_toml in [
            r#"events = ["push"]"#,
            r#"events = ["push"]
branches = ["main"]"#,
            r#"events = ["merge_group"]"#,
            r#"events = ["push", "merge_group", "workflow_dispatch"]"#,
        ] {
            let map = args_for(events_toml);
            let args = Args(&map);
            let selected = must(
                select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
                "select every profile",
            );
            let events = must(
                select_events(&args, "scheduled-daily.yml"),
                "select the declared events",
            );
            let branches = must(
                select_branches(&args, "scheduled-daily.yml", &events),
                "select the declared branches",
            );
            let rendered =
                render_with_events_and_branches(&config, None, &selected, &events, &branches);
            assert!(
                rendered.contains("cancel-in-progress: false"),
                "a push/merge-group file must retain every committed/candidate verdict: {events_toml}\n{rendered}"
            );
            assert!(
                !rendered.contains("cancel-in-progress: ${{"),
                "the PR expression would be constant-false without the trigger: {events_toml}\n{rendered}"
            );
        }
    }

    #[test]
    fn dispatch_only_files_keep_supersession_without_commit_evidence() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for(r#"events = ["workflow_dispatch"]"#);
        let args = Args(&map);
        let selected = must(
            select_profiles(&config.check_profiles, &args, "scheduled-daily.yml"),
            "select every profile",
        );
        let events = must(
            select_events(&args, "scheduled-daily.yml"),
            "select the declared events",
        );
        let rendered = render_with_events(&config, None, &selected, &events);
        assert!(rendered.contains("cancel-in-progress: true"), "{rendered}");
    }

    #[test]
    fn cron_only_trigger_block_is_unchanged() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-daily.yml"),
            "select every profile",
        );
        let workflow = render(&config, None, &selected);
        assert!(
            workflow
                .contains("on:\n  schedule:\n    - cron: \"23 2 * * *\"\n  workflow_dispatch:\n"),
            "{workflow}"
        );
    }

    fn render_with_lanes(
        config: &ProjectConfig,
        profiles: &[&CheckProfileSpec],
    ) -> Result<String, GeneratorError> {
        render_checks_file(
            config,
            "scheduled-daily.yml",
            None,
            profiles,
            &[],
            &[],
            true,
        )
    }

    fn select_all<'a>(config: &'a ProjectConfig, args: &Args<'_>) -> Vec<&'a CheckProfileSpec> {
        must(
            select_profiles(&config.check_profiles, args, "scheduled-daily.yml"),
            "select every profile",
        )
    }

    #[test]
    fn absent_lanes_input_renders_no_lanes_surface() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for("");
        let selected = select_all(&config, &Args(&map));
        let workflow = render(&config, None, &selected);
        assert!(
            workflow.contains("  workflow_dispatch:\n\npermissions:"),
            "the dispatch trigger stays bare: {workflow}"
        );
        for marker in ["lanes:", "inputs.lanes", "fromJSON"] {
            assert!(
                !workflow.contains(marker),
                "an undeclared lanes input renders nothing: {marker} in {workflow}"
            );
        }
        assert!(
            workflow.contains("    runs-on: ubuntu-24.04"),
            "the job stays on its static lane: {workflow}"
        );
    }

    #[test]
    fn lanes_input_renders_homogeneous_github_dispatch_file() {
        let smoke = profile("smoke");
        let load = profile("load");
        let config = profile_config(vec![smoke, load]);
        let map = args_for("lanes_input = true");
        let args = Args(&map);
        assert!(
            must(args.flag("lanes_input"), "read the lanes flag"),
            "the row declares the lanes input"
        );
        let selected = select_all(&config, &args);
        let workflow = must(
            render_with_lanes(&config, &selected),
            "render the lanes file",
        );
        assert!(
            workflow.contains(
                "  workflow_dispatch:\n    inputs:\n      lanes:\n        description: github (default) | velnor\n        type: choice\n        default: github\n        options: [github, velnor]\n"
            ),
            "the dispatch carries the github-default lanes choice: {workflow}"
        );
        assert_eq!(
            workflow.matches("inputs.lanes == 'velnor'").count(),
            2,
            "every job dispatches across lanes: {workflow}"
        );
        for job in ["  smoke:\n", "  load:\n"] {
            assert_eq!(
                workflow.matches(job).count(),
                1,
                "one conditional job per profile, never per-lane legs: {workflow}"
            );
        }
        for marker in ["(Velnor)", "(GitHub)", "matrix"] {
            assert!(
                !workflow.contains(marker),
                "no lane-suffixed legs or fan-out: {marker} in {workflow}"
            );
        }
    }

    #[test]
    fn lanes_input_renders_velnor_default_for_velnor_files() {
        let mut fleet = profile("fleet");
        fleet.runner = "velnor".to_owned();
        let mut sweep = profile("sweep");
        sweep.runner = "velnor".to_owned();
        let config = profile_config(vec![fleet, sweep]);
        let map = args_for("lanes_input = true");
        let args = Args(&map);
        let selected = select_all(&config, &args);
        let workflow = must(
            render_with_lanes(&config, &selected),
            "render the lanes file",
        );
        assert!(
            workflow.contains(
                "      lanes:\n        description: velnor (default) | github\n        type: choice\n        default: velnor\n        options: [velnor, github]\n"
            ),
            "the dispatch carries the velnor-default lanes choice: {workflow}"
        );
        assert_eq!(
            workflow.matches("inputs.lanes == 'github'").count(),
            2,
            "every job dispatches across lanes: {workflow}"
        );
    }

    #[test]
    fn lanes_input_leaves_macos_jobs_static() {
        let smoke = profile("smoke");
        let mut apple = profile("apple");
        apple.runner = "macos".to_owned();
        let config = profile_config(vec![smoke, apple]);
        let map = args_for("lanes_input = true");
        let args = Args(&map);
        let selected = select_all(&config, &args);
        let workflow = must(
            render_with_lanes(&config, &selected),
            "render the lanes file",
        );
        assert!(
            workflow.contains("default: github"),
            "the dispatchable lane sets the default: {workflow}"
        );
        assert!(
            workflow.contains("    runs-on: macos-15"),
            "the macos job keeps its static label: {workflow}"
        );
        assert_eq!(
            workflow.matches("inputs.lanes").count(),
            1,
            "only the dispatchable job threads the conditional: {workflow}"
        );
    }

    #[test]
    fn lanes_input_refuses_mixed_dispatch_lanes() {
        let smoke = profile("smoke");
        let mut fleet = profile("fleet");
        fleet.runner = "velnor".to_owned();
        let config = profile_config(vec![smoke, fleet]);
        let map = args_for("lanes_input = true");
        let args = Args(&map);
        let selected = select_all(&config, &args);
        let error = must_fail(
            render_with_lanes(&config, &selected),
            "mixed dispatch lanes must fail the file",
        );
        assert!(
            error.to_string().contains("scheduled-daily.yml"),
            "the error names the file: {error}"
        );
        assert!(error.to_string().contains("smoke"), "{error}");
        assert!(error.to_string().contains("fleet"), "{error}");
        assert!(error.to_string().contains("one lane"), "{error}");
    }

    #[test]
    fn lanes_input_needs_a_dispatchable_profile() {
        let mut apple = profile("apple");
        apple.runner = "macos".to_owned();
        let config = profile_config(vec![apple]);
        let map = args_for("lanes_input = true");
        let args = Args(&map);
        let selected = select_all(&config, &args);
        let error = must_fail(
            render_with_lanes(&config, &selected),
            "a macos-only file must fail the lanes input",
        );
        assert!(
            error.to_string().contains("scheduled-daily.yml"),
            "the error names the file: {error}"
        );
        assert!(error.to_string().contains("macos"), "{error}");
    }

    #[test]
    fn lanes_input_needs_both_lanes() {
        let smoke = profile("smoke");
        let mut config = profile_config(vec![smoke]);
        config.runners = crate::RunnerMode::Github;
        let map = args_for("lanes_input = true");
        let args = Args(&map);
        let selected = select_all(&config, &args);
        let error = must_fail(
            render_with_lanes(&config, &selected),
            "a single-lane repository must fail admission",
        );
        assert!(error.to_string().contains("both"), "{error}");
    }
}

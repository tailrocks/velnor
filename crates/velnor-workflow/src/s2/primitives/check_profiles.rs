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
//! `merge_group`, `workflow_dispatch`): the file then renders those triggers alongside the
//! shared cron, and profiles in an evented file may omit `schedule` entirely
//! for a cron-less evented file. One file carries one trigger set — scheduled
//! and schedule-less profiles never mix in one file — and runners stay
//! per-profile: each job runs on its own profile's runner exactly as in a
//! cron-only file, because triggers change when a job runs, never where.
//!
//! Why a file, not a CI unit: a scheduled check is a whole-repo compliance
//! probe that needs its own required status context plus main-branch runs
//! independent of affected-unit selection. A CI unit renders only when the
//! scan selects it and reports under unit providers, so folding a repo-wide gate
//! into a unit would make compliance conditional on selection and lose the
//! standalone required signal. Event triggers therefore live on the
//! scheduled-checks file, not on a unit.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use super::{Args, Primitive, ProviderAdmission, RenderCtx, Rendered, WorkflowIr};
use crate::s2::provider::{runs_on_for, ProviderId, RunnerTarget};
use crate::s2::{
    runs_on_labels_yaml, yaml_scalar, ActionPin, CheckProfileSpec, GeneratorError, ProjectConfig,
    GENERATED_HEADER,
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
        &["name", "profiles", "events", "branches"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let file = ctx.file.unwrap_or_default();
        let label = if file.is_empty() { ctx.family } else { file };
        let profiles = select_profiles(&ctx.config.check_profiles, args, label)?;
        let events = select_events(args, label)?;
        let branches = select_branches(args, label, &events)?;
        let content = render_checks_file(
            ctx.config,
            file,
            args.string("name")?,
            &profiles,
            &events,
            &branches,
        )?;
        render_file(ctx, content)
    }
}

/// A profile with an extra repository token capability must never run from a
/// pull-request trigger. Pull-request jobs execute contributor-controlled
/// task code; granting that code access to Actions history would turn a
/// read-only capability into an information-disclosure path. Scheduled and
/// default-branch push/dispatch jobs remain eligible, while a mixed file fails
/// closed instead of relying on a task author to remember an event distinction.
fn validate_profile_permissions(
    profiles: &[&CheckProfileSpec],
    events: &[String],
    file: &str,
) -> Result<(), GeneratorError> {
    if !events.iter().any(|event| event == "pull_request") {
        return Ok(());
    }
    for profile in profiles {
        if !profile.permissions.is_empty() {
            return Err(GeneratorError::usage(format!(
                "`{file}` cannot grant permissions to check profile `{}` because `pull_request` runs contributor-controlled tasks; place the read capability on a schedule/push/dispatch-only file",
                profile.id
            )));
        }
    }
    Ok(())
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

/// The push branches a `scheduled-checks` row scopes its push trigger to.
///
/// An absent `branches` argument renders an unscoped push trigger.
/// `branches` without a `push` event is incoherent — the filter would
/// scope a trigger the file never renders — so it fails with the file
/// named.
///
/// # Errors
/// Returns a usage error naming the file for an empty list, a `branches`
/// declaration without `push` in `events`, or a branch that is empty or
/// spans lines.
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
) -> Result<String, GeneratorError> {
    validate_profile_permissions(profiles, events, file)?;
    let stem = file.strip_suffix(".yml").unwrap_or(file);
    let name = name.unwrap_or_else(|| stem.to_owned());
    let schedule = profiles
        .first()
        .map(|profile| profile.schedule.as_str())
        .unwrap_or_default();
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
    output.push_str("  workflow_dispatch:\n\npermissions:\n  contents: read\n\nconcurrency:\n");
    let has_pull_request = events.iter().any(|event| event == "pull_request");
    let has_committed_event = events
        .iter()
        .any(|event| matches!(event.as_str(), "push" | "merge_group"));
    if has_committed_event {
        // GitHub keeps only one running and one pending run per concurrency
        // group. A shared ref key therefore still loses an older pending
        // commit even with cancellation disabled. Commit-scoping committed
        // and merge-queue events makes each tree's evidence independently
        // queueable; PR attempts remain supersedable by their ref.
        if has_pull_request {
            let _ = writeln!(
                output,
                "  group: {stem}-${{{{ github.repository }}}}-${{{{ github.event_name == 'pull_request' && github.ref || github.sha }}}}"
            );
        } else {
            let _ = writeln!(
                output,
                "  group: {stem}-${{{{ github.repository }}}}-${{{{ github.sha }}}}"
            );
        }
    } else {
        let _ = writeln!(
            output,
            "  group: {stem}-${{{{ github.repository }}}}-${{{{ github.ref }}}}"
        );
    }
    if has_pull_request {
        // PR attempts supersede each other for fast feedback. Committed and
        // merge-queue evidence uses the non-canceling path above, so a later
        // event cannot erase a predecessor's verdict.
        output.push_str(
            "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n\njobs:\n",
        );
    } else if has_committed_event {
        // A push or merge-group run is evidence for a committed/candidate
        // tree. The SHA-scoped group above prevents pending replacement.
        output.push_str("  cancel-in-progress: false\n\njobs:\n");
    } else {
        // A cron-only or dispatch-only file has no candidate/main event to
        // preserve, so retain the historical supersession behavior.
        output.push_str("  cancel-in-progress: true\n\njobs:\n");
    }
    for profile in profiles {
        render_profile_job(&mut output, config, profile)?;
    }
    Ok(output)
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
    render_checks_file(config, file, name, profiles, &[], &[])
}

/// One profile job: the runner it runs on, the timeout it holds, the threshold
/// environment its tasks read, and the steps that check out, provision tools,
/// run the named tasks, and upload the declared artifacts.
fn render_profile_job(
    output: &mut String,
    config: &ProjectConfig,
    profile: &CheckProfileSpec,
) -> Result<(), GeneratorError> {
    let _ = writeln!(output, "  {}:", profile.id);
    let _ = writeln!(output, "    name: {}", yaml_scalar(&profile.name));
    if !profile.needs.is_empty() {
        let _ = writeln!(output, "    needs: [{}]", profile.needs.join(", "));
    }
    // Both local providers mount a caller-managed workspace, so both skip
    // fork and bot pull requests. Hosted lanes remain available to untrusted
    // events, including when they are rendered from the same provider plan.
    if profile.runner.is_local() {
        let admission = WorkflowIr::from_config(config).provider_admission_expression(
            ProviderAdmission::ProviderTrusted(profile.runner.provider()),
        );
        let _ = writeln!(output, "    if: ${{{{ ({admission}) }}}}");
    }
    let runs_on = profile_runs_on(config, profile)?;
    let _ = writeln!(output, "    runs-on: {runs_on}");
    let _ = writeln!(output, "    timeout-minutes: {}", profile.timeout_minutes);
    if profile.advisory {
        output.push_str("    continue-on-error: true\n");
    }
    if !profile.permissions.is_empty() {
        // GitHub replaces a job-level permissions map instead of merging it
        // with the workflow default. Keep checkout access explicit while
        // adding only the validated read-only capabilities.
        let mut permissions = profile.permissions.clone();
        permissions
            .entry("contents".to_owned())
            .or_insert_with(|| "read".to_owned());
        output.push_str("    permissions:\n");
        for (scope, level) in &permissions {
            let _ = writeln!(output, "      {scope}: {level}");
        }
    }
    if !profile.env.is_empty() {
        output.push_str("    env:\n");
        for (key, value) in &profile.env {
            let _ = writeln!(output, "      {key}: {}", yaml_scalar(value));
        }
    }
    output.push_str("    steps:\n");
    render_checkout_step(output, profile.full_history);
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

/// The `runs-on` for one profile: the hosted selector, the fixed
/// GitHub-owned Apple image, or the repository's own Velnor selector.
fn profile_runs_on(
    config: &ProjectConfig,
    profile: &CheckProfileSpec,
) -> Result<String, GeneratorError> {
    match profile.runner {
        RunnerTarget::Provider(provider) => {
            if !config.providers.contains(&provider) {
                return Err(GeneratorError::usage(format!(
                    "check profile `{}` selects provider `{provider}`, but it is absent from [workflow] providers",
                    profile.id
                )));
            }
            runs_on_for(&config.selectors, provider).map(runs_on_labels_yaml)
        }
        RunnerTarget::GithubHostedMacos => {
            if !config.providers.contains(&ProviderId::GithubHosted) {
                return Err(GeneratorError::usage(format!(
                    "check profile `{}` selects hosted macOS, but github-hosted is absent from [workflow] providers",
                    profile.id
                )));
            }
            Ok(yaml_scalar(crate::s2::MACOS_HOSTED_RUNS_ON))
        }
    }
}

fn render_checkout_step(output: &mut String, full_history: bool) {
    let checkout = ActionPin::Checkout.reference();
    // Profile jobs are standalone: no `inputs` indirection, so a deep
    // profile renders the static depth and every other profile keeps
    // today's exact bytes.
    let fetch_depth = if full_history {
        "\n          fetch-depth: 0"
    } else {
        ""
    };
    let _ = writeln!(
        output,
        "      - name: Checkout repository\n        uses: {checkout}\n        with:\n          persist-credentials: false{fetch_depth}"
    );
}

/// Tool provisioning per profile runner. Hosted runners install through the
/// pinned `mise` action, which the Velnor fleet cannot admit, so Velnor
/// installs with the preinstalled `mise` binary instead. Every profile runs
/// named tasks, so hosted runners always set up even when no tool needs
/// installing.
/// Refuse a check profile whose closed tool subset planning cannot prove
/// installable: the same promise as the unit validator, over the profile's
/// own `tools` list. A member whose backend planning cannot model, or a
/// `depends` name the lock does not pin, fails here with the exact missing
/// edge instead of failing at install time on the runner.
///
/// # Errors
/// Returns a usage error naming the first profile whose subset is not
/// provably closed, with every key the lock does pin.
pub(crate) fn validate_profile_install_deps_are_closed(
    profiles: &[CheckProfileSpec],
    lock_keys: &BTreeSet<String>,
    lock_backends: &std::collections::BTreeMap<String, String>,
    install_deps: &crate::s2::config::MiseInstallDeps,
) -> Result<(), GeneratorError> {
    for profile in profiles {
        let mut tools = profile.tools.clone();
        super::close_mise_tool_subset(&mut tools, lock_keys, lock_backends, install_deps);
        let known = || lock_keys.iter().cloned().collect::<Vec<_>>().join(", ");
        for tool in &tools {
            if let Err(unknown) = super::member_backend_key(tool, lock_backends) {
                let reason = super::unknown_backend_reason(&unknown, "tools");
                return Err(GeneratorError::usage(format!(
                    "[[check_profile]] {} installs {tool}, {reason} (planning models mise {} install dependencies), known keys: {}",
                    profile.id,
                    super::MISE_INSTALL_DEPS_MODEL_VERSION,
                    known()
                )));
            }
        }
        for tool in &tools {
            let Some(names) = install_deps.depends.get(tool) else {
                continue;
            };
            for name in names {
                if super::resolve_install_dep_names(name, lock_keys).is_empty() {
                    return Err(GeneratorError::usage(format!(
                        "[[check_profile]] {} installs {tool}, whose mise.toml `depends` names `{name}`, but mise.lock pins no such key; pin it and re-lock so every install_args subset is installable, known keys: {}",
                        profile.id,
                        known()
                    )));
                }
            }
        }
    }
    Ok(())
}

fn render_tool_steps(output: &mut String, config: &ProjectConfig, profile: &CheckProfileSpec) {
    let mise = ActionPin::Mise.reference();
    // The declared subset closes over the root config's install edges like
    // every derived subset: mise refuses a locked install that omits a
    // configured dependency.
    let mut tools = profile.tools.clone();
    super::close_mise_tool_subset(
        &mut tools,
        &config.mise_lock_keys,
        &config.mise_lock_backends,
        &config.mise_install_deps,
    );
    if profile.runner.is_local() {
        if !tools.is_empty() {
            let _ = writeln!(
                output,
                "      - name: Install declared Mise tools\n        env:\n          MISE_TOOLS: {}\n        run: |\n          set -euo pipefail\n          read -ra tools <<<\"$MISE_TOOLS\"\n          mise --yes --locked install \"${{tools[@]}}\"",
                yaml_scalar(&tools.join(" "))
            );
        }
        return;
    }
    if tools.is_empty() {
        let _ = writeln!(
            output,
            "      - name: Set up Mise\n        uses: {mise}\n        with:\n          install: false"
        );
    } else {
        let trusted = super::trusted_cache_save_expression(&config.default_branch);
        let _ = writeln!(
            output,
            "      - name: Set up Mise\n        uses: {mise}\n        with:\n          install_args: {}\n          cache: true\n          cache_save: ${{{{ {trusted} }}}}",
            yaml_scalar(&tools.join(" "))
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

    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::s2::config;
    use crate::s2::provider::RunnerTarget;

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
            runner: RunnerTarget::Provider(ProviderId::GithubHosted),
            tools: Vec::new(),
            tasks: vec![format!("check-{id}")],
            needs: Vec::new(),
            timeout_minutes: DEFAULT_CHECK_PROFILE_TIMEOUT_MINUTES,
            artifacts: Vec::new(),
            advisory: false,
            env: BTreeMap::new(),
            permissions: BTreeMap::new(),
            full_history: false,
        }
    }

    fn profile_config(profiles: Vec<CheckProfileSpec>) -> ProjectConfig {
        ProjectConfig {
            repository: String::new(),
            workflow_revision: crate::s2::SOURCE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::s2::AnalysisSummary {
                method: "test".to_owned(),
                detected: Vec::new(),
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: vec!["scheduled-daily.yml".to_owned()],
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            providers: std::collections::BTreeSet::from([
                crate::s2::provider::ProviderId::GithubHosted,
                crate::s2::provider::ProviderId::Velnor,
            ]),
            automatic_providers: std::collections::BTreeSet::from([
                crate::s2::provider::ProviderId::GithubHosted,
                crate::s2::provider::ProviderId::Velnor,
            ]),
            selectors: std::collections::BTreeMap::from([
                (
                    crate::s2::provider::ProviderId::GithubHosted,
                    crate::s2::provider::ProviderSelector {
                        runs_on: vec!["ubuntu-24.04".to_owned()],
                    },
                ),
                (
                    crate::s2::provider::ProviderId::Velnor,
                    crate::s2::provider::ProviderSelector {
                        runs_on: vec!["self-hosted".to_owned(), "example-lane".to_owned()],
                    },
                ),
            ]),
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
            rust_pin: None,
            maintenance: crate::s2::MaintenanceSpec::default(),
            units: Vec::new(),
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            rust_needs: crate::s2::RustNeeds::Parallel,
            concurrency_group: None,
            serial_stack_groups: false,
            static_files: Vec::new(),
            reviewers: Vec::new(),
            declared_surface: true,
            mise_lock_keys: std::collections::BTreeSet::new(),
            mise_lock_backends: std::collections::BTreeMap::new(),
            mise_install_deps: crate::s2::config::MiseInstallDeps::default(),
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
    fn profile_tool_subset_refuses_an_unknowable_backend() {
        let mut plugin = profile("smoke");
        plugin.tools = vec!["vfox:example/example-lint".to_owned()];
        let lock = BTreeSet::from(["vfox:example/example-lint".to_owned()]);
        let error = must_fail(
            super::validate_profile_install_deps_are_closed(
                std::slice::from_ref(&plugin),
                &lock,
                &BTreeMap::new(),
                &config::MiseInstallDeps::default(),
            ),
            "a vfox profile tool must fail generation",
        );
        let message = error.to_string();
        assert!(message.contains("smoke"), "{message}");
        assert!(message.contains("vfox:example/example-lint"), "{message}");
        assert!(message.contains("plugin metadata"), "{message}");
        // A closed backend-helper subset passes: the profile installs the
        // `cargo:` tool beside its locked helper.
        let mut closed = profile("smoke");
        closed.tools = vec!["cargo:example-cli".to_owned()];
        let lock = BTreeSet::from(["cargo:example-cli".to_owned(), "cargo:sccache".to_owned()]);
        must(
            super::validate_profile_install_deps_are_closed(
                std::slice::from_ref(&closed),
                &lock,
                &BTreeMap::new(),
                &config::MiseInstallDeps::default(),
            ),
            "a closable profile subset must pass",
        );
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
            render_profile_job(&mut required, &config, &config.check_profiles[0]),
            "render the required job",
        );
        assert!(
            !required.contains("continue-on-error"),
            "a required job gates the workflow: {required}"
        );
        let mut advisory = String::new();
        must(
            render_profile_job(&mut advisory, &config, &config.check_profiles[1]),
            "render the advisory job",
        );
        assert!(
            advisory.contains("continue-on-error: true"),
            "an advisory job reports without gating: {advisory}"
        );
    }

    #[test]
    fn deep_profile_checks_out_full_history_while_default_stays_shallow() {
        let config = profile_config(vec![profile("smoke")]);
        let mut shallow = String::new();
        must(
            render_profile_job(&mut shallow, &config, &config.check_profiles[0]),
            "render the shallow job",
        );
        assert!(
            !shallow.contains("fetch-depth"),
            "a default profile carries no fetch-depth key: {shallow}"
        );
        let checkout = format!(
            "      - name: Checkout repository\n        uses: {}\n        with:\n          persist-credentials: false\n",
            ActionPin::Checkout.reference()
        );
        assert!(
            shallow.contains(&checkout),
            "the shallow checkout keeps today's exact bytes: {shallow}"
        );
        let mut deep_spec = profile("perf");
        deep_spec.full_history = true;
        let deep_config = profile_config(vec![deep_spec]);
        let mut deep = String::new();
        must(
            render_profile_job(&mut deep, &deep_config, &deep_config.check_profiles[0]),
            "render the deep job",
        );
        assert!(
            deep.contains("          fetch-depth: 0\n"),
            "a deep profile clones full history: {deep}"
        );
    }

    #[test]
    fn mixed_profiles_keep_independent_checkout_depths() {
        let mut perf = profile("perf");
        perf.full_history = true;
        let strict = profile("perf-strict");
        let config = profile_config(vec![perf, strict]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-checks"),
            "select every profile",
        );
        let workflow = render(&config, None, &selected);
        assert_eq!(
            workflow.matches("fetch-depth: 0").count(),
            1,
            "exactly the deep profile clones full history: {workflow}"
        );
        let mut deep = String::new();
        must(
            render_profile_job(&mut deep, &config, &config.check_profiles[0]),
            "render perf",
        );
        assert!(
            deep.contains("fetch-depth: 0"),
            "perf clones full history: {deep}"
        );
        let mut shallow = String::new();
        must(
            render_profile_job(&mut shallow, &config, &config.check_profiles[1]),
            "render perf-strict",
        );
        assert!(
            !shallow.contains("fetch-depth"),
            "perf-strict stays shallow: {shallow}"
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
            workflow.contains(crate::s2::ActionPin::UploadArtifact.reference()),
            "{workflow}"
        );
    }

    #[test]
    fn tools_render_per_lane() {
        let mut hosted = profile("hosted");
        hosted.tools = vec!["ripgrep".to_owned(), "cargo:example-tool".to_owned()];
        let mut fleet = profile("fleet");
        fleet.runner = RunnerTarget::Provider(ProviderId::Velnor);
        fleet.tools = vec!["ripgrep".to_owned()];
        let mut bare = profile("bare");
        bare.runner = RunnerTarget::Provider(ProviderId::Velnor);
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
            render_profile_job(&mut bare_job, &config, &config.check_profiles[2]),
            "render the tool-less Velnor job",
        );
        assert!(
            !bare_job.contains("Install declared Mise tools"),
            "a tool-less Velnor job installs nothing: {bare_job}"
        );
        assert!(
            workflow.contains(crate::s2::ActionPin::Mise.reference()),
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
    fn velnor_profile_needs_a_selector_and_renders_it() {
        let mut fleet = profile("fleet");
        fleet.runner = RunnerTarget::Provider(ProviderId::Velnor);
        let config = profile_config(vec![fleet.clone()]);
        let runs_on = must(
            profile_runs_on(&config, &fleet),
            "render the Velnor selector",
        );
        assert_eq!(runs_on, "[self-hosted, example-lane]");

        let mut unlabeled = profile_config(Vec::new());
        unlabeled.selectors = std::collections::BTreeMap::new();
        let error = must_fail(
            profile_runs_on(&unlabeled, &fleet),
            "a Velnor profile without a selector must fail",
        );
        assert!(error.to_string().contains("velnor"), "{error}");
    }

    #[test]
    fn velnor_profile_uses_canonical_provider_admission() {
        let mut fleet = profile("fleet");
        fleet.runner = RunnerTarget::Provider(ProviderId::Velnor);
        let mut config = profile_config(vec![fleet.clone()]);

        let admission = WorkflowIr::from_config(&config)
            .provider_admission_expression(ProviderAdmission::ProviderTrusted(ProviderId::Velnor));
        assert!(
            admission.contains("github.event.pull_request.head.repo.fork"),
            "Velnor profiles retain the canonical trusted-event/fork gate: {admission}"
        );
        let mut job = String::new();
        must(
            render_profile_job(&mut job, &config, &fleet),
            "render the Velnor profile job",
        );
        assert!(
            job.contains(&format!("    if: ${{{{ ({admission}) }}}}\n")),
            "the profile job uses the canonical provider admission: {job}"
        );

        let hosted = profile("hosted");
        let mut hosted_job = String::new();
        must(
            render_profile_job(&mut hosted_job, &config, &hosted),
            "render the hosted profile job",
        );
        assert!(
            !hosted_job.contains("    if:"),
            "hosted profile admission stays unchanged: {hosted_job}"
        );

        config.automatic_providers.remove(&ProviderId::Velnor);
        let disabled = WorkflowIr::from_config(&config)
            .provider_admission_expression(ProviderAdmission::ProviderTrusted(ProviderId::Velnor));
        assert_ne!(disabled, admission);
        assert!(
            disabled.contains("github.event_name == 'workflow_dispatch'")
                && disabled.contains("github.event.pull_request.head.repo.fork"),
            "a manual Velnor provider remains trusted-event gated: {disabled}"
        );
    }

    #[test]
    fn official_scale_set_profile_is_host_qualified_and_trusted_gated() {
        let mut official = profile("official");
        official.runner = RunnerTarget::Provider(ProviderId::GithubSelfHosted);
        let mut config = profile_config(vec![official.clone()]);
        config.providers.insert(ProviderId::GithubSelfHosted);
        config.selectors.insert(
            ProviderId::GithubSelfHosted,
            crate::s2::provider::ProviderSelector {
                runs_on: vec![
                    "self-hosted".to_owned(),
                    "velnor-scale-set".to_owned(),
                    "local-mac".to_owned(),
                ],
            },
        );

        let mut job = String::new();
        must(
            render_profile_job(&mut job, &config, &official),
            "render the official Scale Set profile job",
        );
        assert!(
            job.contains("runs-on: [self-hosted, velnor-scale-set, local-mac]"),
            "official Scale Set placement must carry its provider and host labels: {job}"
        );
        assert!(
            job.contains("github.event.pull_request.head.repo.fork"),
            "official Scale Set jobs must reject fork PRs before local admission: {job}"
        );
    }

    #[test]
    fn runner_selection_covers_hosted_runners_and_refuses_unknown() {
        let config = profile_config(Vec::new());
        let hosted = profile("hosted");
        assert_eq!(
            must(
                profile_runs_on(&config, &hosted),
                "render the hosted runner"
            ),
            "ubuntu-24.04"
        );
        let mut apple = profile("apple");
        apple.runner = RunnerTarget::GithubHostedMacos;
        assert_eq!(
            must(profile_runs_on(&config, &apple), "render the Apple runner"),
            "macos-26"
        );
        let error = must_fail(
            RunnerTarget::parse("planetary"),
            "an unknown runner must fail",
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

    #[test]
    fn actions_read_is_job_scoped_and_preserves_checkout_access() {
        let mut collector = profile("collector");
        collector
            .permissions
            .insert("actions".to_owned(), "read".to_owned());
        let config = profile_config(vec![collector]);
        let map = args_for("");
        let selected = must(
            select_profiles(&config.check_profiles, &Args(&map), "scheduled-checks"),
            "select the collector profile",
        );
        let workflow = render(&config, None, &selected);
        assert!(
            workflow.contains("    permissions:\n      actions: read\n      contents: read\n"),
            "the capability must be scoped to the collector job and retain checkout access: {workflow}"
        );
        assert_eq!(
            workflow.matches("permissions:\n").count(),
            2,
            "workflow and collector job each carry an explicit permission map: {workflow}"
        );
        assert_eq!(
            workflow.matches("      actions: read\n").count(),
            1,
            "no unrelated job receives Actions history access: {workflow}"
        );
    }

    #[test]
    fn actions_read_is_rejected_on_pull_request_files() {
        let mut collector = profile("collector");
        collector
            .permissions
            .insert("actions".to_owned(), "read".to_owned());
        let config = profile_config(vec![collector]);
        let error = must_fail(
            render_checks_file(
                &config,
                "collector.yml",
                None,
                &[&config.check_profiles[0]],
                &["pull_request".to_owned()],
                &[],
            ),
            "Actions history access on pull-request tasks must fail closed",
        );
        assert!(error.to_string().contains("pull_request"), "{error}");
        assert!(
            error.to_string().contains("contributor-controlled"),
            "{error}"
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
            evented.contains(
                "group: scheduled-daily-${{ github.repository }}-${{ github.event_name == 'pull_request' && github.ref || github.sha }}"
            ),
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
            assert!(
                rendered.contains("group: scheduled-daily-${{ github.repository }}-${{ github.sha }}"),
                "committed/candidate evidence must use a SHA-scoped group: {events_toml}\n{rendered}"
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

    fn select_all<'a>(config: &'a ProjectConfig, args: &Args<'_>) -> Vec<&'a CheckProfileSpec> {
        must(
            select_profiles(&config.check_profiles, args, "scheduled-daily.yml"),
            "select every profile",
        )
    }

    #[test]
    fn absent_providers_input_renders_no_providers_surface() {
        let smoke = profile("smoke");
        let config = profile_config(vec![smoke]);
        let map = args_for("");
        let selected = select_all(&config, &Args(&map));
        let workflow = render(&config, None, &selected);
        assert!(
            workflow.contains("  workflow_dispatch:\n\npermissions:"),
            "the dispatch trigger stays bare: {workflow}"
        );
        for marker in ["providers:", "inputs.providers", "fromJSON"] {
            assert!(
                !workflow.contains(marker),
                "an undeclared providers input renders nothing: {marker} in {workflow}"
            );
        }
        assert!(
            workflow.contains("    runs-on: ubuntu-24.04"),
            "the job stays on its static runner: {workflow}"
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
}

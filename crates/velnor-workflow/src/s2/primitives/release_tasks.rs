//! Renderer for the config-only named-task release publisher.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::s2::{ActionPin, ReleaseJobSpec, ReleaseSpec, selector_runs_on_yaml, yaml_scalar};

/// Render the default-branch dispatcher for a task publisher. The task code
/// is checked out by the validated full commit SHA only after the admission
/// job proves the SHA/tag binding and branch reachability.
pub(super) fn render_publisher(
    release: &ReleaseSpec,
    default_branch: &str,
    admission_runner_yaml: &str,
) -> String {
    let trigger = super::release_dispatch_trigger_block();
    let admission_job = super::render_release_dispatch_admission_job(
        default_branch,
        super::release_tag_pattern(release),
        admission_runner_yaml,
    );
    let mut output = format!(
        "{}# repository_dispatch loads this workflow from the protected default branch.\n# The admission job validates the target before any publisher job receives permissions or secrets.\nname: Release\nrun-name: Release · ${{{{ github.event.client_payload.release_tag }}}}\n\n{trigger}\nconcurrency:\n  group: release-${{{{ github.event.client_payload.source_sha }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n{admission_job}\n",
        super::GENERATED_HEADER,
    );
    for job in release.jobs.iter().filter(|job| runs_on_publish(job)) {
        render_job(&mut output, job, true, true);
    }
    output
}

/// Whether this task contract needs the separate selected-ref validation
/// workflow. Validation rows receive only the workflow-level `contents: read`
/// token and cannot carry repository secrets, write scopes, OIDC, or a local
/// Velnor runner.
pub(super) fn has_validation_jobs(release: &ReleaseSpec) -> bool {
    release.jobs.iter().any(runs_on_dispatch)
}

/// Render the read-only workflow-dispatch validator, if its selected
/// jobs pass the renderer's read-only backstop.
pub(super) fn render_validator(release: &ReleaseSpec) -> Option<String> {
    let jobs = release
        .jobs
        .iter()
        .filter(|job| runs_on_dispatch(job))
        .collect::<Vec<_>>();
    if jobs.is_empty() || jobs.iter().any(|job| !validation_job_is_read_only(job)) {
        return None;
    }
    let ids = jobs
        .iter()
        .map(|job| job.id.as_str())
        .collect::<BTreeSet<_>>();
    if jobs
        .iter()
        .any(|job| job.needs.iter().any(|dependency| !ids.contains(dependency.as_str())))
    {
        return None;
    }

    let mut output = format!(
        "{}# Selected-ref task code is untrusted. This dispatcher grants only contents: read,\n# disables checkout credential persistence, and rejects secrets, environments,\n# write scopes, OIDC, attestations, and Velnor runners.\nname: Release validation\nrun-name: Release validation · ${{{{ github.ref_name }}}}\n\non:\n  workflow_dispatch:\n\nconcurrency:\n  group: release-validation-${{{{ github.ref }}}}\n  cancel-in-progress: true\n\npermissions:\n  contents: read\n\njobs:\n",
        super::GENERATED_HEADER,
    );
    for job in jobs {
        render_job(&mut output, job, false, false);
    }
    Some(output)
}

fn runs_on_dispatch(job: &ReleaseJobSpec) -> bool {
    job.modes.iter().any(|mode| mode == "validate")
}

/// Unspecified task modes are publish-only. Validation must be opted into
/// explicitly because its separate workflow_dispatch checks out a user-selected ref.
fn runs_on_publish(job: &ReleaseJobSpec) -> bool {
    job.modes.is_empty() || job.modes.iter().any(|mode| mode == "publish")
}

fn validation_job_is_read_only(job: &ReleaseJobSpec) -> bool {
    matches!(job.runner.as_str(), "github" | "macos")
        && job.environment.is_empty()
        && job.attest_subjects.is_empty()
        && job.env.is_empty()
        && job
            .permissions
            .iter()
            .all(|(scope, level)| scope == "contents" && level == "read")
}

fn render_job(
    output: &mut String,
    job: &ReleaseJobSpec,
    include_publisher_inputs: bool,
    include_admission: bool,
) {
    let _ = writeln!(
        output,
        "  {}:\n    name: {}",
        yaml_quoted(&job.id),
        yaml_scalar(&job.name)
    );
    let mut dependencies = job.needs.clone();
    if include_admission {
        dependencies.insert(0, super::RELEASE_ADMISSION_JOB_ID.to_owned());
    }
    if !dependencies.is_empty() {
        let needs = dependencies
            .iter()
            .map(|dependency| yaml_quoted(dependency))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(output, "    needs: [{needs}]");
    }
    let selector = crate::s2::provider::ProviderSelector {
        group: job.runner_group.clone(),
        runs_on: job.runs_on.clone(),
    };
    let _ = writeln!(output, "    runs-on: {}", selector_runs_on_yaml(&selector));
    let _ = writeln!(output, "    timeout-minutes: {}", job.timeout_minutes);
    if include_publisher_inputs && !job.environment.is_empty() {
        let _ = writeln!(
            output,
            "    environment: {}",
            yaml_scalar(&job.environment)
        );
    }
    if include_publisher_inputs && !job.permissions.is_empty() {
        output.push_str("    permissions:\n");
        // A job-level map replaces workflow-level permissions. Preserve the
        // checkout read scope when a publisher declares other scopes.
        let mut permissions = job.permissions.clone();
        permissions
            .entry("contents".to_owned())
            .or_insert_with(|| "read".to_owned());
        for (scope, level) in &permissions {
            let _ = writeln!(output, "      {scope}: {level}");
        }
    }
    let mut env = job.env.clone();
    if include_admission {
        env.insert(
            "VELNOR_RELEASE_TAG".to_owned(),
            format!(
                "${{{{ needs.{}.outputs.release_tag }}}}",
                super::RELEASE_ADMISSION_JOB_ID
            ),
        );
    }
    if include_publisher_inputs && !env.is_empty() {
        output.push_str("    env:\n");
        for (key, value) in &env {
            let _ = writeln!(output, "      {key}: {}", yaml_scalar(value));
        }
    }
    let ref_input = if include_admission {
        format!(
            "\n          ref: ${{{{ needs.{}.outputs.source_sha }}}}",
            super::RELEASE_ADMISSION_JOB_ID
        )
    } else {
        String::new()
    };
    let _ = writeln!(
        output,
        "    steps:\n      - name: Checkout\n        uses: {}\n        with:{ref_input}\n          persist-credentials: false",
        ActionPin::Checkout.reference(),
    );
    if job.runner != "velnor" {
        let _ = writeln!(
            output,
            "      - name: Set up Mise\n        uses: {}\n        with:\n          install: false",
            ActionPin::Mise.reference(),
        );
    }
    for task in &job.tasks {
        let _ = writeln!(
            output,
            "      - name: {}\n        run: mise run {task}",
            yaml_scalar(&format!("Run {task}"))
        );
    }
    if include_publisher_inputs && !job.attest_subjects.is_empty() {
        output.push_str("      - name: Attest release artifacts\n");
        let _ = writeln!(output, "        uses: {}", ActionPin::Attest.reference());
        output.push_str("        with:\n          subject-path: |\n");
        for subject in &job.attest_subjects {
            let _ = writeln!(output, "            {subject}");
        }
    }
}

fn yaml_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn task_job(id: &str, tasks: &[&str]) -> ReleaseJobSpec {
        ReleaseJobSpec {
            id: id.to_owned(),
            name: id.to_owned(),
            tasks: tasks.iter().map(|task| (*task).to_owned()).collect(),
            runner: "github".to_owned(),
            runs_on: vec!["ubuntu-24.04".to_owned()],
            timeout_minutes: 20,
            ..ReleaseJobSpec::default()
        }
    }

    fn tasks_spec() -> ReleaseSpec {
        let mut sign = task_job("sign", &["sign-release"]);
        sign.name = "Sign release".to_owned();
        sign.needs = vec!["build".to_owned()];
        sign.runner = "macos".to_owned();
        sign.runs_on = vec![crate::s2::MACOS_HOSTED_RUNS_ON.to_owned()];
        sign.modes = vec!["publish".to_owned()];
        sign.environment = "example-signing".to_owned();
        sign.attest_subjects = vec!["dist/example-app.zip".to_owned()];
        sign.permissions = BTreeMap::from([
            ("attestations".to_owned(), "write".to_owned()),
            ("id-token".to_owned(), "write".to_owned()),
        ]);
        sign.env = BTreeMap::from([
            (
                "DEVELOPER_DIR".to_owned(),
                "/Applications/Xcode.app/Contents/Developer".to_owned(),
            ),
            (
                "SIGNING_KEY_ID".to_owned(),
                "${{ secrets.EXAMPLE_SIGNING_KEY_ID }}".to_owned(),
            ),
        ]);
        let mut build = task_job("build", &["build-release", "verify-release"]);
        build.modes = vec!["validate".to_owned(), "publish".to_owned()];
        let mut manual = task_job("manual-review", &["review-release"]);
        manual.modes = vec!["validate".to_owned()];
        ReleaseSpec {
            kind: "tasks".to_owned(),
            tag_pattern: "v[0-9]*".to_owned(),
            jobs: vec![build, sign, manual],
            ..ReleaseSpec::default()
        }
    }

    #[test]
    fn task_release_publisher_uses_validated_dispatch_and_gates_privileged_jobs() {
        let workflow = render_publisher(&tasks_spec(), "main", "ubuntu-24.04");
        for expected in [
            "name: Release",
            "repository_dispatch:",
            "types: [trusted-release-publish]",
            "run-name: Release · ${{ github.event.client_payload.release_tag }}",
            "github.event.client_payload.source_sha",
            "github.event.client_payload.release_tag",
            "  admit-release:\n",
            "runs-on: ubuntu-24.04",
            "  \"build\":\n",
            "  \"sign\":\n",
            "needs: [\"admit-release\"]",
            "needs: [\"admit-release\", \"build\"]",
            "needs.admit-release.outputs.source_sha",
            "needs.admit-release.outputs.release_tag",
            "[[ \"$SOURCE_SHA\" =~ ^[0-9a-f]{40}$ ]]",
            "[[ \"$RELEASE_TAG\" == $TAG_PATTERN ]]",
            "--jq '.default_branch'",
            "[[ \"$repo_default_branch\" = \"$DEFAULT_BRANCH\" ]]",
            "[[ \"$object_type\" = commit && \"$object_sha\" = \"$SOURCE_SHA\" ]]",
            "compare/$SOURCE_SHA...$branch_sha",
            "ahead|identical",
            "DEFAULT_BRANCH: main",
            "TAG_PATTERN: \"v[0-9]*\"",
            "run: mise run build-release",
            "run: mise run verify-release",
            "run: mise run sign-release",
            "runs-on: macos-15",
            "timeout-minutes: 20",
            "environment: example-signing",
            "permissions:\n      attestations: write\n      contents: read\n      id-token: write",
            "id-token: write",
            "attestations: write",
            "release_tag",
            "source_sha",
            "DEVELOPER_DIR:",
            "SIGNING_KEY_ID:",
            "Attest release artifacts",
            "actions/attest-build-provenance@",
            "dist/example-app.zip",
        ] {
            assert!(
                workflow.contains(expected),
                "missing {expected:?}: {workflow}"
            );
        }
        let admission = workflow.find("  admit-release:\n").expect("admission job");
        let publisher = workflow.find("  \"build\":\n").expect("publisher job");
        assert!(admission < publisher, "{workflow}");
        let admission_job = &workflow[admission..publisher];
        let repository_default_branch_lookup = workflow
            .find("repo_default_branch=\"$(gh api")
            .expect("repository default branch lookup");
        let configured_default_branch_check = workflow
            .find("[[ \"$repo_default_branch\" = \"$DEFAULT_BRANCH\" ]]")
            .expect("configured default branch check");
        let branch_lookup = workflow
            .find("branch_sha=\"$(gh api")
            .expect("protected branch lookup");
        assert!(
            repository_default_branch_lookup < configured_default_branch_check
                && configured_default_branch_check < branch_lookup,
            "{workflow}"
        );
        assert!(
            admission_job.contains("permissions:\n      contents: read"),
            "{admission_job}"
        );
        assert!(!admission_job.contains("uses:"), "{admission_job}");
        assert!(!admission_job.contains("environment:"), "{admission_job}");
        assert!(!admission_job.contains("write"), "{admission_job}");
        assert!(!workflow.contains("workflow_dispatch:"), "{workflow}");
        assert!(!workflow.contains("push:\n"), "{workflow}");
        assert!(!workflow.contains("tags:"), "{workflow}");
        assert!(!workflow.contains("manual-review"), "{workflow}");
    }

    #[test]
    fn selected_ref_validator_is_read_only_and_contains_only_validate_jobs() {
        let workflow = render_validator(&tasks_spec()).expect("validate jobs render");
        for expected in [
            "name: Release validation",
            "workflow_dispatch:",
            "permissions:\n  contents: read",
            "Selected-ref task code is untrusted",
            "  \"build\":\n",
            "  \"manual-review\":\n",
            "run: mise run build-release",
            "persist-credentials: false",
            "runs-on: ubuntu-24.04",
        ] {
            assert!(
                workflow.contains(expected),
                "missing {expected:?}: {workflow}"
            );
        }
        for forbidden in [
            "tags:",
            "  \"sign\":",
            "permissions:\n      ",
            "contents: write",
            "attestations:",
            "artifact-metadata:",
            "id-token:",
            "environment:",
            "secrets.",
            "runs-on: [self-hosted",
            "velnor-target",
            "actions: write",
            "attestations: write",
            "id-token: write",
        ] {
            assert!(
                !workflow.contains(forbidden),
                "validator includes forbidden {forbidden:?}: {workflow}"
            );
        }
        assert!(!workflow.contains("SIGNING_KEY_ID"), "{workflow}");
        assert!(!workflow.contains("RELEASE_TOKEN"), "{workflow}");
        assert!(has_validation_jobs(&tasks_spec()));
    }

    #[test]
    fn selected_ref_validator_renderer_fails_closed_on_privileged_rows() {
        let mut unsafe_job = task_job("validate", &["release-check"]);
        unsafe_job.modes = vec!["validate".to_owned()];
        unsafe_job.environment = "production".to_owned();
        unsafe_job.env = BTreeMap::from([("TOKEN".to_owned(), "${{ secrets.TOKEN }}".to_owned())]);
        unsafe_job.permissions = BTreeMap::from([("contents".to_owned(), "write".to_owned())]);
        assert!(render_validator(&ReleaseSpec {
            kind: "tasks".to_owned(),
            jobs: vec![unsafe_job],
            ..ReleaseSpec::default()
        })
        .is_none());
    }

    #[test]
    fn task_release_renders_velnor_selector_without_hosted_mise_action() {
        let mut job = task_job("native", &["build-release"]);
        job.runner = "velnor".to_owned();
        job.runner_group = Some("velnor-trusted".to_owned());
        job.runs_on = vec!["self-hosted".to_owned(), "velnor-target".to_owned()];
        let workflow = render_publisher(
            &ReleaseSpec {
                kind: "tasks".to_owned(),
                tag_pattern: "v[0-9]*".to_owned(),
                jobs: vec![job],
                ..ReleaseSpec::default()
            },
            "main",
            "ubuntu-24.04",
        );
        assert!(
            workflow.contains(
                "runs-on: {group: velnor-trusted, labels: [self-hosted, velnor-target]}"
            ),
            "{workflow}"
        );
        assert!(!workflow.contains("Set up Mise"), "{workflow}");
    }

    #[test]
    fn task_release_keeps_the_hosted_default_as_a_label_selector() {
        let workflow = render_publisher(
            &ReleaseSpec {
                kind: "tasks".to_owned(),
                tag_pattern: "v[0-9]*".to_owned(),
                jobs: vec![task_job("hosted", &["build-release"])],
                ..ReleaseSpec::default()
            },
            "main",
            "ubuntu-24.04",
        );
        assert!(workflow.contains("runs-on: ubuntu-24.04"), "{workflow}");
        assert!(!workflow.contains("group:"), "{workflow}");
    }

    #[test]
    fn task_release_admission_defaults_an_empty_tag_pattern_to_v_star() {
        let workflow = render_publisher(
            &ReleaseSpec {
                kind: "tasks".to_owned(),
                jobs: vec![task_job("hosted", &["build-release"])],
                ..ReleaseSpec::default()
            },
            "main",
            "ubuntu-24.04",
        );
        assert!(workflow.contains("TAG_PATTERN: \"v*\""), "{workflow}");
    }

    #[test]
    fn validator_workflow_is_omitted_without_validate_jobs() {
        let job = task_job("ship", &["ship-release"]);
        assert!(!has_validation_jobs(&ReleaseSpec {
            kind: "tasks".to_owned(),
            jobs: vec![job],
            ..ReleaseSpec::default()
        }));
        assert!(render_validator(&ReleaseSpec::default()).is_none());
    }
}

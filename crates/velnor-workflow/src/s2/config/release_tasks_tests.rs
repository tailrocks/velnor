use std::collections::BTreeSet;
use std::path::Path;

use super::{parse, RepoGenerationConfig};

const PROVIDER_CONFIG: &str = "schema = 2\n\n\
[generator]\nrepository = \"example/tasks\"\n\n\
[workflow]\nproviders = [\"github-hosted\", \"velnor\"]\nrequire_local_runner_group = true\n\n\
[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n\
[workflow.selectors.velnor]\ngroup = \"velnor-trusted\"\nruns_on = [\"self-hosted\", \"example-velnor\"]\n\n";

#[expect(
    clippy::panic,
    reason = "test setup errors should retain their parser or validation context"
)]
fn parsed(config: &str) -> RepoGenerationConfig {
    match parse(
        Path::new(".github-gen/velnor-workflow.toml"),
        config.as_bytes(),
    ) {
        Ok(config) => config,
        Err(error) => panic!("parse test config: {error}"),
    }
}

#[expect(
    clippy::panic,
    reason = "test setup errors should retain their parser or validation context"
)]
fn task_config(job_rows: &str) -> RepoGenerationConfig {
    task_config_with_capabilities("", job_rows)
}

#[expect(
    clippy::panic,
    reason = "test setup errors should retain their parser or validation context"
)]
fn task_config_with_capabilities(
    capabilities: &str,
    job_rows: &str,
) -> RepoGenerationConfig {
    parsed(&format!(
        "{PROVIDER_CONFIG}[release]\nenabled = true\nkind = \"tasks\"\ntag_pattern = \"v[0-9]*\"\n{capabilities}\n{job_rows}\n[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n"
    ))
}

fn validate(config: &RepoGenerationConfig) -> Result<(), crate::s2::GeneratorError> {
    config.validate(&[], &[], &BTreeSet::new())
}

#[test]
fn task_release_accepts_named_jobs_with_dependencies_and_lanes() {
    let config = task_config(
        "\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\", \"verify-release\"]\nrunner = \"github\"\nmodes = [\"validate\", \"publish\"]\n\n[[release.job]]\nid = \"sign\"\nname = \"Sign release\"\ntasks = [\"sign-release\"]\nneeds = [\"build\"]\nrunner = \"macos\"\nmodes = [\"publish\"]\ntimeout_minutes = 45\nenvironment = \"example-signing\"\nattest_subjects = [\"dist/example-app.zip\"]\n\n[release.job.permissions]\nid-token = \"write\"\nattestations = \"write\"\n\n[release.job.env]\nDEVELOPER_DIR = \"/Applications/Xcode.app/Contents/Developer\"\nSIGNING_KEY_ID = \"${{ secrets.EXAMPLE_SIGNING_KEY_ID }}\"\n",
    );
    assert!(validate(&config).is_ok());
    assert_eq!(config.release.jobs().len(), 2);
    assert_eq!(config.release.jobs()[0].id(), Some("build"));
    assert_eq!(
        config.release.jobs()[0].tasks(),
        Some(&["build-release".to_owned(), "verify-release".to_owned()][..])
    );
    assert_eq!(
        config.release.jobs()[0].modes(),
        Some(&["validate".to_owned(), "publish".to_owned()][..])
    );
    assert_eq!(config.release.jobs()[1].runner(), Some("macos"));
    assert_eq!(config.release.jobs()[1].name(), Some("Sign release"));
    assert_eq!(
        config.release.jobs()[1].needs(),
        Some(&["build".to_owned()][..])
    );
    assert_eq!(
        config.release.jobs()[1].modes(),
        Some(&["publish".to_owned()][..])
    );
    assert_eq!(config.release.jobs()[1].timeout_minutes(), Some(45));
}

#[test]
fn local_task_provider_config_requires_a_static_runner_group() {
    let missing_group = parsed(&PROVIDER_CONFIG.replace("group = \"velnor-trusted\"\n", ""));
    let missing_group_error = validate(&missing_group)
        .expect_err("local provider without a runner group must fail")
        .to_string();
    assert!(
        missing_group_error.contains("group"),
        "{missing_group_error}"
    );

    let expression_group = parsed(&PROVIDER_CONFIG.replace(
        "velnor-trusted",
        "${{ github.repository }}",
    ));
    let expression_group_error = validate(&expression_group)
        .expect_err("expression runner group must fail")
        .to_string();
    assert!(
        expression_group_error.contains("group"),
        "{expression_group_error}"
    );
}

#[test]
fn task_attestation_subjects_follow_the_selected_runner_contract() {
    let capability = "[release.runner_capabilities.velnor]\nattest_subjects = [\"artifacts/*.tar.gz\", \"artifacts/l2-subject.json\"]\n";
    for subject in ["artifacts/*.tar.gz", "artifacts/l2-subject.json"] {
        let config = task_config_with_capabilities(capability, &format!(
            "\n[[release.job]]\nid = \"publish\"\ntasks = [\"publish-release\"]\nrunner = \"velnor\"\nattest_subjects = [\"{subject}\"]\n\n[release.job.permissions]\nid-token = \"write\"\nattestations = \"write\"\n"
        ));
        assert!(
            validate(&config).is_ok(),
            "the configured runner capability admits `{subject}`"
        );
    }

    let unsupported = task_config_with_capabilities(
        capability,
        "\n[[release.job]]\nid = \"publish\"\ntasks = [\"publish-release\"]\nrunner = \"velnor\"\nattest_subjects = [\"artifacts/app.zip\"]\n\n[release.job.permissions]\nid-token = \"write\"\nattestations = \"write\"\n",
    );
    let error = validate(&unsupported)
        .expect_err("task jobs cannot render unsupported runner manifest inputs")
        .to_string();
    assert!(
        error.contains("runner `velnor` does not admit attestation subject `artifacts/app.zip`"),
        "the error identifies the provider capability mismatch: {error}"
    );

    let hosted = task_config(
        "\n[[release.job]]\nid = \"publish\"\ntasks = [\"publish-release\"]\nrunner = \"github\"\nattest_subjects = [\"artifacts/app.zip\"]\n\n[release.job.permissions]\nid-token = \"write\"\nattestations = \"write\"\n",
    );
    assert!(
        validate(&hosted).is_ok(),
        "GitHub-hosted task jobs retain upstream subject-path support"
    );

    let undeclared_local_capability = task_config(
        "\n[[release.job]]\nid = \"publish\"\ntasks = [\"publish-release\"]\nrunner = \"velnor\"\nattest_subjects = [\"artifacts/*.tar.gz\"]\n\n[release.job.permissions]\nid-token = \"write\"\nattestations = \"write\"\n",
    );
    let missing_error = validate(&undeclared_local_capability)
        .expect_err("local attestation inputs require a declared runner contract")
        .to_string();
    assert!(
        missing_error.contains("has no declared attestation subject contract"),
        "missing capability fails closed: {missing_error}"
    );
}

#[test]
fn task_release_runner_capabilities_are_validated() {
    for (value, expected) in [
        ("[]", "must declare at least one attest_subjects pattern"),
        (
            "[\"../outside.zip\"]",
            "is not a valid relative artifact path or one-file glob",
        ),
    ] {
        let config = task_config_with_capabilities(
            &format!(
                "[release.runner_capabilities.velnor]\nattest_subjects = {value}\n"
            ),
            "\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\n",
        );
        let error = validate(&config)
            .expect_err("invalid runner capability declarations fail closed")
            .to_string();
        assert!(error.contains(expected), "{value}: {error}");
    }
}

#[test]
fn task_job_validation_rejects_bad_rows_and_graphs() {
    for (label, row, expected) in [
        ("missing-id", "tasks = [\"build-release\"]\n", "missing `id`"),
        (
            "reserved-admission-id",
            "id = \"admit-release\"\ntasks = [\"build-release\"]\n",
            "reserved for the generated release admission job",
        ),
        (
            "bad-id",
            "id = \"9build\"\ntasks = [\"build-release\"]\n",
            "is not a job id",
        ),
        ("missing-tasks", "id = \"build\"\n", "missing `tasks`"),
        (
            "empty-tasks",
            "id = \"build\"\ntasks = []\n",
            "empty tasks",
        ),
        (
            "shell-task",
            "id = \"build\"\ntasks = [\"build-release; rm -rf /\"]\n",
            "not a plain task reference",
        ),
        (
            "self-need",
            "id = \"build\"\ntasks = [\"build-release\"]\nneeds = [\"build\"]\n",
            "needs itself",
        ),
        (
            "unknown-need",
            "id = \"build\"\ntasks = [\"build-release\"]\nneeds = [\"absent\"]\n",
            "which no job declares",
        ),
        (
            "bad-runner",
            "id = \"build\"\ntasks = [\"build-release\"]\nrunner = \"windows\"\n",
            "runner must be one of",
        ),
        (
            "bad-mode",
            "id = \"build\"\ntasks = [\"build-release\"]\nmodes = [\"rehearse\"]\n",
            "modes must be one of",
        ),
        (
            "bad-timeout",
            "id = \"build\"\ntasks = [\"build-release\"]\ntimeout_minutes = 0\n",
            "timeout_minutes must be a positive number",
        ),
        (
            "timeout-overflow",
            "id = \"build\"\ntasks = [\"build-release\"]\ntimeout_minutes = 4294967296\n",
            "timeout_minutes must be a positive number",
        ),
        (
            "empty-environment",
            "id = \"build\"\ntasks = [\"build-release\"]\nenvironment = \"\"\n",
            "environment must be a literal identifier",
        ),
        (
            "environment-expression",
            "id = \"build\"\ntasks = [\"build-release\"]\nenvironment = \"${{ github.ref_name }}\"\n",
            "expressions and control characters are forbidden",
        ),
        (
            "empty-subject",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"\"]\n",
            "one non-empty line per subject",
        ),
        (
            "attest-traversal",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"../secret.zip\"]\n",
            "relative artifact path without traversal",
        ),
        (
            "attest-absolute",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"/tmp/app.zip\"]\n",
            "relative artifact path without traversal",
        ),
        (
            "attest-broad-glob",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"*\"]\n",
            "relative artifact path without traversal",
        ),
        (
            "attest-extensionless-glob",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"dist/*\"]\n",
            "relative artifact path without traversal",
        ),
        (
            "attest-recursive-glob",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"dist/**/*.zip\"]\n",
            "relative artifact path without traversal",
        ),
        (
            "bad-permission-scope",
            "id = \"build\"\ntasks = [\"build-release\"]\n[release.job.permissions]\noidc = \"write\"\n",
            "not a job permission scope",
        ),
        (
            "bad-permission-level",
            "id = \"build\"\ntasks = [\"build-release\"]\n[release.job.permissions]\ncontents = \"admin\"\n",
            "must be one of read, write, none",
        ),
        (
            "contents-none",
            "id = \"build\"\ntasks = [\"build-release\"]\n[release.job.permissions]\ncontents = \"none\"\n",
            "permissions.contents cannot be `none`",
        ),
        (
            "attest-missing-id-token",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"dist/app.zip\"]\n[release.job.permissions]\nattestations = \"write\"\n",
            "requires `permissions.id-token = \"write\"`",
        ),
        (
            "attest-missing-attestations",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"dist/app.zip\"]\n[release.job.permissions]\nid-token = \"write\"\n",
            "requires `permissions.attestations = \"write\"`",
        ),
        (
            "attest-id-token-read",
            "id = \"build\"\ntasks = [\"build-release\"]\nattest_subjects = [\"dist/app.zip\"]\n[release.job.permissions]\nid-token = \"read\"\nattestations = \"write\"\n",
            "requires `permissions.id-token = \"write\"`",
        ),
        (
            "bad-env-name",
            "id = \"build\"\ntasks = [\"build-release\"]\n[release.job.env]\n\"BAD KEY\" = \"x\"\n",
            "not an environment name",
        ),
        (
            "reserved-release-tag-env",
            "id = \"build\"\ntasks = [\"build-release\"]\n[release.job.env]\nVELNOR_RELEASE_TAG = \"untrusted\"\n",
            "VELNOR_RELEASE_TAG",
        ),
        (
            "multiline-env-value",
            "id = \"build\"\ntasks = [\"build-release\"]\n[release.job.env]\nKEY = \"a\\nb\"\n",
            "must be one line",
        ),
        (
            "duplicate-mode",
            "id = \"build\"\ntasks = [\"build-release\"]\nmodes = [\"validate\", \"validate\"]\n",
            "modes repeats `validate`",
        ),
    ] {
        let config = task_config(&format!("\n[[release.job]]\n{row}"));
        let error = validate(&config).expect_err(label).to_string();
        assert!(error.contains(expected), "{label}: {error}");
    }
}

#[test]
fn task_job_rows_require_their_provider_and_task_kind() {
    let wrong_kind = parsed(&format!(
        "{PROVIDER_CONFIG}[release]\nenabled = true\nkind = \"pages\"\nartifact_path = \"dist\"\n\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\n\n[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n"
    ));
    let wrong_kind_error = validate(&wrong_kind)
        .expect_err("jobs on pages must fail")
        .to_string();
    assert!(
        wrong_kind_error.contains("only for kind `tasks`"),
        "{wrong_kind_error}"
    );

    let no_jobs = parsed(&format!(
        "{PROVIDER_CONFIG}[release]\nenabled = true\nkind = \"tasks\"\n\n[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n"
    ));
    let no_jobs_error = validate(&no_jobs)
        .expect_err("jobless tasks release must fail")
        .to_string();
    assert!(no_jobs_error.contains("tasks"), "{no_jobs_error}");

    let no_velnor = parsed(
        "schema = 2\n\n[generator]\nrepository = \"example/tasks\"\n\n\
         [workflow]\nproviders = [\"github-hosted\"]\n\n\
         [workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n\
         [release]\nenabled = true\nkind = \"tasks\"\n\n\
         [[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\nrunner = \"velnor\"\n\n\
         [[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n",
    );
    let no_velnor_error = validate(&no_velnor)
        .expect_err("Velnor job outside the provider set must fail")
        .to_string();
    assert!(
        no_velnor_error.contains("does not include `velnor`"),
        "{no_velnor_error}"
    );
}

#[test]
fn task_validation_rows_cannot_receive_privileged_inputs() {
    for (label, row, expected) in [
        (
            "validation-velnor",
            "id = \"check\"\ntasks = [\"check-release\"]\nrunner = \"velnor\"\nmodes = [\"validate\"]\n",
            "cannot run on the Velnor privileged runner",
        ),
        (
            "validation-environment",
            "id = \"check\"\ntasks = [\"check-release\"]\nmodes = [\"validate\"]\nenvironment = \"release-secrets\"\n",
            "cannot attach a GitHub environment",
        ),
        (
            "validation-secret-env",
            "id = \"check\"\ntasks = [\"check-release\"]\nmodes = [\"validate\"]\n[release.job.env]\nTOKEN = \"${{ secrets.TOKEN }}\"\n",
            "cannot receive configured env values or secrets",
        ),
        (
            "validation-write",
            "id = \"check\"\ntasks = [\"check-release\"]\nmodes = [\"validate\"]\n[release.job.permissions]\ncontents = \"write\"\n",
            "may declare only `permissions.contents = \"read\"`",
        ),
        (
            "validation-oidc",
            "id = \"check\"\ntasks = [\"check-release\"]\nmodes = [\"validate\"]\n[release.job.permissions]\nid-token = \"write\"\n",
            "may declare only `permissions.contents = \"read\"`",
        ),
        (
            "validation-attestation",
            "id = \"check\"\ntasks = [\"check-release\"]\nmodes = [\"validate\"]\nattest_subjects = [\"dist/app.zip\"]\n[release.job.permissions]\nid-token = \"write\"\nattestations = \"write\"\n",
            "selected-ref jobs cannot attest artifacts",
        ),
    ] {
        let config = task_config(&format!("\n[[release.job]]\n{row}"));
        let error = validate(&config).expect_err(label).to_string();
        assert!(error.contains(expected), "{label}: {error}");
    }
}

#[test]
fn task_job_graph_rejects_cycles_and_cross_event_dependencies() {
    let cycle = task_config(
        "\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\nneeds = [\"sign\"]\n\n[[release.job]]\nid = \"sign\"\ntasks = [\"sign-release\"]\nneeds = [\"build\"]\n",
    );
    let cycle_error = validate(&cycle)
        .expect_err("multi-job cycles must fail")
        .to_string();
    assert!(cycle_error.contains("dependency cycle"), "{cycle_error}");

    for (label, jobs, expected) in [
        (
            "tag dependency",
            "\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\nmodes = [\"validate\"]\n\n[[release.job]]\nid = \"sign\"\ntasks = [\"sign-release\"]\nmodes = [\"publish\"]\nneeds = [\"build\"]\n",
            "not present in the publish workflow",
        ),
        (
            "validation dependency",
            "\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\nmodes = [\"publish\"]\n\n[[release.job]]\nid = \"check\"\ntasks = [\"check-release\"]\nmodes = [\"validate\"]\nneeds = [\"build\"]\n",
            "not present in the validate workflow",
        ),
    ] {
        let config = task_config(jobs);
        let error = validate(&config).expect_err(label).to_string();
        assert!(error.contains(expected), "{label}: {error}");
    }
}

#[test]
fn task_release_rejects_unsupported_top_level_modes() {
    let config = parsed(&format!(
        "{PROVIDER_CONFIG}[release]\nenabled = true\nkind = \"tasks\"\nmodes = [\"validate\"]\n\n[[release.job]]\nid = \"check\"\ntasks = [\"check-release\"]\n\n[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n"
    ));
    let error = validate(&config)
        .expect_err("task releases must not silently ignore release modes")
        .to_string();
    assert!(
        error.contains("top-level `modes` are unsupported for kind `tasks`"),
        "{error}"
    );
}

#[test]
fn task_job_ids_are_unique_and_publish_bindings_stay_typed() {
    let duplicates = task_config(
        "\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\n\n[[release.job]]\nid = \"build\"\ntasks = [\"verify-release\"]\n",
    );
    let duplicate_error = validate(&duplicates)
        .expect_err("duplicate job ids must fail")
        .to_string();
    assert!(
        duplicate_error.contains("declared twice"),
        "{duplicate_error}"
    );

    for binding in [
        ("producer", "producer_workflow = \"CI\"\n"),
        ("archive", "archive_members = [\"example.zip\"]\n"),
        (
            "credential",
            "[[release.credential]]\nname = \"store\"\nsetup = \"mise run store\"\nteardown = \"mise run restore\"\n",
        ),
    ] {
        let config = parsed(&format!(
            "{PROVIDER_CONFIG}[release]\nenabled = true\nkind = \"tasks\"\n{}\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\n\n[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n",
            binding.1
        ));
        let error = validate(&config).expect_err(binding.0).to_string();
        assert!(error.contains("unsupported for kind `tasks`"), "{}: {error}", binding.0);
    }
}

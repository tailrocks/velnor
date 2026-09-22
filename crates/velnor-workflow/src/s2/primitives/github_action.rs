//! Repository-owned consumer fixtures for generic GitHub Actions.
//!
//! The scanner owns metadata and local-entrypoint checks.  A repository owns
//! the way it consumes its action, so this primitive accepts only paths to
//! checked-in consumer fixtures.  It appends typed success, expected-failure,
//! and no-build invocations to the scanned action unit; it never embeds an
//! estate name or assumes a product-specific action API.  The fixtures are
//! action consumers: they invoke the scanned action and own the action's
//! `uses`, input, environment, output, and branch assertions.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Args, Primitive, RenderCtx, Rendered, ACTION_FIXTURES};
use crate::s2::{GeneratorError, UnitKind};

/// A checked-in consumer workflow names its action through this marker.  The
/// primitive replaces it with the scanned unit root, so the emitted job graph
/// exercises the real local `uses:` edge instead of a standalone shell file.
const ACTION_PATH_MARKER: &str = "__VELNOR_ACTION_PATH__";
const CHECKOUT_ACTION_MARKER: &str = "__VELNOR_CHECKOUT_ACTION__";

pub(crate) struct GithubActionFixtures;

impl Primitive for GithubActionFixtures {
    fn id(&self) -> &'static str {
        ACTION_FIXTURES
    }

    fn schema(&self) -> &'static [&'static str] {
        &[
            "success_fixtures",
            "failure_fixtures",
            "skip_build_fixtures",
            "consumer_workflows",
        ]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let success = args.strings("success_fixtures")?.unwrap_or_default();
        let failure = args.strings("failure_fixtures")?.unwrap_or_default();
        let skip_build = args.strings("skip_build_fixtures")?.unwrap_or_default();
        let consumer_workflows = args.strings("consumer_workflows")?.unwrap_or_default();
        if success.is_empty()
            && failure.is_empty()
            && skip_build.is_empty()
            && consumer_workflows.is_empty()
        {
            return Err(GeneratorError::usage(format!(
                "{ACTION_FIXTURES} requires at least one fixture"
            )));
        }
        for fixture in success
            .iter()
            .chain(&failure)
            .chain(&skip_build)
            .chain(&consumer_workflows)
        {
            fixture_path(ctx, fixture)?;
        }
        let action_units = ctx
            .units
            .iter()
            .copied()
            .filter(|unit| unit.kind == UnitKind::GithubAction)
            .collect::<Vec<_>>();
        if action_units.is_empty() {
            let scoped_units = ctx
                .units
                .iter()
                .map(|unit| unit.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(GeneratorError::usage(format!(
                "{ACTION_FIXTURES} applies to no GitHub Action units; scoped units: {}",
                if scoped_units.is_empty() {
                    "<none>"
                } else {
                    &scoped_units
                }
            )));
        }
        let mut updates = Vec::new();
        let mut files = std::collections::BTreeMap::new();
        for unit in action_units {
            let mut update = (*unit).clone();
            for fixture in success
                .iter()
                .chain(&failure)
                .chain(&skip_build)
                .chain(&consumer_workflows)
            {
                let path = fixture_path(ctx, fixture)?;
                if !update.watch.contains(&path) {
                    update.watch.push(path);
                }
            }
            append_success_commands(&mut update.pr_commands, &success, ctx)?;
            append_success_commands(&mut update.full_commands, &success, ctx)?;
            append_expected_failure_commands(&mut update.pr_commands, &failure, ctx)?;
            append_expected_failure_commands(&mut update.full_commands, &failure, ctx)?;
            append_success_commands(&mut update.pr_commands, &skip_build, ctx)?;
            append_success_commands(&mut update.full_commands, &skip_build, ctx)?;
            update.watch.sort();
            update.watch.dedup();
            updates.push(update);

            for fixture in &consumer_workflows {
                let fixture = fixture_path(ctx, fixture)?;
                let source = fs::read_to_string(ctx.root.join(&fixture)).map_err(|error| {
                    GeneratorError::usage(format!(
                        "read github-action-fixtures consumer workflow `{fixture}`: {error}"
                    ))
                })?;
                validate_consumer_workflow_source(&source, &fixture)?;
                let action_path = if unit.root == "." {
                    "./".to_owned()
                } else {
                    format!("./{}", unit.root)
                };
                let rendered = source
                    .replace(ACTION_PATH_MARKER, &action_path)
                    .replace(CHECKOUT_ACTION_MARKER, ctx.pins.checkout);
                if rendered.contains(ACTION_PATH_MARKER) {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow `{fixture}` contains an unresolved action path marker"
                    )));
                }
                if rendered.contains(CHECKOUT_ACTION_MARKER) {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow `{fixture}` contains an unresolved checkout action marker"
                    )));
                }
                serde_yaml::from_str::<serde_yaml::Value>(&rendered).map_err(|error| {
                    GeneratorError::usage(format!(
                        "parse github-action-fixtures consumer workflow `{fixture}`: {error}"
                    ))
                })?;
                validate_consumer_workflow_graph(
                    &rendered,
                    ctx.pins.checkout,
                    &action_path,
                    &fixture,
                )?;
                let output = consumer_workflow_path(&unit.id, &fixture);
                if files.insert(output.clone(), rendered).is_some() {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow output collides at {}",
                        output.display()
                    )));
                }
            }
        }
        Ok(Rendered {
            files,
            units: updates,
            ..Rendered::default()
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConsumerMarkerScope {
    Root,
    Jobs,
    Job,
    Steps,
    Step,
    Uses,
    Other,
}

#[derive(Default)]
struct ConsumerMarkerCounts {
    action: usize,
    checkout: usize,
}

fn validate_consumer_workflow_source(source: &str, fixture: &str) -> Result<(), GeneratorError> {
    let document = serde_yaml::from_str::<serde_yaml::Value>(source).map_err(|error| {
        GeneratorError::usage(format!(
            "parse github-action-fixtures consumer workflow `{fixture}`: {error}"
        ))
    })?;
    let jobs = document
        .get("jobs")
        .and_then(serde_yaml::Value::as_mapping)
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "github-action-fixtures consumer workflow `{fixture}` must declare jobs"
            ))
        })?;
    let mut markers = ConsumerMarkerCounts::default();
    visit_consumer_markers(&document, ConsumerMarkerScope::Root, &mut markers, fixture)?;

    let mut action_jobs = 0;
    for (job_name, job) in jobs {
        let steps = job
            .get("steps")
            .and_then(serde_yaml::Value::as_sequence)
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` must declare steps"
                ))
            })?;
        let mut action_step = None;
        let mut checkout_step = None;
        for (index, step) in steps.iter().enumerate() {
            let Some(uses) = step.get("uses").and_then(serde_yaml::Value::as_str) else {
                continue;
            };
            if uses == ACTION_PATH_MARKER {
                if action_step.replace(index).is_some() {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` must contain one action marker step"
                    )));
                }
                if checkout_step.is_none() {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` action marker must follow its checkout marker"
                    )));
                }
            } else if uses == CHECKOUT_ACTION_MARKER {
                if checkout_step.replace(index).is_some() {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` must contain one checkout marker step"
                    )));
                }
            } else if uses.starts_with("./") {
                return Err(GeneratorError::usage(format!(
                    "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` local action at step {index} must use the action marker"
                )));
            }
        }
        match (action_step, checkout_step) {
            (Some(_), Some(_)) => action_jobs += 1,
            (Some(_), None) => {
                return Err(GeneratorError::usage(format!(
                    "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` action marker must have a checkout marker"
                )));
            }
            (None, Some(_)) => {
                return Err(GeneratorError::usage(format!(
                    "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` checkout marker must bind to an action marker"
                )));
            }
            (None, None) => {}
        }
    }
    if markers.action == 0 || action_jobs == 0 {
        return Err(GeneratorError::usage(format!(
            "github-action-fixtures consumer workflow `{fixture}` must contain an action marker in `jobs.*.steps[*].uses`"
        )));
    }
    if markers.checkout == 0 {
        return Err(GeneratorError::usage(format!(
            "github-action-fixtures consumer workflow `{fixture}` must contain a pinned checkout marker in `jobs.*.steps[*].uses`"
        )));
    }
    if markers.action != action_jobs || markers.checkout != action_jobs {
        return Err(GeneratorError::usage(format!(
            "github-action-fixtures consumer workflow `{fixture}` must bind each action marker to one checkout marker in the same job"
        )));
    }
    Ok(())
}

fn visit_consumer_markers(
    value: &serde_yaml::Value,
    scope: ConsumerMarkerScope,
    counts: &mut ConsumerMarkerCounts,
    fixture: &str,
) -> Result<(), GeneratorError> {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if key.as_str().contains(ACTION_PATH_MARKER)
                    || key.as_str().contains(CHECKOUT_ACTION_MARKER)
                {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow `{fixture}` markers must be exact `uses` scalars under `jobs.*.steps`"
                    )));
                }
                let child_scope = match (scope, key.as_str()) {
                    (ConsumerMarkerScope::Root, "jobs") => ConsumerMarkerScope::Jobs,
                    (ConsumerMarkerScope::Jobs, _) => ConsumerMarkerScope::Job,
                    (ConsumerMarkerScope::Job, "steps") => ConsumerMarkerScope::Steps,
                    (ConsumerMarkerScope::Step, "uses") => ConsumerMarkerScope::Uses,
                    _ => ConsumerMarkerScope::Other,
                };
                visit_consumer_markers(value, child_scope, counts, fixture)?;
            }
        }
        serde_yaml::Value::Sequence(sequence) => {
            let child_scope = match scope {
                ConsumerMarkerScope::Jobs => ConsumerMarkerScope::Job,
                ConsumerMarkerScope::Steps => ConsumerMarkerScope::Step,
                _ => ConsumerMarkerScope::Other,
            };
            for value in sequence {
                visit_consumer_markers(value, child_scope, counts, fixture)?;
            }
        }
        serde_yaml::Value::String(value) => {
            let has_action_marker = value.contains(ACTION_PATH_MARKER);
            let has_checkout_marker = value.contains(CHECKOUT_ACTION_MARKER);
            if !has_action_marker && !has_checkout_marker {
                return Ok(());
            }
            if scope != ConsumerMarkerScope::Uses
                || (value != ACTION_PATH_MARKER && value != CHECKOUT_ACTION_MARKER)
            {
                return Err(GeneratorError::usage(format!(
                    "github-action-fixtures consumer workflow `{fixture}` markers must be exact `uses` scalars under `jobs.*.steps`"
                )));
            }
            if value == ACTION_PATH_MARKER {
                counts.action += 1;
            } else {
                counts.checkout += 1;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_consumer_workflow_graph(
    source: &str,
    checkout_reference: &str,
    action_path: &str,
    fixture: &str,
) -> Result<(), GeneratorError> {
    let document = serde_yaml::from_str::<serde_yaml::Value>(source).map_err(|error| {
        GeneratorError::usage(format!(
            "parse github-action-fixtures consumer workflow `{fixture}`: {error}"
        ))
    })?;
    let triggers = document.get("on").ok_or_else(|| {
        GeneratorError::usage(format!(
            "github-action-fixtures consumer workflow `{fixture}` must declare automatic triggers"
        ))
    })?;
    for trigger in ["pull_request", "push"] {
        if !workflow_trigger_present(triggers, trigger) {
            return Err(GeneratorError::usage(format!(
                "github-action-fixtures consumer workflow `{fixture}` must trigger on `{trigger}`"
            )));
        }
    }
    let jobs = document
        .get("jobs")
        .and_then(serde_yaml::Value::as_mapping)
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "github-action-fixtures consumer workflow `{fixture}` must declare jobs"
            ))
        })?;
    let checkout_reference = checkout_reference
        .split_once(" #")
        .map_or(checkout_reference, |(reference, _)| reference);
    let mut local_action_seen = false;
    for (job_name, job) in jobs {
        let steps = job
            .get("steps")
            .and_then(serde_yaml::Value::as_sequence)
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` must declare steps"
                ))
            })?;
        let mut checkout_seen = false;
        for (index, step) in steps.iter().enumerate() {
            let Some(uses) = step.get("uses").and_then(serde_yaml::Value::as_str) else {
                continue;
            };
            if uses == checkout_reference {
                checkout_seen = true;
            } else if uses.starts_with("./") {
                if !checkout_seen {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` local action at step {index} must follow pinned checkout `{checkout_reference}`"
                    )));
                }
                if uses != action_path {
                    return Err(GeneratorError::usage(format!(
                        "github-action-fixtures consumer workflow `{fixture}` job `{job_name:?}` local action at step {index} must invoke the scanned action `{action_path}`"
                    )));
                }
                local_action_seen = true;
            }
        }
    }
    if !local_action_seen {
        return Err(GeneratorError::usage(format!(
            "github-action-fixtures consumer workflow `{fixture}` must invoke a local action after pinned checkout `{checkout_reference}`"
        )));
    }
    Ok(())
}

fn workflow_trigger_present(triggers: &serde_yaml::Value, expected: &str) -> bool {
    match triggers {
        serde_yaml::Value::Mapping(mapping) => mapping
            .keys()
            .any(|key| key.as_str().eq_ignore_ascii_case(expected)),
        serde_yaml::Value::Sequence(sequence) => sequence.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(expected))
        }),
        serde_yaml::Value::String(value) => value.eq_ignore_ascii_case(expected),
        _ => false,
    }
}

fn consumer_workflow_path(unit_id: &str, fixture: &str) -> PathBuf {
    let slug = |value: &str| {
        let mut result = String::new();
        for character in value.chars() {
            if character.is_ascii_alphanumeric() {
                result.push(character.to_ascii_lowercase());
            } else if !result.ends_with('-') {
                result.push('-');
            }
        }
        result.trim_matches('-').to_owned()
    };
    PathBuf::from(".github/workflows").join(format!(
        "velnor-action-consumer-{}-{}.yml",
        slug(unit_id),
        slug(fixture)
    ))
}

fn fixture_path(ctx: &RenderCtx<'_>, fixture: &str) -> Result<String, GeneratorError> {
    let fixture = fixture.trim();
    if fixture.is_empty() {
        return Err(GeneratorError::usage(
            "github-action-fixtures paths must not be empty",
        ));
    }
    let path = Path::new(fixture);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(GeneratorError::usage(format!(
            "github-action-fixtures path `{fixture}` must be repository-relative and cannot escape the repository"
        )));
    }
    let normalized = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if normalized.is_empty() || !ctx.shape.files().iter().any(|file| file == &normalized) {
        return Err(GeneratorError::usage(format!(
            "github-action-fixtures path `{fixture}` is not a tracked repository file"
        )));
    }
    Ok(normalized)
}

fn append_success_commands(
    commands: &mut Vec<String>,
    fixtures: &[String],
    ctx: &RenderCtx<'_>,
) -> Result<(), GeneratorError> {
    for fixture in fixtures {
        let fixture = fixture_path(ctx, fixture)?;
        commands.push(success_command(&fixture));
    }
    Ok(())
}

fn append_expected_failure_commands(
    commands: &mut Vec<String>,
    fixtures: &[String],
    ctx: &RenderCtx<'_>,
) -> Result<(), GeneratorError> {
    for fixture in fixtures {
        let fixture = fixture_path(ctx, fixture)?;
        commands.push(expected_failure_command(&fixture));
    }
    Ok(())
}

fn success_command(path: &str) -> String {
    format!("bash -- {}", crate::s2::shell_quote(path))
}

fn expected_failure_command(path: &str) -> String {
    let message = format!("GitHub Action failure fixture unexpectedly succeeded: {path}");
    format!(
        "if bash -- {}; then echo {} >&2; exit 1; fi",
        crate::s2::shell_quote(path),
        crate::s2::shell_quote(&message),
    )
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        clippy::too_many_lines,
        reason = "fixture assertions name concrete generated-command failures"
    )]

    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{expected_failure_command, success_command, Primitive};
    use crate::s2::provider::ProviderId;
    use crate::s2::scan::scan_shape_for_tests as scan_shape;
    use crate::s2::{ProjectConfig, UnitKind};

    #[expect(
        clippy::panic,
        reason = "fixture setup failures must name their root cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-github-action-fixtures-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        must(
            fs::create_dir_all(root.join("tests")),
            "create fixture root",
        );
        root
    }

    #[expect(
        clippy::panic,
        reason = "fixture execution failures must name their root cause"
    )]
    fn run_fixture(root: &Path, command: &str) -> std::process::Output {
        match Command::new("bash")
            .args(["-euo", "pipefail", "-c", command])
            .current_dir(root)
            .output()
        {
            Ok(output) => output,
            Err(error) => panic!("run fixture command `{command}`: {error}"),
        }
    }

    #[test]
    fn fixture_commands_preserve_success_and_expected_failure_order() {
        assert_eq!(
            success_command("tests/consumer-success.sh"),
            "bash -- 'tests/consumer-success.sh'"
        );
        let failure = expected_failure_command("tests/consumer-failure.sh");
        assert!(failure.starts_with("if bash -- 'tests/consumer-failure.sh'; then"));
        assert!(failure.contains("unexpectedly succeeded"));
        assert!(failure.ends_with("exit 1; fi"));
    }

    #[test]
    fn generated_consumer_graph_requires_automatic_triggers_and_checkout_order() {
        let embedded_action_marker = "jobs:\n  consume:\n    steps:\n      - uses: __VELNOR_CHECKOUT_ACTION__\n      - uses: octo/__VELNOR_ACTION_PATH__\n";
        assert!(
            super::validate_consumer_workflow_source(embedded_action_marker, "fixture.yml")
                .is_err(),
            "an embedded action marker is not a local consumer uses scalar"
        );
        let exact_action_marker = "jobs:\n  consume:\n    steps:\n      - uses: __VELNOR_CHECKOUT_ACTION__\n      - uses: __VELNOR_ACTION_PATH__\n";
        assert!(
            super::validate_consumer_workflow_source(exact_action_marker, "fixture.yml").is_ok(),
            "the exact action marker is accepted in a uses scalar"
        );
        let duplicate_action_marker = "jobs:\n  consume:\n    steps:\n      - uses: __VELNOR_CHECKOUT_ACTION__\n      - uses: __VELNOR_ACTION_PATH__\n      - uses: __VELNOR_ACTION_PATH__\n";
        assert!(
            super::validate_consumer_workflow_source(duplicate_action_marker, "fixture.yml")
                .is_err(),
            "a job cannot bind multiple action marker steps"
        );
        let unrelated_source_action = "jobs:\n  consume:\n    steps:\n      - uses: __VELNOR_CHECKOUT_ACTION__\n      - uses: __VELNOR_ACTION_PATH__\n      - uses: ./unrelated\n";
        assert!(
            super::validate_consumer_workflow_source(unrelated_source_action, "fixture.yml")
                .is_err(),
            "a job cannot contain an unrelated local action"
        );

        let missing_trigger = "on: workflow_dispatch\njobs:\n  consume:\n    steps:\n      - uses: actions/checkout@deadbeef\n      - uses: ./\n";
        let error = super::validate_consumer_workflow_graph(
            missing_trigger,
            "actions/checkout@deadbeef",
            "./",
            "fixture.yml",
        )
        .err()
        .unwrap_or_else(|| panic!("dispatch-only consumers must be rejected"));
        assert!(error.to_string().contains("pull_request"), "{error}");

        let checkout_after_local = "on:\n  pull_request:\n  push:\njobs:\n  consume:\n    steps:\n      - uses: ./\n      - uses: actions/checkout@deadbeef\n";
        let error = super::validate_consumer_workflow_graph(
            checkout_after_local,
            "actions/checkout@deadbeef",
            "./",
            "fixture.yml",
        )
        .err()
        .unwrap_or_else(|| panic!("local actions must follow checkout"));
        assert!(
            error.to_string().contains("must follow pinned checkout"),
            "{error}"
        );

        let checkout_only = "on:\n  pull_request:\n  push:\njobs:\n  consume:\n    steps:\n      - uses: actions/checkout@deadbeef\n";
        let error = super::validate_consumer_workflow_graph(
            checkout_only,
            "actions/checkout@deadbeef",
            "./",
            "fixture.yml",
        )
        .err()
        .unwrap_or_else(|| panic!("consumer graphs must invoke a local action"));
        assert!(
            error.to_string().contains("invoke a local action"),
            "{error}"
        );

        let unrelated_after_checkout = "on:\n  pull_request:\n  push:\njobs:\n  consume:\n    steps:\n      - uses: actions/checkout@deadbeef\n      - uses: ./unrelated\n";
        let error = super::validate_consumer_workflow_graph(
            unrelated_after_checkout,
            "actions/checkout@deadbeef",
            "./",
            "fixture.yml",
        )
        .err()
        .unwrap_or_else(|| panic!("unrelated local actions must be rejected"));
        assert!(
            error.to_string().contains("invoke the scanned action"),
            "{error}"
        );
    }

    #[test]
    fn configured_consumer_fixtures_append_real_success_and_failure_commands() {
        let root = scratch("commands");
        let fixture_config = must(
            crate::s2::config::parse(
                Path::new(".github-gen/velnor-workflow.toml"),
                include_bytes!(
                    "../../../tests/fixtures/github-action-consumer/velnor-workflow.toml"
                ),
            ),
            "parse tracked action consumer generation config",
        );
        assert_eq!(
            fixture_config
                .declare()
                .first()
                .map(crate::s2::config::DeclareRow::primitive),
            Some("github-action-fixtures")
        );
        must(
            fs::write(root.join("action.yml"), action_metadata()),
            "write action metadata",
        );
        must(
            fs::create_dir_all(root.join("src")),
            "create Rust unit directory",
        );
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"action-consumer-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
            ),
            "write Rust unit manifest",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"stable\"\n",
            ),
            "write Rust toolchain pin",
        );
        must(
            fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n"),
            "write Rust unit source",
        );
        write_nested_consumer_actions(&root);
        must(
            fs::write(root.join("tests/success.sh"), "exit 0\n"),
            "write success fixture",
        );
        must(
            fs::write(root.join("tests/failure.sh"), "exit 7\n"),
            "write failure fixture",
        );
        must(
            fs::write(root.join("tests/skip-build.sh"), "exit 0\n"),
            "write skip-build fixture",
        );
        let consumer_fixture = root.join("tests/fixtures/github-action-consumer");
        must(
            fs::create_dir_all(&consumer_fixture),
            "create tracked consumer workflow fixture directory",
        );
        must(
            fs::write(
                consumer_fixture.join("workflow.yml"),
                include_bytes!("../../../tests/fixtures/github-action-consumer/workflow.yml"),
            ),
            "write tracked consumer workflow fixture",
        );
        let providers: std::collections::BTreeSet<ProviderId> =
            ProviderId::ALL.into_iter().collect();
        let shape = must(
            scan_shape(&root, &providers, "main", &[]),
            "scan action fixture",
        );
        let config = ProjectConfig::from(shape.clone());
        assert!(config
            .units
            .iter()
            .any(|unit| unit.kind == UnitKind::GithubAction));
        assert!(config
            .units
            .iter()
            .any(|unit| unit.kind != UnitKind::GithubAction));
        let providers = must(
            super::super::providers::resolve(&config, &[]),
            "resolve provider fixture",
        );
        let cache = must(super::super::cache::resolve(&[]), "resolve cache fixture");
        let pins = super::super::Pins::resolved();
        let values = BTreeMap::from([
            (
                "success_fixtures".to_owned(),
                toml::Value::Array(vec![toml::Value::String("tests/success.sh".to_owned())]),
            ),
            (
                "failure_fixtures".to_owned(),
                toml::Value::Array(vec![toml::Value::String("tests/failure.sh".to_owned())]),
            ),
            (
                "skip_build_fixtures".to_owned(),
                toml::Value::Array(vec![toml::Value::String("tests/skip-build.sh".to_owned())]),
            ),
            (
                "consumer_workflows".to_owned(),
                toml::Value::Array(vec![toml::Value::String(
                    "tests/fixtures/github-action-consumer/workflow.yml".to_owned(),
                )]),
            ),
        ]);
        let args = super::super::Args(&values);
        let units = config.units.iter().collect::<Vec<_>>();
        let nodes = Vec::new();
        let contracts = BTreeMap::new();
        let ctx = super::super::RenderCtx {
            root: &root,
            shape: &shape,
            config: &config,
            unit: None,
            units: &units,
            file: None,
            family: super::ACTION_FIXTURES,
            pins: &pins,
            providers: &providers,
            cache: &cache,
            nodes: &nodes,
            contracts: &contracts,
        };
        let workflow_path = root.join("tests/fixtures/github-action-consumer/workflow.yml");
        let valid_workflow = must(
            fs::read_to_string(&workflow_path),
            "read tracked action consumer workflow",
        );
        let invalid_workflow = valid_workflow.replacen(
            "uses: __VELNOR_ACTION_PATH__",
            "uses: octo/__VELNOR_ACTION_PATH__",
            1,
        );
        assert_ne!(invalid_workflow, valid_workflow);
        must(
            fs::write(&workflow_path, invalid_workflow),
            "write invalid action marker fixture",
        );
        let invalid_render = super::GithubActionFixtures.render(&ctx, &args);
        assert!(
            invalid_render.is_err(),
            "embedded action markers must not generate a consumer workflow"
        );
        must(
            fs::write(&workflow_path, &valid_workflow),
            "restore valid action marker fixture",
        );
        let misplaced_marker_workflow = valid_workflow
            .replacen("uses: __VELNOR_ACTION_PATH__", "uses: ./unrelated", 1)
            .replacen(
                "name: generic action consumer\n",
                "name: generic action consumer\nuses: __VELNOR_ACTION_PATH__\n",
                1,
            );
        assert_ne!(misplaced_marker_workflow, valid_workflow);
        must(
            fs::write(&workflow_path, misplaced_marker_workflow),
            "write misplaced action marker fixture",
        );
        let misplaced_render = super::GithubActionFixtures.render(&ctx, &args);
        assert!(
            misplaced_render.is_err(),
            "markers outside job steps and unrelated local actions must not generate a consumer workflow"
        );
        must(
            fs::write(&workflow_path, &valid_workflow),
            "restore valid action marker fixture after misplaced-marker test",
        );
        let rendered = must(
            super::GithubActionFixtures.render(&ctx, &args),
            "render action fixtures",
        );
        let update = rendered
            .units
            .iter()
            .find(|unit| unit.kind == UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("action fixture update missing"));
        assert!(rendered
            .units
            .iter()
            .all(|unit| unit.kind == UnitKind::GithubAction));
        assert_eq!(
            rendered.units.len(),
            config
                .units
                .iter()
                .filter(|unit| unit.kind == UnitKind::GithubAction)
                .count()
        );
        assert!(update
            .pr_commands
            .iter()
            .any(|command| command == "velnor-workflow verify-action --path 'action.yml'"));
        assert!(update
            .pr_commands
            .iter()
            .any(|command| command == "bash -- 'tests/success.sh'"));
        assert!(update
            .pr_commands
            .iter()
            .any(|command| command.starts_with("if bash -- 'tests/failure.sh'; then")));
        assert!(update
            .pr_commands
            .iter()
            .any(|command| command == "bash -- 'tests/skip-build.sh'"));
        assert!(update.watch.iter().any(|path| path == "tests/success.sh"));
        assert!(update
            .watch
            .iter()
            .any(|path| path == "tests/skip-build.sh"));
        assert!(update
            .watch
            .iter()
            .any(|path| path == "tests/fixtures/github-action-consumer/workflow.yml"));
        assert_eq!(
            rendered.files.len(),
            config
                .units
                .iter()
                .filter(|unit| unit.kind == UnitKind::GithubAction)
                .count()
        );
        let generated_consumer = rendered
            .files
            .values()
            .next()
            .unwrap_or_else(|| panic!("generated consumer workflow missing"));
        assert!(generated_consumer.contains("uses: ./"));
        assert!(generated_consumer.contains("on:\n  pull_request:\n  push:\n  workflow_dispatch:"));
        assert!(generated_consumer.contains(&format!("uses: {}", pins.checkout)));
        assert!(generated_consumer.contains("with:"));
        assert!(generated_consumer.contains("env:"));
        assert!(generated_consumer.contains("steps.action.outputs.result"));
        assert!(generated_consumer.contains("steps.action.outcome"));
        assert!(generated_consumer.contains("grep -Fx hadolint action-consumer.log"));
        assert!(generated_consumer.contains("grep -Fx buildx action-consumer.log"));
        assert!(generated_consumer.contains("test \"${{ steps.downstream.outcome }}\" = success"));
        assert!(generated_consumer.contains("if: ${{ always() }}"));
        assert!(
            !generated_consumer.contains("if: ${{ steps.action.outputs.result == 'downloaded' }}")
        );
        for mode in [
            "validate-failure",
            "hadolint-failure",
            "buildx-failure",
            "downstream-failure",
        ] {
            assert!(
                generated_consumer.contains(mode),
                "generated consumer is missing {mode} failure case"
            );
        }
        assert!(generated_consumer.contains("! grep -Fx hadolint action-skip-build.log"));
        assert!(generated_consumer.contains("! grep -Fx buildx action-skip-build.log"));
        assert!(generated_consumer.contains("test \"${{ steps.downstream.outcome }}\" = skipped"));
        assert!(!generated_consumer.contains(super::ACTION_PATH_MARKER));
        let non_action_units = units
            .iter()
            .copied()
            .filter(|unit| unit.kind != UnitKind::GithubAction)
            .collect::<Vec<_>>();
        let non_action_ctx = super::super::RenderCtx {
            root: ctx.root,
            shape: ctx.shape,
            config: ctx.config,
            unit: ctx.unit,
            units: &non_action_units,
            file: ctx.file,
            family: ctx.family,
            pins: ctx.pins,
            providers: ctx.providers,
            cache: ctx.cache,
            nodes: ctx.nodes,
            contracts: ctx.contracts,
        };
        let no_action_render = match super::GithubActionFixtures.render(&non_action_ctx, &args) {
            Ok(_) => panic!("non-action-only scope unexpectedly rendered"),
            Err(error) => error,
        };
        assert!(
            no_action_render
                .to_string()
                .contains("applies to no GitHub Action units"),
            "non-action-only scope must fail closed: {no_action_render}"
        );
        let empty_values = BTreeMap::new();
        let empty_args = super::super::Args(&empty_values);
        let empty_render = match super::GithubActionFixtures.render(&ctx, &empty_args) {
            Ok(_) => panic!("empty fixture declaration unexpectedly rendered"),
            Err(error) => error,
        };
        assert!(
            empty_render
                .to_string()
                .contains("requires at least one fixture"),
            "empty fixture declaration must fail closed: {empty_render}"
        );
        let surface = must(
            super::super::generate(&root, &shape, &config, Some(&fixture_config)),
            "generate action consumer surface",
        );
        let mut generated_config = config.clone();
        generated_config.units.clone_from(&surface.units);
        for file in &surface.added_files {
            if !generated_config.workflow_files.contains(file) {
                generated_config.workflow_files.push(file.clone());
            }
        }
        let generated = must(
            super::super::super::generated_files_with_surface(&generated_config, Some(&surface)),
            "render action consumer surface",
        );
        assert!(
            generated
                .keys()
                .any(|path| path == Path::new(".github/workflows/velnor-action-consumer-github-action-tests-fixtures-github-action-consumer-workflow-yml.yml")),
            "generated action consumer workflow must be part of the emitted surface"
        );
        let success = update
            .pr_commands
            .iter()
            .find(|command| *command == "bash -- 'tests/success.sh'")
            .unwrap_or_else(|| panic!("success fixture command missing"));
        let failure = update
            .pr_commands
            .iter()
            .find(|command| command.starts_with("if bash -- 'tests/failure.sh'; then"))
            .unwrap_or_else(|| panic!("failure fixture command missing"));
        let success_output = run_fixture(&root, success);
        assert!(
            success_output.status.success(),
            "success fixture failed: {}",
            String::from_utf8_lossy(&success_output.stderr)
        );
        let expected_failure_output = run_fixture(&root, failure);
        assert!(
            expected_failure_output.status.success(),
            "expected failure fixture did not fail as expected: {}",
            String::from_utf8_lossy(&expected_failure_output.stderr)
        );
        must(
            fs::write(root.join("tests/failure.sh"), "exit 0\n"),
            "rewrite failure fixture",
        );
        let unexpected_success_output = run_fixture(&root, failure);
        assert!(
            !unexpected_success_output.status.success(),
            "failure fixture unexpectedly propagated success"
        );
        let _ = fs::remove_dir_all(root);
    }

    fn action_metadata() -> &'static str {
        r#"name: consumer
inputs:
  mode:
    default: success
  skip-build:
    default: 'false'
  marker:
    default: action-consumer.log
outputs:
  result:
    value: ${{ steps.downloader.outputs.result }}
  nested-result:
    value: ${{ steps.external.outputs.result }}
runs:
  using: composite
  steps:
    - id: downloader
      uses: ./downloader
      with:
        mode: ${{ inputs.mode }}
        marker: ${{ inputs.marker }}
    - id: validator
      uses: ./validator
      with:
        mode: ${{ inputs.mode }}
        marker: ${{ inputs.marker }}
    - id: external
      if: ${{ inputs.mode == 'external' }}
      uses: octo/example@0123456789abcdef0123456789abcdef01234567
      with:
        mode: ${{ inputs.mode }}
      env:
        ACTION_MODE: ${{ inputs.mode }}
    - id: nested-output-visible
      if: ${{ steps.external.outputs.result == 'external-value' }}
      shell: bash
      run: printf 'nested-output-visible\n' >> "$ACTION_MARKER"
    - id: nested-output-leak
      if: ${{ steps.consumer-external-run.outputs.result == 'external-value' }}
      shell: bash
      run: printf 'nested-output-leaked\n' >> "$ACTION_MARKER"
    - id: hadolint
      if: ${{ inputs.skip-build != 'true' }}
      shell: bash
      env:
        CONSUMER_MODE: ${{ inputs.mode }}
        CONSUMER_MARKER: ${{ inputs.marker }}
      run: |
        printf 'hadolint\n' >> "$CONSUMER_MARKER"
        if [ "$CONSUMER_MODE" = hadolint-failure ]; then exit 17; fi
    - id: buildx
      if: ${{ inputs.skip-build != 'true' }}
      shell: bash
      env:
        CONSUMER_MODE: ${{ inputs.mode }}
        CONSUMER_MARKER: ${{ inputs.marker }}
      run: |
        printf 'buildx\n' >> "$CONSUMER_MARKER"
        if [ "$CONSUMER_MODE" = buildx-failure ]; then exit 19; fi
    - id: downstream
      uses: ./downstream
      with:
        mode: ${{ inputs.mode }}
        marker: ${{ inputs.marker }}
"#
    }

    fn nested_action_metadata(name: &str) -> String {
        let body = match name {
            "downloader" => {
                "printf 'downloader\\n' >> \"$ACTION_MARKER\"\nprintf 'result=downloaded\\n' >> \"$GITHUB_OUTPUT\"\nif [ \"$ACTION_MODE\" = download-failure ]; then exit 11; fi\n"
            }
            "validator" => {
                "printf 'validator\\n' >> \"$ACTION_MARKER\"\nif [ \"$ACTION_MODE\" = validate-failure ]; then exit 13; fi\n"
            }
            "downstream" => {
                "printf 'downstream\\n' >> \"$ACTION_MARKER\"\nif [ \"$ACTION_MODE\" = downstream-failure ]; then exit 23; fi\n"
            }
            "external" => {
                "printf 'external\\n' >> \"$ACTION_MARKER\"\nprintf 'result=external-value\\n' >> \"$GITHUB_OUTPUT\"\nif [ \"$ACTION_MODE\" = external-failure ]; then exit 29; fi\n"
            }
            other => panic!("unknown nested consumer action: {other}"),
        };
        let output = if matches!(name, "downloader" | "external") {
            "outputs:\n  result:\n    value: ${{ steps.run.outputs.result }}\n"
        } else {
            ""
        };
        let body = body
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    line.to_owned()
                } else {
                    format!("        {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "name: {name}\ninputs:\n  mode:\n    default: success\n  marker:\n    default: action-consumer.log\n{output}runs:\n  using: composite\n  steps:\n    - id: run\n      shell: bash\n      env:\n        ACTION_MODE: ${{{{ inputs.mode }}}}\n        ACTION_MARKER: ${{{{ inputs.marker }}}}\n      run: |\n        {body}"
        )
    }

    fn write_nested_consumer_actions(root: &Path) {
        for name in ["downloader", "validator", "downstream"] {
            let directory = root.join(name);
            must(
                fs::create_dir_all(&directory),
                "create nested consumer action directory",
            );
            must(
                fs::write(directory.join("action.yml"), nested_action_metadata(name)),
                "write nested consumer action metadata",
            );
        }
    }

    fn write_pinned_external_action(root: &Path) {
        let directory = root.join("_actions/octo_example/0123456789abcdef0123456789abcdef01234567");
        must(
            fs::create_dir_all(&directory),
            "create pinned repository action fixture",
        );
        must(
            fs::write(
                directory.join("action.yml"),
                nested_action_metadata("external"),
            ),
            "write pinned repository action metadata",
        );
    }

    fn expanded_invocations(
        root: &Path,
        mode: &str,
        skip_build: bool,
        marker: &Path,
    ) -> Vec<velnor_runner::action_contract::CompositeActionInvocation> {
        use velnor_runner::action_contract::{
            composite_action_invocations, parse_action_metadata, LocalActionPlan,
        };

        let action_dir = root.join(".github/actions/consumer");
        let metadata = must(
            parse_action_metadata(action_metadata()),
            "parse runner action metadata",
        );
        must(
            composite_action_invocations(
                &LocalActionPlan {
                    step_id: "consumer".to_owned(),
                    action_dir,
                    inputs: BTreeMap::from([
                        ("mode".to_owned(), mode.to_owned()),
                        (
                            "skip-build".to_owned(),
                            if skip_build { "true" } else { "false" }.to_owned(),
                        ),
                        ("marker".to_owned(), marker.to_string_lossy().into_owned()),
                    ]),
                },
                &metadata,
                &root.to_string_lossy(),
                root,
            ),
            "expand runner composite action",
        )
    }

    fn expanded_action(
        root: &Path,
        step_id: &str,
        metadata_contents: &str,
    ) -> Vec<velnor_runner::action_contract::CompositeActionInvocation> {
        expanded_action_with_inputs(root, step_id, metadata_contents, BTreeMap::new())
    }

    fn expanded_action_with_inputs(
        root: &Path,
        step_id: &str,
        metadata_contents: &str,
        inputs: BTreeMap<String, String>,
    ) -> Vec<velnor_runner::action_contract::CompositeActionInvocation> {
        use velnor_runner::action_contract::{
            composite_action_invocations, parse_action_metadata, LocalActionPlan,
        };

        let action_dir = root.join(".github/actions").join(step_id);
        must(
            fs::create_dir_all(&action_dir),
            "create custom runner action directory",
        );
        must(
            fs::write(action_dir.join("action.yml"), metadata_contents),
            "write custom runner action metadata",
        );
        let metadata = must(
            parse_action_metadata(metadata_contents),
            "parse custom runner action metadata",
        );
        must(
            composite_action_invocations(
                &LocalActionPlan {
                    step_id: step_id.to_owned(),
                    action_dir,
                    inputs,
                },
                &metadata,
                &root.to_string_lossy(),
                root,
            ),
            "expand custom runner composite action",
        )
    }

    type ActionStepOutputs = BTreeMap<String, BTreeMap<String, String>>;
    type ActionStepStatuses = BTreeMap<String, velnor_runner::action_contract::ActionStepStatus>;
    #[derive(Clone)]
    struct LocalCompositeScope {
        scope_id: String,
        parent_scope_id: String,
        invocation_range: std::ops::Range<usize>,
        step_aliases: BTreeMap<String, String>,
        condition: Option<String>,
        continue_on_error: Option<String>,
        input_defaults: BTreeMap<String, String>,
        inputs: BTreeMap<String, String>,
        env: BTreeMap<String, String>,
    }

    type LocalCompositeScopes = Vec<LocalCompositeScope>;

    #[derive(Clone, Default)]
    struct ActionExecutionContext {
        inputs: BTreeMap<String, String>,
        env: BTreeMap<String, String>,
        step_aliases: BTreeMap<String, String>,
        step_outputs: ActionStepOutputs,
        step_statuses: ActionStepStatuses,
    }

    #[derive(Default)]
    struct ActionExecutionScope {
        step_outputs: ActionStepOutputs,
        step_statuses: ActionStepStatuses,
        inputs: BTreeMap<String, String>,
        env: BTreeMap<String, String>,
        step_aliases: BTreeMap<String, String>,
    }

    struct CompositeExecutionResult {
        output: std::process::Output,
        outputs: Option<BTreeMap<String, String>>,
        failed: bool,
    }

    impl CompositeExecutionResult {
        fn succeeded(&self) -> bool {
            !self.failed
        }

        fn exit_code(&self) -> i32 {
            self.output
                .status
                .code()
                .filter(|code| *code != 0 || !self.failed)
                .unwrap_or(i32::from(self.failed))
        }
    }

    fn condition_runs(condition: Option<&str>, scope: &ActionExecutionScope) -> bool {
        condition_runs_with_env(condition, scope, &scope.env)
    }

    fn condition_runs_with_env(
        condition: Option<&str>,
        scope: &ActionExecutionScope,
        env: &BTreeMap<String, String>,
    ) -> bool {
        must(
            velnor_runner::action_contract::evaluate_action_condition_with_step_context(
                condition,
                &scope.inputs,
                env,
                &scope.step_outputs,
                &scope.step_statuses,
                &scope.step_aliases,
            ),
            "evaluate runner action condition with composite context",
        )
    }

    fn render_action_value(value: &str, scope: &ActionExecutionScope) -> String {
        render_action_value_with_env(value, scope, &scope.env)
    }

    fn render_action_value_with_env(
        value: &str,
        scope: &ActionExecutionScope,
        env: &BTreeMap<String, String>,
    ) -> String {
        must(
            velnor_runner::action_contract::render_action_expression_with_step_context(
                value,
                &scope.inputs,
                env,
                &scope.step_outputs,
                &scope.step_statuses,
                &scope.step_aliases,
            ),
            "evaluate runner action expression",
        )
    }

    fn environment_for_step(
        scope: &ActionExecutionScope,
        values: impl IntoIterator<Item = (String, String)>,
    ) -> BTreeMap<String, String> {
        let mut env = scope.env.clone();
        for (name, value) in values {
            env.insert(
                name,
                render_action_value_with_env(&value, scope, &scope.env),
            );
        }
        env
    }

    fn action_input_defaults(
        metadata: &velnor_runner::action_contract::ActionMetadata,
    ) -> BTreeMap<String, String> {
        metadata
            .inputs
            .iter()
            .filter_map(|(name, input)| {
                input
                    .default_value
                    .as_ref()
                    .map(|value| (name.to_ascii_lowercase(), value.clone()))
            })
            .collect()
    }

    fn action_inputs(
        metadata: &velnor_runner::action_contract::ActionMetadata,
        provided: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        let mut inputs = action_input_defaults(metadata);
        inputs.extend(
            provided
                .iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.clone())),
        );
        inputs
    }

    fn record_step_outputs(
        output_file: &Path,
        step_id: &str,
        scope: &mut ActionExecutionScope,
    ) -> Result<(), String> {
        let contents = match fs::read_to_string(output_file) {
            Ok(contents) => contents,
            // Runner's EnvFileKeyValuePairs skips a missing command file.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(format!(
                    "read action output file {}: {error}",
                    output_file.display()
                ));
            }
        };
        let outputs = velnor_runner::action_contract::parse_action_output_file_contents(&contents)?;
        if !outputs.is_empty() {
            scope.step_outputs.insert(step_id.to_owned(), outputs);
        }
        Ok(())
    }

    fn mirror_step_outputs(step_file: &Path, action_output_file: &Path) -> Result<(), String> {
        use std::io::Write;

        let contents = match fs::read(step_file) {
            Ok(contents) => contents,
            // FileCommandManager treats a deleted or missing command file as
            // an empty output set.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "read runner step output file {}: {error}",
                    step_file.display()
                ));
            }
        };
        if contents.is_empty() {
            return Ok(());
        }
        let mut action_output = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(action_output_file)
            .map_err(|error| {
                format!(
                    "open action output mirror {}: {error}",
                    action_output_file.display()
                )
            })?;
        action_output.write_all(&contents).map_err(|error| {
            format!(
                "write action output mirror {}: {error}",
                action_output_file.display()
            )
        })
    }

    fn record_step_status(
        scope: &mut ActionExecutionScope,
        step_id: &str,
        exit_code: i32,
        skipped: bool,
        continue_on_error: bool,
    ) {
        scope.step_statuses.insert(
            step_id.to_owned(),
            velnor_runner::action_contract::ActionStepStatus {
                exit_code,
                skipped,
                continue_on_error,
            },
        );
    }

    fn successful_output() -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn action_metadata_path(action_dir: &Path) -> PathBuf {
        ["action.yml", "action.yaml"]
            .iter()
            .map(|name| action_dir.join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| panic!("runner action metadata missing: {}", action_dir.display()))
    }

    fn expression_legal_segment(value: &str) -> String {
        let segment = value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        if segment
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
        {
            format!("_{segment}")
        } else {
            segment
        }
    }

    fn composite_step_id(prefix: &str, id: Option<&str>, index: usize) -> String {
        let prefix = expression_legal_segment(prefix);
        id.map(|id| format!("{prefix}-{}", expression_legal_segment(id)))
            .filter(|value| !value.ends_with('-'))
            .unwrap_or_else(|| format!("{prefix}-{}", index + 1))
    }

    fn action_step_aliases(
        metadata: &velnor_runner::action_contract::ActionMetadata,
        scope_prefix: &str,
    ) -> BTreeMap<String, String> {
        metadata
            .runs
            .steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| {
                step.id.as_deref().map(|id| {
                    (
                        id.to_owned(),
                        composite_step_id(scope_prefix, Some(id), index),
                    )
                })
            })
            .collect()
    }

    fn read_action_metadata(action_dir: &Path) -> velnor_runner::action_contract::ActionMetadata {
        use velnor_runner::action_contract::parse_action_metadata;

        let metadata_path = action_metadata_path(action_dir);
        must(
            fs::read_to_string(&metadata_path)
                .map_err(|error| format!("{}: {error}", metadata_path.display()))
                .and_then(|contents| {
                    parse_action_metadata(&contents)
                        .map_err(|error| format!("{}: {error}", metadata_path.display()))
                }),
            "read local composite scope metadata",
        )
    }

    fn collect_local_composite_scopes_from_metadata(
        workspace: &Path,
        scope_prefix: &str,
        metadata: &velnor_runner::action_contract::ActionMetadata,
        scopes: &mut LocalCompositeScopes,
    ) -> usize {
        let mut invocation_count = 0;
        for (index, step) in metadata.runs.steps.iter().enumerate() {
            let step_id = composite_step_id(scope_prefix, step.id.as_deref(), index);
            let Some(uses) = step.uses.as_deref() else {
                if step.run.is_some() {
                    invocation_count += 1;
                }
                continue;
            };
            if !uses.starts_with('.') {
                invocation_count += 1;
                continue;
            }
            let local_path = uses
                .strip_prefix("./")
                .or_else(|| uses.strip_prefix(".\\"))
                .unwrap_or(uses);

            let local_scope_id = step_id;
            let nested_action_dir = workspace.join(local_path.replace('\\', "/"));
            let nested_metadata = read_action_metadata(&nested_action_dir);
            let step_aliases = action_step_aliases(&nested_metadata, &local_scope_id);
            let invocation_start = invocation_count;
            let nested_invocation_count = collect_local_composite_scopes_from_metadata(
                workspace,
                &local_scope_id,
                &nested_metadata,
                scopes,
            );
            invocation_count += nested_invocation_count;
            scopes.push(LocalCompositeScope {
                scope_id: local_scope_id,
                parent_scope_id: scope_prefix.to_owned(),
                invocation_range: invocation_start..invocation_count,
                step_aliases,
                condition: step.condition.clone(),
                continue_on_error: step.continue_on_error.clone(),
                input_defaults: action_input_defaults(&nested_metadata),
                inputs: step.with.clone(),
                env: step.env.clone(),
            });
        }
        if metadata
            .outputs
            .values()
            .any(|output| output.value.is_some())
        {
            invocation_count += 1;
        }
        invocation_count
    }

    fn collect_local_composite_scopes(
        workspace: &Path,
        action_dir: &Path,
        scope_prefix: &str,
        scopes: &mut LocalCompositeScopes,
        expected_invocation_count: usize,
    ) {
        let metadata = read_action_metadata(action_dir);
        let invocation_count = collect_local_composite_scopes_from_metadata(
            workspace,
            scope_prefix,
            &metadata,
            scopes,
        );
        assert_eq!(
            invocation_count, expected_invocation_count,
            "fixture scope map did not cover the expanded action invocation list"
        );
    }

    fn local_composite_scopes(
        workspace: &Path,
        action_dir: &Path,
        scope_prefix: &str,
        expected_invocation_count: usize,
    ) -> LocalCompositeScopes {
        let mut scopes = Vec::new();
        collect_local_composite_scopes(
            workspace,
            action_dir,
            scope_prefix,
            &mut scopes,
            expected_invocation_count,
        );
        scopes
    }

    fn local_scope_segment_at(
        invocations: &[velnor_runner::action_contract::CompositeActionInvocation],
        index: usize,
        action_step_id: &str,
        local_scopes: &LocalCompositeScopes,
    ) -> Option<(LocalCompositeScope, usize)> {
        if index >= invocations.len() {
            return None;
        }
        let scope = local_scopes.iter().find(|scope| {
            scope.parent_scope_id == action_step_id
                && scope.invocation_range.start == index
                && scope.invocation_range.start < scope.invocation_range.end
        })?;
        (scope.invocation_range.end <= invocations.len())
            .then(|| (scope.clone(), scope.invocation_range.end))
    }

    fn output_file_for_step(
        output_file: &Path,
        scope_id: &str,
        step_id: &str,
        step_index: usize,
    ) -> PathBuf {
        let base_name = output_file.file_name().map_or_else(
            || "output".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let scope_id = scope_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let step_id = step_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        output_file.with_file_name(format!("{base_name}.{scope_id}.{step_index}.{step_id}"))
    }

    fn runner_working_directory(root: &Path, container_path: &str) -> PathBuf {
        let root = must(
            fs::canonicalize(root),
            "canonicalize runner consumer workspace",
        );
        let candidate = Path::new(container_path);
        let candidate = if let Some(relative) = container_path.strip_prefix("/__w") {
            root.join(relative.trim_start_matches('/'))
        } else if candidate.is_absolute() {
            candidate.to_owned()
        } else {
            root.join(candidate)
        };
        let candidate = match fs::canonicalize(&candidate) {
            Ok(candidate) => candidate,
            Err(error) => panic!(
                "resolve runner composite working directory {}: {error}",
                candidate.display()
            ),
        };
        assert!(
            candidate.starts_with(&root),
            "runner composite working directory escaped workspace: {}",
            candidate.display()
        );
        candidate
    }

    fn execute_action_invocations(
        root: &Path,
        action_step_id: &str,
        invocations: &[velnor_runner::action_contract::CompositeActionInvocation],
        marker: &Path,
        downstream: &Path,
        output_file: &Path,
        step_outputs: &mut ActionStepOutputs,
    ) -> CompositeExecutionResult {
        execute_action_invocations_with_context(
            root,
            action_step_id,
            invocations,
            marker,
            downstream,
            output_file,
            &ActionExecutionContext::default(),
            step_outputs,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "fixture executor keeps the runner action context explicit"
    )]
    fn execute_action_invocations_with_context(
        root: &Path,
        action_step_id: &str,
        invocations: &[velnor_runner::action_contract::CompositeActionInvocation],
        marker: &Path,
        downstream: &Path,
        output_file: &Path,
        provided_context: &ActionExecutionContext,
        step_outputs: &mut ActionStepOutputs,
    ) -> CompositeExecutionResult {
        let action_dir = root.join(".github/actions").join(action_step_id);
        let metadata = read_action_metadata(&action_dir);
        let action_context = ActionExecutionContext {
            inputs: action_inputs(&metadata, &provided_context.inputs),
            env: provided_context.env.clone(),
            step_aliases: action_step_aliases(&metadata, action_step_id),
            step_outputs: provided_context.step_outputs.clone(),
            step_statuses: provided_context.step_statuses.clone(),
        };
        let local_scopes =
            local_composite_scopes(root, &action_dir, action_step_id, invocations.len());
        let mut result = execute_composite_scope(
            root,
            action_step_id,
            invocations,
            marker,
            downstream,
            output_file,
            &local_scopes,
            &action_context,
        );
        if let Some(outputs) = result.outputs.take() {
            step_outputs.insert(action_step_id.to_owned(), outputs);
        }
        result
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "fixture executor carries nested action context explicitly"
    )]
    fn execute_composite_scope(
        root: &Path,
        action_step_id: &str,
        invocations: &[velnor_runner::action_contract::CompositeActionInvocation],
        marker: &Path,
        downstream: &Path,
        output_file: &Path,
        local_scopes: &LocalCompositeScopes,
        action_context: &ActionExecutionContext,
    ) -> CompositeExecutionResult {
        use velnor_runner::action_contract::{
            parse_action_metadata, CompositeActionInvocation, ResolvedAction,
        };

        let mut scope = ActionExecutionScope {
            inputs: action_context.inputs.clone(),
            env: action_context.env.clone(),
            step_aliases: action_context.step_aliases.clone(),
            step_outputs: action_context.step_outputs.clone(),
            step_statuses: action_context.step_statuses.clone(),
        };
        let mut failed_output = None;
        let mut mapped_outputs = None;
        let mut index = 0;
        while index < invocations.len() {
            if let Some((local_scope, end)) =
                local_scope_segment_at(invocations, index, action_step_id, local_scopes)
            {
                let local_scope_id = local_scope.scope_id.clone();
                let local_env = environment_for_step(
                    &scope,
                    local_scope
                        .env
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone())),
                );
                let continue_on_error =
                    local_scope
                        .continue_on_error
                        .as_deref()
                        .is_some_and(|value| {
                            render_action_value_with_env(value, &scope, &local_env)
                                .eq_ignore_ascii_case("true")
                        });
                let mut local_inputs = local_scope.input_defaults.clone();
                local_inputs.extend(local_scope.inputs.iter().map(|(name, value)| {
                    (
                        name.to_ascii_lowercase(),
                        render_action_value_with_env(value, &scope, &local_env),
                    )
                }));
                if !condition_runs_with_env(local_scope.condition.as_deref(), &scope, &local_env) {
                    scope
                        .step_outputs
                        .insert(local_scope_id.clone(), BTreeMap::new());
                    record_step_status(&mut scope, &local_scope_id, 0, true, continue_on_error);
                    index = end;
                    continue;
                }
                let nested_context = ActionExecutionContext {
                    inputs: local_inputs,
                    env: local_env,
                    step_aliases: local_scope.step_aliases.clone(),
                    step_outputs: scope.step_outputs.clone(),
                    step_statuses: scope.step_statuses.clone(),
                };
                let nested_result = execute_composite_scope(
                    root,
                    &local_scope_id,
                    &invocations[index..end],
                    marker,
                    downstream,
                    output_file,
                    local_scopes,
                    &nested_context,
                );
                let failed = !nested_result.succeeded();
                let exit_code = nested_result.exit_code();
                scope.step_outputs.insert(
                    local_scope_id.clone(),
                    nested_result.outputs.unwrap_or_default(),
                );
                record_step_status(
                    &mut scope,
                    &local_scope_id,
                    exit_code,
                    false,
                    continue_on_error,
                );
                if failed && !continue_on_error && failed_output.is_none() {
                    failed_output = Some(nested_result.output);
                }
                index = end;
                continue;
            }

            let invocation = &invocations[index];
            match invocation {
                CompositeActionInvocation::Script(step) => {
                    let step_env = environment_for_step(&scope, step.env.iter().cloned());
                    if !condition_runs_with_env(step.condition.as_deref(), &scope, &step_env) {
                        record_step_status(&mut scope, &step.id, 0, true, false);
                        index += 1;
                        continue;
                    }
                    let script = render_action_value_with_env(&step.script, &scope, &step_env);
                    let step_output_file =
                        output_file_for_step(output_file, action_step_id, &step.id, index);
                    let step_script_file = step_output_file.with_extension("script");
                    must(
                        fs::write(&step_output_file, ""),
                        "initialize runner output file",
                    );
                    must(
                        fs::write(&step_script_file, script),
                        "write runner script step file",
                    );
                    let command_args = velnor_runner::action_contract::script_command_args(
                        step,
                        &step_script_file.to_string_lossy(),
                    );
                    let (program, args) = command_args
                        .split_first()
                        .unwrap_or_else(|| panic!("runner returned empty script command"));
                    let working_directory =
                        runner_working_directory(root, &step.working_directory_container);
                    let mut command = Command::new(program);
                    command
                        .args(args)
                        .current_dir(working_directory)
                        .env("ACTION_MARKER", marker)
                        .env("DOWNSTREAM_MARKER", downstream)
                        .envs(step_env.iter())
                        .env("GITHUB_OUTPUT", &step_output_file);
                    let mut output = must(command.output(), "execute runner script step");
                    let output_error =
                        match record_step_outputs(&step_output_file, &step.id, &mut scope) {
                            Err(error) => Some(error),
                            Ok(()) => mirror_step_outputs(&step_output_file, output_file).err(),
                        };
                    if let Some(error) = &output_error {
                        output.stderr.extend_from_slice(
                            format!("GITHUB_OUTPUT processing failed: {error}").as_bytes(),
                        );
                    }
                    let failed = !output.status.success() || output_error.is_some();
                    let exit_code = if output.status.success() && output_error.is_some() {
                        1
                    } else {
                        output.status.code().unwrap_or(1)
                    };
                    record_step_status(
                        &mut scope,
                        &step.id,
                        exit_code,
                        false,
                        step.continue_on_error,
                    );
                    if failed && !step.continue_on_error && failed_output.is_none() {
                        failed_output = Some(output);
                    }
                }
                CompositeActionInvocation::Repository(plan) => {
                    let plan_env = environment_for_step(&scope, plan.env.iter().cloned());
                    if !condition_runs_with_env(plan.condition.as_deref(), &scope, &plan_env) {
                        record_step_status(&mut scope, &plan.step_id, 0, true, false);
                        index += 1;
                        continue;
                    }
                    let metadata_path = action_metadata_path(&plan.action_dir);
                    let metadata = must(
                        fs::read_to_string(&metadata_path)
                            .map_err(|error| format!("{}: {error}", metadata_path.display()))
                            .and_then(|contents| {
                                parse_action_metadata(&contents).map_err(|error| {
                                    format!("{}: {error}", metadata_path.display())
                                })
                            }),
                        "parse runner repository action metadata",
                    );
                    let runtime = must(metadata.runtime(), "classify runner repository action");
                    let nested_inputs = plan
                        .inputs
                        .iter()
                        .map(|(name, value)| {
                            (
                                name.clone(),
                                render_action_value_with_env(value, &scope, &plan_env),
                            )
                        })
                        .collect::<BTreeMap<_, _>>();
                    let nested_context = ActionExecutionContext {
                        inputs: action_inputs(&metadata, &nested_inputs),
                        env: plan_env,
                        step_aliases: action_step_aliases(&metadata, &plan.step_id),
                        step_outputs: scope.step_outputs.clone(),
                        step_statuses: scope.step_statuses.clone(),
                    };
                    let resolved = ResolvedAction {
                        plan: plan.clone(),
                        metadata_path,
                        metadata,
                        runtime,
                    };
                    let nested = must(
                        resolved.composite_invocations("/__w", root),
                        "expand runner repository action",
                    );
                    let nested_local_scopes =
                        local_composite_scopes(root, &plan.action_dir, &plan.step_id, nested.len());
                    let nested_result = execute_composite_scope(
                        root,
                        &plan.step_id,
                        &nested,
                        marker,
                        downstream,
                        output_file,
                        &nested_local_scopes,
                        &nested_context,
                    );
                    let failed = !nested_result.succeeded();
                    let exit_code = nested_result.exit_code();
                    scope.step_outputs.insert(
                        plan.step_id.clone(),
                        nested_result.outputs.unwrap_or_default(),
                    );
                    record_step_status(
                        &mut scope,
                        &plan.step_id,
                        exit_code,
                        false,
                        plan.continue_on_error,
                    );
                    if failed && !plan.continue_on_error && failed_output.is_none() {
                        failed_output = Some(nested_result.output);
                    }
                }
                CompositeActionInvocation::Outputs(outputs) => {
                    let resolved = outputs
                        .outputs
                        .iter()
                        .map(|(name, value)| (name.clone(), render_action_value(value, &scope)))
                        .collect::<BTreeMap<_, _>>();
                    if outputs.step_id == action_step_id {
                        mapped_outputs = Some(resolved);
                    } else {
                        // Preserve a wrapper entry if an empty local action
                        // contributes an output marker without child steps.
                        scope.step_outputs.insert(outputs.step_id.clone(), resolved);
                        record_step_status(&mut scope, &outputs.step_id, 0, false, false);
                    }
                }
            }
            index += 1;
        }
        let failed = failed_output.is_some();
        CompositeExecutionResult {
            output: failed_output.unwrap_or_else(successful_output),
            outputs: mapped_outputs,
            failed,
        }
    }

    #[test]
    fn step_output_capture_uses_runner_parser_and_reports_read_errors() {
        let root = scratch("runner-output-parser");
        let output_file = root.join("output");
        must(
            fs::write(
                &output_file,
                "result<<END\nfirst=one=two\nsecond=x=y\nEND\n",
            ),
            "write multiline output fixture",
        );
        let mut scope = ActionExecutionScope::default();
        must(
            record_step_outputs(&output_file, "producer", &mut scope),
            "parse multiline output fixture",
        );
        assert_eq!(
            scope
                .step_outputs
                .get("producer")
                .and_then(|outputs| outputs.get("result"))
                .map(String::as_str),
            Some("first=one=two\nsecond=x=y")
        );

        let invalid_output_path = root.join("output-directory");
        must(
            fs::create_dir(&invalid_output_path),
            "create invalid output-file fixture",
        );
        let error = match record_step_outputs(&invalid_output_path, "bad-reader", &mut scope) {
            Ok(()) => panic!("reading a directory as an output file must fail"),
            Err(error) => error,
        };
        assert!(error.contains("read action output file"));

        let missing_output_path = root.join("missing-output");
        assert!(record_step_outputs(&missing_output_path, "missing-reader", &mut scope).is_ok());
        assert!(!scope.step_outputs.contains_key("missing-reader"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_child_runs_runner_cleanup_steps_and_maps_outputs() {
        let root = scratch("runner-failure-cleanup");
        let metadata = r#"name: failure-cleanup
outputs:
  result:
    value: ${{ steps.failed.outputs.result }}
runs:
  using: composite
  steps:
    - id: failed
      shell: bash
      run: |
        printf 'result=written-before-failure\n' >> "$GITHUB_OUTPUT"
        exit 7
    - id: always-cleanup
      if: always()
      shell: bash
      run: printf 'always-cleanup\n' >> "$ACTION_MARKER"
    - id: failure-cleanup
      if: failure()
      shell: bash
      run: printf 'failure-cleanup\n' >> "$ACTION_MARKER"
    - id: normal-after-failure
      shell: bash
      run: printf 'normal-after-failure\n' >> "$ACTION_MARKER"
"#;
        let invocations = expanded_action(&root, "cleanup", metadata);
        let marker = root.join("cleanup.log");
        let downstream = root.join("cleanup.downstream");
        let output_file = root.join("cleanup.output");
        let mut outputs = BTreeMap::new();
        let result = execute_action_invocations(
            &root,
            "cleanup",
            &invocations,
            &marker,
            &downstream,
            &output_file,
            &mut outputs,
        );

        assert!(!result.succeeded());
        let log = must(fs::read_to_string(&marker), "read failure cleanup marker");
        assert!(
            log.contains("always-cleanup"),
            "always() cleanup skipped: {log}"
        );
        assert!(
            log.contains("failure-cleanup"),
            "failure() cleanup skipped: {log}"
        );
        assert!(!log.contains("normal-after-failure"));
        assert_eq!(
            outputs
                .get("cleanup")
                .and_then(|action_outputs| action_outputs.get("result"))
                .map(String::as_str),
            Some("written-before-failure")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn continue_on_error_keeps_later_steps_successful_and_maps_outputs() {
        let root = scratch("runner-continue-on-error");
        let metadata = r#"name: continue-on-error
outputs:
  result:
    value: ${{ steps.tolerated.outputs.result }}
runs:
  using: composite
  steps:
    - id: tolerated
      continue-on-error: true
      shell: bash
      run: |
        printf 'result=kept-after-failure\n' >> "$GITHUB_OUTPUT"
        exit 9
    - id: after-failure
      shell: bash
      run: printf 'continued\n' >> "$ACTION_MARKER"
    - id: failure-only
      if: failure()
      shell: bash
      run: printf 'unexpected-failure-status\n' >> "$ACTION_MARKER"
    - id: success-only
      if: success()
      shell: bash
      run: printf 'success-status\n' >> "$ACTION_MARKER"
"#;
        let invocations = expanded_action(&root, "tolerated", metadata);
        let marker = root.join("continue-on-error.log");
        let downstream = root.join("continue-on-error.downstream");
        let output_file = root.join("continue-on-error.output");
        let mut outputs = BTreeMap::new();
        let result = execute_action_invocations(
            &root,
            "tolerated",
            &invocations,
            &marker,
            &downstream,
            &output_file,
            &mut outputs,
        );

        assert!(result.succeeded());
        let log = must(fs::read_to_string(&marker), "read continue-on-error marker");
        assert!(
            log.contains("continued"),
            "continue-on-error blocked later steps: {log}"
        );
        assert!(log.contains("success-status"), "success() was false: {log}");
        assert!(!log.contains("unexpected-failure-status"));
        assert_eq!(
            outputs
                .get("tolerated")
                .and_then(|action_outputs| action_outputs.get("result"))
                .map(String::as_str),
            Some("kept-after-failure")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn later_step_output_rewrite_uses_a_fresh_runner_command_file() {
        let root = scratch("runner-output-rewrite");
        let metadata = r#"name: output-rewrite
outputs:
  result:
    value: ${{ steps.replacement.outputs.result }}
runs:
  using: composite
  steps:
    - id: initial
      shell: bash
      run: printf 'result=x\n' >> "$GITHUB_OUTPUT"
    - id: replacement
      shell: bash
      run: printf 'result=replacement-value\n' > "$GITHUB_OUTPUT"
    - id: verify
      if: ${{ steps.replacement.outputs.result == 'replacement-value' }}
      shell: bash
      run: printf 'replacement-visible\n' >> "$ACTION_MARKER"
"#;
        let invocations = expanded_action(&root, "output-rewrite", metadata);
        let marker = root.join("output-rewrite.log");
        let mut outputs = BTreeMap::new();
        let result = execute_action_invocations(
            &root,
            "output-rewrite",
            &invocations,
            &marker,
            &root.join("output-rewrite.downstream"),
            &root.join("output-rewrite.output"),
            &mut outputs,
        );

        assert!(result.succeeded());
        assert!(
            must(fs::read_to_string(&marker), "read output rewrite marker")
                .contains("replacement-visible")
        );
        assert_eq!(
            outputs
                .get("output-rewrite")
                .and_then(|action_outputs| action_outputs.get("result"))
                .map(String::as_str),
            Some("replacement-value")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_runner_output_file_is_treated_as_empty_output() {
        let root = scratch("runner-output-file-removed");
        let metadata = r#"name: output-file-removed
outputs:
  result:
    value: ${{ steps.remove-file.outputs.result }}
runs:
  using: composite
  steps:
    - id: remove-file
      shell: bash
      run: rm "$GITHUB_OUTPUT"
    - id: after-removal
      if: success()
      shell: bash
      run: printf 'after-removal\n' >> "$ACTION_MARKER"
"#;
        let invocations = expanded_action(&root, "output-file-removed", metadata);
        let marker = root.join("output-file-removed.log");
        let output_file = root.join("output-file-removed.output");
        let mut outputs = BTreeMap::new();
        let result = execute_action_invocations(
            &root,
            "output-file-removed",
            &invocations,
            &marker,
            &root.join("output-file-removed.downstream"),
            &output_file,
            &mut outputs,
        );

        assert!(result.succeeded());
        assert!(
            must(fs::read_to_string(&marker), "read output removal marker")
                .contains("after-removal"),
            "missing output file incorrectly failed the preceding step"
        );
        assert!(!output_file.exists());
        assert_eq!(
            outputs
                .get("output-file-removed")
                .and_then(|action_outputs| action_outputs.get("result"))
                .map(String::as_str),
            Some("")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn consumer_simulator_dispatches_declared_shell_and_working_directory() {
        let root = scratch("runner-shell-working-directory");
        must(
            fs::create_dir_all(root.join("subdir")),
            "create runner working-directory fixture",
        );
        let metadata = r#"name: shell-working-directory
runs:
  using: composite
  steps:
    - id: bash-step
      shell: bash
      working-directory: subdir
      run: printf 'bash-cwd=%s\n' "$PWD" >> "$ACTION_MARKER"
    - id: sh-step
      shell: sh
      working-directory: subdir
      run: printf 'sh-cwd=%s\n' "$PWD" >> "$ACTION_MARKER"
"#;
        let invocations = expanded_action(&root, "shell-working-directory", metadata);
        let marker = root.join("shell-working-directory.log");
        let mut outputs = BTreeMap::new();
        let result = execute_action_invocations(
            &root,
            "shell-working-directory",
            &invocations,
            &marker,
            &root.join("shell-working-directory.downstream"),
            &root.join("shell-working-directory.output"),
            &mut outputs,
        );

        assert!(result.succeeded());
        let log = must(
            fs::read_to_string(&marker),
            "read shell working-directory marker",
        );
        let expected = must(
            fs::canonicalize(root.join("subdir")),
            "canonicalize expected shell working-directory",
        )
        .to_string_lossy()
        .into_owned();
        assert!(
            log.contains(&format!("bash-cwd={expected}")),
            "bash cwd: {log}"
        );
        assert!(log.contains(&format!("sh-cwd={expected}")), "sh cwd: {log}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn output_parser_failure_marks_child_failed_and_runs_runner_cleanup() {
        let root = scratch("runner-output-parser-failure");
        let metadata = r#"name: output-parser-failure
outputs:
  result:
    value: ${{ steps.malformed.outputs.result }}
runs:
  using: composite
  steps:
    - id: malformed
      shell: bash
      run: printf 'not-a-command-file-entry\n' > "$GITHUB_OUTPUT"
    - id: always-cleanup
      if: always()
      shell: bash
      run: printf 'always-cleanup\n' >> "$ACTION_MARKER"
    - id: failure-cleanup
      if: failure()
      shell: bash
      run: printf 'failure-cleanup\n' >> "$ACTION_MARKER"
    - id: normal-after-failure
      shell: bash
      run: printf 'normal-after-failure\n' >> "$ACTION_MARKER"
"#;
        let invocations = expanded_action(&root, "output-parser-failure", metadata);
        let marker = root.join("output-parser-failure.log");
        let mut outputs = BTreeMap::new();
        let result = execute_action_invocations(
            &root,
            "output-parser-failure",
            &invocations,
            &marker,
            &root.join("output-parser-failure.downstream"),
            &root.join("output-parser-failure.output"),
            &mut outputs,
        );

        assert!(!result.succeeded());
        let log = must(fs::read_to_string(&marker), "read parser failure marker");
        assert!(log.contains("always-cleanup"), "always() skipped: {log}");
        assert!(log.contains("failure-cleanup"), "failure() skipped: {log}");
        assert!(!log.contains("normal-after-failure"));
        assert_eq!(
            outputs
                .get("output-parser-failure")
                .and_then(|action_outputs| action_outputs.get("result"))
                .map(String::as_str),
            Some("")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_local_composite_outputs_stay_in_their_steps_scope() {
        let root = scratch("runner-nested-local-scope");
        let nested_action_dir = root.join("nested");
        must(
            fs::create_dir_all(&nested_action_dir),
            "create nested local composite directory",
        );
        must(
            fs::write(
                nested_action_dir.join("action.yml"),
                r#"name: nested-local
outputs:
  result:
    value: ${{ steps.after.outputs.result }}
runs:
  using: composite
  steps:
    - id: after
      shell: bash
      run: printf 'result=nested-value\n' >> "$GITHUB_OUTPUT"
"#,
            ),
            "write nested local composite metadata",
        );
        let metadata = r#"name: local-scope
outputs:
  result:
    value: ${{ steps.nested.outputs.result }}
runs:
  using: composite
  steps:
    - id: nested
      uses: ./nested
    - id: nested-after
      shell: bash
      run: printf 'result=sibling-visible\n' >> "$GITHUB_OUTPUT"
    - id: sibling-output-is-visible
      if: ${{ steps.nested-after.outputs.result == 'sibling-visible' }}
      shell: bash
      run: printf 'sibling-output-visible\n' >> "$ACTION_MARKER"
    - id: nested-again
      uses: ./nested
    - id: repeated-inner-id-output-is-visible
      if: ${{ steps.nested-again.outputs.result == 'nested-value' }}
      shell: bash
      run: printf 'repeated-inner-id-output-visible\n' >> "$ACTION_MARKER"
    - id: child-output-must-stay-private
      if: ${{ steps.local-scope-nested-after.outputs.result == 'nested-value' }}
      shell: bash
      run: printf 'child-output-leaked\n' >> "$ACTION_MARKER"
    - id: repeated-child-output-must-stay-private
      if: ${{ steps.local-scope-nested-again-after.outputs.result == 'nested-value' }}
      shell: bash
      run: printf 'repeated-child-output-leaked\n' >> "$ACTION_MARKER"
    - id: wrapper-output-is-visible
      if: ${{ steps.nested.outputs.result == 'nested-value' }}
      shell: bash
      run: printf 'wrapper-output-visible\n' >> "$ACTION_MARKER"
"#;
        let invocations = expanded_action(&root, "local-scope", metadata);
        let marker = root.join("local-scope.log");
        let mut outputs = BTreeMap::new();
        let result = execute_action_invocations(
            &root,
            "local-scope",
            &invocations,
            &marker,
            &root.join("local-scope.downstream"),
            &root.join("local-scope.output"),
            &mut outputs,
        );

        assert!(result.succeeded());
        let log = must(
            fs::read_to_string(&marker),
            "read nested local scope marker",
        );
        assert!(
            log.contains("wrapper-output-visible"),
            "nested mapped output did not reach its caller: {log}"
        );
        assert!(
            log.contains("sibling-output-visible"),
            "caller step with the same flattened ID as a nested child was captured: {log}"
        );
        assert!(
            log.contains("repeated-inner-id-output-visible"),
            "repeated inner step ID did not stay isolated to the second wrapper: {log}"
        );
        assert!(
            !log.contains("child-output-leaked"),
            "nested child output leaked into its caller's steps scope: {log}"
        );
        assert!(
            !log.contains("repeated-child-output-leaked"),
            "repeated nested child output leaked into its caller's steps scope: {log}"
        );
        assert_eq!(
            outputs
                .get("local-scope")
                .and_then(|action_outputs| action_outputs.get("result"))
                .map(String::as_str),
            Some("nested-value")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_local_gates_use_caller_inputs_and_environment() {
        let root = scratch("runner-nested-local-context");
        let nested_action_dir = root.join("nested-context");
        must(
            fs::create_dir_all(&nested_action_dir),
            "create nested context composite directory",
        );
        must(
            fs::write(
                nested_action_dir.join("action.yml"),
                r#"name: nested-context
inputs:
  run:
    default: 'false'
runs:
  using: composite
  steps:
    - id: gated-failure
      if: ${{ inputs.run == 'true' && env.GATE == 'enabled' && env.NESTED_FLAG == 'true' }}
      shell: bash
      run: |
        printf 'nested-failure-ran\n' >> "$ACTION_MARKER"
        exit 17
"#,
            ),
            "write nested context composite metadata",
        );
        let metadata = r#"name: nested-context-parent
inputs:
  run:
    default: 'false'
  tolerate:
    default: 'false'
runs:
  using: composite
  steps:
    - id: before
      continue-on-error: true
      shell: bash
      run: |
        printf 'flag=true\n' >> "$GITHUB_OUTPUT"
        exit 9
    - id: nested
      if: ${{ steps.before.outputs.flag == 'true' && steps.before.outcome == 'failure' && steps.before.conclusion == 'success' && inputs.run == 'true' && env.GATE == 'enabled' }}
      continue-on-error: ${{ steps.before.outcome == 'failure' && steps.before.conclusion == 'success' && inputs.tolerate == 'true' }}
      env:
        NESTED_FLAG: ${{ steps.before.outputs.flag }}
      uses: ./nested-context
      with:
        run: ${{ steps.before.outputs.flag }}
    - id: after-tolerated-failure
      shell: bash
      run: printf 'after-tolerated-failure\n' >> "$ACTION_MARKER"
    - id: failure-status-must-stay-success
      if: failure()
      shell: bash
      run: printf 'failure-status\n' >> "$ACTION_MARKER"
"#;
        let inputs = BTreeMap::from([
            ("run".to_owned(), "true".to_owned()),
            ("tolerate".to_owned(), "true".to_owned()),
        ]);
        let invocations =
            expanded_action_with_inputs(&root, "nested-context-parent", metadata, inputs.clone());
        let marker = root.join("nested-context.log");
        let output_file = root.join("nested-context.output");
        let context = ActionExecutionContext {
            inputs,
            env: BTreeMap::from([("GATE".to_owned(), "enabled".to_owned())]),
            step_aliases: BTreeMap::new(),
            ..ActionExecutionContext::default()
        };
        let mut outputs = BTreeMap::new();
        let result = execute_action_invocations_with_context(
            &root,
            "nested-context-parent",
            &invocations,
            &marker,
            &root.join("nested-context.downstream"),
            &output_file,
            &context,
            &mut outputs,
        );

        assert!(result.succeeded());
        let log = must(fs::read_to_string(&marker), "read nested context marker");
        assert!(
            log.contains("nested-failure-ran"),
            "input/env gate skipped: {log}"
        );
        assert!(
            log.contains("after-tolerated-failure"),
            "expression-valued continue-on-error did not tolerate failure: {log}"
        );
        assert!(!log.contains("failure-status"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn consumer_fixtures_prove_runner_graph_success_failure_and_skip_build() {
        use velnor_runner::action_contract::CompositeActionInvocation;

        let root = scratch("runner-consumers");
        let action_dir = root.join(".github/actions/consumer");
        must(
            fs::create_dir_all(&action_dir),
            "create runner action directory",
        );
        must(
            fs::write(action_dir.join("action.yml"), action_metadata()),
            "write runner action metadata",
        );
        must(
            fs::write(root.join("action.yml"), action_metadata()),
            "write scanned consumer action metadata",
        );
        write_nested_consumer_actions(&root);
        write_pinned_external_action(&root);

        let success_marker = root.join("success.log");
        let success_downstream = root.join("success.downstream");
        let success_output = root.join("success.output");
        let success = expanded_invocations(&root, "success", false, &success_marker);
        let repository = success
            .iter()
            .find_map(|invocation| match invocation {
                CompositeActionInvocation::Repository(plan) => Some(plan),
                _ => None,
            })
            .unwrap_or_else(|| panic!("runner graph lost external uses step"));
        assert_eq!(repository.repository, "octo/example");
        assert_eq!(
            repository.git_ref,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(repository.inputs.get("mode"), Some(&"success".to_owned()));
        assert_eq!(
            repository.env,
            vec![("ACTION_MODE".to_owned(), "success".to_owned())]
        );
        let outputs = success
            .iter()
            .find_map(|invocation| match invocation {
                CompositeActionInvocation::Outputs(outputs) => Some(outputs),
                _ => None,
            })
            .unwrap_or_else(|| panic!("runner graph lost action outputs"));
        assert_eq!(
            outputs.outputs.get("result").map(String::as_str),
            Some("${{ steps.consumer-downloader-run.outputs.result }}")
        );
        let mut success_outputs = BTreeMap::new();
        let success_result = execute_action_invocations(
            &root,
            "consumer",
            &success,
            &success_marker,
            &success_downstream,
            &success_output,
            &mut success_outputs,
        );
        assert!(success_result.succeeded());
        let success_log = must(fs::read_to_string(&success_marker), "read success marker");
        for stage in [
            "downloader",
            "validator",
            "hadolint",
            "buildx",
            "downstream",
        ] {
            assert!(
                success_log.contains(stage),
                "success action graph omitted {stage}: {success_log}"
            );
        }
        assert!(
            must(fs::read_to_string(&success_output), "read success output")
                .contains("result=downloaded")
        );
        assert_eq!(
            success_outputs
                .get("consumer")
                .and_then(|outputs| outputs.get("result"))
                .map(String::as_str),
            Some("downloaded")
        );
        let downstream_consumer = root.join("tests/downstream-output.sh");
        must(
            fs::write(
                &downstream_consumer,
                "set -euo pipefail\nprintf 'consumed=%s\\n' \"$CONSUMED\" >> \"$ACTION_MARKER\"\ntest \"$CONSUMED\" = downloaded\n",
            ),
            "write downstream output consumer",
        );
        let success_scope = ActionExecutionScope {
            step_outputs: success_outputs.clone(),
            ..ActionExecutionScope::default()
        };
        let consumed = render_action_value("${{ steps.consumer.outputs.result }}", &success_scope);
        let downstream_result = must(
            Command::new("bash")
                .args([
                    "-euo",
                    "pipefail",
                    downstream_consumer.to_string_lossy().as_ref(),
                ])
                .current_dir(&root)
                .env("ACTION_MARKER", &success_marker)
                .env("CONSUMED", consumed)
                .output(),
            "execute downstream output consumer",
        );
        assert!(
            downstream_result.status.success(),
            "downstream output consumer failed: {}",
            String::from_utf8_lossy(&downstream_result.stderr)
        );
        assert!(must(
            fs::read_to_string(&success_marker),
            "read downstream marker"
        )
        .contains("consumed=downloaded"));

        let external_marker = root.join("external.log");
        let external_output = root.join("external.output");
        let external = expanded_invocations(&root, "external", false, &external_marker);
        let external_plan = external
            .iter()
            .find_map(|invocation| match invocation {
                velnor_runner::action_contract::CompositeActionInvocation::Repository(plan)
                    if plan.repository == "octo/example" =>
                {
                    Some(plan)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("external runner repository plan missing"));
        assert_eq!(
            external_plan.condition.as_deref(),
            Some("${{ 'external' == 'external' }}")
        );
        let mut external_outputs = BTreeMap::new();
        let external_result = execute_action_invocations(
            &root,
            "consumer",
            &external,
            &external_marker,
            &root.join("external.downstream"),
            &external_output,
            &mut external_outputs,
        );
        assert!(external_result.succeeded());
        assert_eq!(
            external_outputs
                .get("consumer")
                .and_then(|outputs| outputs.get("nested-result"))
                .map(String::as_str),
            Some("external-value")
        );
        let external_scope_log = must(
            fs::read_to_string(&external_marker),
            "read nested-scope marker",
        );
        assert!(
            external_scope_log.contains("nested-output-visible"),
            "nested composite output did not reach the parent scope: {external_scope_log}"
        );
        assert!(
            !external_scope_log.contains("nested-output-leaked"),
            "nested composite child output leaked into its parent scope: {external_scope_log}"
        );
        let external_log = must(
            fs::read_to_string(root.join("action-consumer.log")),
            "read external repository action marker",
        );
        assert!(
            external_log.contains("external"),
            "external repository action did not execute: {external_log}"
        );

        for (mode, completed_stage) in [
            ("download-failure", "downloader"),
            ("validate-failure", "validator"),
            ("hadolint-failure", "hadolint"),
            ("buildx-failure", "buildx"),
            ("downstream-failure", "downstream"),
        ] {
            let marker = root.join(format!("{mode}.log"));
            let downstream = root.join(format!("{mode}.downstream"));
            let output_file = root.join(format!("{mode}.output"));
            let invocations = expanded_invocations(&root, mode, false, &marker);
            let mut outputs = BTreeMap::new();
            let result = execute_action_invocations(
                &root,
                "consumer",
                &invocations,
                &marker,
                &downstream,
                &output_file,
                &mut outputs,
            );
            assert!(!result.succeeded(), "{mode} failure must propagate");
            let log = must(fs::read_to_string(&marker), "read failure marker");
            assert!(
                log.contains(completed_stage),
                "{mode} did not reach expected stub: {log}"
            );
            if mode == "download-failure" {
                assert_eq!(
                    outputs
                        .get("consumer")
                        .and_then(|action_outputs| action_outputs.get("result"))
                        .map(String::as_str),
                    Some("downloaded"),
                    "composite output mapping must run after the failed child"
                );
            }
            if mode != "downstream-failure" {
                assert!(
                    !log.contains("downstream"),
                    "{mode} must prevent downstream work"
                );
                assert!(
                    !downstream.exists(),
                    "{mode} must prevent downstream marker"
                );
            }
        }

        let skip_marker = root.join("skip.log");
        let skip_downstream = root.join("skip.downstream");
        let skip_output = root.join("skip.output");
        let skip = expanded_invocations(&root, "success", true, &skip_marker);
        let skip_build = skip
            .iter()
            .filter_map(|invocation| match invocation {
                CompositeActionInvocation::Script(step) => Some(step),
                _ => None,
            })
            .find(|step| step.id.ends_with("-buildx"))
            .unwrap_or_else(|| panic!("runner graph lost conditional Buildx step"));
        let skip_scope = ActionExecutionScope::default();
        assert!(!condition_runs(
            skip_build.condition.as_deref(),
            &skip_scope
        ));
        let mut skip_outputs = BTreeMap::new();
        let skip_result = execute_action_invocations(
            &root,
            "consumer",
            &skip,
            &skip_marker,
            &skip_downstream,
            &skip_output,
            &mut skip_outputs,
        );
        assert!(skip_result.succeeded());
        let skip_log = must(fs::read_to_string(&skip_marker), "read skip marker");
        assert!(skip_log.contains("downloader"));
        assert!(skip_log.contains("validator"));
        assert!(skip_log.contains("downstream"));
        assert!(!skip_log.contains("hadolint"));
        assert!(!skip_log.contains("buildx"));
        let _ = fs::remove_dir_all(root);
    }
}

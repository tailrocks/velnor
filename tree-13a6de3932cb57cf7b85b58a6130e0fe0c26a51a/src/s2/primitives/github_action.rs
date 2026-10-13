//! Repository-owned consumer fixtures for generic GitHub Actions.
//!
//! The scanner owns metadata and local-entrypoint checks.  A repository owns
//! the way it consumes its action, so this primitive accepts only paths to
//! checked-in consumer fixtures.  It appends typed success, expected-failure,
//! and no-build invocations to the scanned action unit; it never embeds an
//! estate name or assumes a product-specific action API.  The fixtures are
//! action consumers: they invoke the scanned action and own the action's
//! `uses`, input, environment, output, and branch assertions.

use std::path::Path;

use super::{Args, Primitive, RenderCtx, Rendered, ACTION_FIXTURES};
use crate::s2::{GeneratorError, UnitKind};

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
        ]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let success = args.strings("success_fixtures")?.unwrap_or_default();
        let failure = args.strings("failure_fixtures")?.unwrap_or_default();
        let skip_build = args.strings("skip_build_fixtures")?.unwrap_or_default();
        let mut updates = Vec::new();
        for unit in ctx.units {
            if unit.kind != UnitKind::GithubAction {
                return Err(GeneratorError::usage(format!(
                    "unit `{}` is a {} unit, which `{ACTION_FIXTURES}` does not render",
                    unit.id,
                    unit.kind.label()
                )));
            }
            let mut update = (*unit).clone();
            for fixture in success.iter().chain(&failure).chain(&skip_build) {
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
        }
        Ok(Rendered {
            units: updates,
            ..Rendered::default()
        })
    }
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
    use crate::s2::scan::scan_shape;
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
    fn configured_consumer_fixtures_append_real_success_and_failure_commands() {
        let root = scratch("commands");
        must(
            fs::write(root.join("action.yml"), action_metadata()),
            "write action metadata",
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
        let providers: std::collections::BTreeSet<ProviderId> =
            ProviderId::ALL.into_iter().collect();
        let shape = must(
            scan_shape(&root, &providers, "main", &[]),
            "scan action fixture",
        );
        let config = ProjectConfig::from(shape.clone());
        let unit = config
            .units
            .iter()
            .find(|unit| unit.kind == UnitKind::GithubAction)
            .unwrap_or_else(|| panic!("action unit missing"));
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
        ]);
        let args = super::super::Args(&values);
        let units = [unit];
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
        let rendered = must(
            super::GithubActionFixtures.render(&ctx, &args),
            "render action fixtures",
        );
        let update = rendered
            .units
            .first()
            .unwrap_or_else(|| panic!("action fixture update missing"));
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
                    inputs: BTreeMap::new(),
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
    #[derive(Clone, Default)]
    struct LocalCompositeScope {
        condition: Option<String>,
        continue_on_error: bool,
    }

    type LocalCompositeScopes = BTreeMap<String, LocalCompositeScope>;

    #[derive(Default)]
    struct ActionExecutionScope {
        step_outputs: ActionStepOutputs,
        step_statuses: ActionStepStatuses,
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
        let Some(condition) = condition else {
            return must(
                velnor_runner::action_contract::evaluate_action_condition(
                    None,
                    &scope.step_outputs,
                    &scope.step_statuses,
                ),
                "evaluate default runner action condition",
            );
        };
        must(
            velnor_runner::action_contract::evaluate_action_condition(
                Some(condition),
                &scope.step_outputs,
                &scope.step_statuses,
            ),
            "evaluate runner action condition",
        )
    }

    fn render_action_value(value: &str, step_outputs: &ActionStepOutputs) -> String {
        must(
            velnor_runner::action_contract::render_action_expression(value, step_outputs),
            "evaluate runner action expression",
        )
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

        let contents = fs::read(step_file).map_err(|error| {
            format!(
                "read runner step output file {}: {error}",
                step_file.display()
            )
        })?;
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

    fn collect_local_composite_scopes(
        workspace: &Path,
        action_dir: &Path,
        scope_prefix: &str,
        scopes: &mut LocalCompositeScopes,
    ) {
        use velnor_runner::action_contract::parse_action_metadata;

        let metadata_path = action_metadata_path(action_dir);
        let metadata = must(
            fs::read_to_string(&metadata_path)
                .map_err(|error| format!("{}: {error}", metadata_path.display()))
                .and_then(|contents| {
                    parse_action_metadata(&contents)
                        .map_err(|error| format!("{}: {error}", metadata_path.display()))
                }),
            "read local composite scope metadata",
        );

        for (index, step) in metadata.runs.steps.iter().enumerate() {
            let Some(uses) = step.uses.as_deref() else {
                continue;
            };
            if !uses.starts_with('.') {
                continue;
            }
            let local_path = uses
                .strip_prefix("./")
                .or_else(|| uses.strip_prefix(".\\"))
                .unwrap_or(uses);
            if local_path.is_empty() {
                continue;
            }

            let local_scope_id = composite_step_id(scope_prefix, step.id.as_deref(), index);
            scopes.insert(
                local_scope_id.clone(),
                LocalCompositeScope {
                    condition: step.condition.clone(),
                    continue_on_error: step
                        .continue_on_error
                        .as_deref()
                        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true")),
                },
            );
            let action_dir = workspace.join(local_path.replace('\\', "/"));
            collect_local_composite_scopes(workspace, &action_dir, &local_scope_id, scopes);
        }
    }

    fn local_composite_scopes(
        workspace: &Path,
        action_dir: &Path,
        scope_prefix: &str,
    ) -> LocalCompositeScopes {
        let mut scopes = BTreeMap::new();
        collect_local_composite_scopes(workspace, action_dir, scope_prefix, &mut scopes);
        scopes
    }

    fn invocation_step_id(
        invocation: &velnor_runner::action_contract::CompositeActionInvocation,
    ) -> &str {
        use velnor_runner::action_contract::CompositeActionInvocation;

        match invocation {
            CompositeActionInvocation::Script(step) => &step.id,
            CompositeActionInvocation::Repository(plan) => &plan.step_id,
            CompositeActionInvocation::Outputs(outputs) => &outputs.step_id,
        }
    }

    fn local_scope_segment_at(
        invocations: &[velnor_runner::action_contract::CompositeActionInvocation],
        index: usize,
        action_step_id: &str,
        local_scopes: &LocalCompositeScopes,
    ) -> Option<(String, usize)> {
        let invocation_id = invocation_step_id(invocations.get(index)?);
        let scope_id = local_scopes
            .keys()
            .filter(|scope_id| scope_id.as_str() != action_step_id)
            .filter(|scope_id| {
                invocation_id == scope_id.as_str()
                    || invocation_id
                        .strip_prefix(scope_id.as_str())
                        .is_some_and(|suffix| suffix.starts_with('-'))
            })
            .min_by_key(|scope_id| scope_id.len())?
            .clone();

        let mut end = index;
        while let Some(invocation) = invocations.get(end) {
            let invocation_id = invocation_step_id(invocation);
            if invocation_id == scope_id
                || invocation_id
                    .strip_prefix(&scope_id)
                    .is_some_and(|suffix| suffix.starts_with('-'))
            {
                end += 1;
            } else {
                break;
            }
        }
        Some((scope_id, end))
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

    fn execute_action_invocations(
        root: &Path,
        action_step_id: &str,
        invocations: &[velnor_runner::action_contract::CompositeActionInvocation],
        marker: &Path,
        downstream: &Path,
        output_file: &Path,
        step_outputs: &mut ActionStepOutputs,
    ) -> CompositeExecutionResult {
        let action_dir = root.join(".github/actions").join(action_step_id);
        let local_scopes = local_composite_scopes(root, &action_dir, action_step_id);
        let mut result = execute_composite_scope(
            root,
            action_step_id,
            invocations,
            marker,
            downstream,
            output_file,
            &local_scopes,
        );
        if let Some(outputs) = result.outputs.take() {
            step_outputs.insert(action_step_id.to_owned(), outputs);
        }
        result
    }

    fn execute_composite_scope(
        root: &Path,
        action_step_id: &str,
        invocations: &[velnor_runner::action_contract::CompositeActionInvocation],
        marker: &Path,
        downstream: &Path,
        output_file: &Path,
        local_scopes: &LocalCompositeScopes,
    ) -> CompositeExecutionResult {
        use velnor_runner::action_contract::{
            parse_action_metadata, CompositeActionInvocation, ResolvedAction,
        };

        let mut scope = ActionExecutionScope::default();
        let mut failed_output = None;
        let mut mapped_outputs = None;
        let mut index = 0;
        while index < invocations.len() {
            if let Some((local_scope_id, end)) =
                local_scope_segment_at(invocations, index, action_step_id, local_scopes)
            {
                let local_scope = local_scopes
                    .get(&local_scope_id)
                    .cloned()
                    .unwrap_or_default();
                if !condition_runs(local_scope.condition.as_deref(), &scope) {
                    scope
                        .step_outputs
                        .insert(local_scope_id.clone(), BTreeMap::new());
                    record_step_status(
                        &mut scope,
                        &local_scope_id,
                        0,
                        true,
                        local_scope.continue_on_error,
                    );
                    index = end;
                    continue;
                }
                let nested_result = execute_composite_scope(
                    root,
                    &local_scope_id,
                    &invocations[index..end],
                    marker,
                    downstream,
                    output_file,
                    local_scopes,
                );
                let failed = !nested_result.succeeded();
                let exit_code = nested_result.exit_code();
                let continue_on_error = local_scope.continue_on_error;
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
                    if !condition_runs(step.condition.as_deref(), &scope) {
                        record_step_status(&mut scope, &step.id, 0, true, false);
                        index += 1;
                        continue;
                    }
                    let script = render_action_value(&step.script, &scope.step_outputs);
                    let step_output_file =
                        output_file_for_step(output_file, action_step_id, &step.id, index);
                    must(
                        fs::write(&step_output_file, ""),
                        "initialize runner output file",
                    );
                    let mut command = Command::new("bash");
                    command
                        .args(["-euo", "pipefail", "-c", script.as_str()])
                        .current_dir(root)
                        .env("ACTION_MARKER", marker)
                        .env("DOWNSTREAM_MARKER", downstream)
                        .env("GITHUB_OUTPUT", &step_output_file);
                    for (name, value) in &step.env {
                        command.env(name, render_action_value(value, &scope.step_outputs));
                    }
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
                    if !condition_runs(plan.condition.as_deref(), &scope) {
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
                        local_composite_scopes(root, &plan.action_dir, &plan.step_id);
                    let nested_result = execute_composite_scope(
                        root,
                        &plan.step_id,
                        &nested,
                        marker,
                        downstream,
                        output_file,
                        &nested_local_scopes,
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
                        .map(|(name, value)| {
                            (
                                name.clone(),
                                render_action_value(value, &scope.step_outputs),
                            )
                        })
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
    value: ${{ steps.hidden.outputs.result }}
runs:
  using: composite
  steps:
    - id: hidden
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
    - id: child-output-must-stay-private
      if: ${{ steps.local-scope-nested-hidden.outputs.result == 'nested-value' }}
      shell: bash
      run: printf 'child-output-leaked\n' >> "$ACTION_MARKER"
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
            !log.contains("child-output-leaked"),
            "nested child output leaked into its caller's steps scope: {log}"
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
        let consumed =
            render_action_value("${{ steps.consumer.outputs.result }}", &success_outputs);
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

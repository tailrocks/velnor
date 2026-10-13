//! Repository-owned consumer fixtures for generic GitHub Actions.
//!
//! The scanner owns metadata and local-entrypoint checks.  A repository owns
//! the way it consumes its action, so this primitive accepts only paths to
//! checked-in shell fixtures.  It appends typed success and expected-failure
//! invocations to the scanned action unit; it never embeds an estate name or
//! assumes a product-specific action API.

use std::path::Path;

use super::{Args, Primitive, RenderCtx, Rendered, ACTION_FIXTURES};
use crate::s2::{GeneratorError, UnitKind};

pub(crate) struct GithubActionFixtures;

impl Primitive for GithubActionFixtures {
    fn id(&self) -> &'static str {
        ACTION_FIXTURES
    }

    fn schema(&self) -> &'static [&'static str] {
        &["success_fixtures", "failure_fixtures"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let success = args.strings("success_fixtures")?.unwrap_or_default();
        let failure = args.strings("failure_fixtures")?.unwrap_or_default();
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
            for fixture in success.iter().chain(&failure) {
                let path = fixture_path(ctx, fixture)?;
                if !update.watch.contains(&path) {
                    update.watch.push(path);
                }
            }
            append_success_commands(&mut update.pr_commands, &success, ctx)?;
            append_success_commands(&mut update.full_commands, &success, ctx)?;
            append_expected_failure_commands(&mut update.pr_commands, &failure, ctx)?;
            append_expected_failure_commands(&mut update.full_commands, &failure, ctx)?;
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
            fs::write(
                root.join("action.yml"),
                "runs:\n  using: composite\n  steps:\n    - shell: bash\n      run: echo action\n",
            ),
            "write action metadata",
        );
        must(
            fs::write(root.join("tests/success.sh"), "exit 0\n"),
            "write success fixture",
        );
        must(
            fs::write(root.join("tests/failure.sh"), "exit 7\n"),
            "write failure fixture",
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
            .any(|command| command == "bash -- 'tests/success.sh'"));
        assert!(update
            .pr_commands
            .iter()
            .any(|command| command.starts_with("if bash -- 'tests/failure.sh'; then")));
        assert!(update.watch.iter().any(|path| path == "tests/success.sh"));
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
}

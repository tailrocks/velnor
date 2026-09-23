//! Dispatch the sole workflow-generator pipeline.
//!
//! Schema 2 owns scanning, typed provider routing, and rendering for every
//! generator invocation. The only command left outside this module is the
//! non-generator `visibility` evidence command, which belongs to the
//! repository-level evidence boundary. In particular, target shape or the
//! presence of a generation config must never select an older runner model.

use std::env;
use std::ffi::OsString;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

/// Runtime subcommands owned by schema 2. Kept in the test-only dispatch
/// probe below to document the command boundary without making it a second
/// runtime router.
#[cfg(test)]
const RUNTIME_COMMANDS: &[&str] = &[
    "plan",
    "run",
    "test-crates",
    "policy",
    "release",
    "version",
    "closure",
    "prepared-tool-install",
    "stage-product",
    "verify-product",
    "cache-plan",
    "aggregate",
    "select",
    "fingerprint",
    "reuse-decision",
];

/// Run schema 2 for every workflow-generator/runtime invocation.
///
/// `visibility` is evidence plumbing, not workflow generation, so it remains
/// owned by the repository-level command parser. No target or option may
/// reach the legacy `Github`/`Velnor`/`Both` generator path.
pub(crate) fn run_if_s2() -> Option<Result<(), crate::GeneratorError>> {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    if arguments
        .first()
        .and_then(|value| value.to_str())
        .is_some_and(|command| command == "visibility")
    {
        return None;
    }
    // Both error types carry a single message string, so the boundary maps
    // without losing context.
    Some(super::run_from_env().map_err(|error| crate::GeneratorError::usage(error.to_string())))
}

// The functions below retain the old routing probe only for the historical
// unit tests in this module. They are not called by `run_if_s2`; production
// dispatch no longer consults target schema, provider flags, or legacy lane
// names to choose an executable pipeline.
#[cfg(test)]
fn wants_s2(arguments: &[OsString]) -> bool {
    // `visibility` is schema-agnostic evidence plumbing; it always stays on
    // the schema-1 path, which owns the subcommand for both pipelines.
    if arguments
        .first()
        .and_then(|value| value.to_str())
        .is_some_and(|command| command == "visibility")
    {
        return false;
    }
    if has_provider_selection_flag(arguments) {
        return true;
    }
    if let Some(command) = arguments.first().and_then(|value| value.to_str())
        && RUNTIME_COMMANDS.contains(&command)
    {
        return wants_s2_runtime(command, arguments);
    }
    wants_s2_generator(arguments)
}

#[cfg(test)]
fn has_provider_selection_flag(arguments: &[OsString]) -> bool {
    arguments.iter().any(|argument| {
        let text = argument.to_string_lossy();
        text == "--providers"
            || text.starts_with("--providers=")
            || text == "--provider-mode"
            || text.starts_with("--provider-mode=")
    })
}

/// Runtime subcommands operate on the working directory (or the policy
/// `--workflow-root`), never on a generator target. `version` and `closure`
/// print build constants shared by both pipelines, so they stay put.
#[cfg(test)]
fn wants_s2_runtime(command: &str, arguments: &[OsString]) -> bool {
    if command == "version" || command == "closure" {
        return false;
    }
    if command == "policy"
        && let Some(root) = workflow_root_argument(arguments)
        && dir_is_schema2(&root)
    {
        return true;
    }
    env::current_dir().is_ok_and(|root| dir_is_schema2(&root))
}

/// Generator invocations route on the resolved target: a local directory
/// whose generation config declares `schema = 2`. Anything else (a remote
/// target, an unparsable command line) stays on the schema-1 path, which
/// reports the real error.
#[cfg(test)]
fn wants_s2_generator(arguments: &[OsString]) -> bool {
    let Ok(cli) = crate::Cli::parse_args(arguments.to_vec()) else {
        return false;
    };
    let candidate = PathBuf::from(&cli.target);
    let Ok(source) = crate::RepositorySource::parse_with_candidate_path(&cli.target, candidate)
    else {
        return false;
    };
    match source {
        crate::RepositorySource::Local(path) => dir_is_schema2(&path),
        crate::RepositorySource::GitHub { .. } => false,
    }
}

/// The policy `--workflow-root` value in either `--flag value` or
/// `--flag=value` form, mirroring `policy::run_cli`.
#[cfg(test)]
fn workflow_root_argument(arguments: &[OsString]) -> Option<PathBuf> {
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy().into_owned();
        if let Some(value) = argument.strip_prefix("--workflow-root=") {
            return Some(PathBuf::from(value));
        }
        if argument == "--workflow-root" {
            return arguments.get(index + 1).map(PathBuf::from);
        }
        index += 1;
    }
    None
}

/// Whether `dir` carries a generation config declaring `schema = 2`. A
/// missing or unparsable config is not schema 2; the pipelines' own schema
/// gates report the real error.
pub(crate) fn dir_is_schema2(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(dir.join(".github-gen/velnor-workflow.toml")) else {
        return false;
    };
    match text.parse::<toml::Table>() {
        Ok(table) => table.get("schema").and_then(toml::Value::as_integer) == Some(2),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn fixture_dir(name: &str, config: Option<&str>) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "velnor-r2-dispatch-{}-{name}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create dispatch fixture",
        );
        if let Some(config) = config {
            must(
                std::fs::write(root.join(".github-gen/velnor-workflow.toml"), config),
                "write dispatch fixture config",
            );
        }
        root
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn providers_flag_routes_schema2_without_a_target() {
        assert!(wants_s2(&args(&["--providers", "github-hosted"])));
        assert!(wants_s2(&args(&["--providers=github-hosted"])));
    }

    #[test]
    fn provider_mode_flag_routes_schema2_without_a_target() {
        assert!(wants_s2(&args(&["--provider-mode", "native-only"])));
        assert!(wants_s2(&args(&["--provider-mode=scale-set-only"])));
    }

    #[test]
    fn legacy_flag_routes_schema2_for_a_local_schema2_target() {
        let root = fixture_dir(
            "legacy-flag-schema2",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        // Target detection keeps the typed parser in control; it then rejects
        // the unknown schema-1 option fail-closed. Do not depend on the cargo
        // test harness CWD being the repository root.
        assert!(wants_s2(&args(&[target.as_str(), "--runners", "both"])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_schema2_target_routes_schema2() {
        let root = fixture_dir(
            "target",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        assert!(wants_s2(&args(&[target.as_str(), "--plain"])));
        assert!(wants_s2(&args(&["generate", target.as_str()])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_schema1_target_stays_schema1() {
        let root = fixture_dir(
            "target1",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        assert!(!wants_s2(&args(&[target.as_str(), "--plain"])));
        assert!(!wants_s2(&args(&[target.as_str(), "--runners", "both"])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_config_stays_schema1() {
        let root = fixture_dir("noconfig", None);
        let target = root.to_string_lossy().into_owned();
        assert!(!wants_s2(&args(&[target.as_str()])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn remote_target_stays_schema1_without_the_flag() {
        assert!(!wants_s2(&args(&["example/fixture", "--plain"])));
        assert!(!wants_s2(&args(&[
            "https://github.com/example/fixture",
            "--plain"
        ])));
    }

    #[test]
    fn policy_routes_on_workflow_root() {
        let root = fixture_dir(
            "policy",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let path = root.to_string_lossy().into_owned();
        assert!(wants_s2(&args(&[
            "policy",
            "--workflow-root",
            path.as_str()
        ])));
        assert!(wants_s2(&args(&[
            "policy",
            format!("--workflow-root={path}").as_str()
        ])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn version_and_closure_stay_put() {
        assert!(!wants_s2(&args(&["version"])));
        assert!(!wants_s2(&args(&["closure"])));
    }

    #[test]
    fn product_transport_subcommands_route_as_runtime_commands() {
        // Membership pins the bridge peek: without it the schema-1 parser
        // would reject the transport subcommands before `try_run` sees them.
        for command in ["stage-product", "verify-product"] {
            assert!(
                RUNTIME_COMMANDS.contains(&command),
                "{command} routes as a binary-only runtime command"
            );
        }
    }
}

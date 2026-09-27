//! Schema dispatch for the R2 bridge release.
//!
//! The bridge binary serves two configuration schemas: schema 1 renders
//! through the original pipeline in the crate root, schema 2 through the
//! provider pipeline in [`crate::s2`]. This module peeks at the invocation
//! (an explicit `--providers` flag, or the target's own
//! `.github-gen/velnor-workflow.toml`) and routes before either parser runs.
//!
//! The peek is fail-closed in both directions: a misrouted invocation still
//! hits the destination pipeline's strict schema gate, which rejects the
//! foreign schema with a usage error instead of rendering it.

use std::env;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// Binary-only subcommands, mirroring `runtime::try_run` plus the reuse
/// slice it falls through to. These never take a generator target, so the
/// peek roots them at the policy `--workflow-root` or the working directory.
const RUNTIME_COMMANDS: &[&str] = &[
    "plan",
    "run",
    "verify-action",
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

/// Run the schema-2 pipeline when this invocation targets it, else yield to
/// the schema-1 path. `None` means "not a schema-2 invocation".
pub(crate) fn run_if_s2() -> Option<Result<(), crate::GeneratorError>> {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    let routes_to_s2 = match wants_s2(&arguments) {
        Ok(routes_to_s2) => routes_to_s2,
        Err(error) => return Some(Err(error)),
    };
    if routes_to_s2 {
        // Both error types carry a single message string, so the bridge maps
        // across the pipeline boundary without losing context.
        Some(super::run_from_env().map_err(|error| crate::GeneratorError::usage(error.to_string())))
    } else {
        None
    }
}

fn wants_s2(arguments: &[OsString]) -> Result<bool, crate::GeneratorError> {
    // `visibility` is schema-agnostic evidence plumbing; it always stays on
    // the schema-1 path, which owns the subcommand for both pipelines.
    if arguments
        .first()
        .and_then(|value| value.to_str())
        .is_some_and(|command| command == "visibility")
    {
        return Ok(false);
    }
    if has_providers_flag(arguments) {
        return Ok(true);
    }
    if let Some(command) = arguments.first().and_then(|value| value.to_str())
        && RUNTIME_COMMANDS.contains(&command)
    {
        return wants_s2_runtime(command, arguments);
    }
    wants_s2_generator(arguments)
}

fn has_providers_flag(arguments: &[OsString]) -> bool {
    arguments.iter().any(|argument| {
        let text = argument.to_string_lossy();
        text == "--providers" || text.starts_with("--providers=")
    })
}

/// Runtime subcommands operate on the working directory (or the policy
/// `--workflow-root`), never on a generator target. `version` and `closure`
/// print build constants shared by both pipelines, so they stay put.
fn wants_s2_runtime(command: &str, arguments: &[OsString]) -> Result<bool, crate::GeneratorError> {
    if command == "version" || command == "closure" {
        return Ok(false);
    }
    if command == "policy"
        && let Some(root) = workflow_root_argument(arguments)
        && try_dir_is_schema2(&root)?
    {
        return Ok(true);
    }
    match env::current_dir() {
        Ok(root) => try_dir_is_schema2(&root),
        Err(_) => Ok(false),
    }
}

/// Generator invocations route on the resolved target: a local directory
/// whose generation config declares `schema = 2`. Anything else (a remote
/// target, an unparsable command line) stays on the schema-1 path, which
/// reports the real error.
fn wants_s2_generator(arguments: &[OsString]) -> Result<bool, crate::GeneratorError> {
    let Ok(cli) = crate::Cli::parse_args(arguments.to_vec()) else {
        return Ok(false);
    };
    let candidate = PathBuf::from(&cli.target);
    let Ok(source) = crate::RepositorySource::parse_with_candidate_path(&cli.target, candidate)
    else {
        return Ok(false);
    };
    match source {
        crate::RepositorySource::Local(path) => try_dir_is_schema2(&path),
        crate::RepositorySource::GitHub { .. } => Ok(false),
    }
}

/// The policy `--workflow-root` value in either `--flag value` or
/// `--flag=value` form, mirroring `policy::run_cli`.
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
/// gates report the real error. Full repository preflight belongs to the
/// selected pipeline, after routing.
pub(crate) fn dir_is_schema2(dir: &Path) -> bool {
    // Promotion still has a boolean routing API. An invalid config path must
    // take the strict S2 path so it cannot fall through to the legacy renderer.
    // The selected pipeline owns full physical-tree preflight.
    try_dir_is_schema2(dir).unwrap_or(true)
}

fn try_dir_is_schema2(dir: &Path) -> Result<bool, crate::GeneratorError> {
    validate_workflow_root_path(dir)?;
    let config_path = dir.join(super::config::GENERATION_CONFIG_PATH);
    validate_generation_config_path(dir, &config_path)?;
    let text = match std::fs::read_to_string(&config_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(crate::GeneratorError::io(
                "read generation config",
                &config_path,
                &error,
            ));
        }
    };
    match text.parse::<toml::Table>() {
        Ok(table) => Ok(table.get("schema").and_then(toml::Value::as_integer) == Some(2)),
        Err(_) => Ok(false),
    }
}

/// Reject symlinks in the supplied root path before reading its routing
/// config. This inspects only path ancestors; repository contents remain the
/// selected pipeline's responsibility.
fn validate_workflow_root_path(root: &Path) -> Result<(), crate::GeneratorError> {
    let absolute = super::scan::file_walk::absolute_normalized_path(root)
        .map_err(|error| crate::GeneratorError::usage(error.to_string()))?;
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                current.pop();
                continue;
            }
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(crate::GeneratorError::usage(format!(
                    "refusing symlinked workflow root path: {}",
                    current.display()
                )));
            }
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(crate::GeneratorError::usage(format!(
                    "workflow root path component is not a directory: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(crate::GeneratorError::io(
                    "inspect workflow root path",
                    &current,
                    &error,
                ));
            }
        }
    }
    Ok(())
}

/// Validate only the generation config's path components and type before the
/// dispatch peek reads it. This file is a routing authority and must be a
/// regular file at its declared path; unrelated repository entries are left
/// for the selected pipeline's full preflight.
fn validate_generation_config_path(
    root: &Path,
    config_path: &Path,
) -> Result<(), crate::GeneratorError> {
    for (path, expected_type) in [
        (root.join(".github-gen"), "directory"),
        (config_path.to_path_buf(), "regular file"),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(crate::GeneratorError::usage(format!(
                    "refusing symlinked generation config path: {}",
                    path.display()
                )));
            }
            Ok(metadata)
                if (expected_type == "directory" && !metadata.file_type().is_dir())
                    || (expected_type == "regular file" && !metadata.file_type().is_file()) =>
            {
                return Err(crate::GeneratorError::usage(format!(
                    "generation config path must be a {expected_type}: {}",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(crate::GeneratorError::io(
                    "inspect generation config path",
                    &path,
                    &error,
                ));
            }
        }
    }
    Ok(())
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

    #[expect(
        clippy::panic,
        reason = "tests need an explicit failure when an expected error is absent"
    )]
    fn must_fail<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}: expected an error"),
            Err(error) => error,
        }
    }

    fn fixture_dir(name: &str, config: Option<&str>) -> PathBuf {
        let temp = must(
            std::fs::canonicalize(env::temp_dir()),
            "resolve dispatch temporary directory",
        );
        let root = temp.join(format!(
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

    fn short_fixture_dir(_name: &str, config: Option<&str>) -> PathBuf {
        let temp = must(
            std::fs::canonicalize("/tmp"),
            "resolve short temporary directory",
        );
        let root = temp.join(format!("vwd-{}", crate::unique_suffix()));
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create short dispatch fixture",
        );
        if let Some(config) = config {
            must(
                std::fs::write(root.join(".github-gen/velnor-workflow.toml"), config),
                "write short dispatch fixture config",
            );
        }
        root
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn routes_to_s2(arguments: &[OsString]) -> bool {
        must(wants_s2(arguments), "schema dispatch preflight")
    }

    #[test]
    fn providers_flag_routes_schema2_without_a_target() {
        assert!(routes_to_s2(&args(&["--providers", "github-hosted"])));
        assert!(routes_to_s2(&args(&["--providers=github-hosted"])));
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
        assert!(routes_to_s2(&args(&[target.as_str(), "--runners", "both"])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_schema2_target_routes_schema2() {
        let root = fixture_dir(
            "target",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        assert!(routes_to_s2(&args(&[target.as_str(), "--plain"])));
        assert!(routes_to_s2(&args(&["generate", target.as_str()])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn raw_tmp_alias_routes_schema1_after_normalization() {
        let root = short_fixture_dir(
            "raw-tmp-schema1",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let raw_tmp_root = Path::new("/tmp").join(root.file_name().unwrap_or_default());
        assert!(
            !must(
                try_dir_is_schema2(&raw_tmp_root),
                "macOS /tmp alias must normalize before root validation"
            ),
            "ordinary schema-1 repository remains on schema 1 through /tmp"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn raw_var_alias_routes_schema1_after_normalization() {
        let root = fixture_dir(
            "raw-var-schema1",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let private_var = Path::new("/private/var");
        let suffix = must(
            root.strip_prefix(private_var),
            "macOS temporary fixture must be below /private/var",
        );
        let raw_var_root = Path::new("/var").join(suffix);
        assert!(
            !must(
                try_dir_is_schema2(&raw_var_root),
                "macOS /var alias must normalize before root validation"
            ),
            "ordinary schema-1 repository remains on schema 1 through /var"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn schema2_dispatch_ignores_unrelated_special_files_and_dangling_links() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let root = short_fixture_dir(
            "unrelated-physical-entries",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let listener = must(
            UnixListener::bind(root.join("unrelated-socket")),
            "create unrelated special file",
        );
        must(
            symlink("missing-target", root.join("unrelated-dangling-link")),
            "create unrelated dangling symlink",
        );

        let target = root.to_string_lossy().into_owned();
        assert!(
            routes_to_s2(&args(&[target.as_str()])),
            "dispatch reads the schema config without walking unrelated entries"
        );
        assert!(
            crate::s2::scan::file_walk::validate_repository_tree(&root).is_err(),
            "the S2 pipeline still owns and rejects full-tree preflight failures"
        );

        drop(listener);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_and_ancestor_fail_before_schema1_dispatch() {
        use std::os::unix::fs::symlink;

        let root = short_fixture_dir(
            "schema1-root",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        assert!(
            !must(try_dir_is_schema2(&root), "real schema-1 path is readable"),
            "control path is an ordinary schema-1 directory"
        );

        let name = root.file_name().unwrap_or_default();
        let parent = root.parent().unwrap_or(Path::new("/"));
        let root_link = parent.join(format!("vw-dsp-{}-root-link", crate::unique_suffix()));
        must(symlink(&root, &root_link), "create symlinked workflow root");
        let parent_link = parent.join(format!("vw-dsp-{}-parent-link", crate::unique_suffix()));
        must(
            symlink(parent, &parent_link),
            "create symlinked workflow ancestor",
        );
        let ancestor_linked_root = parent_link.join(name);

        for (path, label) in [
            (&root_link, "root symlink"),
            (&ancestor_linked_root, "ancestor symlink"),
        ] {
            let error = must_fail(
                try_dir_is_schema2(path),
                &format!("{label} must fail before routing"),
            );
            assert!(
                error
                    .to_string()
                    .contains("symlinked repository root component"),
                "{label} is named in the refusal: {error}"
            );
            let target = path.to_string_lossy().into_owned();
            let error = must_fail(
                wants_s2(&args(&[target.as_str()])),
                &format!("{label} must not fall through to schema 1"),
            );
            assert!(
                error
                    .to_string()
                    .contains("symlinked repository root component"),
                "dispatch preserves the root-path refusal: {error}"
            );
        }

        let _ = std::fs::remove_file(root_link);
        let _ = std::fs::remove_file(parent_link);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_component_before_parent_reference_fails_dispatch() {
        use std::os::unix::fs::symlink;

        let root = fixture_dir("symlink-before-parent", None);
        let repository = root.join("repo");
        must(
            std::fs::create_dir_all(repository.join(".github-gen")),
            "create routed repository",
        );
        must(
            std::fs::write(
                repository.join(".github-gen/velnor-workflow.toml"),
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n",
            ),
            "write routed repository config",
        );
        let link = root.join("link");
        must(
            symlink(root.join("outside"), &link),
            "create path component symlink",
        );
        let aliased_root = link.join("..").join("repo");

        let error = must_fail(
            try_dir_is_schema2(&aliased_root),
            "dispatcher must inspect symlinks before collapsing parent components",
        );
        assert!(
            error
                .to_string()
                .contains("symlinked repository root component"),
            "error identifies the path component: {error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_generation_config_is_rejected_without_reading_it() {
        use std::os::unix::net::UnixListener;

        let root = short_fixture_dir("special-config", None);
        let config_path = root.join(super::super::config::GENERATION_CONFIG_PATH);
        let listener = must(
            UnixListener::bind(&config_path),
            "create special generation config",
        );
        let error = must_fail(
            try_dir_is_schema2(&root),
            "special generation config must be rejected",
        );
        assert!(
            error.to_string().contains("regular file"),
            "error identifies the config file type: {error}"
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_schema1_target_stays_schema1() {
        let root = fixture_dir(
            "target1",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        assert!(!routes_to_s2(&args(&[target.as_str(), "--plain"])));
        assert!(!routes_to_s2(&args(&[
            target.as_str(),
            "--runners",
            "both"
        ])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_config_stays_schema1() {
        let root = fixture_dir("noconfig", None);
        let target = root.to_string_lossy().into_owned();
        assert!(!routes_to_s2(&args(&[target.as_str()])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_generation_config_fails_dispatch_preflight() {
        use std::os::unix::fs::symlink;

        let root = fixture_dir("symlinked-config", None);
        let source = root.join("config-source.toml");
        let config_path = root.join(crate::s2::config::GENERATION_CONFIG_PATH);
        must(
            std::fs::write(
                &source,
                "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n",
            ),
            "write symlink target",
        );
        must(symlink(&source, &config_path), "create symlinked config");

        let error = must_fail(
            try_dir_is_schema2(&root),
            "symlinked config must be rejected",
        );
        assert!(
            error
                .to_string()
                .contains("symlinked generation config path"),
            "error identifies the routing path: {error}"
        );
        let target = root.to_string_lossy().into_owned();
        let error = must_fail(
            wants_s2(&args(&[target.as_str()])),
            "dispatch must propagate the preflight failure",
        );
        assert!(
            error
                .to_string()
                .contains("symlinked generation config path"),
            "dispatch preserves the preflight error: {error}"
        );
        assert!(
            dir_is_schema2(&root),
            "the boolean promotion facade routes invalid trees to strict S2"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn remote_target_stays_schema1_without_the_flag() {
        assert!(!routes_to_s2(&args(&["example/fixture", "--plain"])));
        assert!(!routes_to_s2(&args(&[
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
        assert!(routes_to_s2(&args(&[
            "policy",
            "--workflow-root",
            path.as_str()
        ])));
        assert!(routes_to_s2(&args(&[
            "policy",
            format!("--workflow-root={path}").as_str()
        ])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn version_and_closure_stay_put() {
        assert!(!routes_to_s2(&args(&["version"])));
        assert!(!routes_to_s2(&args(&["closure"])));
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

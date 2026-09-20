//! The sole typed CLI entrypoint. It captures a repository root before
//! dispatch and sends every command through the schema-2 parser.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::safe_fs::SafeRoot;
use super::GeneratorSelection;

/// Binary-only subcommands, mirrored by the captured-root runtime dispatcher.
/// These never take a generator target, so dispatch roots them at the policy
/// root precedence or the working directory.
const RUNTIME_COMMANDS: &[&str] = &[
    "plan",
    "run",
    "test-crates",
    "policy",
    "release",
    "version",
    "closure",
    "prepared-tool-install",
    "cache-plan",
    "aggregate",
    "select",
    "fingerprint",
    "reuse-decision",
];

/// Run a typed schema-2 command. Every repository-bound command requires an
/// explicit schema-2 config and carries the captured root into its consumer.
pub(crate) fn run_from_env() -> Result<(), crate::GeneratorError> {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    finish_dispatch(&arguments, dispatch_decision(&arguments))
}

fn finish_dispatch(
    arguments: &[OsString],
    decision: DispatchDecision,
) -> Result<(), crate::GeneratorError> {
    match decision {
        DispatchDecision::Help(message) => {
            print!("{message}");
            Ok(())
        }
        DispatchDecision::Reject(error) => Err(crate::GeneratorError::usage(error)),
        DispatchDecision::Schema2Policy(root) => {
            super::runtime::run_policy_with_safe_root(&arguments[1..], root)
        }
        DispatchDecision::Schema2Runtime(root) => {
            super::runtime::run_with_safe_root(arguments, root)
        }
        DispatchDecision::Schema2Closure(root) => {
            super::runtime::run_closure_with_safe_root(&arguments[1..], root)
        }
        DispatchDecision::Schema2Promote {
            request,
            repository,
            generator_repository,
        } => {
            let report = super::promote::run_promote_with_safe_roots(
                &request,
                repository,
                generator_repository,
            )?;
            print!("{}", super::promote::render_report(&report));
            Ok(())
        }
        DispatchDecision::Schema2(selection) => super::run_from_env_with_selection(selection),
        DispatchDecision::BuildMetadata(value) => {
            println!("{value}");
            Ok(())
        }
        DispatchDecision::RootlessRuntime => super::runtime::run_rootless_command(arguments),
    }
}

#[cfg(test)]
fn is_supported_invocation(arguments: &[OsString]) -> bool {
    !matches!(dispatch_decision(arguments), DispatchDecision::Reject(_))
}

enum DispatchDecision {
    Schema2(GeneratorSelection),
    Schema2Policy(SafeRoot),
    Schema2Runtime(SafeRoot),
    Schema2Closure(SafeRoot),
    Schema2Promote {
        request: super::promote::PromoteRequest,
        repository: SafeRoot,
        generator_repository: Option<SafeRoot>,
    },
    RootlessRuntime,
    BuildMetadata(String),
    Help(String),
    Reject(String),
}

fn dispatch_decision(arguments: &[OsString]) -> DispatchDecision {
    if arguments.first().and_then(|argument| argument.to_str()) == Some("promote") {
        return promote_dispatch_decision(&arguments[1..]);
    }
    // Runtime verbs own their command grammar and root precedence. Parse them
    // before generator options so an option value such as `--providers` can
    // never steal a policy invocation away from its workflow-root schema gate.
    if let Some(command) = arguments.first().and_then(|value| value.to_str())
        && RUNTIME_COMMANDS.contains(&command)
    {
        if command == "version" {
            return DispatchDecision::RootlessRuntime;
        }
        if command == "policy" {
            let root = match super::policy::workflow_root_for_dispatch(&arguments[1..]) {
                Ok(root) => root,
                Err(error) => return DispatchDecision::Reject(error.to_string()),
            };
            return match schema2_safe_root(&root) {
                Ok(Some(root)) => DispatchDecision::Schema2Policy(root),
                Ok(None) => reject_non_schema2_root(&root),
                Err(error) => DispatchDecision::Reject(error),
            };
        }
        let root = if command == "closure" {
            match closure_repository_root(&arguments[1..]) {
                Ok(root) => root,
                Err(error) => return DispatchDecision::Reject(error),
            }
        } else {
            // `.` is resolved by the kernel against the process's already-open
            // CWD. Resolving CWD to a pathname here and opening that pathname
            // later would let a rename-and-replace race route dispatch to a new
            // tree at the old name.
            PathBuf::from(".")
        };
        return runtime_dispatch_decision(command, &root);
    }
    if let Some(value) = build_metadata_value(arguments) {
        return DispatchDecision::BuildMetadata(value);
    }
    generator_dispatch_decision(arguments)
}

fn promote_dispatch_decision(arguments: &[OsString]) -> DispatchDecision {
    let parsed = match super::promote::parse_request(arguments) {
        Ok(parsed) => parsed,
        Err(error) => return DispatchDecision::Reject(error.to_string()),
    };
    let repository = match schema2_safe_root(&parsed.repository) {
        Ok(Some(root)) => root,
        Ok(None) => return reject_non_schema2_root(&parsed.repository),
        Err(error) => return DispatchDecision::Reject(error),
    };
    let generator_repository = match parsed.generator_repository {
        Some(path) => match SafeRoot::open(&path) {
            Ok(root) => Some(root),
            Err(error) => return DispatchDecision::Reject(error.to_string()),
        },
        None => None,
    };
    DispatchDecision::Schema2Promote {
        request: parsed.request,
        repository,
        generator_repository,
    }
}

fn build_metadata_value(arguments: &[OsString]) -> Option<String> {
    // Build metadata is a rootless query. Keep the parser's exclusive-option
    // rule for combinations with other switches, but let one ordinary target
    // token accompany the query without attempting to parse or probe it.
    let metadata = arguments
        .iter()
        .enumerate()
        .filter(|(_, argument)| {
            argument.as_os_str() == "--revision" || argument.as_os_str() == "--closure"
        })
        .collect::<Vec<_>>();
    if let [(metadata_index, metadata_argument)] = metadata.as_slice() {
        let other_arguments = arguments
            .iter()
            .enumerate()
            .filter(|(index, _)| index != metadata_index)
            .map(|(_, argument)| argument);
        let mut target = None;
        let mut has_conflict = false;
        for argument in other_arguments {
            if argument.as_os_str().as_encoded_bytes().first() == Some(&b'-')
                || argument.as_os_str() == OsStr::new("generate")
                || target.replace(argument).is_some()
            {
                has_conflict = true;
                break;
            }
        }
        if !has_conflict {
            return match metadata_argument.to_str() {
                Some("--revision") => Some(super::SOURCE_REVISION.to_owned()),
                Some("--closure") => Some(super::SOURCE_CLOSURE.to_owned()),
                _ => None,
            };
        }
    }

    let raw = super::parse_raw_clap(arguments.iter().cloned()).ok()?;
    if raw.revision {
        Some(super::SOURCE_REVISION.to_owned())
    } else if raw.closure {
        Some(super::SOURCE_CLOSURE.to_owned())
    } else {
        None
    }
}

fn closure_repository_root(arguments: &[OsString]) -> Result<PathBuf, String> {
    let mut root = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy();
        if argument == "--repo" {
            if root.is_some() {
                return Err("closure accepts --repo only once".to_owned());
            }
            let value = arguments
                .get(index + 1)
                .ok_or_else(|| "closure --repo requires a path".to_owned())?;
            if value.is_empty() || value.to_string_lossy().starts_with('-') {
                return Err("closure --repo requires a path".to_owned());
            }
            root = Some(PathBuf::from(value));
            index += 1;
        } else if let Some(value) = argument.strip_prefix("--repo=") {
            if root.is_some() || value.is_empty() {
                return Err("closure accepts one non-empty --repo path".to_owned());
            }
            root = Some(PathBuf::from(value));
        }
        index += 1;
    }
    Ok(root.unwrap_or_else(|| PathBuf::from(".")))
}

fn reject_non_schema2_root(root: &Path) -> DispatchDecision {
    DispatchDecision::Reject(format!(
        "refusing repository root {}: a valid schema = 2 config is required",
        root.display()
    ))
}

fn generator_dispatch_decision(arguments: &[OsString]) -> DispatchDecision {
    let raw = match super::parse_raw_clap(arguments.iter().cloned()) {
        Ok(raw) => raw,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            return DispatchDecision::Help(error.to_string());
        }
        Err(error) => {
            return DispatchDecision::Reject(super::clap_usage_error(&error).to_string());
        }
    };
    let cli: super::Cli = match raw.try_into() {
        Ok(cli) => cli,
        Err(error) => return DispatchDecision::Reject(error.to_string()),
    };
    let selection = match super::capture_generator_selection(&cli.target, cli.output.as_deref()) {
        Ok(selection) => selection,
        Err(error) => return DispatchDecision::Reject(error.to_string()),
    };
    let Some(local_root) = selection.source.local_root.as_deref() else {
        // A typed GitHub source remains remote even if a directory with the
        // same spelling appears before its checkout worker starts.
        return DispatchDecision::Schema2(selection);
    };
    match schema2_root_marker(local_root) {
        Ok(true) => DispatchDecision::Schema2(selection),
        Ok(false) => reject_non_schema2_root(local_root.command_directory()),
        Err(error) => DispatchDecision::Reject(error),
    }
}

/// Runtime subcommands operate on the working directory (or the policy
/// `--workflow-root`), never on a generator target. `closure` is tied to an
/// explicitly captured repository root.
fn runtime_dispatch_decision(command: &str, root: &Path) -> DispatchDecision {
    match schema2_safe_root(root) {
        Ok(Some(root)) if command == "closure" => DispatchDecision::Schema2Closure(root),
        Ok(Some(root)) => DispatchDecision::Schema2Runtime(root),
        Ok(None) => reject_non_schema2_root(root),
        Err(error) => DispatchDecision::Reject(error),
    }
}

/// Probe a root once, returning the same descriptor that proved its schema.
/// Root-bound dispatch rejects a missing or non-schema-2 config before a
/// consumer can reopen the path.
fn schema2_safe_root(dir: &Path) -> Result<Option<SafeRoot>, String> {
    let observed = match fs::symlink_metadata(dir) {
        Ok(observed) => observed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "inspect repository root {}: {error}",
                dir.display()
            ));
        }
    };
    if observed.file_type().is_symlink() {
        return Err(format!(
            "refusing symlinked repository root: {}",
            dir.display()
        ));
    }
    if !observed.is_dir() {
        return Err(format!(
            "repository root is not a directory: {}",
            dir.display()
        ));
    }
    let safe_root = SafeRoot::open(dir).map_err(|error| error.to_string())?;
    let identity = safe_root.identity().map_err(|error| error.to_string())?;
    if !identity.matches_metadata(&observed) {
        return Err(format!(
            "repository root changed while dispatching schema: {}",
            dir.display()
        ));
    }
    if schema2_root_marker(&safe_root)? {
        Ok(Some(safe_root))
    } else {
        Ok(None)
    }
}

fn schema2_root_marker(root: &SafeRoot) -> Result<bool, String> {
    let generation =
        super::config::discover_with_safe_root(root).map_err(|error| error.to_string())?;
    root.validate_root_binding()
        .map_err(|error| error.to_string())?;
    Ok(generation.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

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
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_fail<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error,
        }
    }

    fn fixture_dir(name: &str, config: Option<&str>) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "velnor-r2-dispatch-{}-{name}-{}",
            std::process::id(),
            super::super::unique_suffix()
        ));
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
    fn parsed_providers_flag_routes_only_a_schema2_target() {
        let root = fixture_dir(
            "providers-target",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        for invocation in [
            args(&[target.as_str(), "--providers", "github-hosted"]),
            args(&[target.as_str(), "--providers=github-hosted"]),
        ] {
            assert!(matches!(
                dispatch_decision(&invocation),
                DispatchDecision::Schema2(selection)
                    if matches!(selection.source.source, super::super::RepositorySource::Local(_))
            ));
        }
        let non_schema2 = fixture_dir(
            "providers-non-schema2",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = non_schema2.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&[target.as_str(), "--providers", "github-hosted"])),
            DispatchDecision::Reject(error) if error.contains("reads schema 2 only")
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(non_schema2);
    }

    #[test]
    fn provider_spelling_consumed_as_policy_value_cannot_bypass_schema_probe() {
        let root = fixture_dir(
            "policy-provider-value",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let path = root.to_string_lossy().into_owned();
        let revision = "0123456789abcdef0123456789abcdef01234567";
        assert!(matches!(
            dispatch_decision(&args(&[
                "policy",
                "--ruleset-contexts",
                "--providers",
                "--base-revision",
                revision,
                "--workflow-root",
                path.as_str(),
            ])),
            DispatchDecision::Schema2Policy(_)
        ));

        assert!(!matches!(
            dispatch_decision(&args(&["--", "--providers"])),
            DispatchDecision::Schema2(_)
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn promote_dispatch_captures_schema2_target_and_generator_roots() {
        let repository = fixture_dir(
            "promote-target",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let generator = fixture_dir("promote-generator", None);
        let repository_path = repository.to_string_lossy().into_owned();
        let generator_path = generator.to_string_lossy().into_owned();
        let canonical_repository = must(
            repository.canonicalize(),
            "canonicalize promote target fixture",
        );
        let canonical_generator = must(
            generator.canonicalize(),
            "canonicalize promote generator fixture",
        );
        let invocation = args(&[
            "promote",
            "--rev",
            "0123456789abcdef0123456789abcdef01234567",
            "--repo",
            repository_path.as_str(),
            "--generator-repo",
            generator_path.as_str(),
            "--providers",
            "github-hosted",
        ]);
        let decision = dispatch_decision(&invocation);
        assert!(matches!(
            &decision,
            DispatchDecision::Schema2Promote {
                repository: captured,
                generator_repository: Some(generator),
                ..
            } if captured.command_directory() == canonical_repository.as_path()
                && generator.command_directory() == canonical_generator.as_path()
        ));
        let _ = std::fs::remove_dir_all(repository);
        let _ = std::fs::remove_dir_all(generator);
    }

    #[test]
    fn promote_root_replacement_after_dispatch_fails_before_tree_access() {
        let root = fixture_dir(
            "promote-root-swap",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let path = root.to_string_lossy().into_owned();
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let invocation = args(&["promote", "--rev", revision, "--repo", path.as_str()]);
        let decision = dispatch_decision(&invocation);
        assert!(matches!(&decision, DispatchDecision::Schema2Promote { .. }));

        let original = root.with_extension("original");
        must(
            std::fs::rename(&root, &original),
            "move captured promotion root",
        );
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create replacement promotion root",
        );
        must(
            std::fs::write(
                root.join(".github-gen/velnor-workflow.toml"),
                "schema = 2\n\n[generator]\nrepository = \"example/replacement\"\n",
            ),
            "write replacement promotion config",
        );

        let result = finish_dispatch(&invocation, decision);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("repository root changed")),
            "promotion must stay bound to the root selected by dispatch: {result:?}"
        );
        let replacement = std::fs::read_to_string(root.join(".github-gen/velnor-workflow.toml"))
            .expect("replacement config remains readable");
        assert!(
            replacement.contains("example/replacement"),
            "promotion must not stamp a replacement at the old path"
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(original);
    }

    #[test]
    fn rootless_top_level_build_metadata_flags_skip_target_probe() {
        for (invocation, expected) in [
            (args(&["--revision"]), super::super::SOURCE_REVISION),
            (args(&["--closure"]), super::super::SOURCE_CLOSURE),
            (
                args(&["some/target", "--revision"]),
                super::super::SOURCE_REVISION,
            ),
            (
                args(&["some/target", "--closure"]),
                super::super::SOURCE_CLOSURE,
            ),
            (
                args(&["--revision", "some/target"]),
                super::super::SOURCE_REVISION,
            ),
            (
                args(&["--closure", "some/target"]),
                super::super::SOURCE_CLOSURE,
            ),
        ] {
            assert!(matches!(
                dispatch_decision(&invocation),
                DispatchDecision::BuildMetadata(value) if value == expected
            ));
        }

        let local_schema1 = fixture_dir(
            "metadata-skips-schema-probe",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = local_schema1.to_string_lossy().into_owned();
        for (metadata, expected) in [
            ("--revision", super::super::SOURCE_REVISION),
            ("--closure", super::super::SOURCE_CLOSURE),
        ] {
            assert!(matches!(
                dispatch_decision(&args(&[target.as_str(), metadata])),
                DispatchDecision::BuildMetadata(value) if value == expected
            ));
        }
        let _ = std::fs::remove_dir_all(local_schema1);

        for invocation in [
            args(&["some/target", "--revision", "--plain"]),
            args(&["some/target", "--revision", "--closure"]),
        ] {
            assert!(matches!(
                dispatch_decision(&invocation),
                DispatchDecision::Reject(_)
            ));
        }
    }

    #[test]
    fn retired_runner_flag_is_rejected_by_the_typed_parser() {
        let root = fixture_dir(
            "legacy-flag-schema2",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        // The retired option is unknown to the only parser.
        assert!(matches!(
            dispatch_decision(&args(&[target.as_str(), "--runners", "both"])),
            DispatchDecision::Reject(error) if error.contains("--runners")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_schema2_target_routes_schema2() {
        let root = fixture_dir(
            "target",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        assert!(is_supported_invocation(&args(&[
            target.as_str(),
            "--plain"
        ])));
        assert!(is_supported_invocation(&args(&[
            "generate",
            target.as_str()
        ])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replacement_after_schema_dispatch_is_rejected_before_scan() {
        let root = fixture_dir(
            "root-swap",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        let selection = match dispatch_decision(&args(&[target.as_str(), "--plain"])) {
            DispatchDecision::Schema2(selection) => selection,
            _ => panic!("schema-2 dispatch should capture source and output roots"),
        };
        let captured = selection
            .source
            .local_root
            .as_ref()
            .expect("local source root handle")
            .identity();
        let identity = must(captured, "capture source identity");

        let original = root.with_extension("original");
        must(std::fs::rename(&root, &original), "move dispatched root");
        must(
            std::fs::create_dir_all(root.join(".github-gen")),
            "create replacement root",
        );
        must(
            std::fs::write(
                root.join(".github-gen/velnor-workflow.toml"),
                "schema = 2\n\n[generator]\nrepository = \"example/replacement\"\n",
            ),
            "write replacement config",
        );

        let result = super::super::open_source_root(&root, Some(&identity));
        assert!(
            matches!(result, Err(error) if error.to_string().contains("changed after schema dispatch"))
        );
        let output =
            SafeRoot::open_existing_output_bound(&selection.output.root, &selection.output.binding);
        assert!(
            matches!(output, Err(ref error) if error.to_string().contains("output path changed after planning")),
            "the output binding must reject replacement B: {output:?}"
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(original);
    }

    #[test]
    fn local_non_schema2_target_fails_closed() {
        let root = fixture_dir(
            "non-schema2-target",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&[target.as_str(), "--plain"])),
            DispatchDecision::Reject(error) if error.contains("reads schema 2 only")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_config_target_fails_closed() {
        let root = fixture_dir("noconfig", None);
        let target = root.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&[target.as_str()])),
            DispatchDecision::Reject(error)
                if error.contains("schema = 2 config is required")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn schema2_dispatch_rejects_untyped_or_unknown_config_fields() {
        let root = fixture_dir(
            "schema2-unknown-field",
            Some("schema = 2\nunknown = true\n"),
        );
        let target = root.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&[target.as_str(), "--plain"])),
            DispatchDecision::Reject(error) if error.contains("invalid generation config")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_config_removed_after_dispatch_is_rejected_before_scan() {
        let root = fixture_dir(
            "config-removed-after-dispatch",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        let selection = match dispatch_decision(&args(&[target.as_str(), "--plain"])) {
            DispatchDecision::Schema2(selection) => selection,
            _ => panic!("typed schema-2 config must route to S2"),
        };
        must(
            std::fs::remove_file(root.join(".github-gen/velnor-workflow.toml")),
            "remove config after dispatch",
        );
        let safe_root = match selection.source.local_root {
            Some(root) => root,
            None => panic!("local dispatch must retain its pinned root"),
        };
        let error = must_fail(
            super::super::scan_target_with_safe_root(
                std::sync::Arc::clone(&safe_root),
                None,
                "main",
            ),
            "local scan must require the typed config again at its boundary",
        );
        assert!(
            error.to_string().contains("schema-2 operation requires"),
            "missing config must fail before scanning: {error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_config_removed_after_dispatch_is_rejected_before_command() {
        let root = fixture_dir(
            "runtime-config-removed-after-dispatch",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let invocation = args(&["fingerprint"]);
        let decision = runtime_dispatch_decision("fingerprint", &root);
        assert!(matches!(&decision, DispatchDecision::Schema2Runtime(_)));
        must(
            std::fs::remove_file(root.join(".github-gen/velnor-workflow.toml")),
            "remove config after runtime dispatch",
        );
        let result = finish_dispatch(&invocation, decision);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("schema-2 operation requires")),
            "runtime must fail before processing a root with missing config: {result:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn policy_config_removed_after_dispatch_is_rejected_before_evaluation() {
        const CHILD_ENV: &str = "VELNOR_DISPATCH_POLICY_CONFIG_CHILD";
        if env::var_os(CHILD_ENV).is_none() {
            let event_root = fixture_dir("policy-trusted-event", None);
            let event_path = event_root.join("event.json");
            must(
                std::fs::write(&event_path, r#"{"repository":{"default_branch":"main"}}"#),
                "write trusted policy event",
            );
            let executable = must(env::current_exe(), "locate dispatch test executable");
            let output = must(
                Command::new(executable)
                    .args([
                        "--exact",
                        "s2::dispatch::tests::policy_config_removed_after_dispatch_is_rejected_before_evaluation",
                    ])
                    .env(CHILD_ENV, "1")
                    .env("GITHUB_REPOSITORY", "example/fixture")
                    .env("GITHUB_EVENT_PATH", &event_path)
                    .output(),
                "run policy test with trusted caller context",
            );
            assert!(
                output.status.success(),
                "trusted policy child failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let _ = std::fs::remove_dir_all(event_root);
            return;
        }

        let root = fixture_dir(
            "policy-config-removed-after-dispatch",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let path = root.to_string_lossy().into_owned();
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let invocation = args(&[
            "policy",
            "--workflow-root",
            path.as_str(),
            "--base-revision",
            revision,
        ]);
        let decision = dispatch_decision(&invocation);
        assert!(matches!(&decision, DispatchDecision::Schema2Policy(_)));
        must(
            std::fs::remove_file(root.join(".github-gen/velnor-workflow.toml")),
            "remove config after policy dispatch",
        );
        let result = finish_dispatch(&invocation, decision);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("schema-2 policy requires")),
            "policy must fail before evaluation without the captured root config: {result:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn remote_checkout_root_must_have_typed_config_before_scan() {
        let target = format!(
            "velnor-dispatch-{}-{}/repository",
            std::process::id(),
            super::super::unique_suffix()
        );
        let selection = match dispatch_decision(&args(&[target.as_str(), "--plain"])) {
            DispatchDecision::Schema2(selection) => selection,
            _ => panic!("remote generator target must remain typed"),
        };
        assert!(matches!(
            &selection.source.source,
            super::super::RepositorySource::GitHub { .. }
        ));
        assert!(selection.source.local_root.is_none());

        // This stands for the directory returned by remote checkout. The
        // root must prove the same typed config before any scan or render.
        let checkout = fixture_dir("remote-checkout-without-config", None);
        let safe_root =
            std::sync::Arc::new(SafeRoot::open(&checkout).expect("open simulated remote checkout"));
        let error = must_fail(
            super::super::scan_target_with_safe_root(safe_root, None, "main"),
            "remote checkout must fail before scanning without config",
        );
        assert!(
            error.to_string().contains("schema-2 operation requires"),
            "missing remote config must fail closed: {error}"
        );
        let _ = std::fs::remove_dir_all(checkout);
    }

    #[test]
    fn target_replacement_after_schema_probe_stays_fail_closed() {
        let root_a = fixture_dir(
            "root-swap-a",
            Some("schema = 1\n\n[generator]\nrepository = \"example/a\"\n"),
        );
        let root_b = fixture_dir(
            "root-swap-b",
            Some("schema = 2\n\n[generator]\nrepository = \"example/b\"\n"),
        );
        let target_a = root_a.to_string_lossy().into_owned();
        let invocation = args(&[target_a.as_str(), "--plain"]);
        let decision = dispatch_decision(&invocation);
        assert!(matches!(
            &decision,
            DispatchDecision::Reject(error)
                if error.contains("reads schema 2 only")
        ));

        // Model a rename-and-replace in the interval between dispatch probing
        // A and dispatch completion. The already selected rejection must not
        // turn into a path-based open of schema-2 tree B at A's old name.
        let original_a = root_a.with_extension("original");
        must(std::fs::rename(&root_a, &original_a), "move probed root A");
        must(
            std::fs::rename(&root_b, &root_a),
            "replace probed root A with root B",
        );
        let result = finish_dispatch(&invocation, decision);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("reads schema 2 only")),
            "replacement root B must not reach a path-based consumer: {result:?}"
        );
        assert!(
            std::fs::read_to_string(root_a.join(".github-gen/velnor-workflow.toml"))
                .is_ok_and(|config| config.contains("example/b")),
            "schema-2 replacement B remains untouched"
        );

        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(original_a);
    }

    #[test]
    fn remote_target_routes_through_schema2_source_loader() {
        for invocation in [
            args(&["example/fixture", "--plain"]),
            args(&["https://github.com/example/fixture", "--plain"]),
        ] {
            assert!(matches!(
                dispatch_decision(&invocation),
                DispatchDecision::Schema2(selection)
                    if matches!(selection.source.source, super::super::RepositorySource::GitHub { .. })
            ));
        }
    }

    #[test]
    fn remote_classification_stays_remote_after_same_spelling_directory_appears() {
        let target = format!(
            "velnor-dispatch-{}-{}/repository",
            std::process::id(),
            super::super::unique_suffix()
        );
        let selection = super::super::capture_generator_selection(&target, None);
        let selection = must(selection, "capture remote target selection");
        assert!(matches!(
            &selection.source.source,
            super::super::RepositorySource::GitHub { .. }
        ));

        let target_path = PathBuf::from(&target);
        must(
            std::fs::create_dir_all(&target_path),
            "create local directory after selection",
        );
        assert!(matches!(
            &selection.source.source,
            super::super::RepositorySource::GitHub { .. }
        ));
        let _ = std::fs::remove_dir_all(target_path.parent().unwrap_or(Path::new(".")));
    }

    #[test]
    fn explicit_output_parent_swap_after_dispatch_cannot_redirect_render() {
        let source = fixture_dir(
            "output-swap-source",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let source_arg = source.to_string_lossy().into_owned();
        let launch = env::temp_dir().join(format!(
            "velnor-r2-dispatch-output-{}-{}",
            std::process::id(),
            super::super::unique_suffix()
        ));
        let parent = launch.join("approved-output-parent");
        must(std::fs::create_dir_all(&parent), "create output parent A");
        let output = parent.join("generated");
        let output_arg = output.to_string_lossy().into_owned();
        let selection = match dispatch_decision(&args(&[
            source_arg.as_str(),
            "--output",
            output_arg.as_str(),
        ])) {
            DispatchDecision::Schema2(selection) => selection,
            _ => panic!("schema-2 dispatch must capture source and output"),
        };

        let original_parent = launch.join("approved-output-parent-original");
        must(
            std::fs::rename(&parent, &original_parent),
            "move captured output parent A",
        );
        must(
            std::fs::create_dir(&parent),
            "create replacement output parent B",
        );
        must(
            std::fs::write(parent.join("sentinel"), "preserve replacement"),
            "plant replacement sentinel",
        );

        let result =
            SafeRoot::open_existing_output_bound(&selection.output.root, &selection.output.binding);
        assert!(
            matches!(result, Err(ref error) if error.to_string().contains("output path changed after planning")),
            "render must reject output parent B: {result:?}"
        );
        assert!(
            std::fs::read_to_string(parent.join("sentinel"))
                .is_ok_and(|value| value == "preserve replacement"),
            "replacement B remains untouched"
        );
        assert!(
            !original_parent.join("generated").exists(),
            "captured parent A gets no output after its path moved"
        );
        let _ = std::fs::remove_dir_all(launch);
        let _ = std::fs::remove_dir_all(source);
    }

    #[test]
    fn policy_routes_on_workflow_root() {
        let root = fixture_dir(
            "policy",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let path = root.to_string_lossy().into_owned();
        assert!(is_supported_invocation(&args(&[
            "policy",
            "--workflow-root",
            path.as_str()
        ])));
        assert!(is_supported_invocation(&args(&[
            "policy",
            format!("--workflow-root={path}").as_str()
        ])));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_config_policy_root_fails_closed() {
        let root = fixture_dir("policy-no-config", None);
        let path = root.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&["policy", "--workflow-root", path.as_str()])),
            DispatchDecision::Reject(error)
                if error.contains("schema = 2 config is required")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_config_runtime_root_fails_closed() {
        let root = fixture_dir("runtime-no-config", None);
        assert!(matches!(
            runtime_dispatch_decision("fingerprint", &root),
            DispatchDecision::Reject(error)
                if error.contains("schema = 2 config is required")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn policy_route_rejects_replacement_tree_b_after_capturing_a() {
        let root_a = fixture_dir(
            "policy-route-a",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let root_b = fixture_dir(
            "policy-route-b",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let path_a = root_a.to_string_lossy().into_owned();
        let route_args = args(&[
            "policy",
            "--workflow-root",
            path_a.as_str(),
            "--base-revision",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ]);
        let captured = match dispatch_decision(&route_args) {
            DispatchDecision::Schema2Policy(root) => root,
            _ => panic!("schema-2 policy dispatch should capture workflow root A"),
        };

        let original_a = root_a.with_extension("original");
        must(std::fs::rename(&root_a, &original_a), "move policy root A");
        must(
            std::fs::rename(&root_b, &root_a),
            "replace A with policy root B",
        );

        let result = super::super::runtime::run_policy_with_safe_root(&route_args[1..], captured);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("policy workflow root differs from the repository captured by dispatch")),
            "policy must reject the replacement at A, got {result:?}"
        );

        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(original_a);
    }

    #[test]
    fn runtime_route_rejects_replacement_tree_b_after_capturing_a() {
        let root_a = fixture_dir(
            "runtime-route-a",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let root_b = fixture_dir(
            "runtime-route-b",
            Some("schema = 2\n\n[generator]\nrepository = \"example/replacement\"\n"),
        );
        let captured = match runtime_dispatch_decision("fingerprint", &root_a) {
            DispatchDecision::Schema2Runtime(root) => root,
            _ => panic!("schema-2 runtime dispatch should capture working root A"),
        };

        let original_a = root_a.with_extension("original");
        must(std::fs::rename(&root_a, &original_a), "move runtime root A");
        must(
            std::fs::rename(&root_b, &root_a),
            "replace A with runtime root B",
        );

        let result = super::super::runtime::run_with_safe_root(&args(&["fingerprint"]), captured);
        assert!(
            matches!(&result, Err(error) if error.to_string().contains("repository root changed")),
            "runtime must reject replacement root B before processing, got {result:?}"
        );

        let _ = std::fs::remove_dir_all(root_a);
        let _ = std::fs::remove_dir_all(original_a);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_route_rejects_symlink_and_overlong_root_probe_errors() {
        let root = fixture_dir(
            "runtime-root-link-target",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let alias = root.with_extension("link");
        std::os::unix::fs::symlink(&root, &alias)
            .unwrap_or_else(|error| panic!("link runtime root: {error}"));
        assert!(matches!(
            runtime_dispatch_decision("fingerprint", &alias),
            DispatchDecision::Reject(error) if error.contains("symlinked repository root")
        ));

        let overlong = PathBuf::from(format!("/tmp/{}", "x".repeat(1024)));
        assert!(matches!(
            runtime_dispatch_decision("fingerprint", &overlong),
            DispatchDecision::Reject(error) if !error.is_empty()
        ));
        let _ = std::fs::remove_file(alias);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn version_is_rootless_and_closure_captures_a_typed_root() {
        assert!(matches!(
            dispatch_decision(&args(&["version"])),
            DispatchDecision::RootlessRuntime
        ));
        let root = fixture_dir(
            "closure-root",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let path = root.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&[
                "closure",
                "--repo",
                path.as_str(),
                "--rev",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ])),
            DispatchDecision::Schema2Closure(_)
        ));
        let missing = fixture_dir("closure-no-config", None);
        let missing_path = missing.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&["closure", "--repo", missing_path.as_str(), "--rev", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"])),
            DispatchDecision::Reject(error) if error.contains("schema = 2")
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(missing);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_generation_config_is_rejected_before_policy_dispatch() {
        let root = fixture_dir("symlinked-config", None);
        let outside = fixture_dir(
            "symlinked-config-target",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        std::fs::remove_dir(root.join(".github-gen"))
            .unwrap_or_else(|error| panic!("remove empty config directory: {error}"));
        std::os::unix::fs::symlink(outside.join(".github-gen"), root.join(".github-gen"))
            .unwrap_or_else(|error| panic!("link external config directory: {error}"));

        let target = root.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&["policy", "--workflow-root", target.as_str()])),
            DispatchDecision::Reject(_)
        ));

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[cfg(unix)]
    #[test]
    fn policy_dispatch_rejects_symlinked_workflow_root() {
        let root = fixture_dir(
            "policy-root-link-target",
            Some("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let alias = root.with_extension("link");
        std::os::unix::fs::symlink(&root, &alias)
            .unwrap_or_else(|error| panic!("link policy root: {error}"));
        let target = alias.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&["policy", "--workflow-root", target.as_str()])),
            DispatchDecision::Reject(error) if error.contains("symlinked repository root")
        ));
        let _ = std::fs::remove_file(alias);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn policy_dispatch_rejects_overlong_root_probe_errors() {
        let overlong = format!("/tmp/{}", "x".repeat(1024));
        assert!(matches!(
            dispatch_decision(&args(&["policy", "--workflow-root", overlong.as_str()])),
            DispatchDecision::Reject(error) if !error.is_empty()
        ));
    }
}

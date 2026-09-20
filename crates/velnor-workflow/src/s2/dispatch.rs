//! Schema dispatch for the R2 bridge release.
//!
//! The bridge binary serves two configuration schemas: schema 1 renders
//! through the original pipeline in the crate root, schema 2 through the
//! provider pipeline in [`crate::s2`]. This module peeks at the invocation
//! (an explicit `--providers` flag, the target's own
//! `.github-gen/velnor-workflow.toml`, or a policy runtime root) and routes
//! before either parser runs.
//!
//! The peek is fail-closed in both directions: a misrouted invocation still
//! hits the destination pipeline's strict schema gate, which rejects the
//! foreign schema with a usage error instead of rendering it.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use super::safe_fs::{SafeRoot, SafeRootIdentity};
use super::GeneratorSelection;

/// Binary-only subcommands, mirroring `runtime::try_run` plus the reuse
/// slice it falls through to. These never take a generator target, so the
/// peek roots them at the policy root precedence or the working directory.
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

/// Run the schema-2 pipeline when this invocation targets a supported root.
/// `None` is reserved for commands that do not bind a local repository root.
pub(crate) fn run_if_s2() -> Option<Result<(), crate::GeneratorError>> {
    let arguments: Vec<OsString> = env::args_os().skip(1).collect();
    finish_dispatch(&arguments, dispatch_decision(&arguments))
}

fn finish_dispatch(
    arguments: &[OsString],
    decision: DispatchDecision,
) -> Option<Result<(), crate::GeneratorError>> {
    match decision {
        DispatchDecision::Legacy => None,
        DispatchDecision::Reject(error) => Some(Err(crate::GeneratorError::usage(error))),
        DispatchDecision::Schema2Policy(root) => Some(
            super::runtime::run_policy_with_safe_root(&arguments[1..], root)
                .map_err(|error| crate::GeneratorError::usage(error.to_string())),
        ),
        DispatchDecision::Schema2Runtime(root) => Some(
            super::runtime::run_with_safe_root(arguments, root)
                .map_err(|error| crate::GeneratorError::usage(error.to_string())),
        ),
        DispatchDecision::Schema2(selection) => {
            // Both error types carry a single message string, so the bridge
            // maps across the pipeline boundary without losing context.
            Some(
                super::run_from_env_with_selection(Some(selection))
                    .map_err(|error| crate::GeneratorError::usage(error.to_string())),
            )
        }
    }
}

#[cfg(test)]
fn wants_s2(arguments: &[OsString]) -> bool {
    !matches!(dispatch_decision(arguments), DispatchDecision::Legacy)
}

enum DispatchDecision {
    Legacy,
    Schema2(GeneratorSelection),
    Schema2Policy(SafeRoot),
    Schema2Runtime(SafeRoot),
    Reject(String),
}

fn dispatch_decision(arguments: &[OsString]) -> DispatchDecision {
    if arguments.first().and_then(|argument| argument.to_str()) == Some("promote") {
        return DispatchDecision::Reject(
            "promote is unavailable until it uses the captured repository root".to_owned(),
        );
    }
    // Runtime verbs own their command grammar and root precedence. Parse them
    // before generator options so an option value such as `--providers` can
    // never steal a policy invocation away from its workflow-root schema gate.
    if let Some(command) = arguments.first().and_then(|value| value.to_str())
        && RUNTIME_COMMANDS.contains(&command)
    {
        if command == "policy" {
            let root = match super::policy::workflow_root_for_dispatch(&arguments[1..]) {
                Ok(root) => root,
                Err(error) => return DispatchDecision::Reject(error.to_string()),
            };
            return match schema2_safe_root(&root) {
                Ok(Some(root)) => DispatchDecision::Schema2Policy(root),
                Ok(None) => reject_legacy_root(&root),
                Err(error) => DispatchDecision::Reject(error),
            };
        }
        // `.` is resolved by the kernel against the process's already-open
        // CWD. Resolving CWD to a pathname here and opening that pathname
        // later would let a rename-and-replace race route dispatch to a new
        // tree at the old name.
        return runtime_dispatch_decision(command, Path::new("."));
    }
    if is_rootless_build_metadata_request(arguments) {
        return DispatchDecision::Legacy;
    }
    generator_dispatch_decision(arguments)
}

fn is_rootless_build_metadata_request(arguments: &[OsString]) -> bool {
    let Ok(raw) = crate::parse_raw_clap(arguments.iter().cloned()) else {
        return false;
    };
    raw.revision || raw.closure
}

fn reject_legacy_root(root: &Path) -> DispatchDecision {
    DispatchDecision::Reject(format!(
        "refusing path-based legacy dispatch for {}: a valid schema = 2 repository root is required",
        root.display()
    ))
}

fn generator_dispatch_decision(arguments: &[OsString]) -> DispatchDecision {
    // Parse S2 first so `--providers` is recognized only when it is an actual
    // option. If that grammar rejects a legacy-only flag, parse the old CLI
    // just far enough to bind and schema-check its target before S2 reports
    // the unsupported flag.
    let (target, output) = match super::Cli::parse_args(arguments.to_vec()) {
        Ok(cli) => (cli.target, cli.output),
        Err(_) => match crate::Cli::parse_args(arguments.to_vec()) {
            Ok(cli) => (cli.target, cli.output),
            Err(_) => return DispatchDecision::Legacy,
        },
    };
    let selection = match super::capture_generator_selection(&target, output.as_deref()) {
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
        Ok(false) => reject_legacy_root(local_root.command_directory()),
        Err(error) => DispatchDecision::Reject(error),
    }
}

/// Runtime subcommands operate on the working directory (or the policy
/// `--workflow-root`), never on a generator target. `version` and `closure`
/// print build constants shared by both pipelines, so they stay put.
fn runtime_dispatch_decision(command: &str, root: &Path) -> DispatchDecision {
    if command == "version" || command == "closure" {
        return DispatchDecision::Legacy;
    }
    match schema2_safe_root(root) {
        Ok(Some(root)) => DispatchDecision::Schema2Runtime(root),
        Ok(None) => reject_legacy_root(root),
        Err(error) => DispatchDecision::Reject(error),
    }
}

/// Whether `dir` carries a fully parsed typed schema-2 generation config.
/// Missing config remains a legacy root; malformed or unsupported config is
/// an error and never reaches either pipeline.
pub(crate) fn dir_is_schema2(dir: &Path) -> bool {
    match schema2_root_identity(dir) {
        Ok(Some(_)) | Err(_) => true,
        Ok(None) => false,
    }
}

/// Probe the schema through one pinned root and return its identity for the
/// generator run. The first metadata observation is checked against that
/// handle so a replacement between `lstat` and `SafeRoot::open` cannot be
/// mistaken for the originally observed target.
fn schema2_root_identity(dir: &Path) -> Result<Option<SafeRootIdentity>, String> {
    schema2_safe_root(dir)?
        .map(|root| root.identity().map_err(|error| error.to_string()))
        .transpose()
}

/// Probe a root once, returning the same descriptor that proved its schema.
/// A missing or non-schema-2 config is never handed to the path-based schema-1
/// code; root-bound dispatch rejects it before a consumer can reopen the path.
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
        let schema1 = fixture_dir(
            "providers-schema1",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let legacy = schema1.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&[legacy.as_str(), "--providers", "github-hosted"])),
            DispatchDecision::Reject(error) if error.contains("reads schema 2 only")
        ));
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(schema1);
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
    fn exact_promote_fails_closed_before_provider_routing() {
        for invocation in [
            args(&["promote"]),
            args(&["promote", "--providers", "github-hosted"]),
            args(&["promote", "--providers=github-hosted"]),
            args(&[
                "promote",
                "--rev",
                "0123456789abcdef0123456789abcdef01234567",
            ]),
        ] {
            assert!(matches!(
                dispatch_decision(&invocation),
                DispatchDecision::Reject(error)
                    if error.contains("promote is unavailable until it uses the captured repository root")
            ));
        }
    }

    #[test]
    fn rootless_top_level_build_metadata_flags_skip_target_probe() {
        for invocation in [
            args(&["--revision"]),
            args(&["--closure"]),
            args(&["some/target", "--revision"]),
            args(&["some/target", "--closure"]),
        ] {
            assert!(matches!(
                dispatch_decision(&invocation),
                DispatchDecision::Legacy
            ));
        }
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
    fn local_schema1_target_rejects_path_based_legacy_dispatch() {
        let root = fixture_dir(
            "target1",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        let target = root.to_string_lossy().into_owned();
        for invocation in [
            args(&[target.as_str(), "--plain"]),
            args(&[target.as_str(), "--runners", "both"]),
        ] {
            assert!(matches!(
                dispatch_decision(&invocation),
                DispatchDecision::Reject(error)
                    if error.contains("reads schema 2 only")
            ));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_config_target_rejects_path_based_legacy_dispatch() {
        let root = fixture_dir("noconfig", None);
        let target = root.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&[target.as_str()])),
            DispatchDecision::Reject(error)
                if error.contains("path-based legacy dispatch")
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
            error.to_string().contains("schema-2 dispatch requires"),
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
            matches!(&result, Some(Err(error)) if error.to_string().contains("schema-2 dispatch requires")),
            "runtime must fail before processing a root with missing config: {result:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn policy_config_removed_after_dispatch_is_rejected_before_evaluation() {
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
            matches!(&result, Some(Err(error)) if error.to_string().contains("schema-2 dispatch requires")),
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
            error.to_string().contains("schema-2 dispatch requires"),
            "missing remote config must fail closed: {error}"
        );
        let _ = std::fs::remove_dir_all(checkout);
    }

    #[test]
    fn schema1_target_replacement_after_probe_stays_fail_closed() {
        let root_a = fixture_dir(
            "schema1-replaced-a",
            Some("schema = 1\n\n[generator]\nrepository = \"example/a\"\n"),
        );
        let root_b = fixture_dir(
            "schema1-replaced-b",
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
        must(
            std::fs::rename(&root_a, &original_a),
            "move schema-1 root A",
        );
        must(
            std::fs::rename(&root_b, &root_a),
            "replace schema-1 root A with schema-2 root B",
        );
        let result = finish_dispatch(&invocation, decision);
        assert!(
            matches!(&result, Some(Err(error)) if error.to_string().contains("reads schema 2 only")),
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
    fn missing_config_policy_root_rejects_path_based_legacy_dispatch() {
        let root = fixture_dir("policy-no-config", None);
        let path = root.to_string_lossy().into_owned();
        assert!(matches!(
            dispatch_decision(&args(&["policy", "--workflow-root", path.as_str()])),
            DispatchDecision::Reject(error)
                if error.contains("path-based legacy dispatch")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_config_runtime_root_rejects_path_based_legacy_dispatch() {
        let root = fixture_dir("runtime-no-config", None);
        assert!(matches!(
            runtime_dispatch_decision("fingerprint", &root),
            DispatchDecision::Reject(error)
                if error.contains("path-based legacy dispatch")
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
            matches!(&result, Err(error) if error.to_string().contains("changed")),
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
    fn version_and_closure_stay_put() {
        assert!(!wants_s2(&args(&["version"])));
        assert!(!wants_s2(&args(&["closure"])));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_generation_config_routes_to_safe_schema2_loader() {
        let root = fixture_dir("symlinked-config", None);
        let outside = fixture_dir(
            "symlinked-config-target",
            Some("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n"),
        );
        std::fs::remove_dir(root.join(".github-gen"))
            .unwrap_or_else(|error| panic!("remove empty config directory: {error}"));
        std::os::unix::fs::symlink(outside.join(".github-gen"), root.join(".github-gen"))
            .unwrap_or_else(|error| panic!("link external config directory: {error}"));

        assert!(dir_is_schema2(&root));
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

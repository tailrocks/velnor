#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
//! Binary-level behavior of the clap-native `velnorctl`.

use std::process::{Command, Output};

use clap::CommandFactory;
use velnorctl::Cli;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_velnorctl")
}

fn run(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .env_remove("CLICOLOR")
        .output()
        .expect("spawn velnorctl")
}

fn code(output: &Output) -> u8 {
    output
        .status
        .code()
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(u8::MAX)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn cli_c005_root_help_goes_to_stdout_and_exits_success_with_silent_stderr() {
    let output = run(&["--help"]);
    assert_eq!(code(&output), 0);
    assert!(text(&output.stderr).is_empty());
    let help = text(&output.stdout);
    assert!(help.contains("Usage:"), "{help}");
    assert!(help.contains("velnorctl"), "{help}");
    assert!(help.contains("man"), "{help}");
    assert!(help.contains("completion"), "{help}");
}

#[test]
fn cli_c005_version_prints_binary_and_release_identity_to_stdout() {
    // The version is the stamped release identity `velnor-runner` carries
    // (release version and source SHA, or `development`), not velnorctl's
    // own unbumped crate version: the operator must be able to match a host
    // to a build from the CLI alone.
    let output = run(&["--version"]);
    assert_eq!(code(&output), 0);
    assert!(text(&output.stderr).is_empty());
    let version = text(&output.stdout);
    assert_eq!(
        version.trim_end(),
        format!("velnorctl {}", velnorctl::cli_version())
    );
    let identity = velnor_runner::embedded_build_identity();
    assert!(version.contains(&identity.crate_version), "{version}");
    assert!(version.contains(&identity.source_sha), "{version}");
}

#[test]
fn cli_c005_bare_invocation_prints_usage_to_stderr_and_exits_two() {
    let output = run(&[]);
    assert_eq!(code(&output), 2);
    assert!(text(&output.stdout).is_empty());
    let usage = text(&output.stderr);
    assert!(!usage.trim().is_empty(), "usage must be shown");
}

#[test]
fn cli_c005_unknown_commands_fail_like_any_unknown_clap_subcommand() {
    for name in ["definitely-not-a-command", "runn", "stat"] {
        let output = run(&[name]);
        assert_eq!(code(&output), 2, "{name}");
        let stderr = text(&output.stderr);
        assert!(
            stderr.contains("unrecognized subcommand"),
            "{name}: {stderr}"
        );
    }
}

#[test]
fn cli_migrated_legacy_names_are_first_class_subcommands() {
    // The full velnor-runner surface is owned by this binary now; the old
    // spellings parse as real commands with no alias layer (C001–C075).
    for name in [
        "cache",
        "capabilities",
        "configure",
        "docker",
        "host",
        "doctor",
        "preflight",
        "remove",
        "status",
        "storage",
    ] {
        let output = run(&[name, "--help"]);
        assert_eq!(code(&output), 0, "{name}");
        assert!(text(&output.stderr).is_empty(), "{name}");
        assert!(text(&output.stdout).contains("Usage:"), "{name}");
    }
}

#[test]
fn cli_run_worker_is_not_a_public_command() {
    // C075: the single-worker mode folds into `daemon --once` service
    // plumbing; `run` stays reserved for the future workflow-run resource.
    let output = run(&["daemon", "--help"]);
    assert_eq!(code(&output), 0, "daemon");
    assert!(text(&output.stderr).is_empty(), "daemon");
    assert!(text(&output.stdout).contains("Usage:"), "daemon");

    let name = "release";
    let output = run(&[name, "--help"]);
    assert_eq!(code(&output), 2, "{name}");
    assert!(
        text(&output.stderr).contains("unrecognized subcommand"),
        "{name}"
    );
    let output = run(&["run", "--help"]);
    assert_eq!(code(&output), 0);
    assert!(text(&output.stderr).is_empty());
    assert!(text(&output.stdout).contains("Usage: velnorctl"));
}

#[test]
fn cli_host_start_has_actionable_help() {
    let output = run(&["host", "start", "--help"]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    let help = text(&output.stdout);
    assert!(help.contains("--repo"), "{help}");
    assert!(help.contains("--pr"), "{help}");
    assert!(help.contains("--slots"), "{help}");
}

#[test]
fn cli_macos_docker_diagnostics_have_actionable_help() {
    let output = run(&["docker", "report", "--help"]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    let help = text(&output.stdout);
    assert!(help.contains("--check-bind-mount"), "{help}");
    assert!(help.contains("--docker-host-work-dir"), "{help}");
    assert!(help.contains("--work-dir"), "{help}");
}

#[test]
fn cli_c005_close_typos_get_a_clap_suggestion() {
    let output = run(&["mann"]);
    assert_eq!(code(&output), 2);
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("a similar subcommand exists") && stderr.contains("'man'"),
        "{stderr}"
    );
}

#[test]
fn cli_c005_no_color_is_a_switch_and_rejects_inline_values() {
    let output = run(&["--no-color=true", "man"]);
    assert_eq!(code(&output), 2);
    let stderr = text(&output.stderr);
    assert!(stderr.contains("--no-color"), "{stderr}");
}

#[test]
fn cli_c005_repeated_verbosity_flags_parse_before_subcommands() {
    for argv in [["-v", "man"], ["-vv", "man"], ["-vvv", "man"]] {
        let output = run(&argv);
        assert_eq!(code(&output), 0, "{argv:?}");
        assert!(text(&output.stdout).contains(".TH"), "{argv:?}");
    }
}

#[test]
fn cli_c005_output_global_is_accepted_before_and_after_the_subcommand() {
    let dir = tempfile_dir("c005-after");
    let output = run(&[
        "--output",
        "json",
        "man",
        "--directory",
        &dir,
        "--force",
        "--no-color",
    ]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(std::path::Path::new(&dir).join("velnorctl.1").exists());

    let dir2 = tempfile_dir("c005-before");
    let output = run(&["man", "--directory", &dir2, "--force", "-o", "json"]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    assert!(std::path::Path::new(&dir2).join("velnorctl.1").exists());
}

#[test]
fn cli_c005_invalid_closed_choice_values_exit_two() {
    let output = run(&["--output", "csv", "man"]);
    assert_eq!(code(&output), 2);
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("csv") || stderr.contains("invalid"),
        "{stderr}"
    );
}

#[test]
fn cli_c005_success_paths_keep_stderr_silent() {
    let dir = tempfile_dir("c005-silent");
    let output = run(&["man", "--directory", &dir]);
    assert_eq!(code(&output), 0);
    assert!(text(&output.stderr).is_empty(), "{}", text(&output.stderr));

    let output = run(&["completion", "bash"]);
    assert_eq!(code(&output), 0);
    assert!(text(&output.stderr).is_empty());
}

#[test]
fn cli_c005_runtime_failure_reports_machine_envelope_under_json_output() {
    let dir = tempfile_dir("c005-iofail");
    make_read_only(&dir);
    if writes_succeed_anyway(&dir) {
        return;
    }
    let output = run(&["--output", "json", "man", "--directory", &dir]);
    restore_writable(&dir);
    assert_eq!(code(&output), 8);
    let stderr = text(&output.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("machine envelope JSON");
    assert_eq!(parsed["class"], "OPERATION", "{parsed}");
    assert_eq!(parsed["code"], 8, "{parsed}");
}

#[test]
fn cli_c005_runtime_failure_reports_human_error_by_default() {
    let dir = tempfile_dir("c005-human-fail");
    make_read_only(&dir);
    if writes_succeed_anyway(&dir) {
        return;
    }
    let output = run(&["man", "--directory", &dir]);
    restore_writable(&dir);
    assert_eq!(code(&output), 8);
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("error:"), "{stderr}");
    assert!(
        !stderr.trim_start_matches("error:").starts_with('{'),
        "{stderr}"
    );
}

#[test]
fn every_subcommand_and_nested_help_exits_success() {
    for path in clap_command_paths() {
        let mut args: Vec<&str> = path.iter().map(String::as_str).collect();
        args.push("--help");
        let output = run(&args);
        assert_eq!(code(&output), 0, "{path:?}");
        assert!(
            text(&output.stderr).is_empty(),
            "{path:?}: {}",
            text(&output.stderr)
        );
        assert!(text(&output.stdout).contains("Usage:"), "{path:?}");
    }
}

#[test]
fn missing_required_arguments_exit_two() {
    for args in [
        ["configure"].as_slice(),
        ["doctor"].as_slice(),
        ["cache"].as_slice(),
        ["capabilities"].as_slice(),
        ["storage"].as_slice(),
        ["capabilities", "check"].as_slice(),
        ["completion"].as_slice(),
    ] {
        let output = run(args);
        assert_eq!(code(&output), 2, "{args:?}");
        assert!(!text(&output.stderr).is_empty(), "{args:?}");
        assert!(text(&output.stdout).is_empty(), "{args:?}");
    }
}

#[test]
fn every_completion_shell_writes_stdout() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let output = run(&["completion", shell]);
        assert_eq!(code(&output), 0, "{shell}");
        assert!(text(&output.stderr).is_empty(), "{shell}");
        assert!(!text(&output.stdout).is_empty(), "{shell}");
    }
}

#[test]
fn json_output_placement_is_equivalent_for_nested_help() {
    let before = run(&["--output", "json", "cache", "du", "--help"]);
    let after = run(&["cache", "du", "--output", "json", "--help"]);
    assert_eq!(code(&before), 0);
    assert_eq!(code(&after), 0);
    assert_eq!(text(&before.stdout), text(&after.stdout));
}

#[test]
fn status_json_health_vector_keys_are_stable() {
    let dir = tempfile_dir("health-json");
    std::fs::write(
        std::path::Path::new(&dir).join("execution.toml"),
        "[execution]\nbackend = \"docker\"\n",
    )
    .unwrap();
    let output = run(&[
        "status",
        "--json",
        "--state-dir",
        &dir,
        "--config-dir",
        &dir,
    ]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    let first: serde_json::Value = serde_json::from_str(text(&output.stdout).trim()).unwrap();
    let output2 = run(&["status", "--json", "--state-dir", &dir]);
    let second: serde_json::Value = serde_json::from_str(text(&output2.stdout).trim()).unwrap();
    let keys: Vec<&str> = first
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let keys2: Vec<&str> = second
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, keys2);
    for required in velnor_model::HealthDocument::REQUIRED_KEYS {
        assert!(keys.contains(&required), "{required} missing from {keys:?}");
    }
    assert!(first["alerts"].is_array());
    assert_ne!(first["state"], "ready");
}

#[test]
fn status_output_json_flag_routes_to_machine_health_vector() {
    let dir = tempfile_dir("status-o-json");
    std::fs::write(
        std::path::Path::new(&dir).join("execution.toml"),
        "[execution]\nbackend = \"docker\"\n",
    )
    .unwrap();
    let output = run(&[
        "status",
        "-o",
        "json",
        "--config-dir",
        &dir,
        "--state-dir",
        &dir,
    ]);
    assert_eq!(code(&output), 0, "{}", text(&output.stderr));
    let document: serde_json::Value =
        serde_json::from_str(text(&output.stdout).trim()).expect("machine JSON on stdout");
    for required in velnor_model::HealthDocument::REQUIRED_KEYS {
        assert!(
            document.get(required).is_some(),
            "{required} missing from {document}"
        );
    }
}

#[test]
fn status_output_json_with_empty_config_dir_reports_machine_envelope() {
    if std::path::Path::new("/etc/velnor/execution.toml").exists() {
        return;
    }
    let dir = tempfile_dir("status-o-json-empty");
    let output = run(&["status", "-o", "json", "--config-dir", &dir]);
    assert_eq!(code(&output), 4, "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    let envelope: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("machine envelope JSON");
    assert_eq!(envelope["class"], "UNAVAILABLE", "{envelope}");
    assert_eq!(envelope["code"], 4, "{envelope}");
    let remediation = envelope["remediation"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(remediation.contains(&dir), "{remediation}");
    assert!(remediation.contains("--config-dir"), "{remediation}");
}

#[test]
fn status_human_with_empty_config_dir_reports_unavailable_with_path() {
    let dir = tempfile_dir("status-human-empty");
    let output = std::process::Command::new(bin())
        .args(["status", "--config-dir", &dir])
        .env_remove("CLICOLOR")
        .env_remove("VELNOR_CAPABILITY_VALIDATION")
        .env_remove("VELNOR_SKIP_CAPABILITY_VALIDATION")
        .env_remove("VELNOR_DIAGNOSTIC_NODE_SIDECAR")
        .output()
        .expect("spawn velnorctl");
    assert_eq!(code(&output), 4, "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("error:"), "{stderr}");
    assert!(stderr.contains(&dir), "{stderr}");
    assert!(stderr.contains("runner.json"), "{stderr}");
    assert!(stderr.contains("--config-dir"), "{stderr}");
}

fn clap_command_paths() -> Vec<Vec<String>> {
    fn walk(cmd: &clap::Command, prefix: Vec<String>, out: &mut Vec<Vec<String>>) {
        let mut subs: Vec<&clap::Command> = cmd
            .get_subcommands()
            .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
            .collect();
        subs.sort_by_key(|sub| sub.get_name());
        for sub in subs {
            let mut path = prefix.clone();
            path.push(sub.get_name().to_owned());
            out.push(path.clone());
            walk(sub, path, out);
        }
    }
    let mut out = Vec::new();
    walk(&Cli::command(), Vec::new(), &mut out);
    out
}

fn writes_succeed_anyway(path: &str) -> bool {
    let dir = std::path::Path::new(path);
    let probe = dir.join(".velnorctl-write-probe");
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn tempfile_dir(label: &str) -> String {
    let base = std::env::temp_dir();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = base.join(format!(
        "velnorctl-cli-{label}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create scratch dir");
    path.to_string_lossy().into_owned()
}

fn make_read_only(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    let target = std::path::Path::new(path);
    let mut perms = std::fs::metadata(target).expect("stat").permissions();
    perms.set_mode(0o500);
    std::fs::set_permissions(target, perms).expect("chmod");
}

fn restore_writable(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    let target = std::path::Path::new(path);
    let mut perms = std::fs::metadata(target).expect("stat").permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(target, perms).expect("chmod");
}

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
fn cli_c005_version_prints_binary_and_workspace_version_to_stdout() {
    let output = run(&["--version"]);
    assert_eq!(code(&output), 0);
    assert!(text(&output.stderr).is_empty());
    let version = text(&output.stdout);
    assert!(version.starts_with("velnorctl "), "{version}");
    assert!(version.contains(env!("CARGO_PKG_VERSION")), "{version}");
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
    for name in ["daemon", "release"] {
        let output = run(&[name, "--help"]);
        assert_eq!(code(&output), 2, "{name}");
        assert!(
            text(&output.stderr).contains("unrecognized subcommand"),
            "{name}"
        );
    }
    let output = run(&["run", "--help"]);
    assert_eq!(code(&output), 2);
    assert!(text(&output.stderr).contains("unrecognized subcommand"));
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

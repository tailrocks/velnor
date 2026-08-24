//! Parser/dispatch seam integration tests (Plan 064 step 3): zero commands
//! may exit success, and registration stays open to future leaf modules.

use std::process::Command;

use velnorctl::cli::{parse_invocation, CliRegistry, CommandSpec, Invocation};
use velnorctl::commands::registry;
use velnorctl::dispatch::{dispatch_args, USAGE_EXIT_CODE};

/// Every legacy runner command name plus `version` (C001's future leaf) must
/// classify as unknown while no command is registered.
#[test]
fn legacy_and_reserved_names_never_match() {
    for name in [
        "cache",
        "capabilities",
        "configure",
        "daemon",
        "doctor",
        "preflight",
        "release",
        "remove",
        "run",
        "status",
        "storage",
        "version",
    ] {
        let parsed = parse_invocation(registry(), [name]);
        assert_eq!(
            parsed,
            Invocation::UnknownCommand { name: name.into() },
            "{name} must stay rejected until its own command task registers it"
        );
        assert!(!parsed.is_matched());
    }
}

#[test]
fn production_dispatch_exits_nonzero_for_every_invocation_shape() {
    for argv in [
        vec!["velnorctl"],
        vec!["velnorctl", "version"],
        vec!["velnorctl", "--version"],
        vec!["velnorctl", "-V"],
        vec!["velnorctl", "--help"],
        vec!["velnorctl", "cache", "du"],
        vec!["velnorctl", "cache", "gc", "--dry-run"],
        vec!["velnorctl", "capabilities", "check", "job.json"],
        vec!["velnorctl", "capabilities", "export"],
        vec!["velnorctl", "configure", "--url", "https://github.com/o/r"],
        vec!["velnorctl", "remove", "--local-only"],
        vec!["velnorctl", "status", "--slots", "2"],
        vec!["velnorctl", "run", "--once"],
        vec!["velnorctl", "daemon", "--slots", "2"],
    ] {
        let args: Vec<String> = argv[1..].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            dispatch_args(registry(), args),
            USAGE_EXIT_CODE,
            "{argv:?} must not exit success"
        );
    }
}

#[test]
fn binary_rejects_unknown_command_via_subprocess() {
    let bin = env!("CARGO_BIN_EXE_velnorctl");
    let output = Command::new(bin)
        .args(["cache", "du"])
        .env_remove("VELNORCTL_TEST")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(USAGE_EXIT_CODE));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown command 'cache'"),
        "stderr should explain the rejection, got: {stderr}"
    );

    let empty = Command::new(bin).output().unwrap();
    assert_eq!(empty.status.code(), Some(USAGE_EXIT_CODE));
}

#[test]
fn later_command_modules_can_register_without_domain_dependencies() {
    fn stub_handler(_args: &[String]) -> i32 {
        0
    }

    let mut local = CliRegistry::new();
    local
        .register(CommandSpec::new("probe", "integration stub", stub_handler))
        .unwrap();

    assert_eq!(local.len(), 1);
    assert!(local.contains("probe"));

    let parsed = parse_invocation(&local, ["probe"]);
    assert!(parsed.is_matched());

    // Duplicate names fail closed instead of silently replacing.
    let second = local.register(CommandSpec::new("probe", "again", stub_handler));
    assert!(second.is_err());
}

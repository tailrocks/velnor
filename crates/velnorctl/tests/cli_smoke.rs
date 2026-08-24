//! CLI smoke tests: parse/dispatch through the public library API plus a real
//! subprocess run of the built binary, without owning any leaf command.

use std::process::Command;

use velnorctl::{self, Outcome};

fn dispatch_exit(args: &[&str]) -> (Outcome, u8) {
    let argv: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let registry = velnorctl::compose();
    let outcome = velnorctl::dispatch(&registry, &argv);
    (outcome.clone(), outcome.exit_code())
}

#[test]
fn explicit_help_succeeds() {
    for args in [&["-h"][..], &["--help"][..]] {
        let (outcome, code) = dispatch_exit(args);
        assert!(matches!(outcome, Outcome::Help(_)));
        assert_eq!(code, 0);
    }
}

#[test]
fn bare_invocation_prints_usage_and_fails() {
    let (outcome, code) = dispatch_exit(&[]);
    assert!(matches!(outcome, Outcome::NoCommand(_)));
    assert_eq!(code, 2);
}

#[test]
fn legacy_runner_names_fail_like_the_binary_would() {
    for name in velnorctl::LEGACY_RUNNER_COMMANDS {
        let (outcome, code) = dispatch_exit(&[name]);
        assert_eq!(
            outcome,
            Outcome::LegacyRejected {
                name: name.to_owned()
            }
        );
        assert_ne!(code, 0);
    }
}

#[test]
fn unimplemented_future_commands_fail_without_placeholder_success() {
    for name in [
        "version",
        "list",
        "--version",
        "-V",
        "anything-unregistered",
    ] {
        let (outcome, code) = dispatch_exit(&[name]);
        assert!(!outcome.succeeded());
        assert_eq!(
            outcome,
            Outcome::Unimplemented {
                name: name.to_owned()
            }
        );
        assert_ne!(code, 0);
    }
}

#[test]
fn binary_smoke_help_success_and_rejections_over_subprocess() {
    let bin = env!("CARGO_BIN_EXE_velnorctl");

    let help = Command::new(bin).arg("--help").output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(stdout.contains(velnorctl::BIN_NAME), "stdout was: {stdout}");

    let bare = Command::new(bin).output().unwrap();
    assert_eq!(bare.status.code(), Some(2));

    let legacy = Command::new(bin).args(["cache", "du"]).output().unwrap();
    assert_eq!(legacy.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&legacy.stderr);
    assert!(
        stderr.contains("cache"),
        "stderr should name the rejected command, got: {stderr}"
    );

    let unknown = Command::new(bin).arg("version").output().unwrap();
    assert_eq!(unknown.status.code(), Some(2));
}

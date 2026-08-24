//! CLI smoke tests: exercise parse/dispatch through the public library API,
//! mirroring what the binary's `main` does, without spawning a subprocess.

use std::process::ExitCode;

use velnorctl::{self, Outcome};

fn dispatch_exit(args: &[&str]) -> (Outcome, ExitCode) {
    let argv: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let registry = velnorctl::compose();
    let outcome = velnorctl::dispatch(&registry, &argv);
    let code = ExitCode::from(outcome.exit_code());
    (outcome, code)
}

#[test]
fn help_without_subcommand_succeeds() {
    for args in [&[][..], &["-h"][..], &["--help"][..]] {
        let (outcome, code) = dispatch_exit(args);
        assert!(matches!(outcome, Outcome::Usage(_)));
        assert_eq!(code, ExitCode::SUCCESS);
    }
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
        assert_ne!(code, ExitCode::SUCCESS);
    }
}

#[test]
fn unimplemented_future_commands_fail_without_placeholder_success() {
    for name in ["version", "list", "anything"] {
        let (outcome, code) = dispatch_exit(&[name]);
        assert!(!outcome.succeeded());
        assert_ne!(code, ExitCode::SUCCESS);
    }
}

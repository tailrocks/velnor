//! Dispatch seam: maps a classified invocation onto process behavior.
//!
//! Until a leaf command registers, every invocation is rejected with the
//! usage exit code; nothing can exit success.

use crate::cli::{CliRegistry, Invocation};

/// Usage-error exit code. Provisional until Plan 065 defines `ExitClass`;
/// class 2 (`Usage`) already matches that mapping.
pub const USAGE_EXIT_CODE: i32 = 2;

/// Classify `args` (without program name) and run the matched command.
///
/// Returns the process exit code: 0 only when a registered handler returned
/// 0, nonzero for every unknown or missing command.
pub fn dispatch_args<I, S>(registry: &CliRegistry, args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let invocation = crate::cli::parse_invocation(registry, args);
    match invocation {
        Invocation::Matched { name, args } => {
            let Some(spec) = registry.get(&name) else {
                eprintln!("velnorctl: internal error: '{name}' matched but is not registered");
                return USAGE_EXIT_CODE;
            };
            let handler = spec.handler;
            handler(&args)
        }
        Invocation::UnknownCommand { name } => {
            if registry.is_empty() {
                eprintln!("velnorctl: unknown command '{name}': no commands are registered yet");
            } else {
                let known = registry.names().collect::<Vec<_>>().join(", ");
                eprintln!("velnorctl: unknown command '{name}'; registered commands: {known}");
            }
            USAGE_EXIT_CODE
        }
        Invocation::MissingCommand => {
            eprintln!("velnorctl: missing command");
            USAGE_EXIT_CODE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CommandSpec;
    use crate::commands::registry;

    #[test]
    fn production_registry_rejects_every_invocation_with_usage_exit() {
        for args in [
            vec!["version"],
            vec!["cache", "du"],
            vec!["capabilities", "export"],
            vec!["configure", "--url", "https://github.com/o/r"],
            vec!["remove"],
            vec!["status"],
            vec!["run", "--once"],
            vec!["daemon"],
            vec![],
            vec!["--help"],
            vec!["--version"],
        ] {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            assert_eq!(
                dispatch_args(registry(), args),
                USAGE_EXIT_CODE,
                "invocation must not exit success"
            );
        }
    }

    #[test]
    fn registered_handler_result_becomes_exit_code() {
        static HANDLED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        fn counting_handler(args: &[String]) -> i32 {
            HANDLED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if args == ["expected-arg"] {
                0
            } else {
                7
            }
        }

        let mut local = CliRegistry::new();
        local
            .register(CommandSpec::new("probe", "test-only", counting_handler))
            .unwrap();

        let ok = ["probe".to_string(), "expected-arg".to_string()];
        assert_eq!(dispatch_args(&local, ok), 0);
        assert_eq!(HANDLED.load(std::sync::atomic::Ordering::SeqCst), 1);

        let failing = ["probe".to_string()];
        assert_eq!(dispatch_args(&local, failing), 7);
    }
}

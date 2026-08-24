//! `velnorctl` — the Velnor operator CLI.
//!
//! Plan 064 scaffold: this crate owns the global parser and dispatch
//! composition points and deliberately no leaf command. Command tasks C001
//! onward register their handlers through [`compose`] without depending on any
//! daemon or runner domain internals.
//!
//! Dependency law (Plan 064): this crate may depend on `velnor-model`,
//! `velnor-control`, `velnor-client`, `velnor-render`, and (interim, until
//! Plan 079) a narrow public facade of `velnor-runner`. It never re-exports
//! legacy `velnor-runner` command names.

use std::collections::BTreeMap;
use std::sync::Arc;

/// Binary name used in generated usage text.
pub const BIN_NAME: &str = "velnorctl";

/// Command names owned by the legacy `velnor-runner` binary.
///
/// The migration has zero backward-compatible aliases: every one of these is
/// rejected by [`dispatch`] forever, and [`Registry::register`] refuses them.
pub const LEGACY_RUNNER_COMMANDS: [&str; 11] = [
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
];

/// Handler a leaf-command module registers during composition.
///
/// Receiving raw trailing argv keeps command modules decoupled from global
/// parser internals; later plans may widen this signature per command task.
pub type Handler = Arc<dyn Fn(&[String]) -> Result<(), String> + Send + Sync>;

/// Result of parsing and dispatching one argv through [`dispatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Global help request (`-h`/`--help`, or no arguments): print usage and
    /// exit success. Not a leaf command.
    Usage(String),
    /// A registered handler completed successfully.
    Handled {
        name: String,
    },
    /// The named command parsed but no implementation is registered in this
    /// build: always exit failure. No unimplemented placeholder ever succeeds.
    Unimplemented {
        name: String,
    },
    /// A legacy `velnor-runner` command name was rejected: always exit failure.
    LegacyRejected {
        name: String,
    },
    /// A registered handler reported failure: always exit failure.
    CommandFailed {
        name: String,
        message: String,
    },
}

impl Outcome {
    /// True only for outcomes allowed to exit success.
    pub fn succeeded(&self) -> bool {
        matches!(self, Outcome::Usage(_) | Outcome::Handled { .. })
    }

    /// Process exit code for this outcome. Every variant except [`Self::Usage`]
    /// and [`Self::Handled`] is nonzero.
    pub fn exit_code(&self) -> u8 {
        match self {
            Outcome::Usage(_) | Outcome::Handled { .. } => 0,
            Outcome::CommandFailed { .. } => 1,
            Outcome::Unimplemented { .. } => 2,
            Outcome::LegacyRejected { .. } => 3,
        }
    }
}

/// Registry of leaf commands composed at startup.
#[derive(Default)]
pub struct Registry {
    handlers: BTreeMap<String, Handler>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a leaf-command handler under its exact subcommand name.
    ///
    /// Panics on a legacy runner alias or a duplicate registration: both are
    /// composition-time bugs, not runtime conditions.
    pub fn register(&mut self, name: &str, handler: Handler) {
        assert!(
            !LEGACY_RUNNER_COMMANDS.contains(&name),
            "velnorctl must never register legacy velnor-runner command alias '{name}'"
        );
        let previous = self.handlers.insert(name.to_owned(), handler);
        assert!(
            previous.is_none(),
            "velnorctl command '{name}' registered twice"
        );
    }

    /// True when `name` has a registered implementation.
    pub fn is_registered(&self, name: &str) -> bool {
        self.handlers.contains_key(name)
    }
}

/// Global composition point.
///
/// Every leaf-command task adds its registration here. Plan 064 owns no leaf
/// command, so the composed registry is empty by design.
pub fn compose() -> Registry {
    Registry::new()
}

/// Parse `args` (argv without the binary name) and dispatch through
/// `registry`.
///
/// This is the single parser/dispatch seam: help requests succeed, every other
/// outcome requires either a registered handler or exits nonzero.
pub fn dispatch(registry: &Registry, args: &[String]) -> Outcome {
    let Some(name) = args.first() else {
        return Outcome::Usage(usage());
    };
    if name == "-h" || name == "--help" {
        return Outcome::Usage(usage());
    }
    if LEGACY_RUNNER_COMMANDS.contains(&name.as_str()) {
        return Outcome::LegacyRejected {
            name: name.clone(),
        };
    }
    match registry.handlers.get(name) {
        Some(handler) => match handler(&args[1..]) {
            Ok(()) => Outcome::Handled {
                name: name.clone(),
            },
            Err(message) => Outcome::CommandFailed {
                name: name.clone(),
                message,
            },
        },
        None => Outcome::Unimplemented {
            name: name.clone(),
        },
    }
}

/// Usage text printed for help outcomes.
pub fn usage() -> String {
    format!(
        "{BIN_NAME} — Velnor operator CLI\n\n\
         USAGE:\n    {BIN_NAME} <COMMAND> [ARGS]...\n\n\
         Commands are added by later migration tasks; unimplemented and legacy\n\
         velnor-runner command names always fail.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn every_legacy_runner_command_is_rejected() {
        let registry = compose();
        for name in LEGACY_RUNNER_COMMANDS {
            let outcome = dispatch(&registry, &argv(&[name]));
            assert_eq!(
                outcome,
                Outcome::LegacyRejected {
                    name: name.to_owned()
                }
            );
            assert!(!outcome.succeeded());
            assert_ne!(outcome.exit_code(), 0);
            assert!(!registry.is_registered(name));
        }
    }

    #[test]
    fn no_unimplemented_command_exits_success() {
        let registry = compose();
        assert!(registry.handlers.is_empty());
        for name in ["version", "fleet", "unknown-future-command", "--version", "-V"] {
            let outcome = dispatch(&registry, &argv(&[name]));
            assert_eq!(
                outcome,
                Outcome::Unimplemented {
                    name: name.to_owned()
                }
            );
            assert!(!outcome.succeeded());
            assert_ne!(outcome.exit_code(), 0);
        }
    }

    #[test]
    fn empty_argv_and_help_are_usage_success() {
        let registry = compose();
        for args in [vec![], vec!["-h".to_owned()], vec!["--help".to_owned()]] {
            let outcome = dispatch(&registry, &args);
            assert_eq!(outcome, Outcome::Usage(usage()));
            assert!(outcome.succeeded());
            assert_eq!(outcome.exit_code(), 0);
            assert!(usage().contains(BIN_NAME));
        }
    }

    #[test]
    fn later_command_modules_register_without_domain_internals() {
        let mut registry = compose();
        let called = AtomicBool::new(false);
        let captured = AtomicBool::new(false);
        registry.register(
            "demo",
            Arc::new(move |args: &[String]| {
                called.store(true, Ordering::SeqCst);
                if args.first().map(String::as_str) == Some("--flag") {
                    captured.store(true, Ordering::SeqCst);
                }
                Ok(())
            }),
        );
        assert!(registry.is_registered("demo"));
        let outcome = dispatch(&registry, &argv(&["demo", "--flag"]));
        assert_eq!(
            outcome,
            Outcome::Handled {
                name: "demo".to_owned()
            }
        );
        assert!(outcome.succeeded());
        assert!(called.load(Ordering::SeqCst));
        assert!(captured.load(Ordering::SeqCst));
    }

    #[test]
    fn failing_handler_exits_nonzero() {
        let mut registry = compose();
        registry.register("demo", Arc::new(|_| Err("boom".to_owned())));
        let outcome = dispatch(&registry, &argv(&["demo"]));
        assert_eq!(
            outcome,
            Outcome::CommandFailed {
                name: "demo".to_owned(),
                message: "boom".to_owned()
            }
        );
        assert!(!outcome.succeeded());
        assert_eq!(outcome.exit_code(), 1);
    }

    #[test]
    #[should_panic(expected = "legacy velnor-runner command alias")]
    fn legacy_names_can_never_register() {
        let mut registry = compose();
        registry.register("cache", Arc::new(|_| Ok(())));
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn duplicate_registration_panics() {
        let mut registry = compose();
        registry.register("demo", Arc::new(|_| Ok(())));
        registry.register("demo", Arc::new(|_| Ok(())));
    }
}

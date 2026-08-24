//! `velnorctl` — the Velnor operator CLI.
//!
//! Plan 065 adds the global CLI conventions: global flags parse before or
//! after any subcommand, every outcome maps onto the one public
//! [`velnor_model::ExitClass`] contract, and command metadata interfaces
//! feed the future schema/help/completion/man leaf commands (C002-C005).
//!
//! Dependency law (Plan 064): this crate may depend on `velnor-model`,
//! `velnor-control`, `velnor-client`, `velnor-render`, and (interim, until
//! Plan 079) a narrow public facade of `velnor-runner`. It never re-exports
//! legacy `velnor-runner` command names.

use std::collections::BTreeMap;
use std::sync::Arc;

use velnor_model::{ExitClass, MachineErrorEnvelope};

pub mod globals;
pub mod man;
pub mod metadata;

pub use globals::{
    parse_invocation, usage_envelope, ParseOutcome, ParsedGlobals, Shell, USAGE_REASON,
};
pub use metadata::{
    completion_script, global_flags, help_text, man_page, schema_document, CompletionFlavor,
    DocumentedCommand,
};

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

/// Error a leaf-command handler returns, carrying its exit class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError {
    /// Exit class; handlers refine reasons, never numeric codes.
    pub class: ExitClass,
    /// Stable machine reason token.
    pub reason: String,
    /// Human-facing message.
    pub message: String,
}

impl CommandError {
    /// Build an error with an explicit class.
    #[must_use]
    pub fn new(class: ExitClass, reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            class,
            reason: reason.into(),
            message: message.into(),
        }
    }

    /// A domain operation reached a definite failure.
    #[must_use]
    pub fn operation(message: impl Into<String>) -> Self {
        Self::new(ExitClass::Operation, "operation.failed", message)
    }

    /// An authoritative resource was absent or not found.
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ExitClass::Unavailable, "resource.not_found", message)
    }

    /// An inspection authoritatively found a degraded condition.
    #[must_use]
    pub fn condition(message: impl Into<String>) -> Self {
        Self::new(ExitClass::Condition, "condition.degraded", message)
    }

    /// The machine error envelope for this failure.
    #[must_use]
    pub fn envelope(&self) -> MachineErrorEnvelope {
        MachineErrorEnvelope::new(self.class.as_str(), self.class.code(), &self.reason)
            .with_remediation(&self.message)
    }
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::operation(message)
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self::operation(message.to_owned())
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{}:{}]",
            self.message,
            self.class.code(),
            self.reason
        )
    }
}

/// Handler a leaf-command module registers during composition.
///
/// Receiving raw trailing argv keeps command modules decoupled from global
/// parser internals; later plans may widen this signature per command task.
pub type Handler = Arc<dyn Fn(&[String]) -> Result<(), CommandError> + Send + Sync>;

/// Result of parsing and dispatching one argv through [`dispatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Explicit `-h`/`--help`: print usage and exit success. Not a leaf command.
    Help(String),
    /// Explicit `--version`: print version and exit success.
    Version(String),
    /// No command name at all: print usage to stderr, exit `Usage`.
    NoCommand(String),
    /// Invalid invocation surfaced while parsing globals: usage text on
    /// stderr, exit `Usage`.
    Usage(String),
    /// A registered handler completed successfully.
    Handled { name: String },
    /// The named command parsed but no implementation is registered in this
    /// build: always exit failure. No unimplemented placeholder ever succeeds.
    Unimplemented { name: String },
    /// A legacy `velnor-runner` command name was rejected: always exit failure.
    LegacyRejected { name: String },
    /// A registered handler reported failure; the class comes from the
    /// handler's [`CommandError`], never from ad hoc mapping.
    CommandFailed { name: String, error: CommandError },
}

impl Outcome {
    /// True only for outcomes allowed to exit success.
    pub fn succeeded(&self) -> bool {
        matches!(
            self,
            Outcome::Help(_) | Outcome::Version(_) | Outcome::Handled { .. }
        )
    }

    /// The single exit-class mapping for this outcome.
    #[must_use]
    pub fn exit_class(&self) -> ExitClass {
        match self {
            Outcome::Help(_) | Outcome::Version(_) | Outcome::Handled { .. } => ExitClass::Success,
            Outcome::NoCommand(_)
            | Outcome::Usage(_)
            | Outcome::Unimplemented { .. }
            | Outcome::LegacyRejected { .. } => ExitClass::Usage,
            Outcome::CommandFailed { error, .. } => error.class,
        }
    }

    /// Process exit code derived from [`Self::exit_class`] only.
    pub fn exit_code(&self) -> u8 {
        u8::try_from(velnor_model::exit_code_for_class(self.exit_class())).unwrap_or(u8::MAX)
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
/// Every leaf-command task adds its registration here. Each documented leaf
/// publishes its [`CommandMetadata`] alongside its handler so metadata
/// consumers (`man`, and later `schema`/`help`/`completion`) always render
/// the exact registered surface.
pub fn compose() -> Registry {
    let mut registry = Registry::new();
    let mut documents: Vec<velnor_model::CommandMetadata> = Vec::new();
    let man = man::ManCommand;
    documents.push(man.metadata());
    registry.register("man", man::handler(documents));
    registry
}

/// Parse globals out of `argv` and dispatch the remainder through `registry`.
///
/// This is the single parser/dispatch seam: help and version succeed, every
/// other outcome requires either a registered handler or exits nonzero with
/// its mapped class.
pub fn run(registry: &Registry, args: &[String]) -> Outcome {
    match parse_invocation(args) {
        ParseOutcome::Help(text) => Outcome::Help(text),
        ParseOutcome::Version(text) => Outcome::Version(text),
        ParseOutcome::Usage(text) => Outcome::Usage(text),
        ParseOutcome::Ok(parsed) => dispatch(registry, &parsed.rest),
    }
}

/// Dispatch an already-parsed remainder through `registry`.
///
/// The first element must be the subcommand name; the rest are passed to the
/// registered handler verbatim.
pub fn dispatch(registry: &Registry, rest: &[String]) -> Outcome {
    let Some(name) = rest.first() else {
        return Outcome::NoCommand(usage());
    };
    if LEGACY_RUNNER_COMMANDS.contains(&name.as_str()) {
        return Outcome::LegacyRejected { name: name.clone() };
    }
    match registry.handlers.get(name) {
        Some(handler) => match handler(&rest[1..]) {
            Ok(()) => Outcome::Handled { name: name.clone() },
            Err(error) => Outcome::CommandFailed {
                name: name.clone(),
                error,
            },
        },
        None => Outcome::Unimplemented { name: name.clone() },
    }
}

/// Usage text printed when no command name is present.
pub fn usage() -> String {
    format!(
        "{BIN_NAME} — Velnor operator CLI\n\n\
         USAGE:\n    {BIN_NAME} [GLOBAL FLAGS] <COMMAND> [ARGS]...\n\n\
         Global flags accept placement before or after the subcommand;\n\
         unimplemented and legacy velnor-runner command names always fail.\n"
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
    fn every_legacy_runner_command_is_rejected_as_usage() {
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
            assert_eq!(outcome.exit_class(), ExitClass::Usage);
            assert_eq!(outcome.exit_code(), 2);
            assert!(!registry.is_registered(name));
        }
    }

    #[test]
    fn no_unimplemented_command_exits_success() {
        let registry = compose();
        assert!(!registry.is_registered("nonexistent"));
        for name in ["version", "fleet", "unknown-future-command"] {
            let outcome = dispatch(&registry, &argv(&[name]));
            assert_eq!(
                outcome,
                Outcome::Unimplemented {
                    name: name.to_owned()
                }
            );
            assert_eq!(outcome.exit_class(), ExitClass::Usage);
            assert_ne!(outcome.exit_code(), 0);
        }
    }

    #[test]
    fn help_version_and_bare_invocation_follow_the_contract() {
        let registry = compose();
        let outcome = run(&registry, &argv(&["-h"]));
        assert!(matches!(outcome, Outcome::Help(_)));
        assert!(outcome.succeeded());
        let outcome = run(&registry, &argv(&["--version"]));
        assert!(matches!(outcome, Outcome::Version(_)));
        assert!(outcome.succeeded());
        let bare = run(&registry, &[]);
        assert!(matches!(bare, Outcome::NoCommand(_)));
        assert_eq!(bare.exit_code(), 2);
        assert!(usage().contains(BIN_NAME));
    }

    #[test]
    fn globals_parse_before_and_after_the_subcommand() {
        for placement in [
            vec!["--output", "json", "--no-color"],
            vec!["--no-color", "--output", "json"],
        ] {
            let mut line = placement;
            line.push("demo");
            let parsed = match parse_invocation(&argv(&line)) {
                ParseOutcome::Ok(parsed) => parsed,
                other => panic!("expected Ok, got {other:?}"),
            };
            assert_eq!(parsed.output_format(), velnor_render::OutputFormat::Json);
            assert!(parsed.no_color);
            assert_eq!(parsed.rest, vec!["demo".to_owned()]);
        }
    }

    #[test]
    fn invalid_global_values_exit_usage_not_success() {
        let registry = compose();
        for bad in [
            vec!["--output", "csv", "demo"],
            vec!["--timeout", "soon", "demo"],
            vec!["--context"],
        ] {
            let outcome = run(&registry, &argv(&bad));
            assert!(
                matches!(outcome, Outcome::Usage(_)),
                "{bad:?} should be Usage, got {outcome:?}"
            );
            assert_eq!(outcome.exit_class(), ExitClass::Usage);
            assert_eq!(outcome.exit_code(), 2);
        }
    }

    #[test]
    fn later_command_modules_register_without_domain_internals() {
        let mut registry = compose();
        let called = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(AtomicBool::new(false));
        let handler_called = called.clone();
        let handler_captured = captured.clone();
        registry.register(
            "demo",
            Arc::new(move |args: &[String]| {
                handler_called.store(true, Ordering::SeqCst);
                if args.first().map(String::as_str) == Some("--flag") {
                    handler_captured.store(true, Ordering::SeqCst);
                }
                Ok(())
            }),
        );
        assert!(registry.is_registered("demo"));
        let outcome = run(&registry, &argv(&["demo", "--flag"]));
        assert_eq!(
            outcome,
            Outcome::Handled {
                name: "demo".to_owned()
            }
        );
        assert!(outcome.succeeded());
        assert_eq!(outcome.exit_class(), ExitClass::Success);
        assert!(called.load(Ordering::SeqCst));
        assert!(captured.load(Ordering::SeqCst));
    }

    #[test]
    fn handler_error_classes_survive_dispatch_and_cannot_collapse() {
        let mut registry = compose();
        registry.register(
            "inspect",
            Arc::new(|_| Err(CommandError::condition("slot degraded"))),
        );
        registry.register(
            "fetch",
            Arc::new(|_| {
                Err(CommandError::new(
                    ExitClass::Transport,
                    "broker.unreachable",
                    "upstream down",
                ))
            }),
        );
        registry.register(
            "boom",
            Arc::new(|_| Err(CommandError::from("plain failure"))),
        );

        let inspect = run(&registry, &argv(&["inspect"]));
        assert_eq!(inspect.exit_class(), ExitClass::Condition);
        assert_eq!(inspect.exit_code(), 1);

        let fetch = run(&registry, &argv(&["fetch"]));
        assert_eq!(fetch.exit_class(), ExitClass::Transport);
        assert_eq!(fetch.exit_code(), 7);

        let boom = run(&registry, &argv(&["boom"]));
        assert_eq!(boom.exit_class(), ExitClass::Operation);
        assert_eq!(boom.exit_code(), 8);
    }

    #[test]
    fn legacy_names_can_never_register() {
        let mut registry = compose();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.register("cache", Arc::new(|_| Ok(())));
        }));
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_registration_panics() {
        let mut registry = compose();
        registry.register("demo", Arc::new(|_| Ok(())));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            registry.register("demo", Arc::new(|_| Ok(())));
        }));
        assert!(result.is_err());
    }
}

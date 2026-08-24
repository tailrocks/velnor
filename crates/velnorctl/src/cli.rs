//! Parser seam: a command registry plus invocation classification.
//!
//! Leaf command modules register [`CommandSpec`]s into one [`CliRegistry`];
//! nothing here depends on domain internals, so registration stays open to
//! every future command module without pulling in shared service crates.

use std::collections::BTreeMap;

/// Handler for one registered leaf command. Receives the arguments after the
/// command name; returns the process exit code. Plan 065 replaces the raw
/// code with the shared `ExitClass` mapping.
pub type CommandHandler = fn(&[String]) -> i32;

/// One registrable leaf command.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// Canonical command name (first positional argument).
    pub name: &'static str,
    /// Short help text; rendered once help output exists.
    pub about: &'static str,
    /// Entry point invoked when this command matches.
    pub handler: CommandHandler,
}

impl CommandSpec {
    /// Convenience constructor for specs with no behavior yet.
    pub fn new(name: &'static str, about: &'static str, handler: CommandHandler) -> Self {
        Self {
            name,
            about,
            handler,
        }
    }
}

/// Error returned when two commands claim the same name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCommandError {
    pub name: String,
}

impl std::fmt::Display for DuplicateCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "command '{}' is already registered", self.name)
    }
}

impl std::error::Error for DuplicateCommandError {}

/// Registry of leaf commands. Order-independent lookup; iteration is sorted by
/// name so help and completion output stay deterministic.
#[derive(Debug, Default, Clone)]
pub struct CliRegistry {
    commands: BTreeMap<String, CommandSpec>,
}

impl CliRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one leaf command. Rejects duplicate names fail-closed.
    pub fn register(&mut self, spec: CommandSpec) -> Result<(), DuplicateCommandError> {
        let name = spec.name.to_string();
        if self.commands.contains_key(&name) {
            return Err(DuplicateCommandError { name });
        }
        self.commands.insert(name, spec);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&CommandSpec> {
        self.commands.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.commands.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.commands.len()
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Registered names, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.commands.keys().map(String::as_str)
    }
}

/// Outcome of classifying one argument vector against a registry.
///
/// Arguments are passed WITHOUT the program name (`std::env::args().skip(1)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// First positional argument names a registered command.
    Matched { name: String, args: Vec<String> },
    /// The first token is not a registered command name. Flag-like tokens
    /// (`--help`, `--version`, …) land here too while no global flags exist,
    /// so no invocation can exit success before Plan 065 defines them.
    UnknownCommand { name: String },
    /// No arguments at all.
    MissingCommand,
}

impl Invocation {
    /// True only when a registered command matched. Every other state must map
    /// to a nonzero process exit.
    pub fn is_matched(&self) -> bool {
        matches!(self, Invocation::Matched { .. })
    }
}

/// Classify `args` against `registry`.
pub fn parse_invocation<I, S>(registry: &CliRegistry, args: I) -> Invocation
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut iter = args.into_iter().map(Into::into);
    let Some(first) = iter.next() else {
        return Invocation::MissingCommand;
    };
    if registry.contains(&first) {
        Invocation::Matched {
            args: iter.collect(),
            name: first,
        }
    } else {
        Invocation::UnknownCommand { name: first }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_handler(_args: &[String]) -> i32 {
        0
    }

    #[test]
    fn fresh_registry_is_empty() {
        let registry = CliRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(!registry.contains("version"));
    }

    #[test]
    fn register_then_lookup_round_trips() {
        let mut registry = CliRegistry::new();
        registry
            .register(CommandSpec::new("alpha", "about alpha", noop_handler))
            .unwrap();
        assert!(registry.contains("alpha"));
        assert_eq!(registry.get("alpha").unwrap().about, "about alpha");
        assert_eq!(registry.names().collect::<Vec<_>>(), ["alpha"]);
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let mut registry = CliRegistry::new();
        registry
            .register(CommandSpec::new("dup", "first", noop_handler))
            .unwrap();
        let error = registry
            .register(CommandSpec::new("dup", "second", noop_handler))
            .unwrap_err();
        assert_eq!(error.name, "dup");
        // First registration wins; the rejected spec never lands.
        assert_eq!(registry.get("dup").unwrap().about, "first");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn empty_args_classify_as_missing_command() {
        let registry = CliRegistry::new();
        assert_eq!(
            parse_invocation(&registry, Vec::<String>::new()),
            Invocation::MissingCommand
        );
        assert!(!parse_invocation(&registry, Vec::<String>::new()).is_matched());
    }

    #[test]
    fn unregistered_names_classify_as_unknown_command() {
        let registry = CliRegistry::new();
        for name in [
            "cache",
            "capabilities",
            "configure",
            "remove",
            "status",
            "run",
            "daemon",
            "preflight",
            "storage",
            "doctor",
            "release",
            "version",
        ] {
            let parsed = parse_invocation(&registry, [name.to_string()]);
            assert_eq!(parsed, Invocation::UnknownCommand { name: name.into() });
            assert!(!parsed.is_matched());
        }
    }

    #[test]
    fn flag_like_tokens_are_unknown_while_no_global_flags_exist() {
        let registry = CliRegistry::new();
        for token in ["--help", "-h", "--version", "-V"] {
            let parsed = parse_invocation(&registry, [token.to_string()]);
            assert_eq!(parsed, Invocation::UnknownCommand { name: token.into() },);
        }
    }

    #[test]
    fn matched_invocation_carries_remaining_args() {
        let mut registry = CliRegistry::new();
        registry
            .register(CommandSpec::new("alpha", "about", noop_handler))
            .unwrap();
        let parsed = parse_invocation(&registry, ["alpha", "--flag", "value"]);
        assert_eq!(
            parsed,
            Invocation::Matched {
                name: "alpha".into(),
                args: vec!["--flag".into(), "value".into()],
            }
        );
    }
}

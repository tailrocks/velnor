//! Schema, help, completion, and man metadata interfaces (Plan 065).
//!
//! This module defines the interfaces only: it exposes the metadata every
//! leaf command must publish and deterministic renderers over that
//! metadata. Leaf commands C002-C005 own the actual `schema`, `help`,
//! `completion`, and `man` commands and consume these types without any
//! domain duplication.

use velnor_model::{CommandMetadata, FlagMetadata, SchemaDocument};

use crate::globals::Shell;

/// Metadata for one global flag; the single source mirroring [`crate::globals::Cli`].
#[must_use]
pub fn global_flags() -> Vec<FlagMetadata> {
    vec![
        FlagMetadata {
            long: "context".to_owned(),
            short: None,
            value_name: Some("<NAME>".to_owned()),
            help: "named connection context".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "output".to_owned(),
            short: Some('o'),
            value_name: Some("<FORMAT>".to_owned()),
            help: "output format: table|wide|json|yaml|jsonl|name".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "instance".to_owned(),
            short: None,
            value_name: Some("<NAME>".to_owned()),
            help: "restrict to one daemon instance".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "repo".to_owned(),
            short: None,
            value_name: Some("<REPO>".to_owned()),
            help: "restrict to one repository (owner/name)".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "selector".to_owned(),
            short: None,
            value_name: Some("<SELECTOR>".to_owned()),
            help: "include-only filter over resource fields".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "field-selector".to_owned(),
            short: None,
            value_name: Some("<SELECTOR>".to_owned()),
            help: "field equality selector (key=value)".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "since".to_owned(),
            short: None,
            value_name: Some("<SINCE>".to_owned()),
            help: "lower time bound: RFC 3339 or relative duration".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "timeout".to_owned(),
            short: None,
            value_name: Some("<SECONDS>".to_owned()),
            help: "deadline in seconds before the command exits with TIMEOUT".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "no-color".to_owned(),
            short: None,
            value_name: None,
            help: "disable ANSI styling regardless of TTY detection".to_owned(),
            global: true,
        },
        FlagMetadata {
            long: "verbose".to_owned(),
            short: Some('v'),
            value_name: None,
            help: "increase verbosity; repeatable".to_owned(),
            global: true,
        },
    ]
}

/// Build the schema document served by the future `schema` command.
#[must_use]
pub fn schema_document(commands: Vec<CommandMetadata>) -> SchemaDocument {
    SchemaDocument {
        binary: crate::BIN_NAME.to_owned(),
        version: velnor_model::CRATE_VERSION.to_owned(),
        global_flags: global_flags(),
        commands,
    }
}

/// Shell completion flavors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionFlavor {
    Bash,
    Zsh,
    Fish,
}

impl From<Shell> for CompletionFlavor {
    fn from(shell: Shell) -> Self {
        match shell {
            Shell::Bash => CompletionFlavor::Bash,
            Shell::Zsh => CompletionFlavor::Zsh,
            Shell::Fish => CompletionFlavor::Fish,
        }
    }
}

impl CompletionFlavor {
    /// Canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CompletionFlavor::Bash => "bash",
            CompletionFlavor::Zsh => "zsh",
            CompletionFlavor::Fish => "fish",
        }
    }

    /// Parse from the canonical spelling.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Shell::ALL
            .iter()
            .copied()
            .find(|shell| shell.as_str() == raw)
            .map(CompletionFlavor::from)
    }
}

/// Render plain-text help from a schema document.
#[must_use]
pub fn help_text(document: &SchemaDocument) -> String {
    let mut text = format!(
        "{} {}\n\nUSAGE:\n    {} <COMMAND> [ARGS]...\n\nGLOBAL FLAGS:\n",
        document.binary, document.version, document.binary
    );
    for flag in &document.global_flags {
        text.push_str(&format!("    {}\n", flag.invocation()));
    }
    if document.commands.is_empty() {
        text.push_str("\nCOMMANDS:\n    (no leaf commands registered yet)\n");
    } else {
        text.push_str("\nCOMMANDS:\n");
        for command in &document.commands {
            text.push_str(&format!("    {:<14} {}\n", command.name, command.about));
        }
    }
    text.push_str(&format!(
        "\nRun '{} <command> --help' for command-specific help.\n",
        document.binary
    ));
    text
}

/// Render a completion script registering the documented surface.
#[must_use]
pub fn completion_script(flavor: CompletionFlavor, document: &SchemaDocument) -> String {
    let binary = &document.binary;
    let command_names: Vec<String> = document.commands.iter().map(|c| c.name.clone()).collect();
    let mut script = match flavor {
        CompletionFlavor::Bash => format!(
            "# {binary} bash completion\n_{binary}() {{\n  local commands=\"{}\"\n  case ${{COMP_CWORD}} in\n    1) COMPREPLY=( $(compgen -W \"$commands --help --version\" -- \"${{COMP_WORDS[COMP_CWORD]}}\") ) ;;\n    *) COMPREPLY=( $(compgen -W \"--output= --no-color --verbose\" -- \"${{COMP_WORDS[COMP_CWORD]}}\") ) ;;\n  esac\n}}\ncomplete -F _{binary} {binary}\n",
            command_names.join(" ")
        ),
        CompletionFlavor::Zsh => format!(
            "#compdef {binary}\n_{binary}() {{\n  local -a commands\n  commands=({})\n  _describe 'command' commands\n}}\ncompdef _{binary} {binary}\n",
            command_names
                .iter()
                .map(|name| format!("'{name}:command'"))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        CompletionFlavor::Fish => {
            let mut fish = format!("# {binary} fish completion\n");
            for name in &command_names {
                fish.push_str(&format!("complete -c {binary} -n '__fish_use_subcommand' -a '{name}'\n"));
            }
            fish.push_str(&format!(
                "complete -c {binary} -l output -o o -x -a 'table wide json yaml jsonl name'\n"
            ));
            fish.push_str(&format!("complete -c {binary} -l no-color -d 'disable color'\n"));
            fish
        }
    };
    if command_names.is_empty() {
        script.push_str("# (no leaf commands registered yet)\n");
    }
    script
}

/// Render the body sections (NAME through OPTIONS) of one command's man
/// page, shared verbatim by [`man_page`] and the combined `man` page so a
/// leaf command renders identically everywhere.
#[must_use]
pub fn command_man_sections(binary: &str, command: &CommandMetadata) -> String {
    let upper = command.name.to_uppercase();
    let mut sections = format!(".SH NAME\n{binary}-{upper}\n");
    sections.push_str(&format!(".SH SYNOPSIS\n.B {binary} {}\n", command.name));
    sections.push_str(&format!(".SH DESCRIPTION\n{}\n", command.about));
    if !command.flags.is_empty() {
        sections.push_str(".SH OPTIONS\n");
        for flag in &command.flags {
            sections.push_str(&format!(
                ".TP\n\\fB{}\\fR\n{}\n",
                flag.invocation(),
                flag.help
            ));
        }
    }
    sections
}

/// Render a roff man page for one command's metadata.
#[must_use]
pub fn man_page(binary: &str, command: &CommandMetadata) -> String {
    let head = format!(
        ".TH {binary} 1 \"{}\" \"{binary} {}\" \"Velnor Manual\"\n",
        velnor_model::CRATE_VERSION,
        velnor_model::CRATE_VERSION
    );
    format!("{head}{}", command_man_sections(binary, command))
}

/// Registry-side seam: leaf commands publish metadata through composition.
pub trait DocumentedCommand {
    /// The command's published CLI metadata.
    fn metadata(&self) -> CommandMetadata;
}

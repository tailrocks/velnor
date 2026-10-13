//! `velnorctl` — the Velnor operator CLI.
//!
//! The command-line surface is a native, modern [`clap`] application: one
//! typed command tree ([`Cli`]) is the single source of truth for parsing,
//! help, version, usage errors, shell completions (`clap_complete`), and man
//! pages (`clap_mangen`). Commands dispatch exhaustively over typed
//! [`Command`] variants; runtime failures flow through the one public
//! [`velnor_model::ExitClass`] contract.
//!
//! Dependency law (Plan 064): this crate may depend on `velnor-model`,
//! `velnor-control`, `velnor-client`, `velnor-render`, and (interim, until
//! Plan 079) a narrow public facade of `velnor-runner`. Domain crates never
//! depend on `clap`; CLI-facing enums convert explicitly at this boundary.

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use velnor_model::{
    CommandMetadata, ExitClass, FlagMetadata, MachineErrorEnvelope, SchemaDocument,
};
use velnor_render::{ColorPolicy, OutputFormat};

pub mod completion;
pub mod man;

/// Binary name used across generated surfaces.
pub const BIN_NAME: &str = "velnorctl";

/// CLI-facing output-format choice converted explicitly into the domain
/// renderer type so `velnor-render` stays framework-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputArg {
    /// Human-oriented default table.
    Table,
    /// Extended human table.
    Wide,
    /// Versioned JSON resources.
    Json,
    /// Versioned YAML resources.
    Yaml,
    /// One versioned JSON object per line.
    Jsonl,
    /// Unversioned newline-delimited identity projection.
    Name,
}

impl From<OutputArg> for OutputFormat {
    fn from(value: OutputArg) -> Self {
        match value {
            OutputArg::Table => OutputFormat::Table,
            OutputArg::Wide => OutputFormat::Wide,
            OutputArg::Json => OutputFormat::Json,
            OutputArg::Yaml => OutputFormat::Yaml,
            OutputArg::Jsonl => OutputFormat::Jsonl,
            OutputArg::Name => OutputFormat::Name,
        }
    }
}

/// The complete typed command tree; parse once, dispatch exhaustively.
#[derive(Debug, Parser)]
#[command(
    name = BIN_NAME,
    version = velnor_model::CRATE_VERSION,
    about = "Velnor operator CLI",
    long_about = "Velnor operator CLI.\n\n\
        Inspect and operate a Velnor runner fleet from the command line.\n\
        Resource data goes to stdout; warnings go to stderr; every command \
        exits with one documented status class.",
    after_help = "Run 'velnorctl <command> --help' for command-specific help."
)]
pub struct Cli {
    /// Global flags shared by every subcommand.
    #[command(flatten)]
    pub globals: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

/// Global conventions accepted before or after any subcommand.
#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Named connection context.
    #[arg(long, global = true, value_name = "NAME")]
    pub context: Option<String>,

    /// Output format for rendered resources.
    #[arg(
        short = 'o',
        long = "output",
        global = true,
        value_name = "FORMAT",
        default_value = "table"
    )]
    pub output: OutputArg,

    /// Restrict to one daemon instance.
    #[arg(long, global = true, value_name = "NAME")]
    pub instance: Option<String>,

    /// Restrict to one repository (owner/name).
    #[arg(long, global = true, value_name = "REPO")]
    pub repo: Option<String>,

    /// Include-only filter over resource fields.
    #[arg(long, global = true, value_name = "SELECTOR")]
    pub selector: Option<String>,

    /// Field equality selector (key=value).
    #[arg(long, global = true, value_name = "SELECTOR")]
    pub field_selector: Option<String>,

    /// Lower time bound: RFC 3339 or relative duration.
    #[arg(long, global = true, value_name = "SINCE")]
    pub since: Option<String>,

    /// Deadline in seconds before the command exits with TIMEOUT.
    #[arg(long, global = true, value_name = "SECONDS")]
    pub timeout: Option<u64>,

    /// Disable ANSI styling regardless of TTY detection.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Increase verbosity; repeatable.
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

impl GlobalArgs {
    /// Effective domain output format; defaults to the human table view.
    #[must_use]
    pub fn output_format(&self) -> OutputFormat {
        self.output.into()
    }

    /// Resolve the color policy from `--no-color` and TTY state.
    #[must_use]
    pub fn color_policy(&self, stdout_is_tty: bool) -> ColorPolicy {
        ColorPolicy::resolve(self.no_color, stdout_is_tty)
    }
}

/// Every leaf command; the exhaustive match in `main` is the dispatch
/// registry, so the compiler proves each command is wired.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate man pages for the current command tree.
    Man(man::ManArgs),
    /// Generate shell completion scripts.
    Completion(completion::CompletionArgs),
}

/// Error a command execution returns, carrying its exit class.
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

    /// Process exit code derived from the class only.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        exit_code_for(&self.class)
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

/// The single process exit-code mapping for any outcome class.
#[must_use]
pub fn exit_code_for(class: &ExitClass) -> u8 {
    u8::try_from(velnor_model::exit_code_for_class(*class)).unwrap_or(u8::MAX)
}

/// Derive the machine-readable schema document from the live clap command
/// tree, so serialized metadata can never drift from the executable CLI.
#[must_use]
pub fn schema_document() -> SchemaDocument {
    let cmd = Cli::command();
    SchemaDocument {
        binary: BIN_NAME.to_owned(),
        version: velnor_model::CRATE_VERSION.to_owned(),
        global_flags: flag_metadata(cmd.get_arguments(), true),
        commands: cmd
            .get_subcommands()
            .map(|sub| CommandMetadata {
                name: sub.get_name().to_owned(),
                about: sub
                    .get_about()
                    .map(|styled| styled.to_string())
                    .unwrap_or_default(),
                flags: flag_metadata(sub.get_arguments(), false),
            })
            .collect(),
    }
}

fn flag_metadata<'a>(args: impl Iterator<Item = &'a clap::Arg>, global: bool) -> Vec<FlagMetadata> {
    args.filter(|arg| {
        !matches!(arg.get_id().as_str(), "help" | "version")
            && arg.get_long().is_some()
            && arg.is_global_set() == global
    })
    .map(|arg| FlagMetadata {
        long: arg.get_long().unwrap_or_default().to_owned(),
        short: arg.get_short(),
        value_name: arg
            .get_value_names()
            .and_then(|names| names.first())
            .map(|name| format!("<{name}>")),
        help: arg
            .get_help()
            .map(|styled| styled.to_string())
            .unwrap_or_default(),
        global,
    })
    .collect()
}

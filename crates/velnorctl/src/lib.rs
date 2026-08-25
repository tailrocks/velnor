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

use std::time::Duration;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use velnor_model::{
    CommandMetadata, ExitClass, FlagMetadata, MachineErrorEnvelope, SchemaDocument, Since,
};
use velnor_render::{ColorPolicy, OutputFormat};

pub mod completion;
pub mod man;
pub mod runtime;

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

    /// Lower time bound: RFC 3339 instant, or relative duration like 45s,
    /// 10m, 2h, or 1h30m measured back from now.
    #[arg(
        long,
        global = true,
        value_name = "SINCE",
        value_parser = |raw: &str| Since::parse(raw)
    )]
    pub since: Option<Since>,

    /// Deadline in seconds before the command exits with TIMEOUT.
    #[arg(
        long,
        global = true,
        value_name = "SECONDS",
        value_parser = |seconds: &str| seconds.parse::<u64>().map(Duration::from_secs)
    )]
    pub timeout: Option<Duration>,

    /// Disable ANSI styling regardless of TTY detection.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Increase verbosity; repeatable up to maximum detail (-vvv).
    #[arg(short = 'v', long = "verbose", global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// Effective verbosity resolved from repeated `-v/--verbose` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    /// No verbosity flags; default output.
    Normal,
    /// One flag (`-v`).
    Verbose,
    /// Two flags (`-vv`).
    Debug,
    /// Three flags (`-vvv`) and beyond.
    Trace,
}

impl Verbosity {
    /// Map a counted repeat total; counts above three saturate to
    /// [`Verbosity::Trace`] so deeper nesting never regresses detail.
    #[must_use]
    pub fn from_count(count: u8) -> Self {
        match count {
            0 => Self::Normal,
            1 => Self::Verbose,
            2 => Self::Debug,
            _ => Self::Trace,
        }
    }
}

impl GlobalArgs {
    /// Effective domain output format; defaults to the human table view.
    #[must_use]
    pub fn output_format(&self) -> OutputFormat {
        self.output.into()
    }

    /// Effective verbosity level; dispatch reads this, never the raw count.
    #[must_use]
    pub fn verbosity(&self) -> Verbosity {
        Verbosity::from_count(self.verbose)
    }

    /// Resolve the color policy from `--no-color` and TTY state.
    #[must_use]
    pub fn color_policy(&self, stdout_is_tty: bool) -> ColorPolicy {
        ColorPolicy::resolve(self.no_color, stdout_is_tty)
    }
}

/// Every public leaf command. Exhaustive match in [`execute`] is the
/// dispatch registry, so the compiler proves each command is wired.
///
/// `daemon`, `release`, and `run` are not public commands: they remain
/// unrecognized clap subcommands (service plumbing / reserved names).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate man pages for the current command tree.
    Man(man::ManArgs),
    /// Generate shell completion scripts.
    Completion(completion::CompletionArgs),
    /// Inspect Velnor's daemon-shared host cache stores.
    Cache(Box<runtime::CacheArgs>),
    /// Inspect or validate against the compiled strict capability manifest.
    Capabilities(Box<runtime::CapabilitiesArgs>),
    /// Create and store a GitHub JIT runner configuration.
    Configure(Box<runtime::ConfigureArgs>),
    /// Probe GitHub for this daemon's registered runners and fail loudly when
    /// the fleet is gone (run from a systemd timer for alerting).
    Doctor(Box<runtime::DoctorArgs>),
    /// Validate local Docker prerequisites before polling GitHub for jobs.
    Preflight(Box<runtime::PreflightArgs>),
    /// Remove local runner configuration.
    Remove(Box<runtime::RemoveArgs>),
    /// Print local runner configuration status.
    Status(Box<runtime::StatusArgs>),
    /// Inspect the canonical Velnor storage layout and catalog.
    Storage(Box<runtime::StorageArgs>),
    /// External black-box canary (queue → assignment → first step → completion).
    Canary(CanaryArgs),
}

/// CLI-facing canary arguments. Converted into the clap-free domain type.
#[derive(Debug, Clone, Args)]
pub struct CanaryArgs {
    /// Whole-path timeout in seconds. The canary fails closed if any stage is missing.
    #[arg(long, default_value_t = 60, value_name = "SECONDS")]
    pub timeout_seconds: u64,
    /// Record the four stages locally without calling GitHub.
    #[arg(long)]
    pub fixture: bool,
    /// Write the JSON report to this path.
    #[arg(long, value_name = "PATH")]
    pub report: Option<std::path::PathBuf>,
}

impl From<CanaryArgs> for velnor_runner::node::CanaryArgs {
    fn from(args: CanaryArgs) -> Self {
        Self {
            timeout_seconds: args.timeout_seconds,
            fixture: args.fixture,
            report: args.report,
        }
    }
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

impl From<anyhow::Error> for CommandError {
    fn from(error: anyhow::Error) -> Self {
        Self::operation(error.to_string())
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

/// Execute a fully parsed CLI: clap already owns argv. Runtime failures map
/// to [`ExitClass`]; `--timeout` is a real deadline around the handler.
pub async fn execute(cli: Cli) -> Result<(), CommandError> {
    let timeout = cli.globals.timeout;
    let work = execute_parsed(cli);
    match timeout {
        Some(limit) => tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
            Err(CommandError::new(
                ExitClass::Timeout,
                "deadline.elapsed",
                "the deadline elapsed before a terminal result",
            ))
        }),
        None => work.await,
    }
}

async fn execute_parsed(cli: Cli) -> Result<(), CommandError> {
    match cli.command {
        Command::Man(args) => man::run(&args),
        Command::Completion(args) => completion::run(&args),
        Command::Cache(args) => {
            run_runtime(velnor_runner::args::Command::Cache((*args).into())).await
        }
        Command::Capabilities(args) => {
            run_runtime(velnor_runner::args::Command::Capabilities((*args).into())).await
        }
        Command::Configure(args) => {
            run_runtime(velnor_runner::args::Command::Configure((*args).into())).await
        }
        Command::Doctor(args) => {
            run_runtime(velnor_runner::args::Command::Doctor((*args).into())).await
        }
        Command::Preflight(args) => {
            run_runtime(velnor_runner::args::Command::Preflight((*args).into())).await
        }
        Command::Remove(args) => {
            run_runtime(velnor_runner::args::Command::Remove((*args).into())).await
        }
        Command::Status(args) => {
            if args.json {
                return status_health_json(&args);
            }
            run_runtime(velnor_runner::args::Command::Status((*args).into())).await
        }
        Command::Storage(args) => {
            run_runtime(velnor_runner::args::Command::Storage((*args).into())).await
        }
        Command::Canary(args) => {
            let report = velnor_runner::node::run_canary(&args.into())?;
            println!(
                "{}",
                serde_json::to_string(&report)
                    .map_err(|error| CommandError::operation(error.to_string()))?
            );
            Ok(())
        }
    }
}

fn status_health_json(args: &runtime::StatusArgs) -> Result<(), CommandError> {
    use velnor_control::journal::Journal;
    use velnor_model::HealthDocument;
    let dir = args
        .state_dir
        .clone()
        .or_else(|| args.config_dir.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("/run/velnor"));
    let document = match Journal::open(dir.join("journal.db")) {
        Ok(journal) => journal
            .load_state()
            .map(|state| state.health())
            .unwrap_or_else(|_| HealthDocument::empty().with_derived_state()),
        Err(_) => velnor_runner::node::health::fetch(&dir)
            .unwrap_or_else(|_| HealthDocument::empty().with_derived_state()),
    };
    println!(
        "{}",
        serde_json::to_string(&document)
            .map_err(|error| CommandError::operation(error.to_string()))?
    );
    Ok(())
}

async fn run_runtime(command: velnor_runner::args::Command) -> Result<(), CommandError> {
    runtime::enforce_admission()?;
    let log_dir = runtime::telemetry_dir(&command);
    runtime::init_telemetry(log_dir.as_deref());
    runtime::dispatch(command).await?;
    Ok(())
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
        commands: collect_command_metadata(&cmd, ""),
    }
}

fn collect_command_metadata(cmd: &clap::Command, prefix: &str) -> Vec<CommandMetadata> {
    let mut commands = Vec::new();
    for sub in cmd
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
    {
        let name = if prefix.is_empty() {
            sub.get_name().to_owned()
        } else {
            format!("{prefix} {}", sub.get_name())
        };
        commands.push(CommandMetadata {
            name: name.clone(),
            about: sub
                .get_about()
                .map(|styled| styled.to_string())
                .unwrap_or_default(),
            flags: flag_metadata(sub.get_arguments(), false),
        });
        commands.extend(collect_command_metadata(sub, &name));
    }
    commands
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

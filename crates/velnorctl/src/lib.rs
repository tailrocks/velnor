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

pub mod commands;
pub mod completion;
pub mod http;
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
/// `release` and `run` remain reserved for package production and workflow-run
/// commands; the service daemon is a first-class `velnorctl daemon` command.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate man pages for the current command tree.
    Man(man::ManArgs),
    /// Generate shell completion scripts.
    Completion(completion::CompletionArgs),
    /// Print the installed Velnor control version.
    Version,
    /// Print the versioned API resource catalog.
    ApiResources,
    /// Explain one resource or field.
    Explain(commands::FeatureArgs),
    /// Query resource collections through the control API.
    Get(commands::GetArgs),
    /// Describe one resource through the control API.
    Describe(commands::DescribeArgs),
    /// Read one resource log stream.
    Logs(commands::LogsArgs),
    /// Read daemon-shared performance telemetry.
    Telemetry(commands::TelemetryArgs),
    /// Read normalized control events.
    Events(commands::EventsArgs),
    /// Read live metrics.
    Top(commands::TopArgs),
    /// Wait for a resource condition.
    Wait(commands::WaitArgs),
    /// Execute reviewed reconciliation plans.
    Reconcile(commands::ReconcileArgs),
    /// Cordon an instance.
    Cordon(commands::LifecycleArgs),
    /// Uncordon an instance.
    Uncordon(commands::LifecycleArgs),
    /// Drain an instance.
    Drain(commands::LifecycleArgs),
    /// Resume an instance.
    Resume(commands::LifecycleArgs),
    /// Restart an instance.
    Restart(commands::LifecycleArgs),
    /// Recycle a slot or runner.
    Recycle(commands::LifecycleArgs),
    /// Scale stable slots for an instance.
    Scale(commands::ScaleArgs),
    /// Inspect Velnor's daemon-shared host cache stores.
    Cache(Box<runtime::CacheArgs>),
    /// Inspect or validate against the compiled strict capability manifest.
    Capabilities(Box<runtime::CapabilitiesArgs>),
    /// Create and store a GitHub JIT runner configuration.
    Configure(Box<runtime::ConfigureArgs>),
    /// Probe GitHub for this daemon's registered runners and fail loudly when
    /// the fleet is gone (run from a systemd timer for alerting).
    Doctor(Box<runtime::DoctorArgs>),
    /// Validate the selected execution backend before polling GitHub for jobs.
    Preflight(Box<runtime::PreflightArgs>),
    /// Remove local runner configuration.
    Remove(Box<runtime::RemoveArgs>),
    /// Print local runner configuration status.
    Status(Box<runtime::StatusArgs>),
    /// Run the per-instance daemon lifecycle engine.
    Daemon(Box<runtime::DaemonArgs>),
    /// Inspect the canonical Velnor storage layout and catalog.
    Storage(Box<runtime::StorageArgs>),
    /// Workflow-run operations owned by the GitHub client service.
    Run(commands::RunArgs),
    /// Configuration service operations.
    Config(commands::ConfigArgs),
    /// Named local context operations.
    Context(commands::ContextArgs),
    /// Authentication report/check operations.
    Auth(commands::AuthArgs),
    /// Local instance plan/apply/delete operations.
    Instance(commands::InstanceArgs),
    /// Strict capability manifest operations.
    Capability(commands::CapabilityArgs),
    /// Native action adapter operations.
    Adapter(commands::AdapterArgs),
    /// Static workflow compatibility checks.
    Workflow(commands::WorkflowArgs),
    /// Bounded diagnostics collection.
    Diagnostics(commands::DiagnosticsArgs),
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

impl From<velnor_control::config::ConfigError> for CommandError {
    fn from(error: velnor_control::config::ConfigError) -> Self {
        let class = match error {
            velnor_control::config::ConfigError::ContextNotFound => ExitClass::Unavailable,
            velnor_control::config::ConfigError::ContextCurrent
            | velnor_control::config::ConfigError::DuplicateSource(_)
            | velnor_control::config::ConfigError::Empty(_)
            | velnor_control::config::ConfigError::InvalidSlots
            | velnor_control::config::ConfigError::InvalidCredentialReference
            | velnor_control::config::ConfigError::InvalidJournalCapacity => ExitClass::Usage,
            velnor_control::config::ConfigError::Io(_)
            | velnor_control::config::ConfigError::Decode(_) => ExitClass::Operation,
        };
        Self::new(class, "config.failed", error.to_string())
    }
}

impl From<velnor_client::ClientError> for CommandError {
    fn from(error: velnor_client::ClientError) -> Self {
        let class = error.exit_class();
        let (reason, message) = match &error {
            velnor_client::ClientError::Api { envelope, .. } => (
                envelope.reason.clone(),
                envelope
                    .remediation
                    .clone()
                    .unwrap_or_else(|| "control API request failed".to_owned()),
            ),
            velnor_client::ClientError::Endpoint(_) => {
                ("endpoint.invalid".to_owned(), error.to_string())
            }
            velnor_client::ClientError::Invalid { field, .. } => {
                (format!("field.invalid.{field}"), error.to_string())
            }
            velnor_client::ClientError::UnsupportedApi { .. } => {
                ("api.version_unsupported".to_owned(), error.to_string())
            }
            velnor_client::ClientError::Authorization => {
                ("authorization.denied".to_owned(), error.to_string())
            }
            velnor_client::ClientError::Timeout => {
                ("deadline.elapsed".to_owned(), error.to_string())
            }
            velnor_client::ClientError::Io { .. } => {
                ("control.api.unavailable".to_owned(), error.to_string())
            }
            velnor_client::ClientError::Protocol { .. } => {
                ("control.api.protocol".to_owned(), error.to_string())
            }
        };
        Self::new(class, reason, message)
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
    let globals = cli.globals;
    match cli.command {
        Command::Man(args) => man::run(&args),
        Command::Completion(args) => completion::run(&args),
        Command::Version => {
            println!("{}", velnor_model::CRATE_VERSION);
            Ok(())
        }
        Command::ApiResources => {
            println!(
                "{}",
                serde_json::to_string(&schema_document())
                    .map_err(|error| CommandError::operation(error.to_string()))?
            );
            Ok(())
        }
        Command::Explain(args) => execute_explain(&globals, args).await,
        Command::Get(args) => execute_get(&globals, args).await,
        Command::Describe(args) => execute_describe(&globals, args).await,
        Command::Logs(args) => execute_logs(&globals, args).await,
        Command::Telemetry(args) => execute_telemetry(&globals, args).await,
        Command::Events(args) => execute_events(&globals, args).await,
        Command::Top(args) => execute_top(&globals, args).await,
        Command::Wait(args) => execute_wait(&globals, args).await,
        Command::Reconcile(args) => execute_reconcile(&globals, args).await,
        Command::Cordon(args) => execute_lifecycle(&globals, "cordon", args).await,
        Command::Uncordon(args) => execute_lifecycle(&globals, "uncordon", args).await,
        Command::Drain(args) => execute_lifecycle(&globals, "drain", args).await,
        Command::Resume(args) => execute_lifecycle(&globals, "resume", args).await,
        Command::Restart(args) => execute_lifecycle(&globals, "restart", args).await,
        Command::Recycle(args) => execute_lifecycle(&globals, "recycle", args).await,
        Command::Scale(args) => execute_scale(&globals, args).await,
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
        Command::Daemon(args) => runtime::run_daemon((*args).clone())
            .await
            .map_err(|_| CommandError::operation("daemon.start_failed: unable to start daemon")),
        Command::Storage(args) => {
            if matches!(
                &args.command,
                runtime::StorageCommand::Du
                    | runtime::StorageCommand::Gc(_)
                    | runtime::StorageCommand::History
                    | runtime::StorageCommand::Reservations
                    | runtime::StorageCommand::Leases
                    | runtime::StorageCommand::ExplainPressure
            ) {
                Err(control_api_unavailable("storage"))
            } else {
                run_runtime(velnor_runner::args::Command::Storage((*args).into())).await
            }
        }
        Command::Run(args) => execute_run(&globals, args).await,
        Command::Config(args) => execute_config(args),
        Command::Context(args) => execute_context(args),
        Command::Auth(args) => execute_auth(args),
        Command::Instance(args) => execute_instance(args),
        Command::Capability(args) => execute_capability(args),
        Command::Adapter(args) => execute_adapter(args),
        Command::Workflow(args) => execute_workflow(&globals, args).await,
        Command::Diagnostics(args) => execute_diagnostics(&globals, args).await,
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

fn control_api_unavailable(command: &str) -> CommandError {
    CommandError::new(
        ExitClass::Unavailable,
        "control.api.unavailable",
        format!("{command} requires a reachable v1 control API endpoint"),
    )
}

fn client_for(globals: &GlobalArgs) -> Result<velnor_client::UnixControlClient, CommandError> {
    use velnor_control::config::ContextStore;

    let contexts = context_store()?.list()?;
    let (endpoint, context_selected) = if let Some(context_name) = &globals.context {
        let context = contexts
            .iter()
            .find(|context| context.name == *context_name)
            .ok_or_else(|| CommandError::unavailable("named context was not found"))?;
        (context.endpoint.as_str().to_owned(), true)
    } else if let Some(context) = contexts.iter().find(|context| context.current) {
        (context.endpoint.as_str().to_owned(), true)
    } else {
        let instance = globals
            .instance
            .clone()
            .or_else(|| std::env::var("VELNOR_INSTANCE").ok());
        (
            format!(
                "unix:///run/velnor/{}",
                instance.as_deref().unwrap_or("default")
            ),
            false,
        )
    };
    let endpoint = velnor_client::UnixEndpoint::parse(&endpoint).map_err(|error| {
        CommandError::new(ExitClass::Usage, "endpoint.invalid", error.to_string())
    })?;
    if context_selected {
        validate_context_instance(&endpoint, globals.instance.as_deref())?;
    }
    Ok(velnor_client::UnixControlClient::new(endpoint))
}

fn validate_context_instance(
    endpoint: &velnor_client::UnixEndpoint,
    requested_instance: Option<&str>,
) -> Result<(), CommandError> {
    if let Some(requested_instance) = requested_instance
        && endpoint.instance() != requested_instance
    {
        return Err(CommandError::new(
            ExitClass::Conflict,
            "instance.context_mismatch",
            "requested instance does not match the selected context",
        ));
    }
    Ok(())
}

fn client_query(args: &commands::ResourceQueryArgs) -> velnor_client::ResourceQuery {
    velnor_client::ResourceQuery {
        selector: args.selector.clone(),
        field_selector: args.field_selector.clone(),
        page_token: args.page_token.clone(),
        since: args.since.clone(),
        limit: args.limit,
    }
}

fn render_resources(
    globals: &GlobalArgs,
    resources: &[velnor_model::AnyResource],
) -> Result<(), CommandError> {
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    velnor_render::render(
        globals.output_format(),
        resources,
        &velnor_render::RenderOptions {
            color: globals.color_policy(false),
        },
        &mut stdout,
        &mut stderr,
    )
    .map_err(|error| CommandError::operation(error.to_string()))
}

fn get_target(args: commands::GetArgs) -> (&'static str, commands::ResourceQueryArgs) {
    match args.resource {
        commands::GetResourceCommand::Hosts(args) => ("hosts", args),
        commands::GetResourceCommand::Instances(args) => ("instances", args),
        commands::GetResourceCommand::Slots(args) => ("slots", args),
        commands::GetResourceCommand::Runners(args) => ("runners", args),
        commands::GetResourceCommand::Jobs(args) => ("jobs", args),
        commands::GetResourceCommand::Runs(args) => ("runs", args),
        commands::GetResourceCommand::Queue(args) => ("queue", args),
        commands::GetResourceCommand::Events(args) => ("events", args),
        commands::GetResourceCommand::Reservations(args) => ("reservations", args),
        commands::GetResourceCommand::Leases(args) => ("leases", args),
    }
}

async fn execute_explain(
    globals: &GlobalArgs,
    args: commands::FeatureArgs,
) -> Result<(), CommandError> {
    let info = client_for(globals)?.info().await?;
    print_json(serde_json::json!({
        "feature": args.feature_or_action,
        "apiVersion": info.api_version,
        "schemaVersion": info.schema_version,
    }))
}

async fn execute_get(globals: &GlobalArgs, args: commands::GetArgs) -> Result<(), CommandError> {
    let (resource, query) = get_target(args);
    let page = client_for(globals)?
        .get_resources(resource, &client_query(&query))
        .await?;
    render_resources(globals, &page.resources)
}

async fn execute_describe(
    globals: &GlobalArgs,
    args: commands::DescribeArgs,
) -> Result<(), CommandError> {
    let (resource, name) = args.resource.split_once('/').ok_or_else(|| {
        CommandError::new(
            ExitClass::Usage,
            "resource.invalid",
            "use <resource>/<name>",
        )
    })?;
    validate_resource_name(name)?;
    let resource = resource.strip_suffix('s').unwrap_or(resource);
    let plural = match resource {
        "host" => "hosts",
        "instance" => "instances",
        "slot" => "slots",
        "runner" => "runners",
        "job" => "jobs",
        "run" => "runs",
        "queue" => "queue",
        "event" => "events",
        "reservation" => "reservations",
        "lease" => "leases",
        _ => {
            return Err(CommandError::new(
                ExitClass::Usage,
                "resource.invalid",
                "unsupported resource noun",
            ))
        }
    };
    let client = client_for(globals)?;
    let matching = named_resources(&client, plural, name).await?;
    match matching.as_slice() {
        [] => Err(CommandError::unavailable("resource was not found")),
        [_] => render_resources(globals, &matching),
        _ => Err(CommandError::new(
            ExitClass::Conflict,
            "resource.ambiguous",
            "resource name is ambiguous",
        )),
    }
}

fn validate_resource_name(name: &str) -> Result<(), CommandError> {
    if name.is_empty()
        || name.len() > 128
        || matches!(name, "." | "..")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CommandError::new(
            ExitClass::Usage,
            "resource.invalid",
            "resource name contains an unsupported character",
        ));
    }
    Ok(())
}

fn named_resource_query(name: &str) -> velnor_client::ResourceQuery {
    velnor_client::ResourceQuery {
        field_selector: Some(format!("metadata.name={name}")),
        limit: Some(2),
        ..velnor_client::ResourceQuery::default()
    }
}

async fn named_resources(
    client: &velnor_client::UnixControlClient,
    resource: &str,
    name: &str,
) -> Result<Vec<velnor_model::AnyResource>, CommandError> {
    let page = client
        .get_resources(resource, &named_resource_query(name))
        .await?;
    Ok(page
        .resources
        .into_iter()
        .filter(|item| item.meta().name == name)
        .collect())
}

fn condition_is_true(resources: &[velnor_model::AnyResource], condition: &str) -> bool {
    resources.first().is_some_and(|resource| {
        resource.meta().conditions.iter().any(|candidate| {
            candidate.kind == condition && candidate.status == velnor_model::ConditionStatus::True
        })
    })
}

async fn execute_events(
    globals: &GlobalArgs,
    args: commands::EventsArgs,
) -> Result<(), CommandError> {
    let query = client_query(&args.query);
    let page = client_for(globals)?.get_resources("events", &query).await?;
    render_resources(globals, &page.resources)
}

async fn execute_top(globals: &GlobalArgs, args: commands::TopArgs) -> Result<(), CommandError> {
    let resource = match args.target {
        commands::TopTarget::Host => "hosts",
        commands::TopTarget::Instances => "instances",
        commands::TopTarget::Slots => "slots",
        commands::TopTarget::Jobs => "jobs",
        commands::TopTarget::Storage => "storage",
    };
    let page = client_for(globals)?
        .get_resources(resource, &Default::default())
        .await?;
    render_resources(globals, &page.resources)
}

async fn execute_wait(globals: &GlobalArgs, args: commands::WaitArgs) -> Result<(), CommandError> {
    let (resource, name) = args.resource.split_once('/').ok_or_else(|| {
        CommandError::new(
            ExitClass::Usage,
            "resource.invalid",
            "use <resource>/<name>",
        )
    })?;
    validate_resource_name(name)?;
    let plural = format!("{resource}s");
    let client = client_for(globals)?;
    let mut matching = named_resources(&client, &plural, name).await?;
    if condition_is_true(&matching, &args.condition) {
        return render_resources(globals, &matching);
    }

    // Establish a versioned cursor after the current read, then read the
    // resource again to close the query/watch race. The outer `--timeout`
    // deadline owns the wait; this loop never converts a pending condition
    // into an immediate timeout.
    let mut cursor = client
        .watch(None, None, Some(4_096))
        .await?
        .last()
        .map(|item| item.version)
        .unwrap_or(0);
    matching = named_resources(&client, &plural, name).await?;
    loop {
        if condition_is_true(&matching, &args.condition) {
            return render_resources(globals, &matching);
        }

        let items = match client.watch(None, Some(cursor), Some(256)).await {
            Ok(items) => items,
            Err(error) if error.exit_class() == ExitClass::Conflict => {
                // The bounded stream can expire while a caller is paused.
                // Rebootstrap from a fresh snapshot instead of using a stale
                // cursor or silently missing a transition.
                cursor = client
                    .watch(None, None, Some(4_096))
                    .await?
                    .last()
                    .map(|item| item.version)
                    .unwrap_or(0);
                matching = named_resources(&client, &plural, name).await?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(last) = items.last() {
            cursor = last.version;
            matching = named_resources(&client, &plural, name).await?;
        } else {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

async fn execute_lifecycle(
    globals: &GlobalArgs,
    operation: &str,
    args: commands::LifecycleArgs,
) -> Result<(), CommandError> {
    let instance = args.target.strip_prefix("instance/").ok_or_else(|| {
        CommandError::new(
            ExitClass::Usage,
            "target.invalid",
            "target must be instance/<name>",
        )
    })?;
    let result = client_for(globals)?
        .mutate_instance(
            instance,
            operation,
            &args.reason,
            &args.idempotency_key,
            args.expected_version,
            None,
        )
        .await?;
    print_json(result)
}

async fn execute_scale(
    globals: &GlobalArgs,
    args: commands::ScaleArgs,
) -> Result<(), CommandError> {
    let instance = args.target.strip_prefix("instance/").ok_or_else(|| {
        CommandError::new(
            ExitClass::Usage,
            "target.invalid",
            "target must be instance/<name>",
        )
    })?;
    let result = client_for(globals)?
        .mutate_instance(
            instance,
            "scale",
            &args.reason,
            &args.idempotency_key,
            None,
            Some(args.slots),
        )
        .await?;
    print_json(serde_json::json!({"requestedSlots": args.slots, "operation": result}))
}

async fn execute_logs(globals: &GlobalArgs, args: commands::LogsArgs) -> Result<(), CommandError> {
    let records = client_for(globals)?
        .logs(
            &args.resource,
            args.source.as_deref(),
            args.cursor.as_deref(),
            args.tail,
        )
        .await?;
    if globals.output_format().is_machine() {
        print_json(records)
    } else {
        for record in records {
            println!("{}", record.message);
        }
        Ok(())
    }
}

async fn execute_telemetry(
    globals: &GlobalArgs,
    args: commands::TelemetryArgs,
) -> Result<(), CommandError> {
    let page = client_for(globals)?
        .telemetry(args.after.as_deref(), args.limit)
        .await?;
    print_json(page)
}

async fn execute_reconcile(
    globals: &GlobalArgs,
    args: commands::ReconcileArgs,
) -> Result<(), CommandError> {
    let target = match args.target {
        commands::ReconcileCommand::Runners(_) => "runners",
        commands::ReconcileCommand::Jobs(_) => "jobs",
        commands::ReconcileCommand::Docker(_) => "docker",
        commands::ReconcileCommand::Storage(_) => "storage",
    };
    let _ = client_for(globals)?.info().await?;
    Err(CommandError::new(
        ExitClass::Operation,
        "reconcile.plan_unavailable",
        format!("no reviewed {target} reconciliation plan is available"),
    ))
}

async fn execute_run(globals: &GlobalArgs, args: commands::RunArgs) -> Result<(), CommandError> {
    match args.command {
        commands::RunCommand::List(_) => {
            let page = client_for(globals)?
                .get_resources("runs", &Default::default())
                .await?;
            render_resources(globals, &page.resources)
        }
        commands::RunCommand::View(args) => {
            let page = client_for(globals)?
                .get_resources("runs", &Default::default())
                .await?;
            let matching = page
                .resources
                .into_iter()
                .filter(|resource| resource.meta().name == args.run_id.to_string())
                .collect::<Vec<_>>();
            if matching.is_empty() {
                Err(CommandError::unavailable("workflow run was not found"))
            } else {
                render_resources(globals, &matching)
            }
        }
        commands::RunCommand::Watch(args) => {
            let items = client_for(globals)?
                .watch(Some("runs"), Some(args.run_id), None)
                .await?;
            print_json(items)
        }
        commands::RunCommand::Logs(args) => {
            let logs = client_for(globals)?
                .logs(&args.run_id.to_string(), None, None, None)
                .await?;
            print_json(logs)
        }
        commands::RunCommand::Cancel(_)
        | commands::RunCommand::Rerun(_)
        | commands::RunCommand::Download(_)
        | commands::RunCommand::Dispatch(_)
        | commands::RunCommand::Open(_) => Err(CommandError::new(
            ExitClass::Operation,
            "run.operation_unavailable",
            "the selected workflow operation has no authoritative service route",
        )),
    }
}

async fn execute_workflow(
    globals: &GlobalArgs,
    args: commands::WorkflowArgs,
) -> Result<(), CommandError> {
    let info = client_for(globals)?.info().await?;
    match args.command {
        commands::WorkflowCommand::Check(args) => {
            if !args.repo.contains('/')
                || args.reference.trim().is_empty()
                || args.workflow.trim().is_empty()
            {
                return Err(CommandError::new(
                    ExitClass::Usage,
                    "workflow.invalid",
                    "repository, ref, and workflow are required",
                ));
            }
            print_json(serde_json::json!({
                "valid": true,
                "repo": args.repo,
                "ref": args.reference,
                "workflow": args.workflow,
                "apiVersion": info.api_version,
            }))
        }
    }
}

async fn execute_diagnostics(
    globals: &GlobalArgs,
    args: commands::DiagnosticsArgs,
) -> Result<(), CommandError> {
    let _ = client_for(globals)?.info().await?;
    match args.command {
        commands::DiagnosticsCommand::Bundle(args) => Err(CommandError::new(
            ExitClass::Operation,
            "diagnostics.bundle_unavailable",
            format!(
                "diagnostics archive cannot be created at {}",
                args.archive.display()
            ),
        )),
    }
}

fn execute_capability(args: commands::CapabilityArgs) -> Result<(), CommandError> {
    match args.command {
        commands::CapabilityCommand::Export => {
            print!(
                "{}",
                velnor_runner::manifest::to_json_document()
                    .map_err(|error| CommandError::operation(error.to_string()))?
            );
            Ok(())
        }
        commands::CapabilityCommand::Check(args) => {
            velnor_runner::manifest::run(velnor_runner::args::CapabilitiesArgs {
                command: velnor_runner::args::CapabilitiesCommand::Check {
                    job_dump: args.job_dump,
                },
            })
            .map_err(CommandError::from)
        }
        commands::CapabilityCommand::List => {
            let entries = velnor_runner::manifest::MANIFEST
                .actions
                .iter()
                .map(|action| {
                    serde_json::json!({
                        "repository": action.repository,
                        "adapter": format!("{:?}", action.adapter),
                        "refs": action.allowed_refs.iter().map(|item| item.value).collect::<Vec<_>>(),
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string(&entries)
                    .map_err(|error| CommandError::operation(error.to_string()))?
            );
            Ok(())
        }
        commands::CapabilityCommand::Explain(args) => {
            let action = velnor_runner::manifest::MANIFEST
                .actions
                .iter()
                .find(|action| {
                    action
                        .repository
                        .eq_ignore_ascii_case(&args.feature_or_action)
                })
                .ok_or_else(|| {
                    CommandError::unavailable("capability is not in the compiled manifest")
                })?;
            println!(
                "{}",
                serde_json::json!({
                    "repository": action.repository,
                    "adapter": format!("{:?}", action.adapter),
                    "notes": action.notes,
                    "manifestVersion": velnor_runner::manifest::MANIFEST_VERSION,
                })
            );
            Ok(())
        }
    }
}

fn execute_adapter(args: commands::AdapterArgs) -> Result<(), CommandError> {
    match args.command {
        commands::AdapterCommand::List => {
            let entries = velnor_runner::manifest::MANIFEST
                .actions
                .iter()
                .map(|action| {
                    serde_json::json!({
                        "adapter": format!("{:?}", action.adapter),
                        "repository": action.repository,
                        "version": velnor_runner::manifest::MANIFEST_VERSION,
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string(&entries)
                    .map_err(|error| CommandError::operation(error.to_string()))?
            );
            Ok(())
        }
        commands::AdapterCommand::Describe(args) | commands::AdapterCommand::Check(args) => {
            let repository = args
                .feature_or_action
                .split_once('@')
                .map_or(args.feature_or_action.as_str(), |(repository, _)| {
                    repository
                });
            let action = velnor_runner::manifest::MANIFEST
                .actions
                .iter()
                .find(|action| action.repository.eq_ignore_ascii_case(repository))
                .ok_or_else(|| {
                    CommandError::unavailable("adapter is not in the compiled manifest")
                })?;
            println!(
                "{}",
                serde_json::json!({
                    "repository": action.repository,
                    "adapter": format!("{:?}", action.adapter),
                    "manifestVersion": velnor_runner::manifest::MANIFEST_VERSION,
                })
            );
            Ok(())
        }
    }
}

fn execute_config(args: commands::ConfigArgs) -> Result<(), CommandError> {
    use velnor_control::config::{ConfigLayer, ConfigResolver};
    use velnor_model::ConfigSource;

    let resolver = ConfigResolver::new(vec![ConfigLayer {
        source: ConfigSource::Builtin,
        ..ConfigLayer::default()
    }])
    .map_err(|error| CommandError::operation(error.to_string()))?;
    match args.command {
        commands::ConfigCommand::View => {
            let effective = resolver
                .resolve()
                .map_err(|error| CommandError::operation(error.to_string()))?;
            println!(
                "{}",
                serde_json::to_string(&effective)
                    .map_err(|error| CommandError::operation(error.to_string()))?
            );
        }
        commands::ConfigCommand::Validate => {
            resolver
                .resolve()
                .map_err(|error| CommandError::operation(error.to_string()))?;
            println!("{{\"valid\":true}}");
        }
        commands::ConfigCommand::Diff => println!("[]"),
        commands::ConfigCommand::Sources => {
            println!("[\"BUILTIN\",\"CONTEXT\",\"INSTANCE\",\"SYSTEMD\",\"PROCESS\",\"COMMAND\"]")
        }
    }
    Ok(())
}

fn context_store(
) -> Result<velnor_control::config::FileContextStore, velnor_control::config::ConfigError> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from(".config"));
    velnor_control::config::FileContextStore::new(base.join("velnor").join("config.toml"))
}

fn execute_context(args: commands::ContextArgs) -> Result<(), CommandError> {
    use velnor_control::config::ContextStore;
    use velnor_model::{ContextConfig, SanitizedUrl};

    let store = context_store()?;
    match args.command {
        commands::ContextCommand::List => {
            print_json(store.list()?)?;
        }
        commands::ContextCommand::Current => {
            let current = store
                .list()?
                .into_iter()
                .find(|context| context.current)
                .ok_or_else(|| CommandError::unavailable("no current context"))?;
            print_json(current)?;
        }
        commands::ContextCommand::Use(args) => print_json(store.use_context(&args.name)?)?,
        commands::ContextCommand::Set(args) => {
            let endpoint = SanitizedUrl::try_from(args.endpoint).map_err(|_| {
                CommandError::new(
                    ExitClass::Usage,
                    "endpoint.invalid",
                    "endpoint must be a valid non-opaque URL",
                )
            })?;
            store.set(ContextConfig {
                name: args.name,
                endpoint,
                credential: None,
                current: false,
            })?;
        }
        commands::ContextCommand::Delete(args) => store.delete(&args.name)?,
    }
    Ok(())
}

fn execute_auth(args: commands::AuthArgs) -> Result<(), CommandError> {
    use std::collections::BTreeMap;
    use velnor_model::{AuthReport, PermissionState, Timestamp};

    let mut permissions = BTreeMap::new();
    permissions.insert("github.read".to_owned(), PermissionState::Unproven);
    permissions.insert("github.jit".to_owned(), PermissionState::Unproven);
    let report = AuthReport {
        identity: None,
        permissions,
        observed_at: Timestamp::now(),
    };
    print_json(report)?;
    if matches!(args.command, commands::AuthCommand::Check) {
        return Err(CommandError::condition(
            "GitHub permissions are unproven without an injected client",
        ));
    }
    Ok(())
}

fn execute_instance(args: commands::InstanceArgs) -> Result<(), CommandError> {
    use velnor_model::{InstanceOperation, Timestamp};

    let (kind, name) = match args.command {
        commands::InstanceCommand::Init(args) => ("init", args.name),
        commands::InstanceCommand::Install(args) => ("install", args.name),
        commands::InstanceCommand::Apply(args) => ("apply", args.name),
        commands::InstanceCommand::Delete(args) => ("delete", args.name),
    };
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return Err(CommandError::new(
            ExitClass::Usage,
            "instance.invalid",
            "instance name must be a canonical local identity",
        ));
    }
    let operation = InstanceOperation {
        operation_id: format!(
            "plan-{}",
            Timestamp::now().as_offset_datetime().unix_timestamp_nanos()
        ),
        instance: name,
        kind: kind.to_owned(),
        phase: "planned".to_owned(),
        created_at: Timestamp::now(),
    };
    print_json(operation)
}

fn print_json<T: serde::Serialize>(value: T) -> Result<(), CommandError> {
    println!(
        "{}",
        serde_json::to_string(&value)
            .map_err(|error| CommandError::operation(error.to_string()))?
    );
    Ok(())
}

fn status_health_json(args: &runtime::StatusArgs) -> Result<(), CommandError> {
    use velnor_control::journal::Journal;
    use velnor_model::HealthDocument;
    let dir = args
        .state_dir
        .clone()
        .or_else(|| args.config_dir.clone())
        .unwrap_or_else(|| std::path::PathBuf::from("/run/velnor"));
    let config_dir = args.config_dir.clone().unwrap_or_else(|| dir.clone());
    let execution = velnor_runner::execution::load_execution_file(&config_dir, None)
        .or_else(|_| velnor_runner::execution::load_execution_file(&dir, None))
        .map_err(|error| CommandError::operation(error.to_string()))?;
    let mut document = match Journal::open(dir.join("journal.db")) {
        Ok(journal) => journal
            .load_state()
            .map(|state| state.health())
            .unwrap_or_else(|_| HealthDocument::empty().with_derived_state()),
        Err(_) => velnor_runner::node::health::fetch(&dir)
            .unwrap_or_else(|_| HealthDocument::empty().with_derived_state()),
    };
    document.execution_backend = execution.backend();
    let mut json = serde_json::to_value(&document)
        .map_err(|error| CommandError::operation(error.to_string()))?;
    json["alerts"] = serde_json::to_value(document.alerts())
        .map_err(|error| CommandError::operation(error.to_string()))?;
    println!(
        "{}",
        serde_json::to_string(&json).map_err(|error| CommandError::operation(error.to_string()))?
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_instance_rejects_a_different_context_endpoint() {
        let endpoint = velnor_client::UnixEndpoint::parse("unix:///run/velnor/primary")
            .expect("valid context endpoint");

        let error = validate_context_instance(&endpoint, Some("secondary"))
            .expect_err("different explicit instance must fail closed");

        assert_eq!(error.class, ExitClass::Conflict);
        assert_eq!(error.reason, "instance.context_mismatch");
    }
}

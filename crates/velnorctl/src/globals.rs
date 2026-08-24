//! Global CLI conventions shared by every leaf command (Plan 065).
//!
//! Globals may appear before or after the subcommand. Parsing is fail-closed:
//! unrecognized global spellings before the subcommand, invalid closed-choice
//! values, and missing values all exit with the `Usage` class and usage text
//! on stderr. Everything after the subcommand name that is not a known global
//! passes through to the command verbatim.

use velnor_model::{ExitClass, MachineErrorEnvelope};
use velnor_render::{ColorPolicy, OutputFormat};

/// Long globals that take a value.
const VALUE_FLAGS: [&str; 8] = [
    "context",
    "output",
    "instance",
    "repo",
    "selector",
    "field-selector",
    "since",
    "timeout",
];

/// Long globals that are switches.
const SWITCH_FLAGS: [&str; 1] = ["no-color"];

/// Closed shell choices for the completion metadata interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    /// Canonical spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }

    /// Every shell in canonical order.
    pub const ALL: [Shell; 3] = [Shell::Bash, Shell::Zsh, Shell::Fish];
}

/// Result of parsing globals out of argv.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedGlobals {
    pub context: Option<String>,
    pub output: Option<OutputFormat>,
    pub instance: Option<String>,
    pub repo: Option<String>,
    pub selector: Option<String>,
    pub field_selector: Option<String>,
    pub since: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub no_color: bool,
    pub verbose: u8,
    /// Remaining argv beginning with the subcommand name.
    pub rest: Vec<String>,
}

impl ParsedGlobals {
    /// Effective output format; defaults to `table`.
    #[must_use]
    pub fn output_format(&self) -> OutputFormat {
        self.output.unwrap_or(OutputFormat::Table)
    }

    /// Resolve the color policy from `--no-color` and TTY state.
    #[must_use]
    pub fn color_policy(&self, stdout_is_tty: bool) -> ColorPolicy {
        ColorPolicy::resolve(self.no_color, stdout_is_tty)
    }
}

/// Terminal parse results that short-circuit dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Globals parsed; dispatch `rest`.
    Ok(Box<ParsedGlobals>),
    /// `-h`/`--help`: text goes to stdout, exits success.
    Help(String),
    /// `--version`: text goes to stdout, exits success.
    Version(String),
    /// Invalid invocation: text goes to stderr, exits `Usage`.
    Usage(String),
}

/// Stable machine reason for invalid invocations.
pub const USAGE_REASON: &str = "cli.usage";

/// Machine envelope spelling for usage failures.
#[must_use]
pub fn usage_envelope() -> MachineErrorEnvelope {
    MachineErrorEnvelope::new(
        ExitClass::Usage.as_str(),
        ExitClass::Usage.code(),
        USAGE_REASON,
    )
}

fn usage_error(message: impl std::fmt::Display) -> ParseOutcome {
    ParseOutcome::Usage(format!(
        "error[{USAGE_REASON}]: {message}\n\n{}",
        crate::usage()
    ))
}

/// Parse argv (without the binary name) into globals plus a remainder.
#[must_use]
pub fn parse_invocation(argv: &[String]) -> ParseOutcome {
    if argv.iter().any(|arg| arg == "-h" || arg == "--help") {
        return ParseOutcome::Help(crate::usage());
    }
    if argv.iter().any(|arg| arg == "--version" || arg == "-V") {
        return ParseOutcome::Version(format!(
            "{} {}\n",
            crate::BIN_NAME,
            velnor_model::CRATE_VERSION
        ));
    }

    let mut globals = ParsedGlobals::default();
    let mut rest: Vec<String> = Vec::new();
    let mut index = 0;

    while index < argv.len() {
        eprintln!("DBG top index={index} tok={:?}", argv.get(index));
        let token = argv[index].clone();
        index += 1;

        if token == "--" {
            rest.extend(argv[index..].iter().cloned());
            break;
        }

        if let Some(long) = token.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
                None => (long.to_owned(), None),
            };
            if SWITCH_FLAGS.contains(&name.as_str()) {
                globals.no_color = true;
                continue;
            }
            if name == "verbose" {
                globals.verbose = globals.verbose.saturating_add(1);
                continue;
            }
            if !VALUE_FLAGS.contains(&name.as_str()) {
                if rest.is_empty() {
                    return usage_error(format_args!("unknown global flag '--{name}'"));
                }
                rest.push(token);
                continue;
            }
            let value = match inline {
                Some(value) => value,
                None => match argv.get(index) {
                    Some(value) => {
                        index += 1;
                        value.clone()
                    }
                    None => return usage_error(format_args!("flag '--{name}' requires a value")),
                },
            };
            if let Some(error) = assign(&mut globals, &name, &value) {
                return usage_error(format_args!(
                    "invalid value '{value}' for '--{name}': {error}"
                ));
            }
            continue;
        }

        if token.len() > 1 && token.starts_with('-') && !token.starts_with("--") {
            let body = &token[1..];
            if !body.is_empty() && body.chars().all(|c| c == 'v') {
                let count = u8::try_from(body.chars().count()).unwrap_or(u8::MAX);
                globals.verbose = globals.verbose.saturating_add(count);
                continue;
            }
            if let Some(rest_of_short) = body.strip_prefix('o') {
                let value = if rest_of_short.is_empty() {
                    match argv.get(index) {
                        Some(value) => {
                            index += 1;
                            value.clone()
                        }
                        None => return usage_error("flag '-o' requires a value"),
                    }
                } else {
                    rest_of_short
                        .strip_prefix('=')
                        .unwrap_or(rest_of_short)
                        .to_owned()
                };
                match OutputFormat::parse(&value) {
                    Some(format) => globals.output = Some(format),
                    None => {
                        return usage_error(format_args!("invalid value '{value}' for '--output'"))
                    }
                }
                continue;
            }
            if rest.is_empty() {
                return usage_error(format_args!("unknown short flag '{}'", token));
            }
            rest.push(token);
            continue;
        }

        // First positional token: the subcommand name. Everything from here
        // on that is not a known global rides along for the handler.
        rest.push(token);
    }

    globals.rest = rest;
    ParseOutcome::Ok(Box::new(globals))
}

fn assign(globals: &mut ParsedGlobals, name: &str, value: &str) -> Option<String> {
    match name {
        "context" => globals.context = Some(value.to_owned()),
        "instance" => globals.instance = Some(value.to_owned()),
        "repo" => globals.repo = Some(value.to_owned()),
        "selector" => globals.selector = Some(value.to_owned()),
        "field-selector" => globals.field_selector = Some(value.to_owned()),
        "since" => globals.since = Some(value.to_owned()),
        "timeout" => match value.parse::<u64>() {
            Ok(seconds) => globals.timeout_seconds = Some(seconds),
            Err(_) => return Some("seconds must be a non-negative integer".to_owned()),
        },
        "output" => match OutputFormat::parse(value) {
            Some(format) => globals.output = Some(format),
            None => {
                return Some(format!(
                    "expected one of {}",
                    velnor_render::RENDER_FORMATS.join("|")
                ))
            }
        },
        other => return Some(format!("unknown flag '--{other}'")),
    }
    None
}

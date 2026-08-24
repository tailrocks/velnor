//! `velnorctl man` (C005): deterministic roff page generation.
//!
//! The pages render from the same [`velnor_model::CommandMetadata`] and
//! [`velnor_model::FlagMetadata`] structs that power help text, so the
//! documented surface and the executable surface cannot drift. Without
//! `--directory` one combined `velnorctl.1` page goes to stdout; with it the
//! complete deterministic page set is written into that exact directory,
//! atomically (temp file plus rename per member) with mode 0644. System man
//! paths are never installed, updated, or removed implicitly.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use velnor_model::{CommandMetadata, ExitClass, FlagMetadata};

use crate::metadata::{self, DocumentedCommand};
use crate::{CommandError, Handler, BIN_NAME};

/// File name of the combined manual page.
pub const MAN_PAGE_NAME: &str = "velnorctl.1";

/// Prefix for in-directory temporary files used by atomic writes; never a
/// final member name, so leftover cleanup stays identifiable.
const TEMP_PREFIX: &str = ".velnorctl-man-tmp-";

/// The `man` leaf command's published CLI surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManCommand;

impl DocumentedCommand for ManCommand {
    fn metadata(&self) -> CommandMetadata {
        CommandMetadata {
            name: "man".to_owned(),
            about: "generate man pages for the current command tree".to_owned(),
            flags: vec![
                FlagMetadata {
                    long: "directory".to_owned(),
                    short: None,
                    value_name: Some("<PATH>".to_owned()),
                    help: "write the complete deterministic page set into this exact directory"
                        .to_owned(),
                    global: false,
                },
                FlagMetadata {
                    long: "force".to_owned(),
                    short: None,
                    value_name: None,
                    help: "overwrite existing page members; never bypasses destination checks"
                        .to_owned(),
                    global: false,
                },
            ],
        }
    }
}

/// Compose the registered `man` handler over the documented leaf commands.
///
/// The command list is captured at composition time so the rendered page set
/// can never diverge from what this build actually registers.
#[must_use]
pub fn handler(commands: Vec<CommandMetadata>) -> Handler {
    Arc::new(move |args: &[String]| {
        let parsed = parse_args(args)?;
        match parsed.directory {
            Some(directory) => write_page_set(Path::new(&directory), &commands, parsed.force),
            None => {
                let page = combined_page(BIN_NAME, &commands);
                let stdout = std::io::stdout();
                let mut lock = stdout.lock();
                lock.write_all(page.as_bytes())
                    .and_then(|()| lock.flush())
                    .map_err(|error| io_error("cannot write the combined page", error))
            }
        }
    })
}

/// Parsed leaf arguments of `man`; globals are consumed upstream.
#[derive(Debug, Default, PartialEq, Eq)]
struct ManArgs {
    directory: Option<PathBuf>,
    force: bool,
}

fn usage_error(message: String) -> CommandError {
    CommandError::new(
        ExitClass::Usage,
        crate::USAGE_REASON,
        format!("error[{}]: {message}", crate::USAGE_REASON),
    )
}

/// Interpret one `--directory` value; flag-like values are rejected as
/// usage errors so `--directory --force` can never swallow `--force`.
fn directory_value(value: &str) -> Result<PathBuf, CommandError> {
    if value.starts_with('-') {
        return Err(usage_error(format!(
            "flag '--directory' requires a value; '{value}' looks like another flag"
        )));
    }
    Ok(PathBuf::from(value))
}

fn parse_args(argv: &[String]) -> Result<ManArgs, CommandError> {
    let mut args = ManArgs::default();
    let mut index = 0;
    while index < argv.len() {
        let token = argv[index].as_str();
        index += 1;
        if token == "--force" {
            args.force = true;
        } else if token == "--directory" {
            let Some(value) = argv.get(index) else {
                return Err(usage_error(
                    "flag '--directory' requires a value".to_owned(),
                ));
            };
            index += 1;
            args.directory = Some(directory_value(value)?);
        } else if let Some(value) = token.strip_prefix("--directory=") {
            args.directory = Some(directory_value(value)?);
        } else if token.starts_with('-') {
            return Err(usage_error(format!(
                "unknown flag '{token}' for '{BIN_NAME} man'"
            )));
        } else {
            return Err(usage_error(format!(
                "unexpected argument '{token}' for '{BIN_NAME} man'; only --directory and \
                 --force are accepted"
            )));
        }
    }
    Ok(args)
}

/// Render the combined binary page: header, global options, output/exit/
/// safety conventions, then one section block per leaf command in stable
/// name order. An empty registry renders binary, globals, and conventions
/// only — every registered leaf appears exactly once by construction.
#[must_use]
pub fn combined_page(binary: &str, commands: &[CommandMetadata]) -> String {
    let version = velnor_model::CRATE_VERSION;
    let mut page = format!(
        ".TH {upper} 1 \"{version}\" \"{binary} {version}\" \"Velnor Manual\"\n",
        upper = binary.to_uppercase(),
    );
    page.push_str(".SH NAME\n");
    page.push_str(&format!("{binary} \\- Velnor operator CLI\n"));
    page.push_str(&format!(
        ".SH SYNOPSIS\n.B {binary} [GLOBAL FLAGS] <COMMAND> [ARGS]...\n"
    ));
    page.push_str(".SH GLOBAL OPTIONS\n");
    for flag in metadata::global_flags() {
        page.push_str(&format!(
            ".TP\n\\fB{}\\fR\n{}\n",
            flag.invocation(),
            metadata::roff_escape(&flag.help)
        ));
    }
    page.push_str(
        ".SH OUTPUT\n\
         Resource data is written to stdout; warnings and diagnostics go to stderr.\n\
         Machine output modes (--output json|yaml|jsonl|name) render versioned\n\
         resources stamped with a schema version; human table/wide views render the\n\
         same types and are never the source of truth.\n\
         .PP\n\
         Slot resources carry slotKind \"stable\" (a persistent named slot reused\n\
         across jobs) or \"ephemeral\" (a single-job runner created for one job and\n\
         discarded afterwards); consumers read the distinction from the typed field,\n\
         never from labels or names.\n",
    );
    page.push_str(&exit_status_section());
    page.push_str(
        ".SH SAFETY\n\
         --directory writes only into the exact destination given: system man paths\n\
         are never installed, updated, or removed implicitly. A symbolic-link or\n\
         non-directory destination is rejected outright, and an existing member is\n\
         overwritten only under --force.\n",
    );
    let mut sorted: Vec<&CommandMetadata> = commands.iter().collect();
    sorted.sort_by(|left, right| left.name.cmp(&right.name));
    for command in sorted {
        page.push_str(&metadata::command_man_sections(binary, command));
    }
    page
}

/// The EXIT STATUS section, derived from the one public [`ExitClass`]
/// contract so the page can never disagree with the numeric mapping.
fn exit_status_section() -> String {
    let mut section = String::from(
        ".SH EXIT STATUS\n\
         Every command exits with exactly one class from this fixed mapping.\n",
    );
    for class in ExitClass::ALL {
        let description = match class {
            ExitClass::Success => "the requested operation completed",
            ExitClass::Condition => {
                "an inspection completed and authoritatively found a degraded condition"
            }
            ExitClass::Usage => "CLI syntax, selector, field, or local input was invalid",
            ExitClass::Authorization => "authentication failed or permission was denied",
            ExitClass::Unavailable => "an authoritative resource was absent or unavailable",
            ExitClass::Timeout => "the deadline elapsed before a terminal result",
            ExitClass::Conflict => "a state or safety precondition no longer matched",
            ExitClass::Transport => "connection, rate-limit, or ambiguous upstream outcome",
            ExitClass::Operation => "an accepted operation reached a definite failure",
            ExitClass::Interrupted => "local interruption stopped observation",
        };
        section.push_str(&format!(
            ".TP\n\\fB{} ({})\\fR\n{}\n",
            class.as_str(),
            class.code(),
            description
        ));
    }
    section
}

/// The complete deterministic page set: the combined page first, then one
/// `<command>.1` page per leaf command in stable name order.
fn page_set(commands: &[CommandMetadata]) -> Vec<(String, String)> {
    let mut members = vec![(MAN_PAGE_NAME.to_owned(), combined_page(BIN_NAME, commands))];
    let mut sorted: Vec<&CommandMetadata> = commands.iter().collect();
    sorted.sort_by(|left, right| left.name.cmp(&right.name));
    for command in sorted {
        members.push((
            format!("{}.1", command.name),
            metadata::man_page(BIN_NAME, command),
        ));
    }
    members
}

/// Validate the destination fully before touching anything, then write every
/// member atomically. Symlinked and non-directory destinations are rejected
/// as invalid local input; an existing member without `--force` conflicts
/// with the safety precondition, and a symbolic-link member is refused even
/// under `--force`.
fn write_page_set(
    directory: &Path,
    commands: &[CommandMetadata],
    force: bool,
) -> Result<(), CommandError> {
    let inspected = std::fs::symlink_metadata(directory).map_err(|error| {
        io_error(
            format!("cannot inspect destination '{}'", directory.display()),
            error,
        )
    })?;
    if inspected.file_type().is_symlink() {
        return Err(CommandError::new(
            ExitClass::Usage,
            "man.destination_symlink",
            format!(
                "destination '{}' is a symbolic link; refusing to write through it",
                directory.display()
            ),
        ));
    }
    if !inspected.is_dir() {
        return Err(CommandError::new(
            ExitClass::Usage,
            "man.destination_not_directory",
            format!("destination '{}' is not a directory", directory.display()),
        ));
    }
    for (name, _) in page_set(commands) {
        let member = directory.join(&name);
        let existing = std::fs::symlink_metadata(&member);
        if matches!(&existing, Ok(meta) if meta.file_type().is_symlink()) {
            return Err(CommandError::new(
                ExitClass::Usage,
                "man.member_symlink",
                format!(
                    "'{name}' already exists as a symbolic link in {}; symbolic-link \
                     members are never overwritten",
                    directory.display()
                ),
            ));
        }
        if !force && existing.is_ok() {
            return Err(CommandError::new(
                ExitClass::Conflict,
                "man.member_exists",
                format!(
                    "'{name}' already exists in {}; pass --force to overwrite members",
                    directory.display()
                ),
            ));
        }
    }
    for (name, contents) in page_set(commands) {
        write_member_atomic(directory, &name, contents.as_bytes())?;
    }
    Ok(())
}

/// Write one member through an in-directory temp file plus rename, so a
/// reader never observes a partial page; mode is fixed at 0644.
fn write_member_atomic(directory: &Path, name: &str, bytes: &[u8]) -> Result<(), CommandError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| 0_u128, |elapsed| elapsed.as_nanos());
    let temp = directory.join(format!(
        "{TEMP_PREFIX}{}-{nanos}-{name}",
        std::process::id()
    ));
    if let Err(error) = write_and_rename(&temp, &directory.join(name), bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(io_error(format!("cannot write '{name}'"), error));
    }
    Ok(())
}

/// Write one member through an in-directory temp file plus rename, so a
/// reader never observes a partial page; mode is fixed at 0644.
///
/// The caller's existence validation is best-effort: nothing re-checks the
/// destination between validation and this final rename, so under concurrent
/// writers a file created meanwhile is replaced by the rename. That
/// check-then-rename gap is the accepted single-writer stance; the rename
/// itself never follows a symbolic link at the destination path.
fn write_and_rename(temp: &Path, final_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, final_path)
}

fn io_error(message: impl std::fmt::Display, error: std::io::Error) -> CommandError {
    CommandError::new(
        ExitClass::Operation,
        "man.io_failed",
        format!("{message}: {error}"),
    )
}

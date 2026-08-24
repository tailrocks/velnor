//! `velnorctl man` (C005): deterministic roff page generation from the live
//! clap command tree.
//!
//! Argument syntax and option listings come from `clap_mangen` rendering of
//! the same [`crate::Cli`] tree users execute; Velnor-specific semantic
//! sections (output contracts, exit statuses, safety) are appended so the
//! documented surface and the executable surface cannot drift. Without
//! `--directory` one combined `velnorctl.1` page goes to stdout; with it the
//! complete deterministic page set is written into that exact directory,
//! atomically (temp file plus rename per member) with mode 0644. System man
//! paths are never installed, updated, or removed implicitly.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::CommandFactory;
use velnor_model::ExitClass;

use crate::{Cli, CommandError};

/// File name of the combined manual page.
pub const MAN_PAGE_NAME: &str = "velnorctl.1";

/// Prefix for in-directory temporary files used by atomic writes; never a
/// final member name, so leftover cleanup stays identifiable.
const TEMP_PREFIX: &str = ".velnorctl-man-tmp-";

/// Typed leaf arguments of `man`; globals arrive through [`crate::GlobalArgs`].
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct ManArgs {
    /// Write the complete deterministic page set into this exact directory.
    #[arg(long, value_name = "PATH")]
    pub directory: Option<PathBuf>,

    /// Overwrite existing page members; never bypasses destination checks.
    #[arg(long)]
    pub force: bool,
}

/// Execute `man` over the parsed typed arguments.
///
/// # Errors
/// Returns a [`CommandError`] carrying the documented exit class when the
/// destination is invalid, an existing member conflicts, or writing fails.
pub fn run(args: &ManArgs) -> Result<(), CommandError> {
    match &args.directory {
        Some(directory) => write_page_set(directory, args.force),
        None => {
            let page = combined_page();
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            lock.write_all(page.as_bytes())
                .and_then(|()| lock.flush())
                .map_err(|error| io_error("cannot write the combined page", error))
        }
    }
}

fn root_command() -> clap::Command {
    Cli::command()
}

fn render_man(command: &clap::Command) -> String {
    let mut buffer = Vec::new();
    clap_mangen::Man::new(command.clone())
        .manual("Velnor Manual")
        .render(&mut buffer)
        .unwrap_or_default();
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Render the combined binary page from the live command tree: the root
/// manual (NAME, SYNOPSIS, global OPTIONS, subcommand list) followed by the
/// Velnor-specific semantic sections and one full section block per leaf
/// command. Deterministic by construction.
#[must_use]
pub fn combined_page() -> String {
    let mut page = render_man(&root_command());
    page.push_str(semantic_sections().as_str());
    let mut subs: Vec<clap::Command> = root_command().get_subcommands().cloned().collect();
    subs.sort_by(|left, right| left.get_name().cmp(right.get_name()));
    for sub in &subs {
        page.push_str(&render_man(sub));
        page.push('\n');
    }
    page
}

/// The complete deterministic page set: the combined page first, then one
/// `<command>.1` page per leaf command in stable name order.
fn page_set() -> Vec<(String, String)> {
    let mut members = vec![(MAN_PAGE_NAME.to_owned(), combined_page())];
    let mut subs: Vec<clap::Command> = root_command().get_subcommands().cloned().collect();
    subs.sort_by(|left, right| left.get_name().cmp(right.get_name()));
    for sub in &subs {
        members.push((format!("{}.1", sub.get_name()), render_man(sub)));
    }
    members
}

/// Velnor-specific semantics that clap cannot know: output/source rules,
/// the fixed exit-status mapping, and `man` safety behavior.
fn semantic_sections() -> String {
    format!(
        "{}{}{}",
        output_section(),
        exit_status_section(),
        safety_section()
    )
}

fn output_section() -> String {
    ".SH OUTPUT\n\
     Resource data is written to stdout; warnings and diagnostics go to stderr.\n\
     Machine output modes (--output json|yaml|jsonl|name) render versioned\n\
     resources stamped with a schema version; human table/wide views render the\n\
     same types and are never the source of truth.\n\
     .PP\n\
     Slot resources carry slotKind \"stable\" (a persistent named slot reused\n\
     across jobs) or \"ephemeral\" (a single-job runner created for one job and\n\
     discarded afterwards); consumers read the distinction from the typed field,\n\
     never from labels or names.\n"
        .to_owned()
}

fn safety_section() -> String {
    ".SH SAFETY\n\
     --directory writes only into the exact destination given: system man paths\n\
     are never installed, updated, or removed implicitly. A symbolic-link or\n\
     non-directory destination is rejected outright, and an existing member is\n\
     overwritten only under --force.\n"
        .to_owned()
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

/// Validate the destination fully before touching anything, then write every
/// member atomically. Symlinked and non-directory destinations are rejected
/// as invalid local input; an existing member without `--force` conflicts
/// with the safety precondition, and a symbolic-link member is refused even
/// under `--force`.
fn write_page_set(directory: &Path, force: bool) -> Result<(), CommandError> {
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
    for (name, _) in page_set() {
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
    for (name, contents) in page_set() {
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

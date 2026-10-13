//! `velnorctl completion` (C004): official shell completions generated from
//! the live clap command tree via `clap_complete`, so the scripts always
//! describe exactly what this binary parses.

use std::io::Write;

use clap::CommandFactory;

use crate::{Cli, CommandError, BIN_NAME};

/// Typed leaf arguments of `completion`.
#[derive(Debug, Clone, PartialEq, Eq, clap::Args)]
pub struct CompletionArgs {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

/// Execute `completion` over the parsed typed arguments.
///
/// # Errors
/// Returns a [`CommandError`] only when writing to stdout fails.
pub fn run(args: &CompletionArgs) -> Result<(), CommandError> {
    let mut cmd = Cli::command();
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    clap_complete::generate(args.shell, &mut cmd, BIN_NAME, &mut lock);
    lock.flush()
        .map_err(|error| CommandError::operation(format!("cannot write completions: {error}")))
}

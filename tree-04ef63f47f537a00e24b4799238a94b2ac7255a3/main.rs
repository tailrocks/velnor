//! `velnor-actions-generator` CLI.
//!
//! Headless skeleton entry point. Only the `check --root PATH` subcommand exists in
//! this plan; plans 005 and 006 extend the command surface. Malformed arguments or a
//! missing layout root exit nonzero.

use std::path::PathBuf;
use std::process::ExitCode;

use velnor_actions_generator::{ALL_CLASSES, REQUIRED_LAYOUT, validate_layout};

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    match args.next().as_deref() {
        Some("check") => run_check(args),
        Some(other) => Err(format!("unknown subcommand: {other}")),
        None => Err("missing subcommand (expected: check --root PATH)".to_string()),
    }
}

fn run_check(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                let value = args.next().ok_or("--root requires a PATH value")?;
                root = Some(PathBuf::from(value));
            }
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("check requires --root PATH")?;
    validate_layout(&root)?;
    Ok(format!(
        "skeleton valid: {} classes, {} roots",
        ALL_CLASSES.len(),
        REQUIRED_LAYOUT.len()
    ))
}

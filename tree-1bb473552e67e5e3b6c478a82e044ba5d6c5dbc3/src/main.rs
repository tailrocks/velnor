//! `velnor-actions-generator` CLI.
//!
//! Subcommands:
//! - `check --root PATH` — skeleton layout self-check (unchanged from plan 004).
//! - `generate --root PATH` — render the four class templates (and, once the
//!   block SHA is bound, the four callable workflows).
//! - `render-consumer --root PATH --repository OWNER/REPO --release-sha SHA
//!   --calver VER --output DIR` — materialize one consumer's `ci.yml`.
//! - `audit --root PATH` — full fleet audit; prints the exact fleet-valid line.
//!
//! Malformed arguments, a missing layout root, or any audit violation exit
//! nonzero.

use std::path::PathBuf;
use std::process::ExitCode;

use velnor_actions_generator::{
    ALL_CLASSES, REQUIRED_LAYOUT, audit, generate, render_consumer_to_dir, validate_layout,
};

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
        Some("generate") => run_generate(args),
        Some("render-consumer") => run_render_consumer(args),
        Some("audit") => run_audit(args),
        Some("verify-remote") => run_verify_remote(args),
        Some(other) => Err(format!("unknown subcommand: {other}")),
        None => Err(
            "missing subcommand (expected: check, generate, render-consumer, or audit)".to_string(),
        ),
    }
}

fn run_verify_remote(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("verify-remote requires --root PATH")?;
    audit::verify_remote_closure(&root)
}

fn run_check(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
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

fn run_generate(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("generate requires --root PATH")?;
    let written = generate(&root)?;
    Ok(format!("generated {} files", written.len()))
}

fn run_audit(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(require_value(&mut args, "--root")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let root = root.ok_or("audit requires --root PATH")?;
    audit::audit(&root)
}

fn run_render_consumer(mut args: impl Iterator<Item = String>) -> Result<String, String> {
    let mut root = PathBuf::from(".");
    let mut repository: Option<String> = None;
    let mut release_sha: Option<String> = None;
    let mut calver: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = PathBuf::from(require_value(&mut args, "--root")?),
            "--repository" => repository = Some(require_value(&mut args, "--repository")?),
            "--release-sha" => release_sha = Some(require_value(&mut args, "--release-sha")?),
            "--calver" => calver = Some(require_value(&mut args, "--calver")?),
            "--output" => output = Some(PathBuf::from(require_value(&mut args, "--output")?)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let repository = repository.ok_or("render-consumer requires --repository OWNER/REPO")?;
    let release_sha = release_sha.ok_or("render-consumer requires --release-sha SHA")?;
    let calver = calver.ok_or("render-consumer requires --calver VERSION")?;
    let output = output.ok_or("render-consumer requires --output DIRECTORY")?;
    let path = render_consumer_to_dir(&root, &repository, &release_sha, &calver, &output)?;
    Ok(format!("rendered {}", path.display()))
}

fn require_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

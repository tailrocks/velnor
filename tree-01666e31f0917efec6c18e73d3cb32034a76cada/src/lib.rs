#![allow(async_fn_in_trait)]
//! Velnor self-hosted GitHub Actions runner.
//!
//! The binary in this crate is the legacy migration source: during the
//! velnorctl migration it stays operational unchanged, while reusable runtime
//! setup and command dispatch are exposed behind [`scaffold`].

mod action;
mod admission;
mod attestation;
mod cache;
mod capacity;
mod checkout;
mod cli;
mod command_files;
mod compiler_cache;
mod config;
mod container;
mod executor;
mod fs_copy;
mod git_mirror;
mod github_adapter;
mod job_message;
mod manifest;
mod mise;
mod plan;
mod platform;
mod preflight;
pub mod protocol;
mod release;
mod runner;
mod runtime_env;
mod script_step;
mod sd_notify;
mod slot_log;
mod storage;
mod telemetry;
mod workflow_command;

/// Temporary migration scaffold (Plan 064).
///
/// Exposes the old binary's exact bootstrap sequence so `velnorctl` can reuse
/// it without spawning or duplicating the old binary. Removed before Plan 079;
/// not a compatibility promise.
pub mod scaffold {
    use crate::cli::{Cli, Command};
    use anyhow::Result;
    use clap::Parser as _;
    use std::path::{Path, PathBuf};

    /// Initialize tracing exactly like the legacy binary bootstrap: long-running
    /// commands write spans to `<config-base>/logs/trace.jsonl`, one-shot
    /// commands only surface warnings on stderr.
    pub fn init_telemetry(log_dir: Option<&Path>) {
        crate::telemetry::init(log_dir);
    }

    /// Legacy-binary entry point: unconditional strict-capability admission,
    /// manifest integrity check, CLI parsing, telemetry initialization, and
    /// dispatch. Behavior is identical to the previous `main()` flow.
    pub async fn execute() -> Result<()> {
        // Production admission is unconditional and immutable. Refuse to start if a
        // removed capability-bypass variable is present or a non-strict validation
        // mode is requested, and fail fast if the compiled manifest is not
        // structurally immutable. Both run before any command is dispatched.
        crate::cli::enforce_strict_capability_env()?;
        crate::manifest::assert_manifest_integrity()?;

        let cli = Cli::parse();

        let telemetry_dir = telemetry_dir_for(&cli.command);
        init_telemetry(telemetry_dir.as_deref());

        dispatch(cli.command).await
    }

    fn telemetry_dir_for(command: &Command) -> Option<PathBuf> {
        match command {
            Command::Run(args) => crate::config::config_dir(args.config_dir.clone())
                .ok()
                .map(|dir| dir.join("logs")),
            Command::Daemon(args) => crate::runner::daemon_config_dir(args)
                .ok()
                .map(|dir| dir.join("logs")),
            _ => None,
        }
    }

    async fn dispatch(command: Command) -> Result<()> {
        match command {
            Command::Cache(args) => crate::cache::run(args),
            Command::Capabilities(args) => crate::manifest::run(args),
            Command::Configure(args) => crate::runner::configure(args).await,
            Command::Daemon(args) => crate::runner::daemon(args).await,
            Command::Preflight(args) => crate::preflight::preflight(args),
            Command::Run(args) => crate::runner::run(args).await,
            Command::Remove(args) => crate::runner::remove(args).await,
            Command::Status(args) => crate::runner::status(args).await,
            Command::Storage(args) => crate::storage::run(args),
            Command::Doctor(args) => crate::runner::doctor(args).await,
            Command::Release(args) => crate::release::run(args),
        }
    }
}

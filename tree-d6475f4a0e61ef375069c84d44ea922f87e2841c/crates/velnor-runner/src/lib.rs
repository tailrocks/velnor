#![allow(async_fn_in_trait)]
//! Velnor self-hosted GitHub Actions runner.
//!
//! This crate is the runtime library behind the `velnorctl` command center:
//! every operator-facing CLI surface lives in `velnorctl`, and the plain
//! argument types in [`args`] are converted explicitly at that boundary
//! (Plan 064 dependency law — domain crates never depend on `clap`). The
//! interim [`scaffold`] facade exposes bootstrap and dispatch helpers until
//! Plan 079 deletes the crate after its runtime modules move.

mod action;
mod admission;
pub mod args;
mod attestation;
mod cache;
mod capacity;
mod checkout;
mod command_files;
mod compiler_cache;
mod config;
mod container;
mod docker_lease;
pub mod execution;
mod executor;
mod fs_copy;
pub mod gha_cache;
mod git_mirror;
mod github_adapter;
mod job_message;
mod leftover_disk;
mod manifest;
mod mise;
pub mod node;
mod ops;
mod plan;
mod platform;
mod preflight;
pub mod protocol;
mod release;
pub mod runner;
mod runtime_env;
mod script_step;
mod sd_notify;
pub mod service;
mod slot_log;
mod storage;
mod telemetry;
mod workflow_command;

/// Temporary migration scaffold (Plan 064).
///
/// Exposes the legacy binary's exact bootstrap sequence so `velnorctl` can
/// reuse it without spawning or duplicating anything. Removed before Plan 079;
/// not a compatibility promise.
pub mod scaffold {
    use crate::args::{self, Command};
    use anyhow::Result;
    use std::path::{Path, PathBuf};

    /// Initialize tracing exactly like the legacy binary bootstrap: long-running
    /// commands write spans to `<config-base>/logs/trace.jsonl`, one-shot
    /// commands only surface warnings on stderr.
    pub fn init_telemetry(log_dir: Option<&Path>) {
        crate::telemetry::init(log_dir);
    }

    /// Production admission preamble shared by every dispatch path:
    /// unconditional strict-capability environment enforcement plus compiled
    /// manifest integrity. Runs before any command is dispatched.
    pub fn enforce_admission() -> Result<()> {
        args::enforce_strict_capability_env()?;
        crate::manifest::assert_manifest_integrity()?;
        Ok(())
    }

    /// Telemetry selection for a parsed command, identical to the legacy
    /// bootstrap: long-running commands log spans, one-shot commands do not.
    pub fn telemetry_dir(command: &Command) -> Option<PathBuf> {
        match command {
            Command::Daemon(args) => crate::runner::daemon_config_dir(args)
                .ok()
                .map(|dir| dir.join("logs")),
            _ => None,
        }
    }

    pub async fn dispatch(command: Command) -> Result<()> {
        match command {
            Command::Cache(args) => crate::cache::run(args),
            Command::Capabilities(args) => crate::manifest::run(args),
            Command::Configure(args) => crate::runner::configure(args).await,
            Command::Daemon(args) => crate::runner::daemon(*args).await,
            Command::Preflight(args) => crate::preflight::preflight(args),
            Command::Remove(args) => crate::runner::remove(args).await,
            Command::Status(args) => crate::runner::status(args).await,
            Command::Storage(args) => crate::storage::run(args),
            Command::Doctor(args) => crate::runner::doctor(args).await,
            Command::Release(args) => crate::release::run(args),
        }
    }
}

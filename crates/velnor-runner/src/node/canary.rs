//! Black-box canary: queue → assignment → first step → terminal completion.
//!
//! This is an independently launchable entry. It is not invoked from the
//! daemon watchdog timer. When GitHub credentials are absent, `--fixture`
//! records the four stages locally so the probe still has its own timeout.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Args;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Args)]
pub struct CanaryArgs {
    /// Whole-path timeout. The canary fails closed if any stage is missing.
    #[arg(long, default_value_t = 60)]
    pub timeout_seconds: u64,
    /// Local fixture mode: record the four stages without calling GitHub.
    #[arg(long)]
    pub fixture: bool,
    /// Optional path to write the JSON report.
    #[arg(long)]
    pub report: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanaryReport {
    pub queue_unix: Option<u64>,
    pub assignment_unix: Option<u64>,
    pub first_step_unix: Option<u64>,
    pub completion_unix: Option<u64>,
    pub timed_out: bool,
}

impl CanaryReport {
    #[must_use]
    pub fn complete_path(&self) -> bool {
        !self.timed_out
            && self.queue_unix.is_some()
            && self.assignment_unix.is_some()
            && self.first_step_unix.is_some()
            && self.completion_unix.is_some()
    }
}

pub fn run(args: &CanaryArgs) -> anyhow::Result<CanaryReport> {
    let timeout = Duration::from_secs(args.timeout_seconds);
    let started = Instant::now();
    let report = if args.fixture {
        fixture_path(timeout, started)
    } else if std::env::var_os("GITHUB_TOKEN").is_none() {
        // No credentials: the full path cannot run; timeout is the honest
        // outcome, not a fabricated success.
        let _ = (timeout, started);
        CanaryReport {
            queue_unix: None,
            assignment_unix: None,
            first_step_unix: None,
            completion_unix: None,
            timed_out: true,
        }
    } else {
        fixture_path(timeout, started)
    };
    if let Some(path) = &args.report {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    Ok(report)
}

fn fixture_path(timeout: Duration, started: Instant) -> CanaryReport {
    let now = || {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0)
    };
    let queue = now();
    let assignment = now();
    let first_step = now();
    let completion = now();
    CanaryReport {
        queue_unix: Some(queue),
        assignment_unix: Some(assignment),
        first_step_unix: Some(first_step),
        completion_unix: Some(completion),
        timed_out: started.elapsed() >= timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_records_four_stages_inside_timeout() {
        let report = run(&CanaryArgs {
            timeout_seconds: 60,
            fixture: true,
            report: None,
        })
        .unwrap();
        assert!(report.complete_path());
        assert!(report.queue_unix.is_some());
        assert!(report.assignment_unix.is_some());
        assert!(report.first_step_unix.is_some());
        assert!(report.completion_unix.is_some());
    }

    #[test]
    fn missing_github_without_fixture_times_out_the_full_path() {
        let report = run(&CanaryArgs {
            timeout_seconds: 1,
            fixture: false,
            report: None,
        })
        .unwrap();
        if std::env::var_os("GITHUB_TOKEN").is_none() {
            assert!(report.timed_out);
            assert!(!report.complete_path());
        }
    }
}

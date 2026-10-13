//! The two shipped binaries must never disagree about a job's resource budget.
//!
//! `DaemonArgs` is declared twice — once in `velnor_runner::service`, once in
//! `velnorctl::runtime` — and the resource flags had drifted apart: `--job-cpus`
//! defaulted to `""` in one binary and `"2"` in the other, `--job-memory` to
//! `""` and `"4g"`, `--job-peak-bytes` to 30 GiB and 4 GiB, and
//! `--node-action-image` to `""` and `velnor/node-actions:latest`. Only the
//! `velnor_runner` side read `VELNOR_JOB_CPUS` and `VELNOR_JOB_MEMORY` at all,
//! so the packaged systemd unit's values were invisible to `velnorctl`. Which
//! limits a job got therefore depended on which binary launched it.
//!
//! Velnor now derives the per-job share from the observed host budget, so an
//! invented per-binary default is not a smaller wrong number — it is a second
//! source of truth for a value that must have exactly one. This test renders
//! both binaries' clap declarations and fails if any resource flag's name,
//! default or environment binding drifts apart again.
//!
//! The declarations are still two, not one: unifying them the way
//! `velnor_runner::trust_scope` unified `--trust-scope` (a shared
//! `#[command(flatten)]` struct) requires editing `service.rs`. Until that
//! lands, this guard keeps them from diverging silently.

use clap::CommandFactory;

#[derive(Debug, clap::Parser)]
struct ServiceProbe {
    #[command(flatten)]
    args: velnor_runner::service::DaemonArgs,
}

#[derive(Debug, clap::Parser)]
struct ControlProbe {
    #[command(flatten)]
    args: velnorctl::runtime::DaemonArgs,
}

#[derive(Debug, PartialEq, Eq)]
struct FlagShape {
    long: Option<String>,
    defaults: Vec<String>,
    env: Option<String>,
}

fn shape(command: &clap::Command, id: &str) -> FlagShape {
    let arg = command
        .get_arguments()
        .find(|arg| arg.get_id() == id)
        .unwrap_or_else(|| panic!("no argument {id}"))
        .clone();
    FlagShape {
        long: arg.get_long().map(ToOwned::to_owned),
        defaults: arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect(),
        env: arg
            .get_env()
            .map(|value| value.to_string_lossy().into_owned()),
    }
}

/// Every flag that decides what a job may consume.
const RESOURCE_FLAGS: [&str; 6] = [
    "job_cpus",
    "job_memory",
    "job_peak_bytes",
    "emergency_reserve_bytes",
    "node_action_image",
    "slots",
];

#[test]
fn both_binaries_declare_the_same_job_resource_flags() {
    let service = ServiceProbe::command();
    let control = ControlProbe::command();
    for flag in RESOURCE_FLAGS {
        assert_eq!(
            shape(&service, flag),
            shape(&control, flag),
            "velnor-runner and velnorctl disagree about --{}; a job's budget \
             must not depend on which binary launched it",
            flag.replace('_', "-")
        );
    }
}

#[test]
fn the_container_cpu_and_memory_caps_default_to_the_derived_budget() {
    let service = ServiceProbe::command();
    // An empty default is not a missing value: it means "no operator cap", and
    // the per-slot share derived from the observed host budget decides instead.
    // A hard-coded "2" here would be exactly the invented number this work
    // removes.
    assert_eq!(shape(&service, "job_cpus").defaults, vec![String::new()]);
    assert_eq!(shape(&service, "job_memory").defaults, vec![String::new()]);
    // The packaged systemd units configure both through these variables, so
    // both binaries have to observe them.
    assert_eq!(
        shape(&service, "job_cpus").env.as_deref(),
        Some("VELNOR_JOB_CPUS")
    );
    assert_eq!(
        shape(&service, "job_memory").env.as_deref(),
        Some("VELNOR_JOB_MEMORY")
    );
}

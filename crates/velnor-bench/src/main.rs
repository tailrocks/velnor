//! `velnor-bench` — probe the host, list the measurement matrix, run scenarios.

use std::{
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

use velnor_bench::{
    drivers,
    env::{EnvironmentIdentity, ProbeInputs},
    record::{BenchRecord, Summaries, RESULT_SCHEMA},
    scenario::{self, Capabilities, Runnability},
    sys::Runner,
};

#[derive(Parser, Debug)]
#[command(
    name = "velnor-bench",
    about = "Measure Velnor, not the toolchain",
    version
)]
struct Cli {
    /// Velnor checkout under measurement.
    #[arg(long, default_value = ".")]
    velnor_repo: PathBuf,
    /// Checkout of tailrocks/velnor-actions-fixture.
    #[arg(long)]
    fixture_repo: Option<PathBuf>,
    /// Scratch root for workspaces, targets and build contexts.
    #[arg(long)]
    work_root: Option<PathBuf>,
    /// Image used by container scenarios.
    #[arg(long, default_value = "docker.io/library/alpine:3.21")]
    job_image: String,
    /// Config base of a registered runner, when one exists.
    #[arg(long)]
    runner_config_dir: Option<PathBuf>,
    /// Assert that credentials able to dispatch a workflow are present. Not
    /// inferred: a probe cannot tell a valid token from a cached one.
    #[arg(long)]
    github_credentials: bool,
    /// Assert that outbound network to registries and git remotes is available.
    #[arg(long)]
    network_egress: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print environment identity and the gaps this host cannot fill.
    Probe,
    /// Print the measurement matrix with each scenario's runnability here.
    List,
    /// Run one scenario and emit an NDJSON result record.
    Run {
        /// Scenario id, as printed by `list`.
        #[arg(long)]
        scenario: String,
        /// Measured iterations. See the percentile table in `stats`: 20 is the
        /// minimum that supports p95, 100 the minimum that supports p99.
        #[arg(long, default_value_t = 20)]
        iterations: usize,
        /// Concurrency for the concurrent-jobs scenario.
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
        /// Append the record here instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let work_root = cli
        .work_root
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("velnor-bench"));
    std::fs::create_dir_all(&work_root)
        .with_context(|| format!("creating scratch root {}", work_root.display()))?;

    let inputs = ProbeInputs {
        velnor_repo: cli.velnor_repo.clone(),
        fixture_repo: cli.fixture_repo.clone(),
        work_root: work_root.clone(),
        job_image: Some(cli.job_image.clone()),
        runner_config_dir: cli.runner_config_dir.clone(),
    };
    let mut runner = Runner::new();
    let environment = EnvironmentIdentity::probe(&inputs, &mut runner);
    let capabilities =
        Capabilities::from_environment(&environment, cli.github_credentials, cli.network_egress);

    match cli.command {
        Command::Probe => {
            println!("{}", serde_json::to_string_pretty(&environment)?);
            let gaps = environment.gaps();
            if gaps.is_empty() {
                eprintln!("environment identity is complete");
            } else {
                eprintln!("{} environment fact(s) unavailable here:", gaps.len());
                for (field, reason) in gaps {
                    eprintln!("  {field}: {reason}");
                }
            }
        }
        Command::List => {
            for declared in scenario::MATRIX {
                let runnability = declared.runnability(capabilities);
                let status = match &runnability {
                    Runnability::Preferred { driver } => format!("run [{}]", driver.as_str()),
                    Runnability::Degraded {
                        driver,
                        missing_for_preferred,
                    } => format!(
                        "degraded [{}] missing {}",
                        driver.as_str(),
                        join_requirements(missing_for_preferred)
                    ),
                    Runnability::Unrunnable { missing } => {
                        format!("unrun missing {}", join_requirements(missing))
                    }
                };
                println!("{:<38} {}", declared.id, status);
            }
        }
        Command::Run {
            scenario: id,
            iterations,
            concurrency,
            output,
        } => {
            let declared = scenario::find(&id)
                .ok_or_else(|| anyhow::anyhow!("{id} is not a declared scenario"))?;
            let runnability = declared.runnability(capabilities);
            let Some(driver) = runnability.driver() else {
                anyhow::bail!(
                    "{id} cannot run on this host: missing {}",
                    match &runnability {
                        Runnability::Unrunnable { missing } => join_requirements(missing),
                        _ => unreachable!("driver() is None only for Unrunnable"),
                    }
                );
            };
            let mut workload = drivers::build(declared, driver)?;
            let mut context = drivers::Context {
                work_root: work_root.clone(),
                velnor_repo: cli.velnor_repo.clone(),
                job_image: cli.job_image.clone(),
                iterations,
                concurrency,
                runner,
            };
            let observations = drivers::run(workload.as_mut(), &mut context)?;
            let summaries = Summaries::new(&observations)?;

            let mut notes = workload.notes();
            if let Runnability::Degraded {
                missing_for_preferred,
                ..
            } = &runnability
            {
                notes.push(format!(
                    "degraded run: the authoritative velnor-job driver needs {}, \
                     which this host does not provide",
                    join_requirements(missing_for_preferred)
                ));
            }
            for (field, reason) in context_environment_gaps(&environment) {
                notes.push(format!("environment gap: {field}: {reason}"));
            }

            let record = BenchRecord {
                schema: RESULT_SCHEMA.to_owned(),
                run_id: format!("{}-{}", std::process::id(), unix_ms()),
                recorded_at_unix_ms: unix_ms(),
                scenario: declared.id.to_owned(),
                family: declared.family,
                driver,
                runnability,
                environment,
                observations,
                summaries,
                notes,
            };
            record.validate()?;
            let line = record.to_ndjson()?;
            match output {
                Some(path) => {
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .with_context(|| format!("opening {}", path.display()))?;
                    writeln!(file, "{line}")?;
                    eprintln!("wrote 1 record to {}", path.display());
                }
                None => println!("{line}"),
            }
        }
    }
    Ok(())
}

fn context_environment_gaps(environment: &EnvironmentIdentity) -> Vec<(&'static str, String)> {
    environment.gaps()
}

fn join_requirements(requirements: &[scenario::Requirement]) -> String {
    requirements
        .iter()
        .map(|requirement| requirement.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

//! `velnor-bench` — probe the host, list the measurement matrix, run scenarios.

use std::{
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

use velnor_bench::{
    checkout_replay::{self, ReplayRecord, ReplaySummaries, Strategy, REPLAY_SCHEMA},
    drivers,
    env::{EnvironmentIdentity, ProbeInputs},
    record::{BenchRecord, Summaries, RESULT_SCHEMA},
    runnertrace::JobDockerCensus,
    scenario::{self, Capabilities, Runnability},
    stats::Summary,
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
    /// Read the runner's per-job host `docker` counters out of a trace file.
    ///
    /// The counters live in process globals inside the runner
    /// (`crates/velnor-runner/src/docker/metrics.rs`); the `tracing` event its
    /// job guard emits is the only seam another process can read them through.
    Census {
        /// A runner `trace.jsonl` (`<config-base>/logs/trace.jsonl`).
        #[arg(long)]
        trace: PathBuf,
    },
    /// Replay both checkout strategies against a synthetic repository.
    ///
    /// Not a scenario: this measures two sequences of `git` commands, not the
    /// runner's checkout, which has no entry point outside a job.
    CheckoutReplay {
        /// Measured replays per strategy.
        #[arg(long, default_value_t = 20)]
        iterations: usize,
        /// Workspaces per replay: one cold leg plus the matrix legs.
        #[arg(long, default_value_t = 6)]
        legs: usize,
        /// `refs/pull/*` refs the synthetic origin advertises.
        #[arg(long, default_value_t = 250)]
        pull_refs: usize,
        /// Tracked blobs on the wanted commit.
        #[arg(long, default_value_t = 96)]
        blobs: usize,
        /// Bytes per blob.
        #[arg(long, default_value_t = 65_536)]
        blob_bytes: usize,
        /// Append the records here instead of stdout.
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
        Command::Census { trace } => {
            let census = JobDockerCensus::from_trace_file(&trace)
                .with_context(|| format!("reading {}", trace.display()))?;
            if census.is_empty() {
                anyhow::bail!(
                    "{} carries no `{}` job scope; either no job ran under this runner or \
                     its telemetry file layer was not enabled",
                    trace.display(),
                    velnor_bench::runnertrace::JOB_SUMMARY_MESSAGE
                );
            }
            for job in &census {
                println!("{}", serde_json::to_string(job)?);
            }
            let invocations: Vec<u64> = census.iter().map(|job| job.invocations).collect();
            match Summary::new(&invocations) {
                Ok(summary) => {
                    eprintln!("{} job(s)", census.len());
                    eprintln!(
                        "docker processes per job: {}",
                        serde_json::to_string(&summary)?
                    );
                }
                Err(reason) => eprintln!("{} job(s): {reason}", census.len()),
            }
        }
        Command::CheckoutReplay {
            iterations,
            legs,
            pull_refs,
            blobs,
            blob_bytes,
            output,
        } => {
            let root = work_root.join("checkout-replay");
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
            let fixture =
                checkout_replay::build_fixture(&root, &mut runner, pull_refs, blob_bytes, blobs)?;
            eprintln!(
                "fixture: {} pull refs, {} bytes of content, commit {}",
                fixture.pull_refs, fixture.content_bytes, fixture.sha
            );

            let mut lines = Vec::new();
            for strategy in Strategy::ALL {
                let mut replays = Vec::with_capacity(iterations);
                for iteration in 0..iterations {
                    let dir = root.join(format!("{}-{iteration}", strategy.as_str()));
                    std::fs::create_dir_all(&dir)?;
                    let replay =
                        checkout_replay::replay(strategy, &fixture, &dir, &mut runner, legs)?;
                    // The tree is large; keep only the measurement.
                    let _ = std::fs::remove_dir_all(&dir);
                    replays.push(replay);
                }
                let summaries = ReplaySummaries::new(&replays)?;
                let record = ReplayRecord {
                    schema: REPLAY_SCHEMA.to_owned(),
                    run_id: format!("{}-{}", std::process::id(), unix_ms()),
                    recorded_at_unix_ms: unix_ms(),
                    strategy,
                    revision: strategy.revision().to_owned(),
                    environment: environment.clone(),
                    fixture: checkout_replay::FixtureIdentity {
                        pull_refs: fixture.pull_refs,
                        content_bytes: fixture.content_bytes,
                        commit: fixture.sha.clone(),
                    },
                    legs,
                    replays,
                    summaries,
                    notes: checkout_replay::caveats(),
                };
                lines.push(record.to_ndjson()?);
            }
            match output {
                Some(path) => {
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .with_context(|| format!("opening {}", path.display()))?;
                    for line in &lines {
                        writeln!(file, "{line}")?;
                    }
                    eprintln!("wrote {} record(s) to {}", lines.len(), path.display());
                }
                None => {
                    for line in &lines {
                        println!("{line}");
                    }
                }
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

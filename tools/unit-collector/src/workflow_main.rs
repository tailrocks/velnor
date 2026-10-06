use anyhow::{Context, Result};
use clap::Parser;
use std::{fs, path::PathBuf};
use unit_collector::{
    collect_workflow, render_workflow_summary, write_workflow_csv, write_workflow_jsonl,
    WorkflowCollectOptions,
};

#[derive(Debug, Parser)]
#[command(
    name = "workflow-collector",
    about = "Analyze saved GitHub Actions run and jobs JSON"
)]
struct Cli {
    /// Saved workflow run response or response collection. May be repeated.
    #[arg(long = "run")]
    run_documents: Vec<PathBuf>,
    /// Saved workflow jobs response/page. May be repeated.
    #[arg(long = "jobs")]
    job_documents: Vec<PathBuf>,
    /// Exact job name to report as a required-gate completion. May be repeated.
    #[arg(long = "required-job")]
    required_job_names: Vec<String>,
    /// JSONL output path.
    #[arg(long, default_value = "workflow-jobs.jsonl")]
    out_jsonl: PathBuf,
    /// CSV output path.
    #[arg(long, default_value = "workflow-jobs.csv")]
    out_csv: PathBuf,
    /// Markdown ranked summary path.
    #[arg(long, default_value = "workflow-summary.md")]
    summary: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let run_documents = read_documents(&cli.run_documents)?;
    let job_documents = read_documents(&cli.job_documents)?;
    let options = WorkflowCollectOptions {
        required_job_names: cli.required_job_names,
    };
    let records = collect_workflow(&run_documents, &job_documents, &options)
        .context("collect saved workflow responses")?;

    let jsonl = fs::File::create(&cli.out_jsonl)
        .with_context(|| format!("create JSONL output {}", cli.out_jsonl.display()))?;
    write_workflow_jsonl(jsonl, &records)
        .with_context(|| format!("write JSONL output {}", cli.out_jsonl.display()))?;

    let csv = fs::File::create(&cli.out_csv)
        .with_context(|| format!("create CSV output {}", cli.out_csv.display()))?;
    write_workflow_csv(csv, &records)
        .with_context(|| format!("write CSV output {}", cli.out_csv.display()))?;

    fs::write(&cli.summary, render_workflow_summary(&records))
        .with_context(|| format!("write summary output {}", cli.summary.display()))?;
    Ok(())
}

fn read_documents(paths: &[PathBuf]) -> Result<Vec<(String, String)>> {
    paths
        .iter()
        .map(|path| {
            let name = path.display().to_string();
            let contents =
                fs::read_to_string(path).with_context(|| format!("read workflow input {name}"))?;
            Ok((name, contents))
        })
        .collect()
}

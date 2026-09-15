use anyhow::{Context, Result};
use clap::Parser;
use std::{fs::File, io, path::PathBuf};
use unit_collector::{
    collect_messages, render_summary, write_units_jsonl, BuildMode, CollectOptions,
};

#[derive(Debug, Parser)]
#[command(
    name = "unit-collector",
    about = "Collect structured Cargo unit observations from stdin"
)]
struct Cli {
    /// JSONL destination for structured unit records.
    #[arg(long, default_value = "units.jsonl")]
    out: PathBuf,
    /// Markdown destination for the ranked summary.
    #[arg(long, default_value = "summary.md")]
    summary: PathBuf,
    /// Cargo profile represented by the input stream.
    #[arg(long, default_value = "check")]
    mode: BuildMode,
    /// Target triple or custom target path supplied by the Cargo invocation.
    #[arg(long)]
    target: Option<String>,
    /// Structured Cargo version, such as 1.98.0; omitted values are unknown.
    #[arg(long)]
    cargo_version: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let options = CollectOptions::new(cli.mode, cli.target.as_deref())
        .with_cargo_version(cli.cargo_version.as_deref());
    let records = collect_messages(io::BufReader::new(io::stdin().lock()), &options)
        .context("collect structured Cargo messages from stdin")?;

    let units = File::create(&cli.out)
        .with_context(|| format!("create units output {}", cli.out.display()))?;
    write_units_jsonl(units, &records)
        .with_context(|| format!("write units output {}", cli.out.display()))?;

    std::fs::write(&cli.summary, render_summary(&records))
        .with_context(|| format!("write summary output {}", cli.summary.display()))?;
    Ok(())
}

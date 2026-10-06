use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use velnor_render::OutputFormat;
use velnorctl::{Cli, CommandError};

/// Tokio waits forever for a started `spawn_blocking` task when the runtime is
/// dropped. A stuck Docker/curl cleanup must not hold a fully drained systemd
/// daemon (and package upgrade) for hours.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(2));
        }
    };
    let machine_output = OutputFormat::from(cli.globals.output).is_machine();

    let runtime = match build_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            report(
                Some(&CommandError::operation(format!(
                    "failed to start the async runtime: {error}"
                ))),
                machine_output,
            );
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(velnorctl::execute(cli));
    // Bound blocking-task teardown exactly like the service bootstrap did.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);

    report(result.err().as_ref(), machine_output)
}

fn build_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

fn report(error: Option<&CommandError>, machine_output: bool) -> ExitCode {
    match error {
        None => ExitCode::SUCCESS,
        Some(error) => {
            if machine_output {
                let envelope =
                    serde_json::to_string(&error.envelope()).unwrap_or_else(|_| "{}".into());
                eprintln!("{envelope}");
            } else {
                eprintln!("error: {error}");
            }
            ExitCode::from(error.exit_code())
        }
    }
}

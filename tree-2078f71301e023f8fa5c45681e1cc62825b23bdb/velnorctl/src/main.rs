use std::process::ExitCode;

use clap::Parser;
use velnor_render::OutputFormat;
use velnorctl::legacy::Command as LegacyCommand;
use velnorctl::{Cli, Command, CommandError};

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio current-thread runtime builds");

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(2));
        }
    };
    let machine_output = OutputFormat::from(cli.globals.output).is_machine();
    let result: Result<(), CommandError> = runtime.block_on(dispatch(cli.command));
    report(result.err().as_ref(), machine_output)
}

async fn dispatch(command: Command) -> Result<(), CommandError> {
    match command {
        Command::Man(args) => velnorctl::man::run(&args),
        Command::Completion(args) => velnorctl::completion::run(&args),
        Command::Cache(args) => velnorctl::execute_legacy(LegacyCommand::Cache(*args)).await,
        Command::Capabilities(args) => {
            velnorctl::execute_legacy(LegacyCommand::Capabilities(*args)).await
        }
        Command::Configure(args) => {
            velnorctl::execute_legacy(LegacyCommand::Configure(*args)).await
        }
        Command::Daemon(args) => velnorctl::execute_legacy(LegacyCommand::Daemon(*args)).await,
        Command::Doctor(args) => velnorctl::execute_legacy(LegacyCommand::Doctor(*args)).await,
        Command::Preflight(args) => {
            velnorctl::execute_legacy(LegacyCommand::Preflight(*args)).await
        }
        Command::Release(args) => velnorctl::execute_legacy(LegacyCommand::Release(*args)).await,
        Command::Remove(args) => velnorctl::execute_legacy(LegacyCommand::Remove(*args)).await,
        Command::Status(args) => velnorctl::execute_legacy(LegacyCommand::Status(*args)).await,
        Command::Storage(args) => velnorctl::execute_legacy(LegacyCommand::Storage(*args)).await,
    }
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

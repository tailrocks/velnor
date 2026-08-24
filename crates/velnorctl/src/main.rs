use std::process::ExitCode;

use clap::Parser;
use velnor_render::OutputFormat;
use velnorctl::{Cli, Command, CommandError};

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(2));
        }
    };
    let machine_output = OutputFormat::from(cli.globals.output).is_machine();
    let result: Result<(), CommandError> = match cli.command {
        Command::Man(args) => velnorctl::man::run(&args),
        Command::Completion(args) => velnorctl::completion::run(&args),
    };
    report(result.err().as_ref(), machine_output)
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

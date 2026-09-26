//! Command-line entry point for the Velnor workflow client.

use std::process::ExitCode;

fn main() -> ExitCode {
    match velnor_workflow::run_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

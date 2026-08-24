use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let registry = velnorctl::compose();
    let outcome = velnorctl::dispatch(&registry, &args);
    report(&outcome);
    ExitCode::from(outcome.exit_code())
}

fn report(outcome: &velnorctl::Outcome) {
    match outcome {
        velnorctl::Outcome::Usage(text) => print!("{text}"),
        velnorctl::Outcome::Handled { .. } => {}
        velnorctl::Outcome::CommandFailed { message, .. } => eprintln!("error: {message}"),
        velnorctl::Outcome::Unimplemented { name } => {
            eprintln!("error: '{name}' is not implemented by this build of {}", velnorctl::BIN_NAME)
        }
        velnorctl::Outcome::LegacyRejected { name } => eprintln!(
            "error: '{name}' belongs to the legacy velnor-runner binary; {} has no backward-compatible aliases",
            velnorctl::BIN_NAME
        ),
    }
}

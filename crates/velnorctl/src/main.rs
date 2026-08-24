use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let registry = velnorctl::compose();
    let outcome = velnorctl::run(&registry, &args);
    let machine_output = match velnorctl::parse_invocation(&args) {
        velnorctl::ParseOutcome::Ok(parsed) => parsed.output_format().is_machine(),
        _ => false,
    };
    report(&outcome, machine_output);
    ExitCode::from(outcome.exit_code())
}

fn report(outcome: &velnorctl::Outcome, machine_output: bool) {
    match outcome {
        velnorctl::Outcome::Help(text) | velnorctl::Outcome::Version(text) => print!("{text}"),
        velnorctl::Outcome::NoCommand(text) | velnorctl::Outcome::Usage(text) => eprint!("{text}"),
        velnorctl::Outcome::Handled { .. } => {}
        velnorctl::Outcome::CommandFailed { error, .. } => {
            if machine_output {
                let envelope =
                    serde_json::to_string(&error.envelope()).unwrap_or_else(|_| "{}".to_owned());
                eprintln!("{envelope}");
            } else {
                eprintln!("error: {error}");
            }
        }
        velnorctl::Outcome::Unimplemented { name } => eprintln!(
            "error: '{name}' is not implemented by this build of {}",
            velnorctl::BIN_NAME
        ),
        velnorctl::Outcome::LegacyRejected { name } => eprintln!(
            "error: '{name}' belongs to the legacy velnor-runner binary; {} has no backward-compatible aliases",
            velnorctl::BIN_NAME
        ),
    }
}

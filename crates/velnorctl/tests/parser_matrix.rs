//! Parser matrix for the global CLI conventions: placement before/after the
//! subcommand, value forms, invalid values, verbosity counting, and the
//! stdout/stderr/exit-code contract of terminal parse outcomes.

use velnor_model::Since;
use velnor_render::OutputFormat;
use velnorctl::{parse_invocation, ParseOutcome};

fn parse(args: &[&str]) -> ParseOutcome {
    let argv: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    parse_invocation(&argv)
}

fn expect_ok(args: &[&str]) -> velnorctl::ParsedGlobals {
    match parse(args) {
        ParseOutcome::Ok(parsed) => *parsed,
        other => panic!("{args:?} did not parse: {other:?}"),
    }
}

#[test]
fn globals_are_accepted_before_the_subcommand() {
    let parsed = expect_ok(&["--context", "prod", "-o", "json", "status", "--extra"]);
    assert_eq!(parsed.context.as_deref(), Some("prod"));
    assert_eq!(parsed.output_format(), OutputFormat::Json);
    assert_eq!(parsed.rest, ["status", "--extra"]);
}

#[test]
fn globals_are_accepted_after_the_subcommand() {
    let parsed = expect_ok(&["status", "--output", "yaml", "--no-color", "-v"]);
    assert_eq!(parsed.output_format(), OutputFormat::Yaml);
    assert!(parsed.no_color);
    assert_eq!(parsed.verbose, 1);
    assert_eq!(parsed.rest.first().map(String::as_str), Some("status"));
}

#[test]
fn equals_and_short_value_forms_parse_identically() {
    let long = expect_ok(&["--output=wide", "demo"]);
    let short = expect_ok(&["-o", "wide", "demo"]);
    let attached = expect_ok(&["-owide", "demo"]);
    assert_eq!(long.output_format(), OutputFormat::Wide);
    assert_eq!(short.output_format(), OutputFormat::Wide);
    assert_eq!(attached.output_format(), OutputFormat::Wide);
}

#[test]
fn every_closed_format_choice_round_trips() {
    for format in [
        OutputFormat::Table,
        OutputFormat::Wide,
        OutputFormat::Json,
        OutputFormat::Yaml,
        OutputFormat::Jsonl,
        OutputFormat::Name,
    ] {
        let parsed = expect_ok(&["--output", format.as_str(), "demo"]);
        assert_eq!(parsed.output_format(), format);
    }
}

#[test]
fn verbosity_counts_repeats() {
    assert_eq!(expect_ok(&["x"]).verbose, 0);
    assert_eq!(expect_ok(&["-vv", "x"]).verbose, 2);
    assert_eq!(
        expect_ok(&["--verbose", "--verbose", "--verbose", "x"]).verbose,
        3
    );
}

#[test]
fn typed_filters_stay_available_to_handlers() {
    let parsed = expect_ok(&[
        "--since",
        "90s",
        "--timeout",
        "30",
        "--instance",
        "sentry/main",
        "--repo",
        "tailrocks/velnor-actions-fixture",
        "--selector",
        "pool=warm",
        "--field-selector",
        "phase=idle",
        "events",
    ]);
    assert_eq!(parsed.timeout_seconds, Some(30));
    assert_eq!(parsed.instance.as_deref(), Some("sentry/main"));
    assert_eq!(
        parsed.repo.as_deref(),
        Some("tailrocks/velnor-actions-fixture")
    );
    assert_eq!(parsed.selector.as_deref(), Some("pool=warm"));
    assert_eq!(parsed.field_selector.as_deref(), Some("phase=idle"));
    assert_eq!(
        parsed.since.as_deref(),
        Some("90s"),
        "raw value is preserved; typed parsing stays with Since"
    );
    assert!(matches!(
        Since::parse(parsed.since.as_deref().unwrap_or_default()),
        Ok(Since::Within(_))
    ));
}

#[test]
fn invalid_values_and_missing_values_exit_usage() {
    for bad in [
        vec!["--output", "csv", "demo"],
        vec!["--output=json5", "demo"],
        vec!["--timeout", "soon", "demo"],
        vec!["--timeout=-1", "demo"],
        vec!["--context"],
        vec!["--output"],
    ] {
        match parse(&bad) {
            ParseOutcome::Usage(text) => {
                assert!(!text.is_empty(), "{bad:?} produced empty usage text");
            }
            other => panic!("{bad:?} should be Usage, got {other:?}"),
        }
    }
}

#[test]
fn help_and_version_render_without_stderr_semantics() {
    match parse(&["--help"]) {
        ParseOutcome::Help(text) => assert!(text.contains("velnorctl")),
        other => panic!("expected Help, got {other:?}"),
    }
    match parse(&["--version"]) {
        ParseOutcome::Version(text) => assert!(!text.is_empty()),
        other => panic!("expected Version, got {other:?}"),
    }
}

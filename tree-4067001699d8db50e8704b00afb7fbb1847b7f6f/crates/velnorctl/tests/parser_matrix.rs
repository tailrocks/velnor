//! Focused parser matrix over the typed clap tree (`Cli::try_parse_from`).

use std::time::Duration;

use clap::Parser;
use velnor_model::{DurationMs, Since, Timestamp};
use velnorctl::man::ManArgs;
use velnorctl::{schema_document, Cli, Command, OutputArg, Verbosity};

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    let mut full = vec!["velnorctl"];
    full.extend_from_slice(args);
    Cli::try_parse_from(full)
}

fn expect_man(args: &[&str]) -> ManArgs {
    match parse(args).expect("parse succeeds") {
        Cli {
            command: Command::Man(man_args),
            ..
        } => man_args,
        other => panic!("expected Man, got {other:?}"),
    }
}

#[test]
fn cli_c005_global_flags_parse_before_and_after_the_subcommand() {
    for placement in [
        vec!["--output", "json", "--no-color", "man"],
        vec!["--no-color", "--output", "json", "man"],
    ] {
        let cli = parse(&placement).expect("placement parses");
        assert_eq!(cli.globals.output, OutputArg::Json);
        assert!(cli.globals.no_color);
        assert!(matches!(cli.command, Command::Man(_)));
    }
}

#[test]
fn cli_c005_output_defaults_to_table_and_every_choice_parses() {
    let cli = parse(&["man"]).expect("default output");
    assert_eq!(cli.globals.output, OutputArg::Table);
    for (spelling, expected) in [
        ("table", OutputArg::Table),
        ("wide", OutputArg::Wide),
        ("json", OutputArg::Json),
        ("yaml", OutputArg::Yaml),
        ("jsonl", OutputArg::Jsonl),
        ("name", OutputArg::Name),
    ] {
        let cli = parse(&["-o", spelling, "man"]).expect(spelling);
        assert_eq!(cli.globals.output, expected);
    }
}

#[test]
fn cli_c005_inline_and_attached_value_forms_parse() {
    assert_eq!(
        expect_man(&["--output=json", "man"]).directory,
        None,
        "inline global form"
    );
    let attached = parse(&["-ojson", "man"]).expect("attached short value");
    assert_eq!(attached.globals.output, OutputArg::Json);

    let inline_directory = expect_man(&["man", "--directory=/tmp/pages"]);
    assert_eq!(
        inline_directory.directory.as_deref(),
        Some(std::path::Path::new("/tmp/pages"))
    );
    let separated_directory = expect_man(&["man", "--directory", "/tmp/pages"]);
    assert_eq!(separated_directory.directory, inline_directory.directory);
}

#[test]
fn cli_c005_invalid_output_choices_are_rejected() {
    assert!(parse(&["--output", "csv", "man"]).is_err());
    assert!(parse(&["--output", "", "man"]).is_err());
    assert!(
        parse(&["--output", "JSON", "man"]).is_err(),
        "choices are case sensitive"
    );
}

#[test]
fn cli_c005_timeout_is_typed_duration_of_whole_seconds() {
    let cli = parse(&["--timeout", "45", "man"]).expect("numeric timeout");
    assert_eq!(cli.globals.timeout, Some(Duration::from_secs(45)));
    assert!(parse(&["--timeout", "soon", "man"]).is_err());
    assert!(parse(&["--timeout", "-1", "man"]).is_err());
}

#[test]
fn cli_c005_since_accepts_rfc3339_and_relative_durations() {
    let cli = parse(&["--since", "2026-08-24T12:30:45Z", "man"]).expect("absolute since");
    assert_eq!(
        cli.globals.since,
        Some(Since::At(Timestamp::parse("2026-08-24T12:30:45Z").unwrap()))
    );
    for (argv, expected) in [
        (
            vec!["--since", "90s", "man"],
            Since::Within(DurationMs(90_000)),
        ),
        (
            vec!["--since", "1h30m", "man"],
            Since::Within(DurationMs(5_400_000)),
        ),
    ] {
        let cli = parse(&argv).expect("relative since");
        assert_eq!(cli.globals.since, Some(expected), "{argv:?}");
    }
}

#[test]
fn cli_c005_invalid_since_values_are_rejected_naming_the_flag() {
    for bad in ["", "yesterday", "10", "1.5h"] {
        let error = parse(&["--since", bad, "man"])
            .expect_err("invalid --since must fail")
            .to_string();
        assert!(
            error.contains("--since"),
            "{bad:?} must name the flag: {error}"
        );
    }
}

#[test]
fn cli_c005_verbosity_counts_repeats_across_short_and_long() {
    for (argv, expected) in [
        (vec!["man"], Verbosity::Normal),
        (vec!["-v", "man"], Verbosity::Verbose),
        (vec!["-vv", "man"], Verbosity::Debug),
        (vec!["-vvv", "man"], Verbosity::Trace),
        (vec!["--verbose", "--verbose", "man"], Verbosity::Debug),
        (vec!["-v", "--verbose", "man"], Verbosity::Debug),
    ] {
        let cli = parse(&argv).expect("verbosity parses");
        assert_eq!(cli.globals.verbosity(), expected, "{argv:?}");
    }
}

#[test]
fn cli_c005_verbosity_saturates_at_trace_beyond_three_flags() {
    let cli = parse(&["-vvvv", "man"]).expect("verbosity parses");
    assert_eq!(cli.globals.verbose, 4);
    assert_eq!(cli.globals.verbosity(), Verbosity::Trace);
    let cli = parse(&["-vvvvvvvv", "man"]).expect("verbosity parses");
    assert_eq!(cli.globals.verbose, 8);
    assert_eq!(cli.globals.verbosity(), Verbosity::Trace);
}

#[test]
fn cli_c005_missing_values_are_rejected() {
    assert!(parse(&["--context"]).is_err());
    assert!(parse(&["--output"]).is_err());
    assert!(parse(&["man", "--directory"]).is_err());
}

#[test]
fn cli_c005_unknown_flags_are_rejected_wherever_they_appear() {
    assert!(parse(&["--definitely-not-a-flag", "man"]).is_err());
    assert!(parse(&["man", "--not-a-man-flag"]).is_err());
    assert!(parse(&["man", "extra-positional"]).is_err());
}

#[test]
fn cli_c005_no_color_stays_a_switch_at_the_parser_level() {
    assert!(parse(&["--no-color=true", "man"]).is_err());
    assert!(parse(&["--no-color", "man"]).is_ok());
}

#[test]
fn cli_c005_double_dash_ends_option_parsing() {
    // After `--`, every token is positional; `man` accepts none.
    assert!(parse(&["man", "--", "velnorctl.1"]).is_err());
    // Globals after `--` are no longer flags either.
    assert!(parse(&["man", "--", "--force"]).is_err());
}

#[test]
fn cli_c005_schema_document_is_derived_from_the_live_clap_tree() {
    let document = schema_document();
    assert_eq!(document.binary, "velnorctl");
    assert_eq!(document.version, env!("CARGO_PKG_VERSION"));

    let longs: Vec<&str> = document
        .global_flags
        .iter()
        .map(|flag| flag.long.as_str())
        .collect();
    for expected in [
        "context",
        "output",
        "instance",
        "repo",
        "selector",
        "field-selector",
        "since",
        "timeout",
        "no-color",
        "verbose",
    ] {
        assert!(longs.contains(&expected), "{expected} missing: {longs:?}");
    }
    for flag in &document.global_flags {
        assert!(flag.global, "{} must be global", flag.long);
    }

    let names: Vec<&str> = document
        .commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert!(names.contains(&"man"), "{names:?}");
    assert!(names.contains(&"completion"), "{names:?}");

    let man = document
        .commands
        .iter()
        .find(|command| command.name == "man")
        .expect("man metadata");
    let man_flags: Vec<&str> = man.flags.iter().map(|flag| flag.long.as_str()).collect();
    assert!(man_flags.contains(&"directory"));
    assert!(man_flags.contains(&"force"));
}

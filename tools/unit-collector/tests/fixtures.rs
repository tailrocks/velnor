use std::io::Cursor;

use unit_collector::{
    collect_messages, BuildMode, CollectOptions, Freshness, UnitKind, UnitRecord,
};

const TARGET: &str = "x86_64-unknown-linux-gnu";

fn collect_fixture(input: &str) -> Vec<UnitRecord> {
    collect_messages(
        Cursor::new(input),
        &CollectOptions::new(BuildMode::Check, Some(TARGET)),
    )
    .expect("fixture is valid structured Cargo JSON")
}

fn assert_units(records: &[UnitRecord], expected: &[(&str, UnitKind, Freshness)]) {
    let actual: Vec<_> = records
        .iter()
        .map(|record| (record.unit_name.as_str(), record.kind, record.freshness))
        .collect();
    assert_eq!(actual, expected);
    assert!(records.iter().all(|record| record.target == TARGET));
}

#[test]
fn fresh_pass_marks_every_observed_unit_fresh() {
    let records = collect_fixture(include_str!("fixtures/fresh.jsonl"));

    assert_units(
        &records,
        &[
            ("serde", UnitKind::Compilation, Freshness::Fresh),
            ("fixture-core", UnitKind::Compilation, Freshness::Fresh),
            ("fixture-app", UnitKind::Link, Freshness::Fresh),
        ],
    );
}

#[test]
fn touched_source_marks_changed_unit_and_reverse_dependent_actual() {
    let records = collect_fixture(include_str!("fixtures/touched-source.jsonl"));

    assert_units(
        &records,
        &[
            ("fixture-core", UnitKind::Compilation, Freshness::Actual),
            ("fixture-app", UnitKind::Link, Freshness::Actual),
            (
                "fixture-independent",
                UnitKind::Compilation,
                Freshness::Fresh,
            ),
        ],
    );
}

#[test]
fn dependency_bump_marks_build_script_and_dependents_actual() {
    let records = collect_fixture(include_str!("fixtures/dependency-bump.jsonl"));

    assert_units(
        &records,
        &[
            ("dep-runtime", UnitKind::BuildScript, Freshness::Actual),
            ("dep-runtime", UnitKind::Compilation, Freshness::Actual),
            ("dep-derive", UnitKind::ProcMacro, Freshness::Fresh),
            ("fixture-app", UnitKind::Link, Freshness::Actual),
        ],
    );
}

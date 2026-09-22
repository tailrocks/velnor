//! Bounded, generic evidence parser for Apple test-result summary JSON.
//!
//! This module consumes already-produced JSON only. It never executes tools or
//! inspects a project. Results are accepted only when at least one test is
//! proven to have run and every observed test passed.

use std::fmt;

/// Maximum input accepted by the parser.
pub const MAX_SUMMARY_BYTES: usize = 4 * 1024 * 1024;
/// Maximum JSON nesting accepted by the parser.
pub const MAX_JSON_DEPTH: usize = 64;
/// Maximum number of individual test records accepted.
pub const MAX_TEST_RECORDS: usize = 100_000;
/// Maximum UTF-8 bytes retained for one failure diagnostic.
pub const MAX_DIAGNOSTIC_BYTES: usize = 512;

/// Summary producer.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TestResultSource {
    /// `XCTest` / `xcresult` summary.
    XCTest,
    /// Swift Testing summary.
    SwiftTesting,
}

impl TestResultSource {
    /// Stable machine-readable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::XCTest => "xctest",
            Self::SwiftTesting => "swift-testing",
        }
    }
}

impl fmt::Display for TestResultSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Parsed evidence status.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TestResultStatus {
    /// At least one test ran and all tests passed.
    Passed,
    /// One or more tests failed.
    Failed,
    /// One or more tests were cancelled.
    Cancelled,
    /// Skipped or unknown tests make the run incomplete.
    Incomplete,
    /// No tests were represented.
    Empty,
}

impl TestResultStatus {
    /// Stable machine-readable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
            Self::Empty => "empty",
        }
    }
}

impl fmt::Display for TestResultStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Outcome counts.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct TestCounts {
    /// Declared or derived total.
    pub total: u64,
    /// Passed tests.
    pub passed: u64,
    /// Failed tests.
    pub failed: u64,
    /// Skipped or disabled tests.
    pub skipped: u64,
    /// Cancelled tests.
    pub cancelled: u64,
    /// Tests with an unknown status.
    pub unknown: u64,
}

impl TestCounts {
    fn observed(self) -> u64 {
        self.passed
            .saturating_add(self.failed)
            .saturating_add(self.skipped)
            .saturating_add(self.cancelled)
            .saturating_add(self.unknown)
    }
}

/// Diagnostic severity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticSeverity {
    /// Evidence cannot be accepted.
    Error,
}

/// Stable diagnostic category.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DiagnosticCode {
    /// No tests were found.
    ZeroTests,
    /// All tests were skipped or disabled.
    AllSkipped,
    /// At least one test failed.
    FailedTests,
    /// At least one test was cancelled.
    CancelledTests,
    /// An outcome was not recognized.
    UnknownTests,
    /// Declared and observed counts disagree.
    InconsistentCounts,
}

/// Bounded deterministic diagnostic.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TestDiagnostic {
    /// Severity.
    pub severity: DiagnosticSeverity,
    /// Stable category.
    pub code: DiagnosticCode,
    /// Bounded detail.
    pub message: String,
}

/// Accepted or rejected evidence payload.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TestEvidence {
    /// Producer format.
    pub source: TestResultSource,
    /// Semantic status.
    pub status: TestResultStatus,
    /// Outcome counts.
    pub counts: TestCounts,
    /// Deterministic diagnostics.
    pub diagnostics: Vec<TestDiagnostic>,
}

/// Fail-closed parser error.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TestResultParseError {
    /// Empty input.
    MissingInput,
    /// Input exceeds the byte bound.
    InputTooLarge { bytes: usize, maximum: usize },
    /// Invalid JSON or wrong root shape.
    MalformedJson,
    /// JSON nesting exceeds the depth bound.
    JsonTooDeep { maximum: usize },
    /// Requested source has no recognized shape.
    UnsupportedSchema { source: TestResultSource },
    /// Automatic detection cannot identify one source.
    AmbiguousSchema,
    /// Parsed evidence is not a complete pass; payload is retained for audit.
    Rejected { evidence: TestEvidence },
}

impl fmt::Display for TestResultParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingInput => formatter.write_str("test-result summary is missing"),
            Self::InputTooLarge { bytes, maximum } => {
                write!(formatter, "summary is {bytes} bytes; maximum is {maximum}")
            }
            Self::MalformedJson => formatter.write_str("test-result summary is malformed JSON"),
            Self::JsonTooDeep { maximum } => {
                write!(formatter, "summary exceeds JSON depth {maximum}")
            }
            Self::UnsupportedSchema { source } => write!(formatter, "unsupported {source} summary"),
            Self::AmbiguousSchema => formatter.write_str("summary source is ambiguous"),
            Self::Rejected { evidence } => write!(
                formatter,
                "{} {} evidence rejected",
                evidence.source, evidence.status
            ),
        }
    }
}

/// Parse a source-marked or XCTest-shaped summary.
pub fn parse_summary(input: &[u8]) -> Result<TestEvidence, TestResultParseError> {
    let document = parse_document(input)?;
    let source = match explicit_source(&document) {
        ExplicitSource::Known(source) => source,
        ExplicitSource::Absent => {
            if has_any_key(&document, XCTEST_KEYS) {
                TestResultSource::XCTest
            } else {
                return Err(TestResultParseError::AmbiguousSchema);
            }
        }
        ExplicitSource::Invalid => return Err(TestResultParseError::AmbiguousSchema),
    };
    parse_document_for_source(&document, source)
}

/// Parse an `XCTest` summary JSON document.
pub fn parse_xctest_summary(input: &[u8]) -> Result<TestEvidence, TestResultParseError> {
    let document = parse_document(input)?;
    parse_document_for_source(&document, TestResultSource::XCTest)
}

/// Parse a Swift Testing summary JSON document.
pub fn parse_swift_testing_summary(input: &[u8]) -> Result<TestEvidence, TestResultParseError> {
    let document = parse_document(input)?;
    parse_document_for_source(&document, TestResultSource::SwiftTesting)
}

fn parse_document(input: &[u8]) -> Result<serde_json::Value, TestResultParseError> {
    if input.is_empty() {
        return Err(TestResultParseError::MissingInput);
    }
    if input.len() > MAX_SUMMARY_BYTES {
        return Err(TestResultParseError::InputTooLarge {
            bytes: input.len(),
            maximum: MAX_SUMMARY_BYTES,
        });
    }
    let document =
        serde_json::from_slice(input).map_err(|_| TestResultParseError::MalformedJson)?;
    if exceeds_depth(&document, 0) {
        return Err(TestResultParseError::JsonTooDeep {
            maximum: MAX_JSON_DEPTH,
        });
    }
    if !document.is_object() {
        return Err(TestResultParseError::MalformedJson);
    }
    Ok(document)
}

fn exceeds_depth(value: &serde_json::Value, depth: usize) -> bool {
    if depth > MAX_JSON_DEPTH {
        return true;
    }
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| exceeds_depth(value, depth.saturating_add(1))),
        serde_json::Value::Object(values) => values
            .values()
            .any(|value| exceeds_depth(value, depth.saturating_add(1))),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

const XCTEST_KEYS: &[&str] = &[
    "passedTests",
    "failedTests",
    "skippedTests",
    "expectedFailures",
    "testFailures",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExplicitSource {
    Absent,
    Invalid,
    Known(TestResultSource),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CountDeclaration {
    Absent,
    Value(u64),
    Conflict,
}

impl CountDeclaration {
    fn value(self) -> Option<u64> {
        match self {
            Self::Value(value) => Some(value),
            Self::Absent | Self::Conflict => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DeclaredCounts {
    total: CountDeclaration,
    passed: CountDeclaration,
    failed: CountDeclaration,
    skipped: CountDeclaration,
    cancelled: CountDeclaration,
    unknown: CountDeclaration,
}

impl DeclaredCounts {
    fn has_conflict(self) -> bool {
        [
            self.total,
            self.passed,
            self.failed,
            self.skipped,
            self.cancelled,
            self.unknown,
        ]
        .iter()
        .any(|count| matches!(count, CountDeclaration::Conflict))
    }

    fn as_counts(self) -> TestCounts {
        TestCounts {
            total: self.total.value().unwrap_or(0),
            passed: self.passed.value().unwrap_or(0),
            failed: self.failed.value().unwrap_or(0),
            skipped: self.skipped.value().unwrap_or(0),
            cancelled: self.cancelled.value().unwrap_or(0),
            unknown: self.unknown.value().unwrap_or(0),
        }
    }
}

fn explicit_source(document: &serde_json::Value) -> ExplicitSource {
    let mut source = None;
    for key in ["source", "format", "framework"] {
        let Some(value) = document.get(key) else {
            continue;
        };
        let Some(value) = value.as_str() else {
            return ExplicitSource::Invalid;
        };
        let normalized = value
            .chars()
            .filter(|character| {
                !character.is_ascii_whitespace() && *character != '-' && *character != '_'
            })
            .flat_map(char::to_lowercase)
            .collect::<String>();
        let marker = match normalized.as_str() {
            "xctest" | "xcresult" | "xctestsummary" => TestResultSource::XCTest,
            "swift" | "swifttesting" => TestResultSource::SwiftTesting,
            _ => return ExplicitSource::Invalid,
        };
        if source.is_some_and(|previous| previous != marker) {
            return ExplicitSource::Invalid;
        };
        source = Some(marker);
    }
    source.map_or(ExplicitSource::Absent, ExplicitSource::Known)
}

fn has_any_key(document: &serde_json::Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| document.get(*key).is_some())
}

fn parse_document_for_source(
    document: &serde_json::Value,
    source: TestResultSource,
) -> Result<TestEvidence, TestResultParseError> {
    let explicit = explicit_source(document);
    let shape_matches = match source {
        TestResultSource::XCTest => {
            matches!(explicit, ExplicitSource::Known(explicit_source) if explicit_source == source)
                || matches!(explicit, ExplicitSource::Absent) && has_any_key(document, XCTEST_KEYS)
        }
        TestResultSource::SwiftTesting => {
            !has_any_key(document, XCTEST_KEYS)
                && (matches!(explicit, ExplicitSource::Known(explicit_source) if explicit_source == source)
                    || matches!(explicit, ExplicitSource::Absent)
                        && (test_records(document).is_some()
                            || document
                                .get("counts")
                                .is_some_and(serde_json::Value::is_object)))
        }
    };
    if !shape_matches {
        return Err(TestResultParseError::UnsupportedSchema { source });
    }
    let (counts, diagnostics) = collect_counts(document, source)?;
    let evidence = TestEvidence {
        source,
        status: status_for(counts),
        counts,
        diagnostics,
    };
    if evidence.status == TestResultStatus::Passed {
        Ok(evidence)
    } else {
        Err(TestResultParseError::Rejected { evidence })
    }
}

fn collect_counts(
    document: &serde_json::Value,
    source: TestResultSource,
) -> Result<(TestCounts, Vec<TestDiagnostic>), TestResultParseError> {
    let count_object = document.get("counts");
    let failed_fields: &[&str] = match source {
        TestResultSource::XCTest => &["failedTests", "testFailures", "failed"],
        TestResultSource::SwiftTesting => &["failedTests", "failed"],
    };

    let declared = DeclaredCounts {
        total: declared_count(
            document,
            count_object,
            &["totalTests", "total", "testCount"],
        )?,
        passed: declared_count(document, count_object, &["passedTests", "passed"])?,
        failed: add_declared_counts(
            declared_count(document, count_object, failed_fields)?,
            declared_count(document, count_object, &["expectedFailures"])?,
        )?,
        skipped: declared_count(document, count_object, &["skippedTests", "skipped"])?,
        cancelled: declared_count(
            document,
            count_object,
            &["cancelledTests", "canceledTests", "cancelled", "canceled"],
        )?,
        unknown: declared_count(document, count_object, &["unknownTests", "unknown"])?,
    };
    if declared.has_conflict() {
        return Err(TestResultParseError::Rejected {
            evidence: invalid_evidence(
                source,
                declared.as_counts(),
                DiagnosticCode::InconsistentCounts,
                "declared counts conflict",
            ),
        });
    }

    let records_present = test_records(document).is_some();
    let mut counts = declared.as_counts();
    let mut diagnostics = Vec::new();
    if records_present {
        let mut record_counts = TestCounts::default();
        diagnostics = collect_record_counts(document, source, &mut record_counts)?;
        if !declared_matches_records(declared, record_counts) {
            return Err(TestResultParseError::Rejected {
                evidence: invalid_evidence(
                    source,
                    record_counts,
                    DiagnosticCode::InconsistentCounts,
                    "declared counts disagree with test records",
                ),
            });
        }
        counts = record_counts;
    } else {
        let declared_total = declared.total.value();
        counts.total = declared_total.unwrap_or_else(|| counts.observed());
        if counts.observed() > counts.total {
            return Err(TestResultParseError::Rejected {
                evidence: invalid_evidence(
                    source,
                    counts,
                    DiagnosticCode::InconsistentCounts,
                    "observed counts exceed declared total",
                ),
            });
        }
        if declared_total.is_some() {
            counts.unknown = counts
                .unknown
                .saturating_add(counts.total - counts.observed());
        }
    }

    if counts.total == 0 {
        diagnostics.push(diagnostic(
            DiagnosticCode::ZeroTests,
            "summary contains zero tests",
        ));
    }
    if counts.skipped > 0 && counts.passed == 0 && counts.failed == 0 && counts.cancelled == 0 {
        diagnostics.push(diagnostic(
            DiagnosticCode::AllSkipped,
            "summary contains only skipped tests",
        ));
    }
    if counts.failed > 0 {
        diagnostics.push(diagnostic(
            DiagnosticCode::FailedTests,
            "summary contains failed tests",
        ));
    }
    if counts.cancelled > 0 {
        diagnostics.push(diagnostic(
            DiagnosticCode::CancelledTests,
            "summary contains cancelled tests",
        ));
    }
    if counts.unknown > 0 {
        diagnostics.push(diagnostic(
            DiagnosticCode::UnknownTests,
            "summary contains unknown outcomes",
        ));
    }
    diagnostics.sort();
    diagnostics.dedup();
    Ok((counts, diagnostics))
}

fn collect_record_counts(
    document: &serde_json::Value,
    source: TestResultSource,
    counts: &mut TestCounts,
) -> Result<Vec<TestDiagnostic>, TestResultParseError> {
    let Some(records) = test_records(document) else {
        return Ok(Vec::new());
    };
    if records.len() > MAX_TEST_RECORDS {
        return Err(TestResultParseError::Rejected {
            evidence: invalid_evidence(
                source,
                *counts,
                DiagnosticCode::InconsistentCounts,
                "summary contains too many test records",
            ),
        });
    }
    *counts = TestCounts::default();
    let mut diagnostics = Vec::new();
    for record in records {
        let status = ["status", "testStatus", "outcome", "result"]
            .iter()
            .find_map(|key| record.get(*key).and_then(serde_json::Value::as_str))
            .map(normalize_status);
        match status.as_deref() {
            Some("passed" | "success" | "succeeded") => counts.passed += 1,
            Some("failed" | "failure" | "error" | "expectedfailure") => {
                counts.failed += 1;
                if let Some(message) = record_message(record) {
                    diagnostics.push(TestDiagnostic {
                        severity: DiagnosticSeverity::Error,
                        code: DiagnosticCode::FailedTests,
                        message,
                    });
                }
            }
            Some("skipped" | "disabled" | "pending") => counts.skipped += 1,
            Some("cancelled" | "canceled") => counts.cancelled += 1,
            _ => counts.unknown += 1,
        }
    }
    counts.total = records.len() as u64;
    Ok(diagnostics)
}

fn test_records(document: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    ["tests", "testResults", "results"]
        .iter()
        .find_map(|key| document.get(*key).and_then(serde_json::Value::as_array))
}

fn declared_count(
    document: &serde_json::Value,
    nested: Option<&serde_json::Value>,
    fields: &[&str],
) -> Result<CountDeclaration, TestResultParseError> {
    let mut declared = None;
    let mut conflict = false;
    for object in [Some(document), nested] {
        let Some(object) = object else {
            continue;
        };
        for field in fields {
            let Some(value) = object.get(*field) else {
                continue;
            };
            let value = value.as_u64().ok_or(TestResultParseError::MalformedJson)?;
            if declared.is_some_and(|previous| previous != value) {
                conflict = true;
            } else {
                declared = Some(value);
            }
        }
    }
    Ok(if conflict {
        CountDeclaration::Conflict
    } else {
        declared.map_or(CountDeclaration::Absent, CountDeclaration::Value)
    })
}

fn add_declared_counts(
    first: CountDeclaration,
    second: CountDeclaration,
) -> Result<CountDeclaration, TestResultParseError> {
    match (first, second) {
        (CountDeclaration::Conflict, _) | (_, CountDeclaration::Conflict) => {
            Ok(CountDeclaration::Conflict)
        }
        (CountDeclaration::Absent, CountDeclaration::Absent) => Ok(CountDeclaration::Absent),
        (CountDeclaration::Value(value), CountDeclaration::Absent)
        | (CountDeclaration::Absent, CountDeclaration::Value(value)) => {
            Ok(CountDeclaration::Value(value))
        }
        (CountDeclaration::Value(first), CountDeclaration::Value(second)) => first
            .checked_add(second)
            .map(CountDeclaration::Value)
            .ok_or(TestResultParseError::MalformedJson),
    }
}

fn declared_matches_records(declared: DeclaredCounts, records: TestCounts) -> bool {
    fn matches(declared: CountDeclaration, derived: u64) -> bool {
        match declared {
            CountDeclaration::Absent => true,
            CountDeclaration::Value(value) => value == derived,
            CountDeclaration::Conflict => false,
        }
    }

    matches(declared.total, records.total)
        && matches(declared.passed, records.passed)
        && matches(declared.failed, records.failed)
        && matches(declared.skipped, records.skipped)
        && matches(declared.cancelled, records.cancelled)
        && matches(declared.unknown, records.unknown)
}

fn normalize_status(status: &str) -> String {
    status
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn record_message(record: &serde_json::Value) -> Option<String> {
    ["message", "failureMessage", "failureReason", "details"]
        .iter()
        .find_map(|key| record.get(*key).and_then(serde_json::Value::as_str))
        .map(|message| {
            let ellipsis = '…';
            let limit = MAX_DIAGNOSTIC_BYTES.saturating_sub(ellipsis.len_utf8());
            let mut output = String::new();
            let mut truncated = false;
            for character in message.chars() {
                if output.len().saturating_add(character.len_utf8()) > limit {
                    truncated = true;
                    break;
                }
                output.push(character);
            }
            if truncated {
                output.push(ellipsis);
            }
            output
        })
}

fn status_for(counts: TestCounts) -> TestResultStatus {
    if counts.total == 0 {
        TestResultStatus::Empty
    } else if counts.failed > 0 {
        TestResultStatus::Failed
    } else if counts.cancelled > 0 {
        TestResultStatus::Cancelled
    } else if counts.skipped > 0 || counts.unknown > 0 {
        TestResultStatus::Incomplete
    } else {
        TestResultStatus::Passed
    }
}

fn invalid_evidence(
    source: TestResultSource,
    counts: TestCounts,
    code: DiagnosticCode,
    message: &str,
) -> TestEvidence {
    TestEvidence {
        source,
        status: TestResultStatus::Incomplete,
        counts,
        diagnostics: vec![diagnostic(code, message)],
    }
}

fn diagnostic(code: DiagnosticCode, message: &str) -> TestDiagnostic {
    TestDiagnostic {
        severity: DiagnosticSeverity::Error,
        code,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        clippy::panic,
        reason = "test setup failures should panic"
    )]

    use super::*;

    fn json(value: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(value).expect("test JSON serializes")
    }

    #[test]
    fn parses_xctest_and_swift_testing_passes() {
        let xctest = parse_xctest_summary(&json(&serde_json::json!({
            "result": "Passed", "passedTests": 3, "failedTests": 0,
        })))
        .expect("XCTest pass");
        assert_eq!(xctest.source, TestResultSource::XCTest);
        assert_eq!(xctest.counts.total, 3);
        let swift = parse_swift_testing_summary(&json(&serde_json::json!({
            "source": "swift-testing",
            "tests": [{"status": "passed"}, {"status": "success"}],
        })))
        .expect("Swift Testing pass");
        assert_eq!(swift.source, TestResultSource::SwiftTesting);
        assert_eq!(
            swift.counts,
            TestCounts {
                total: 2,
                passed: 2,
                ..TestCounts::default()
            }
        );
    }

    #[test]
    fn rejects_missing_malformed_zero_and_all_skipped() {
        assert_eq!(parse_summary(&[]), Err(TestResultParseError::MissingInput));
        assert_eq!(
            parse_summary(b"{"),
            Err(TestResultParseError::MalformedJson)
        );
        assert!(
            matches!(parse_xctest_summary(&json(&serde_json::json!({"passedTests": 0}))), Err(TestResultParseError::Rejected { evidence }) if evidence.status == TestResultStatus::Empty)
        );
        assert!(
            matches!(parse_swift_testing_summary(&json(&serde_json::json!({"source": "swift-testing", "tests": [{"status": "skipped"}]}))), Err(TestResultParseError::Rejected { evidence }) if evidence.diagnostics.iter().any(|item| item.code == DiagnosticCode::AllSkipped))
        );
    }

    #[test]
    fn rejects_failure_cancellation_unknown_and_inconsistent_counts() {
        assert!(
            matches!(parse_xctest_summary(&json(&serde_json::json!({"passedTests": 1, "failedTests": 1}))), Err(TestResultParseError::Rejected { evidence }) if evidence.status == TestResultStatus::Failed)
        );
        assert!(
            matches!(parse_swift_testing_summary(&json(&serde_json::json!({"source": "swift-testing", "tests": [{"status": "cancelled"}]}))), Err(TestResultParseError::Rejected { evidence }) if evidence.status == TestResultStatus::Cancelled)
        );
        assert!(
            matches!(parse_swift_testing_summary(&json(&serde_json::json!({"source": "swift-testing", "tests": [{"status": "future"}]}))), Err(TestResultParseError::Rejected { evidence }) if evidence.status == TestResultStatus::Incomplete)
        );
        assert!(
            matches!(parse_xctest_summary(&json(&serde_json::json!({"passedTests": 2, "totalTests": 1}))), Err(TestResultParseError::Rejected { evidence }) if evidence.diagnostics.iter().any(|item| item.code == DiagnosticCode::InconsistentCounts))
        );
    }

    #[test]
    fn rejects_declared_counts_that_disagree_with_records() {
        let declarations = [
            serde_json::json!({"passed": 1, "failed": 1}),
            serde_json::json!({"passed": 1, "skipped": 1}),
            serde_json::json!({"passed": 1, "cancelled": 1}),
            serde_json::json!({"passed": 1, "unknown": 1}),
        ];
        for counts in declarations {
            let error = parse_swift_testing_summary(&json(&serde_json::json!({
                "source": "swift-testing",
                "counts": counts,
                "tests": [{"status": "passed"}],
            })))
            .expect_err("contradictory declared counts reject");
            let TestResultParseError::Rejected { evidence } = error else {
                panic!("wrong error")
            };
            assert!(evidence
                .diagnostics
                .iter()
                .any(|item| item.code == DiagnosticCode::InconsistentCounts));
        }
    }

    #[test]
    fn counts_xctest_test_failures_as_failures() {
        let error = parse_xctest_summary(&json(&serde_json::json!({
            "passedTests": 1,
            "testFailures": 1,
        })))
        .expect_err("XCTest failures reject");
        let TestResultParseError::Rejected { evidence } = error else {
            panic!("wrong error")
        };
        assert_eq!(evidence.status, TestResultStatus::Failed);
        assert_eq!(evidence.counts.failed, 1);
        assert!(evidence
            .diagnostics
            .iter()
            .any(|item| item.code == DiagnosticCode::FailedTests));
    }

    #[test]
    fn rejects_conflicting_markers_and_swift_marked_xctest_keys() {
        let conflicting = json(&serde_json::json!({
            "source": "swift-testing",
            "format": "swift",
            "framework": "xctest",
            "passedTests": 1,
        }));
        assert_eq!(
            parse_summary(&conflicting),
            Err(TestResultParseError::AmbiguousSchema)
        );
        assert!(matches!(
            parse_xctest_summary(&conflicting),
            Err(TestResultParseError::UnsupportedSchema {
                source: TestResultSource::XCTest
            })
        ));

        let swift_marked_xctest_keys = json(&serde_json::json!({
            "source": "swift-testing",
            "passedTests": 1,
        }));
        assert!(matches!(
            parse_summary(&swift_marked_xctest_keys),
            Err(TestResultParseError::UnsupportedSchema {
                source: TestResultSource::SwiftTesting
            })
        ));
        assert!(matches!(
            parse_xctest_summary(&swift_marked_xctest_keys),
            Err(TestResultParseError::UnsupportedSchema {
                source: TestResultSource::XCTest
            })
        ));
    }

    #[test]
    fn rejects_ambiguous_and_oversized_or_deep_input() {
        assert_eq!(
            parse_summary(br#"{"counts":{"passed":1}}"#),
            Err(TestResultParseError::AmbiguousSchema)
        );
        assert!(matches!(
            parse_summary(&vec![b' '; MAX_SUMMARY_BYTES + 1]),
            Err(TestResultParseError::InputTooLarge { .. })
        ));
        let mut value =
            serde_json::json!({"source": "swift-testing", "tests": [{"status": "passed"}]});
        for _ in 0..=MAX_JSON_DEPTH {
            value = serde_json::json!([value]);
        }
        assert!(matches!(
            parse_summary(&json(&value)),
            Err(TestResultParseError::JsonTooDeep { .. })
        ));
    }

    #[test]
    fn caps_failure_diagnostic_and_record_count() {
        let message = "x".repeat(MAX_DIAGNOSTIC_BYTES + 10);
        let error = parse_swift_testing_summary(&json(&serde_json::json!({
            "source": "swift-testing", "tests": [{"status": "failed", "message": message}],
        })))
        .expect_err("failed evidence rejects");
        let TestResultParseError::Rejected { evidence } = error else {
            panic!("wrong error")
        };
        assert!(evidence
            .diagnostics
            .iter()
            .any(|item| item.message.len() <= MAX_DIAGNOSTIC_BYTES));
        let records = (0..=MAX_TEST_RECORDS)
            .map(|_| serde_json::json!({"status": "passed"}))
            .collect::<Vec<_>>();
        assert!(matches!(
            parse_swift_testing_summary(&json(
                &serde_json::json!({"source": "swift-testing", "tests": records})
            )),
            Err(TestResultParseError::Rejected { .. })
        ));
    }
}

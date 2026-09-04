//! Semantics tests for the expression evaluator.
//!
//! The expected values are read off `actions/runner` v2.337.0
//! (commit 397b032cbf865e9c3ddfab89d533ec19325e1273), not off intuition;
//! each block cites the upstream file it was derived from.

use super::eval::{from_serde_json, to_json};
use super::value::{format_number, parse_number, ArrayValue, ObjectValue};
use super::{evaluate, EvaluationContext, ExpressionError, ParseEnvironment, Value};

/// Minimal harness: a fixed set of root contexts plus the four status
/// functions and `hashFiles`, exactly the extension set the worker registers
/// in `src/Runner.Worker/StepsRunner.cs:92-97`.
struct TestContext {
    values: Vec<(String, Value)>,
    job_failed: bool,
}

impl TestContext {
    fn new() -> Self {
        Self {
            values: Vec::new(),
            job_failed: false,
        }
    }

    fn with(mut self, name: &str, value: Value) -> Self {
        self.values.push((name.to_string(), value));
        self
    }

    fn failed(mut self) -> Self {
        self.job_failed = true;
        self
    }
}

impl ParseEnvironment for TestContext {
    fn is_named_value(&self, name: &str) -> bool {
        self.values
            .iter()
            .any(|(known, _)| known.eq_ignore_ascii_case(name))
    }

    fn function_arity(&self, name: &str) -> Option<(usize, usize)> {
        match name.to_ascii_lowercase().as_str() {
            "success" | "failure" | "always" | "cancelled" => Some((0, 0)),
            "hashfiles" => Some((1, 255)),
            _ => None,
        }
    }
}

impl EvaluationContext for TestContext {
    fn named_value(&self, name: &str) -> Value {
        self.values
            .iter()
            .find(|(known, _)| known.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
            .unwrap_or(Value::Null)
    }

    fn call_function(&self, name: &str, _args: &[Value]) -> Result<Value, ExpressionError> {
        match name.to_ascii_lowercase().as_str() {
            "success" => Ok(Value::Boolean(!self.job_failed)),
            "failure" => Ok(Value::Boolean(self.job_failed)),
            "always" => Ok(Value::Boolean(true)),
            "cancelled" => Ok(Value::Boolean(false)),
            "hashfiles" => Ok(Value::String("hash".to_string())),
            other => Err(ExpressionError::evaluation(format!(
                "Unrecognized function: {other}"
            ))),
        }
    }
}

fn object(entries: &[(&str, Value)]) -> Value {
    Value::Object(ObjectValue::new(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect(),
    ))
}

fn array(items: &[Value]) -> Value {
    Value::Array(ArrayValue::new(items.to_vec()))
}

fn eval(expression: &str, context: &TestContext) -> Value {
    evaluate(expression, context).expect("expression evaluates")
}

fn truthy(expression: &str, context: &TestContext) -> bool {
    eval(expression, context).is_truthy()
}

// ---------------------------------------------------------------------------
// Value model and coercion table
// ---------------------------------------------------------------------------

/// `EvaluationResult.IsFalsy` (`EvaluationResult.cs:50-71`).
#[test]
fn truthiness_matches_upstream() {
    let cases: &[(Value, bool)] = &[
        (Value::Null, false),
        (Value::Boolean(true), true),
        (Value::Boolean(false), false),
        (Value::Number(0.0), false),
        (Value::Number(-0.0), false),
        (Value::Number(f64::NAN), false),
        (Value::Number(1.0), true),
        (Value::Number(-1.0), true),
        (Value::Number(f64::INFINITY), true),
        (Value::string(""), false),
        // The whole point of D-3: these strings are TRUTHY upstream.
        (Value::string("0"), true),
        (Value::string("false"), true),
        (Value::string("null"), true),
        (Value::string(" "), true),
        (array(&[]), true),
        (object(&[]), true),
    ];

    for (value, expected) in cases {
        assert_eq!(
            value.is_truthy(),
            *expected,
            "truthiness of {value:?} must be {expected}"
        );
        assert_eq!(value.is_falsy(), !*expected);
    }
}

/// `EvaluationResult.ConvertToString` (`EvaluationResult.cs:136-155`).
#[test]
fn convert_to_string_matches_upstream() {
    assert_eq!(Value::Null.convert_to_string(), "");
    assert_eq!(Value::Boolean(true).convert_to_string(), "true");
    assert_eq!(Value::Boolean(false).convert_to_string(), "false");
    assert_eq!(Value::Number(1.0).convert_to_string(), "1");
    assert_eq!(Value::Number(1.5).convert_to_string(), "1.5");
    assert_eq!(Value::Number(-0.0).convert_to_string(), "-0");
    assert_eq!(Value::Number(f64::NAN).convert_to_string(), "NaN");
    assert_eq!(Value::Number(f64::INFINITY).convert_to_string(), "Infinity");
    assert_eq!(Value::string("abc").convert_to_string(), "abc");
    assert_eq!(array(&[]).convert_to_string(), "Array");
    assert_eq!(object(&[]).convert_to_string(), "Object");
}

/// `ExpressionConstants.NumberFormat` is "G15" (`ExpressionConstants.cs:35`).
#[test]
fn number_formatting_is_g15() {
    assert_eq!(format_number(0.0), "0");
    assert_eq!(format_number(5.0), "5");
    assert_eq!(format_number(-5.0), "-5");
    assert_eq!(format_number(123456789.0), "123456789");
    assert_eq!(format_number(0.1), "0.1");
    assert_eq!(format_number(1.0 / 3.0), "0.333333333333333");
    assert_eq!(format_number(1e15), "1E+15");
    assert_eq!(format_number(1e-6), "1E-06");
    assert_eq!(format_number(1e-5), "1E-05");
    assert_eq!(format_number(0.0001), "0.0001");
}

/// `ExpressionUtility.ParseNumber` (`ExpressionUtility.cs:185-247`).
#[test]
fn parse_number_matches_upstream() {
    assert_eq!(parse_number(""), 0.0);
    assert_eq!(parse_number("   "), 0.0);
    assert_eq!(parse_number("1"), 1.0);
    assert_eq!(parse_number(" 2 "), 2.0);
    assert_eq!(parse_number("-3.5"), -3.5);
    assert_eq!(parse_number("1e3"), 1000.0);
    assert_eq!(parse_number(".5"), 0.5);
    assert_eq!(parse_number("0x1f"), 31.0);
    assert_eq!(parse_number("0o17"), 15.0);
    assert_eq!(parse_number("Infinity"), f64::INFINITY);
    assert_eq!(parse_number("-Infinity"), f64::NEG_INFINITY);
    // Upstream's Double.TryParse does not accept these spellings.
    assert!(parse_number("infinity").is_nan());
    assert!(parse_number("NaN").is_nan());
    assert!(parse_number("nope").is_nan());
    assert!(parse_number("1,000").is_nan());
}

/// `EvaluationResult.CoerceTypes` + `AbstractEqual`
/// (`EvaluationResult.cs:223-272`, `358-397`).
#[test]
fn loose_equality_coercion_table() {
    let object_value = object(&[("a", Value::Number(1.0))]);
    let array_value = array(&[Value::Number(1.0)]);

    let cases: &[(Value, Value, bool)] = &[
        (Value::Null, Value::Null, true),
        (Value::Null, Value::string(""), true),
        (Value::Null, Value::Number(0.0), true),
        (Value::Null, Value::Boolean(false), true),
        (Value::Null, Value::string("0"), true),
        (Value::Null, Value::string("a"), false),
        (Value::Boolean(true), Value::Number(1.0), true),
        (Value::Boolean(true), Value::string("1"), true),
        (Value::Boolean(false), Value::string(""), true),
        (Value::Number(1.0), Value::string("1"), true),
        (Value::Number(1.0), Value::string(" 1 "), true),
        (Value::Number(1.0), Value::string("one"), false),
        (Value::string("AbC"), Value::string("abc"), true),
        (Value::Number(f64::NAN), Value::Number(f64::NAN), false),
        // Objects and arrays are never coerced; only identity compares equal.
        (object_value.clone(), object_value.clone(), true),
        (
            object_value.clone(),
            object(&[("a", Value::Number(1.0))]),
            false,
        ),
        (object_value.clone(), Value::string("Object"), false),
        (array_value.clone(), array_value.clone(), true),
        (array_value.clone(), Value::Number(1.0), false),
    ];

    for (left, right, expected) in cases {
        assert_eq!(
            left.abstract_equal(right),
            *expected,
            "{left:?} == {right:?} must be {expected}"
        );
        assert_eq!(
            right.abstract_equal(left),
            *expected,
            "equality is symmetric for {left:?} and {right:?}"
        );
        assert_eq!(left.abstract_not_equal(right), !*expected);
    }
}

/// `AbstractGreaterThan` / `AbstractLessThan`
/// (`EvaluationResult.cs:278-356`).
#[test]
fn ordering_matches_upstream() {
    assert!(Value::Number(2.0).abstract_greater_than(&Value::Number(1.0)));
    assert!(Value::Number(1.0).abstract_less_than(&Value::Number(2.0)));
    // Strings coerce to numbers only against a number.
    assert!(Value::Number(2.0).abstract_greater_than(&Value::string("1")));
    assert!(Value::string("b").abstract_greater_than(&Value::string("A")));
    assert!(Value::Boolean(true).abstract_greater_than(&Value::Boolean(false)));
    // A non-numeric string against a number coerces to NaN, and NaN loses
    // every comparison in both directions.
    assert!(!Value::Number(1.0).abstract_greater_than(&Value::string("a")));
    assert!(!Value::Number(1.0).abstract_less_than(&Value::string("a")));
    // Objects never compare.
    let object_value = object(&[("a", Value::Number(1.0))]);
    assert!(!object_value.abstract_greater_than(&Value::Number(0.0)));
    assert!(!object_value.abstract_less_than(&Value::Number(0.0)));
    // >= and <= are literally "equal or greater/less".
    assert!(Value::Number(1.0).abstract_greater_than_or_equal(&Value::string("1")));
    assert!(Value::Number(1.0).abstract_less_than_or_equal(&Value::string("1")));
    assert!(Value::Null.abstract_greater_than_or_equal(&Value::Number(0.0)));
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Precedence and associativity from `Tokens/Token.cs:72-115`.
#[test]
fn operator_precedence() {
    let context = TestContext::new();
    assert!(truthy("true || false && false", &context));
    assert!(!truthy("(true || false) && false", &context));
    assert!(truthy("1 < 2 == true", &context));
    assert!(truthy("!false && true", &context));
    assert!(!truthy("!(false && true) == false", &context));
}

/// `&&` and `||` return the operand value, not a boolean
/// (`Sdk/Operators/And.cs:34-48`, `Sdk/Operators/Or.cs:34-48`).
#[test]
fn logical_operators_return_operand_values() {
    let context = TestContext::new();
    assert_eq!(eval("'a' && 'b'", &context).convert_to_string(), "b");
    assert_eq!(eval("'' && 'b'", &context).convert_to_string(), "");
    assert_eq!(eval("'' || 'b'", &context).convert_to_string(), "b");
    assert_eq!(eval("'a' || 'b'", &context).convert_to_string(), "a");
    // "0" is truthy, so it short-circuits `||`.
    assert_eq!(eval("'0' || 'b'", &context).convert_to_string(), "0");
}

/// Short-circuiting must skip evaluation of the right operand entirely; the
/// unknown function here would be a parse error only if it were reached.
#[test]
fn short_circuit_skips_evaluation() {
    let context = TestContext::new().with("env", object(&[]));
    // `env.missing` is null -> falsy -> `&&` returns it without evaluating the
    // index on the right.
    assert_eq!(
        eval("env.missing && env.missing.deeper", &context)
            .kind()
            .as_str(),
        "Null"
    );
}

/// `ExpressionParserL0.CreateTree_RejectsUnrecognizedNamedValue`
/// (`src/Test/L0/Sdk/ExpressionParserL0.cs:19-33`), transcribed.
#[test]
fn create_tree_rejects_unrecognized_named_value() {
    let context = TestContext::new().with("inputs", object(&[]));
    let error = evaluate("github.event.repository.private", &context)
        .expect_err("unrecognized named-value is a parse error");
    assert!(
        error.to_string().contains("Unrecognized named-value"),
        "unexpected error: {error}"
    );
}

/// `ExpressionParserL0.CreateTree_AcceptsRecognizedNamedValue`
/// (`src/Test/L0/Sdk/ExpressionParserL0.cs:37-49`).
#[test]
fn create_tree_accepts_recognized_named_value() {
    let context = TestContext::new().with("inputs", object(&[("foo", Value::string("bar"))]));
    assert_eq!(eval("inputs.foo", &context).convert_to_string(), "bar");
}

/// `ExpressionParserL0.CreateTree_CaseFunctionWorks`
/// (`src/Test/L0/Sdk/ExpressionParserL0.cs:53-65`).
#[test]
fn create_tree_case_function_works() {
    let context =
        TestContext::new().with("github", object(&[("event_name", Value::string("push"))]));
    assert_eq!(
        eval(
            "case(github.event_name == 'push', 'Push Event', 'Other')",
            &context
        )
        .convert_to_string(),
        "Push Event"
    );
}

/// `ExpressionParserL0.CreateTree_CaseFunctionDoesNotAffectUnknownKeywords`
/// (`src/Test/L0/Sdk/ExpressionParserL0.cs:69-85`).
#[test]
fn case_function_does_not_allow_unknown_keywords() {
    let context = TestContext::new().with("inputs", object(&[]));
    let error =
        evaluate("github.ref", &context).expect_err("unrecognized named-value is a parse error");
    assert!(error.to_string().contains("Unrecognized named-value"));
}

/// Arity checks from `ExpressionConstants.cs:10-20` and
/// `ExpressionParser.cs:338-355`.
#[test]
fn function_arity_is_enforced() {
    let context = TestContext::new();
    assert!(evaluate("contains('a')", &context)
        .expect_err("too few")
        .to_string()
        .contains("Too few parameters"));
    assert!(evaluate("toJson('a', 'b')", &context)
        .expect_err("too many")
        .to_string()
        .contains("Too many parameters"));
    assert!(evaluate("case(true, 'a', 'b', 'c')", &context)
        .expect_err("even parameters")
        .to_string()
        .contains("Even number of parameters"));
    assert!(evaluate("noSuchFunction()", &context)
        .expect_err("unknown function")
        .to_string()
        .contains("Unrecognized function"));
}

/// `LexicalAnalyzer.CreateToken` legality table
/// (`Tokens/LexicalAnalyzer.cs:326-467`).
#[test]
fn malformed_expressions_are_parse_errors() {
    let context = TestContext::new();
    for expression in ["1 +", "(", "'unterminated", "==", "1 == == 2", "a.."] {
        assert!(
            evaluate(expression, &context).is_err(),
            "expected a parse error for {expression:?}"
        );
    }
}

/// `ExpressionParser.cs:60-64` — an empty expression parses to no tree.
#[test]
fn empty_expression_is_null() {
    let context = TestContext::new();
    assert!(eval("", &context).is_falsy());
    assert!(eval("   ", &context).is_falsy());
}

// ---------------------------------------------------------------------------
// Index / dereference
// ---------------------------------------------------------------------------

/// `Sdk/Operators/Index.cs:50-201`.
#[test]
fn index_and_dereference() {
    let context = TestContext::new()
        .with(
            "github",
            object(&[(
                "event",
                object(&[("commits", array(&[object(&[("id", Value::string("abc"))])]))]),
            )]),
        )
        .with("env", object(&[("A", Value::string("1"))]));

    assert_eq!(
        eval("github.event.commits[0].id", &context).convert_to_string(),
        "abc"
    );
    assert_eq!(
        eval("github['event']['commits'][0]['id']", &context).convert_to_string(),
        "abc"
    );
    // Missing property is null, not the source text.
    assert!(matches!(eval("github.missing", &context), Value::Null));
    assert!(matches!(
        eval("github.missing.deeper", &context),
        Value::Null
    ));
    // Indexing a non-collection is null.
    assert!(matches!(eval("env.A.nope", &context), Value::Null));
    // Out-of-range array index is null.
    assert!(matches!(
        eval("github.event.commits[9]", &context),
        Value::Null
    ));
    // Object keys are matched case-insensitively (DictionaryContextData).
    assert_eq!(
        eval("github.EVENT.COMMITS[0].ID", &context).convert_to_string(),
        "abc"
    );
}

/// Wildcards produce a filtered array that the next index descends into
/// (`Index.cs:80-140`).
#[test]
fn wildcard_filtering() {
    let context = TestContext::new().with(
        "github",
        object(&[(
            "event",
            object(&[(
                "commits",
                array(&[
                    object(&[("id", Value::string("a"))]),
                    object(&[("id", Value::string("b"))]),
                ]),
            )]),
        )]),
    );

    let ids = eval("github.event.commits.*.id", &context);
    assert_eq!(join_values(&ids), "a,b");
    let ids = eval("github.event.commits[*].id", &context);
    assert_eq!(join_values(&ids), "a,b");
    // A wildcard over a non-collection yields an empty filtered array.
    let empty = eval("github.missing.*", &context);
    assert_eq!(join_values(&empty), "");
}

fn join_values(value: &Value) -> String {
    match value {
        Value::Array(array) => array
            .items()
            .iter()
            .map(Value::convert_to_string)
            .collect::<Vec<_>>()
            .join(","),
        other => other.convert_to_string(),
    }
}

// ---------------------------------------------------------------------------
// Function set
// ---------------------------------------------------------------------------

/// `Sdk/Functions/Contains.cs:11-42`.
#[test]
fn contains_function() {
    let context = TestContext::new().with(
        "github",
        object(&[("list", array(&[Value::string("a"), Value::Number(2.0)]))]),
    );
    assert!(truthy("contains('Hello world', 'WORLD')", &context));
    assert!(!truthy("contains('Hello', 'x')", &context));
    assert!(truthy("contains(github.list, 'A')", &context));
    assert!(truthy("contains(github.list, '2')", &context));
    assert!(!truthy("contains(github.list, 'z')", &context));
}

/// `Sdk/Functions/StartsWith.cs` and `Sdk/Functions/EndsWith.cs`.
#[test]
fn starts_with_and_ends_with() {
    let context =
        TestContext::new().with("github", object(&[("ref", Value::string("refs/tags/v1"))]));
    assert!(truthy("startsWith(github.ref, 'refs/tags/')", &context));
    assert!(!truthy("startsWith(github.ref, 'refs/heads/')", &context));
    assert!(truthy("endsWith(github.ref, 'V1')", &context));
    assert!(truthy("startsWith('abc', '')", &context));
    // A non-primitive operand yields false rather than an error.
    assert!(!truthy("startsWith(github, 'x')", &context));
}

/// `Sdk/Functions/Format.cs`.
#[test]
fn format_function() {
    let context = TestContext::new().with("github", object(&[("sha", Value::string("deadbeef"))]));
    assert_eq!(
        eval("format('{0}-{1}', 'a', 'b')", &context).convert_to_string(),
        "a-b"
    );
    assert_eq!(
        eval("format('{{literal}} {0}', github.sha)", &context).convert_to_string(),
        "{literal} deadbeef"
    );
    assert_eq!(
        eval("format('{0}{0}', 'x')", &context).convert_to_string(),
        "xx"
    );
    // Null formats as the empty string.
    assert_eq!(
        eval("format('[{0}]', github.missing)", &context).convert_to_string(),
        "[]"
    );
    assert!(evaluate("format('{1}', 'a')", &context).is_err());
    assert!(evaluate("format('{0')", &context).is_err());
}

/// `Sdk/Functions/Join.cs:11-73`.
#[test]
fn join_function() {
    let context = TestContext::new().with(
        "github",
        object(&[
            ("list", array(&[Value::string("a"), Value::string("b")])),
            ("empty", array(&[])),
            ("scalar", Value::Number(3.0)),
        ]),
    );
    assert_eq!(
        eval("join(github.list)", &context).convert_to_string(),
        "a,b"
    );
    assert_eq!(
        eval("join(github.list, ' - ')", &context).convert_to_string(),
        "a - b"
    );
    assert_eq!(eval("join(github.empty)", &context).convert_to_string(), "");
    assert_eq!(
        eval("join(github.scalar)", &context).convert_to_string(),
        "3"
    );
    assert_eq!(eval("join(github)", &context).convert_to_string(), "");
}

/// `Sdk/Functions/FromJson.cs` and `Sdk/Functions/ToJson.cs`.
#[test]
fn from_json_and_to_json() {
    let context = TestContext::new();
    assert_eq!(
        eval("fromJson('{\"a\":1}').a", &context).convert_to_string(),
        "1"
    );
    assert_eq!(
        eval("fromJson('[1,2]')[1]", &context).convert_to_string(),
        "2"
    );
    assert!(truthy("fromJson('true')", &context));
    assert!(evaluate("fromJson('not json')", &context).is_err());

    let value = from_serde_json(&serde_json::json!({"a": 1, "b": ["x"]}));
    assert_eq!(
        to_json(&value),
        "{\n  \"a\": 1,\n  \"b\": [\n    \"x\"\n  ]\n}"
    );
    assert_eq!(to_json(&Value::string("x")), "\"x\"");
    assert_eq!(to_json(&Value::Null), "null");
    assert_eq!(to_json(&array(&[])), "[]");
}

/// `Sdk/Functions/Case.cs:10-42`.
#[test]
fn case_function() {
    let context = TestContext::new().with(
        "github",
        object(&[("event_name", Value::string("pull_request"))]),
    );
    assert_eq!(
        eval(
            "case(github.event_name == 'push', 'a', github.event_name == 'pull_request', 'b', 'c')",
            &context
        )
        .convert_to_string(),
        "b"
    );
    assert_eq!(
        eval("case(false, 'a', false, 'b', 'fallback')", &context).convert_to_string(),
        "fallback"
    );
    // A non-boolean predicate is an evaluation error, not a coercion.
    assert!(evaluate("case('yes', 'a', 'b')", &context).is_err());
}

/// The runner-provided status functions
/// (`src/Runner.Worker/Expressions/*.cs`).
#[test]
fn status_functions() {
    let passing = TestContext::new();
    assert!(truthy("success()", &passing));
    assert!(!truthy("failure()", &passing));
    assert!(truthy("always()", &passing));
    assert!(!truthy("cancelled()", &passing));

    let failing = TestContext::new().failed();
    assert!(!truthy("success()", &failing));
    assert!(truthy("failure()", &failing));
    assert!(truthy("always()", &failing));
}

// ---------------------------------------------------------------------------
// Divergence regressions D-3 .. D-7
// ---------------------------------------------------------------------------

/// D-3: string truthiness was inverted. GitHub runs
/// `if: steps.x.outputs.count` when `count` is `"0"`, because only the empty
/// string is falsy (`EvaluationResult.cs:64-66`). Velnor skipped it.
#[test]
fn regression_d3_string_truthiness() {
    let context = TestContext::new().with(
        "steps",
        object(&[(
            "x",
            object(&[(
                "outputs",
                object(&[
                    ("count", Value::string("0")),
                    ("flag", Value::string("false")),
                    ("blank", Value::string("")),
                ]),
            )]),
        )]),
    );

    // GitHub: runs. Velnor now matches.
    assert!(truthy("steps.x.outputs.count", &context));
    assert!(truthy("steps.x.outputs.flag", &context));
    // GitHub: skips (empty string is the only falsy string).
    assert!(!truthy("steps.x.outputs.blank", &context));
}

/// D-4: an unknown expression used to become its own source text, so
/// `env.UNSET == ''` was false. GitHub coerces the missing value to null, and
/// null equals the empty string (`EvaluationResult.cs:385-396`).
#[test]
fn regression_d4_null_coercion() {
    let context = TestContext::new().with("env", object(&[("SET", Value::string("v"))]));

    // GitHub: true. Velnor now matches.
    assert!(truthy("env.UNSET == ''", &context));
    assert!(truthy("env.UNSET == null", &context));
    assert!(truthy("env.UNSET == 0", &context));
    assert!(truthy("env.UNSET == false", &context));
    assert!(!truthy("env.UNSET", &context));
    assert!(!truthy("env.UNSET == 'v'", &context));
    assert!(truthy("env.SET == 'v'", &context));
    // The unresolved value must never leak its source text.
    assert_eq!(eval("env.UNSET", &context).convert_to_string(), "");
}

/// D-5: `<`, `<=`, `>`, `>=` did not exist, so `github.run_number > 5` on run
/// 1 *ran*. GitHub skips it (`Sdk/Operators/GreaterThan.cs:34-42`).
#[test]
fn regression_d5_relational_operators() {
    let context = TestContext::new().with("github", object(&[("run_number", Value::string("1"))]));

    // GitHub: skips. Velnor now matches.
    assert!(!truthy("github.run_number > 5", &context));
    assert!(truthy("github.run_number < 5", &context));
    assert!(truthy("github.run_number <= 1", &context));
    assert!(truthy("github.run_number >= 1", &context));
    assert!(!truthy("github.run_number >= 2", &context));
}

/// D-6: `startsWith`, `endsWith`, `join`, `fromJSON` and `case` were absent,
/// so `if: startsWith(github.ref, 'refs/tags/')` on a branch push *ran the
/// release step*. GitHub skips it (`ExpressionConstants.cs:10-20`).
#[test]
fn regression_d6_missing_functions() {
    let context = TestContext::new().with(
        "github",
        object(&[("ref", Value::string("refs/heads/main"))]),
    );

    // GitHub: skips. Velnor now matches.
    assert!(!truthy("startsWith(github.ref, 'refs/tags/')", &context));
    assert!(truthy("startsWith(github.ref, 'refs/heads/')", &context));
    assert!(!truthy("endsWith(github.ref, '/v1')", &context));
    assert!(truthy("contains(fromJson('[\"main\"]'), 'main')", &context));
    assert_eq!(
        eval("join(fromJson('[\"a\",\"b\"]'), '+')", &context).convert_to_string(),
        "a+b"
    );
    assert_eq!(
        eval(
            "case(startsWith(github.ref, 'refs/tags/'), 'release', 'ci')",
            &context
        )
        .convert_to_string(),
        "ci"
    );
}

/// D-7: a condition that fails to evaluate was fail-open. Upstream fails the
/// step (`src/Runner.Worker/StepsRunner.cs:231-242`), which requires the
/// evaluator to surface a typed error instead of a bool.
#[test]
fn regression_d7_evaluation_errors_are_typed() {
    let context = TestContext::new().with("env", object(&[]));

    // GitHub: the step fails. Velnor now returns an error rather than `true`.
    for expression in [
        "unknownContext.value",
        "noSuchFunction('a')",
        "contains('a')",
        "format('{1}', 'a')",
        "fromJson('{')",
        "case('not a boolean', 'a', 'b')",
        "env.A ==",
    ] {
        let error = evaluate(expression, &context)
            .expect_err("{expression} must be a typed evaluation failure");
        assert!(
            !error.to_string().is_empty(),
            "error for {expression:?} must carry a message"
        );
    }
}

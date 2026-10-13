//! The GitHub Actions expression value union.
//!
//! Transcribed from `actions/runner` v2.337.0
//! (commit 397b032cbf865e9c3ddfab89d533ec19325e1273):
//!
//! * `src/Sdk/DTExpressions2/Expressions2/ValueKind.cs`
//! * `src/Sdk/DTExpressions2/Expressions2/EvaluationResult.cs`
//! * `src/Sdk/DTExpressions2/Expressions2/Sdk/ExpressionUtility.cs`
//!
//! Everything here follows upstream even where upstream is surprising; each
//! surprising rule carries the upstream file and line it came from.

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

/// `src/Sdk/DTExpressions2/Expressions2/ValueKind.cs:6-14`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Array,
    Boolean,
    Null,
    Number,
    Object,
    String,
}

impl ValueKind {
    /// `ValueKind.ToString()` — used by `ConvertToString` for non-primitives
    /// (`EvaluationResult.cs:152-153`) and by `FormatValue`
    /// (`ExpressionUtility.cs:124-126`).
    pub fn as_str(self) -> &'static str {
        match self {
            ValueKind::Array => "Array",
            ValueKind::Boolean => "Boolean",
            ValueKind::Null => "Null",
            ValueKind::Number => "Number",
            ValueKind::Object => "Object",
            ValueKind::String => "String",
        }
    }
}

/// An ordered array. `filtered` marks the result of a wildcard index, which
/// upstream models as `Index.FilteredArray` (`Sdk/Operators/Index.cs:203-219`)
/// — an `IReadOnlyArray` that the index operator descends into differently
/// from a plain array (`Index.cs:62-65`).
#[derive(Debug, Clone)]
pub struct ArrayValue {
    items: Arc<Vec<Value>>,
    filtered: bool,
}

impl ArrayValue {
    pub fn new(items: Vec<Value>) -> Self {
        Self {
            items: Arc::new(items),
            filtered: false,
        }
    }

    pub fn filtered(items: Vec<Value>) -> Self {
        Self {
            items: Arc::new(items),
            filtered: true,
        }
    }

    pub fn is_filtered(&self) -> bool {
        self.filtered
    }

    pub fn items(&self) -> &[Value] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Reference equality, matching `Object.ReferenceEquals` in
    /// `EvaluationResult.cs:267`.
    fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.items, &other.items)
    }
}

/// An ordered mapping.
///
/// Upstream uses `DictionaryContextData`, whose key lookup is
/// `StringComparer.OrdinalIgnoreCase`
/// (`src/Sdk/DTPipelines/Pipelines/ContextData/DictionaryContextData.cs:77`),
/// except the `env` context on non-Windows runners, which is a
/// `CaseSensitiveDictionaryContextData`
/// (`src/Runner.Worker/StepsRunner.cs:101-106`). `case_sensitive` carries that
/// distinction.
#[derive(Debug, Clone)]
pub struct ObjectValue {
    entries: Arc<Vec<(String, Value)>>,
    case_sensitive: bool,
}

impl ObjectValue {
    pub fn new(entries: Vec<(String, Value)>) -> Self {
        Self {
            entries: Arc::new(entries),
            case_sensitive: false,
        }
    }

    pub fn case_sensitive(entries: Vec<(String, Value)>) -> Self {
        Self {
            entries: Arc::new(entries),
            case_sensitive: true,
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(name, _)| {
                if self.case_sensitive {
                    name == key
                } else {
                    name.eq_ignore_ascii_case(key)
                }
            })
            .map(|(_, value)| value)
    }

    fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.entries, &other.entries)
    }
}

/// The expression value union.
///
/// Upstream carries this as `Object` plus a `ValueKind`
/// (`EvaluationResult.cs:41-48`); the whole point of this module is that
/// Velnor carries it as a real union instead of an `Option<String>`.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Array(ArrayValue),
    Object(ObjectValue),
}

impl Value {
    pub fn string(value: impl Into<String>) -> Self {
        Value::String(value.into())
    }

    pub fn kind(&self) -> ValueKind {
        match self {
            Value::Null => ValueKind::Null,
            Value::Boolean(_) => ValueKind::Boolean,
            Value::Number(_) => ValueKind::Number,
            Value::String(_) => ValueKind::String,
            Value::Array(_) => ValueKind::Array,
            Value::Object(_) => ValueKind::Object,
        }
    }

    /// `ExpressionUtility.IsPrimitive` (`ExpressionUtility.cs:167-179`).
    pub fn is_primitive(&self) -> bool {
        matches!(
            self,
            Value::Null | Value::Boolean(_) | Value::Number(_) | Value::String(_)
        )
    }

    /// `EvaluationResult.IsFalsy` (`EvaluationResult.cs:50-71`).
    ///
    /// Note the string rule: **only the empty string is falsy**. `"0"` and
    /// `"false"` are truthy. Objects and arrays are always truthy, empty or
    /// not.
    pub fn is_falsy(&self) -> bool {
        match self {
            Value::Null => true,
            Value::Boolean(value) => !*value,
            Value::Number(value) => *value == 0.0 || value.is_nan(),
            Value::String(value) => value.is_empty(),
            Value::Array(_) | Value::Object(_) => false,
        }
    }

    /// `EvaluationResult.IsTruthy` (`EvaluationResult.cs:75`).
    pub fn is_truthy(&self) -> bool {
        !self.is_falsy()
    }

    /// `EvaluationResult.ConvertToString` (`EvaluationResult.cs:136-155`).
    pub fn convert_to_string(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Boolean(value) => if *value { "true" } else { "false" }.to_string(),
            Value::Number(value) => format_number(*value),
            Value::String(value) => value.clone(),
            other => other.kind().as_str().to_string(),
        }
    }

    /// `EvaluationResult.ConvertToNumber` (`EvaluationResult.cs:399-418`).
    pub fn convert_to_number(&self) -> f64 {
        match self {
            Value::Null => 0.0,
            Value::Boolean(value) => {
                if *value {
                    1.0
                } else {
                    0.0
                }
            }
            Value::Number(value) => *value,
            Value::String(value) => parse_number(value),
            Value::Array(_) | Value::Object(_) => f64::NAN,
        }
    }

    /// `EvaluationResult.AbstractEqual` (`EvaluationResult.cs:223-272`).
    pub fn abstract_equal(&self, right: &Value) -> bool {
        let (left, right) = coerce_types(self.clone(), right.clone());
        match (&left, &right) {
            (Value::Null, Value::Null) => true,
            (Value::Number(left), Value::Number(right)) => {
                if left.is_nan() || right.is_nan() {
                    false
                } else {
                    left == right
                }
            }
            (Value::String(left), Value::String(right)) => ordinal_ignore_case_eq(left, right),
            (Value::Boolean(left), Value::Boolean(right)) => left == right,
            (Value::Object(left), Value::Object(right)) => left.same_instance(right),
            (Value::Array(left), Value::Array(right)) => left.same_instance(right),
            _ => false,
        }
    }

    /// `EvaluationResult.AbstractNotEqual` (`EvaluationResult.cs:126-129`).
    pub fn abstract_not_equal(&self, right: &Value) -> bool {
        !self.abstract_equal(right)
    }

    /// `EvaluationResult.AbstractGreaterThan` (`EvaluationResult.cs:278-314`).
    ///
    /// Surprising-but-upstream: operands whose coerced kinds differ, and any
    /// object/array operand, compare `false` in **both** directions.
    pub fn abstract_greater_than(&self, right: &Value) -> bool {
        let (left, right) = coerce_types(self.clone(), right.clone());
        match (&left, &right) {
            (Value::Number(left), Value::Number(right)) => {
                if left.is_nan() || right.is_nan() {
                    false
                } else {
                    left > right
                }
            }
            (Value::String(left), Value::String(right)) => {
                ordinal_ignore_case_cmp(left, right) == Ordering::Greater
            }
            (Value::Boolean(left), Value::Boolean(right)) => *left && !*right,
            _ => false,
        }
    }

    /// `EvaluationResult.AbstractLessThan` (`EvaluationResult.cs:320-356`).
    pub fn abstract_less_than(&self, right: &Value) -> bool {
        let (left, right) = coerce_types(self.clone(), right.clone());
        match (&left, &right) {
            (Value::Number(left), Value::Number(right)) => {
                if left.is_nan() || right.is_nan() {
                    false
                } else {
                    left < right
                }
            }
            (Value::String(left), Value::String(right)) => {
                ordinal_ignore_case_cmp(left, right) == Ordering::Less
            }
            (Value::Boolean(left), Value::Boolean(right)) => !*left && *right,
            _ => false,
        }
    }

    /// `EvaluationResult.AbstractGreaterThanOrEqual`
    /// (`EvaluationResult.cs:99-102`) — literally equal-or-greater, so it is
    /// also `false` for mismatched kinds.
    pub fn abstract_greater_than_or_equal(&self, right: &Value) -> bool {
        self.abstract_equal(right) || self.abstract_greater_than(right)
    }

    /// `EvaluationResult.AbstractLessThanOrEqual` (`EvaluationResult.cs:117-120`).
    pub fn abstract_less_than_or_equal(&self, right: &Value) -> bool {
        self.abstract_equal(right) || self.abstract_less_than(right)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.convert_to_string())
    }
}

/// `EvaluationResult.CoerceTypes` (`EvaluationResult.cs:358-397`).
///
/// Objects and arrays are deliberately *not* coerced to primitives, which is
/// why every comparison involving one is false.
fn coerce_types(left: Value, right: Value) -> (Value, Value) {
    let left_kind = left.kind();
    let right_kind = right.kind();

    if left_kind == right_kind {
        return (left, right);
    }

    match (left_kind, right_kind) {
        (ValueKind::Number, ValueKind::String) => {
            let right = Value::Number(right.convert_to_number());
            (left, right)
        }
        (ValueKind::String, ValueKind::Number) => {
            let left = Value::Number(left.convert_to_number());
            (left, right)
        }
        _ if matches!(left_kind, ValueKind::Boolean | ValueKind::Null) => {
            let left = Value::Number(left.convert_to_number());
            coerce_types(left, right)
        }
        _ if matches!(right_kind, ValueKind::Boolean | ValueKind::Null) => {
            let right = Value::Number(right.convert_to_number());
            coerce_types(left, right)
        }
        _ => (left, right),
    }
}

/// `String.ToUpperInvariant` on a single char, which is what
/// `StringComparison.OrdinalIgnoreCase` folds with. Multi-char uppercase
/// expansions (e.g. `ß`) are not 1:1 and are left alone, matching .NET.
fn upper_invariant(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(first), None) => first,
        _ => c,
    }
}

pub fn ordinal_ignore_case_eq(left: &str, right: &str) -> bool {
    ordinal_ignore_case_cmp(left, right) == Ordering::Equal
}

/// `String.Compare(left, right, StringComparison.OrdinalIgnoreCase)`.
pub fn ordinal_ignore_case_cmp(left: &str, right: &str) -> Ordering {
    let mut left = left.chars();
    let mut right = right.chars();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(l), Some(r)) => {
                let l = upper_invariant(l);
                let r = upper_invariant(r);
                if l != r {
                    return (l as u32).cmp(&(r as u32));
                }
            }
        }
    }
}

pub fn ordinal_ignore_case_contains(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack: Vec<char> = haystack.chars().map(upper_invariant).collect();
    let needle: Vec<char> = needle.chars().map(upper_invariant).collect();
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

pub fn ordinal_ignore_case_starts_with(haystack: &str, prefix: &str) -> bool {
    let haystack: Vec<char> = haystack.chars().map(upper_invariant).collect();
    let prefix: Vec<char> = prefix.chars().map(upper_invariant).collect();
    haystack.len() >= prefix.len() && haystack[..prefix.len()] == prefix[..]
}

pub fn ordinal_ignore_case_ends_with(haystack: &str, suffix: &str) -> bool {
    let haystack: Vec<char> = haystack.chars().map(upper_invariant).collect();
    let suffix: Vec<char> = suffix.chars().map(upper_invariant).collect();
    haystack.len() >= suffix.len() && haystack[haystack.len() - suffix.len()..] == suffix[..]
}

/// `ExpressionUtility.ParseNumber` (`ExpressionUtility.cs:185-247`) — the
/// JavaScript `Number()` rules as upstream implements them.
pub fn parse_number(value: &str) -> f64 {
    let value = value.trim();

    if value.is_empty() {
        return 0.0;
    }

    // Double.TryParse with AllowLeadingSign | AllowDecimalPoint | AllowExponent.
    // Deliberately stricter than Rust's `f64::from_str`, which also accepts
    // "inf", "infinity" and "nan" in any case.
    if is_dotnet_double(value)
        && let Ok(parsed) = value.parse::<f64>()
    {
        return parsed;
    }

    let bytes = value.as_bytes();

    // 0x[0-9a-fA-F]+ parsed as Int32; out of range falls through to NaN.
    if bytes.len() > 2
        && bytes[0] == b'0'
        && bytes[1] == b'x'
        && value[2..].chars().all(|c| c.is_ascii_hexdigit())
    {
        return match i32::from_str_radix(&value[2..], 16) {
            Ok(parsed) => f64::from(parsed),
            Err(_) => f64::NAN,
        };
    }

    // 0o[0-7]+ parsed as Int32; out of range falls through to NaN.
    if bytes.len() > 2
        && bytes[0] == b'0'
        && bytes[1] == b'o'
        && value[2..].bytes().all(|c| (b'0'..=b'7').contains(&c))
    {
        return match i32::from_str_radix(&value[2..], 8) {
            Ok(parsed) => f64::from(parsed),
            Err(_) => f64::NAN,
        };
    }

    if value == "Infinity" {
        return f64::INFINITY;
    }
    if value == "-Infinity" {
        return f64::NEG_INFINITY;
    }

    f64::NAN
}

/// Whether `value` is accepted by .NET's
/// `Double.TryParse(NumberStyles.AllowLeadingSign | AllowDecimalPoint | AllowExponent)`.
fn is_dotnet_double(value: &str) -> bool {
    let mut chars = value.chars().peekable();
    if matches!(chars.peek(), Some('+' | '-')) {
        chars.next();
    }

    let mut mantissa_digits = 0usize;
    let mut seen_point = false;
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            mantissa_digits += 1;
            chars.next();
        } else if c == '.' && !seen_point {
            seen_point = true;
            chars.next();
        } else {
            break;
        }
    }

    if mantissa_digits == 0 {
        return false;
    }

    if let Some(&c) = chars.peek() {
        if c != 'e' && c != 'E' {
            return false;
        }
        chars.next();
        if matches!(chars.peek(), Some('+' | '-')) {
            chars.next();
        }
        let mut exponent_digits = 0usize;
        for c in chars.by_ref() {
            if c.is_ascii_digit() {
                exponent_digits += 1;
            } else {
                return false;
            }
        }
        return exponent_digits > 0;
    }

    true
}

/// `((Double)value).ToString("G15", CultureInfo.InvariantCulture)`
/// (`ExpressionConstants.NumberFormat`, `ExpressionConstants.cs:35`).
pub fn format_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        // .NET renders negative zero as "-0" under G15.
        return if value.is_sign_negative() {
            "-0".to_string()
        } else {
            "0".to_string()
        };
    }

    const SIGNIFICANT_DIGITS: usize = 15;

    // Round to 15 significant digits, then decide fixed vs scientific the way
    // .NET's "G" specifier does: scientific when the decimal exponent is
    // below -5 or at/above the precision.
    let scientific = format!("{:.*e}", SIGNIFICANT_DIGITS - 1, value);
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("rust exponential formatting always contains 'e'");
    let exponent: i32 = exponent.parse().expect("exponent is an integer");

    if exponent < -5 || exponent >= SIGNIFICANT_DIGITS as i32 {
        let mantissa = trim_trailing_zeros(mantissa);
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{mantissa}E{sign}{:02}", exponent.abs());
    }

    let decimals = (SIGNIFICANT_DIGITS as i32 - 1 - exponent).max(0) as usize;
    trim_trailing_zeros(&format!("{value:.decimals$}"))
}

fn trim_trailing_zeros(value: &str) -> String {
    if !value.contains('.') {
        return value.to_string();
    }
    let trimmed = value.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

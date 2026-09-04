//! Evaluator for parsed GitHub Actions expressions.
//!
//! Transcribed from `actions/runner` v2.337.0
//! (commit 397b032cbf865e9c3ddfab89d533ec19325e1273), files under
//! `src/Sdk/DTExpressions2/Expressions2/Sdk/`.

use super::parser::ParseEnvironment;
use super::value::{
    format_number, ordinal_ignore_case_contains, ordinal_ignore_case_ends_with,
    ordinal_ignore_case_starts_with, ArrayValue, ObjectValue, Value,
};
use super::{parse, BinaryOperator, ExpressionError, Node};

/// What an expression is evaluated against: the root contexts and the
/// extension functions the runner registers
/// (`src/Runner.Worker/StepsRunner.cs:92-97`).
pub trait EvaluationContext: ParseEnvironment {
    /// The value of a root context. Only called for names that
    /// [`ParseEnvironment::is_named_value`] accepted.
    fn named_value(&self, name: &str) -> Value;

    /// Invoke a runner-provided function (`success`, `failure`, `always`,
    /// `cancelled`, `hashFiles`) with already-evaluated arguments.
    fn call_function(&self, name: &str, args: &[Value]) -> Result<Value, ExpressionError>;
}

/// Parse and evaluate `expression` in one step.
pub fn evaluate(
    expression: &str,
    context: &dyn EvaluationContext,
) -> Result<Value, ExpressionError> {
    let node = parse(expression, context)?;
    match node {
        // Upstream returns a null tree for an empty expression
        // (`ExpressionParser.cs:60-64`).
        None => Ok(Value::Null),
        Some(node) => evaluate_node(&node, context),
    }
}

pub fn evaluate_node(
    node: &Node,
    context: &dyn EvaluationContext,
) -> Result<Value, ExpressionError> {
    match node {
        Node::Literal(value) => Ok(value.clone()),
        // `Sdk/Wildcard.cs:23-29` — a bare wildcard evaluates to "*".
        Node::Wildcard => Ok(Value::String("*".to_string())),
        Node::NamedValue(name) => Ok(context.named_value(name)),
        Node::Not(inner) => {
            // `Sdk/Operators/Not.cs:33-39`.
            let value = evaluate_node(inner, context)?;
            Ok(Value::Boolean(value.is_falsy()))
        }
        Node::And(parameters) => {
            // `Sdk/Operators/And.cs:34-48` — short-circuits on the first falsy
            // operand and returns *that operand's value*, not a boolean.
            let mut result = Value::Null;
            for parameter in parameters {
                result = evaluate_node(parameter, context)?;
                if result.is_falsy() {
                    return Ok(result);
                }
            }
            Ok(result)
        }
        Node::Or(parameters) => {
            // `Sdk/Operators/Or.cs:34-48` — short-circuits on the first truthy
            // operand and returns that operand's value.
            let mut result = Value::Null;
            for parameter in parameters {
                result = evaluate_node(parameter, context)?;
                if result.is_truthy() {
                    break;
                }
            }
            Ok(result)
        }
        Node::Binary(operator, left, right) => {
            let left = evaluate_node(left, context)?;
            let right = evaluate_node(right, context)?;
            let result = match operator {
                // `Sdk/Operators/Equal.cs:34-42`
                BinaryOperator::Equal => left.abstract_equal(&right),
                // `Sdk/Operators/NotEqual.cs:34-42`
                BinaryOperator::NotEqual => left.abstract_not_equal(&right),
                // `Sdk/Operators/GreaterThan.cs:34-42`
                BinaryOperator::GreaterThan => left.abstract_greater_than(&right),
                // `Sdk/Operators/GreaterThanOrEqual.cs:34-42`
                BinaryOperator::GreaterThanOrEqual => left.abstract_greater_than_or_equal(&right),
                // `Sdk/Operators/LessThan.cs:34-42`
                BinaryOperator::LessThan => left.abstract_less_than(&right),
                // `Sdk/Operators/LessThanOrEqual.cs:34-42`
                BinaryOperator::LessThanOrEqual => left.abstract_less_than_or_equal(&right),
            };
            Ok(Value::Boolean(result))
        }
        Node::Index(target, index) => evaluate_index(target, index, context),
        Node::Function { name, args } => evaluate_function(name, args, context),
    }
}

/// `Sdk/Operators/Index.cs:50-201`.
fn evaluate_index(
    target: &Node,
    index: &Node,
    context: &dyn EvaluationContext,
) -> Result<Value, ExpressionError> {
    let left = evaluate_node(target, context)?;
    let is_wildcard = matches!(index, Node::Wildcard);

    let collection = match &left {
        Value::Array(array) => CollectionRef::Array(array.clone()),
        Value::Object(object) => CollectionRef::Object(object.clone()),
        // Not a collection: a wildcard yields an empty filtered array,
        // anything else yields null (`Index.cs:55-60`).
        _ => {
            return Ok(if is_wildcard {
                Value::Array(ArrayValue::filtered(Vec::new()))
            } else {
                Value::Null
            });
        }
    };

    let index_value = if is_wildcard {
        None
    } else {
        Some(evaluate_node(index, context)?)
    };

    match collection {
        // `Index.cs:62-65` — a filtered array descends one more level.
        CollectionRef::Array(array) if array.is_filtered() => {
            let mut result = Vec::new();
            for item in array.items() {
                match item {
                    Value::Object(nested) => {
                        if is_wildcard {
                            result.extend(nested.entries().iter().map(|(_, value)| value.clone()));
                        } else if let Some(key) = index_value.as_ref().and_then(string_index)
                            && let Some(value) = nested.get(&key)
                        {
                            result.push(value.clone());
                        }
                    }
                    Value::Array(nested) => {
                        if is_wildcard {
                            result.extend(nested.items().iter().cloned());
                        } else if let Some(position) = index_value.as_ref().and_then(integer_index)
                            && position < nested.len()
                        {
                            result.push(nested.items()[position].clone());
                        }
                    }
                    _ => {}
                }
            }
            Ok(Value::Array(ArrayValue::filtered(result)))
        }
        // `Index.cs:141-172`.
        CollectionRef::Object(object) => {
            if is_wildcard {
                let values = object
                    .entries()
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect();
                return Ok(Value::Array(ArrayValue::filtered(values)));
            }
            let Some(key) = index_value.as_ref().and_then(string_index) else {
                return Ok(Value::Null);
            };
            Ok(object.get(&key).cloned().unwrap_or(Value::Null))
        }
        // `Index.cs:174-201`.
        CollectionRef::Array(array) => {
            if is_wildcard {
                return Ok(Value::Array(ArrayValue::filtered(array.items().to_vec())));
            }
            let Some(position) = index_value.as_ref().and_then(integer_index) else {
                return Ok(Value::Null);
            };
            Ok(array.items().get(position).cloned().unwrap_or(Value::Null))
        }
    }
}

enum CollectionRef {
    Array(ArrayValue),
    Object(ObjectValue),
}

/// `Index.IndexHelper.StringIndex` (`Index.cs:255-258`) — only primitives can
/// index an object.
fn string_index(value: &Value) -> Option<String> {
    value.is_primitive().then(|| value.convert_to_string())
}

/// `Index.IndexHelper.IntegerIndex` (`Index.cs:236-253`).
fn integer_index(value: &Value) -> Option<usize> {
    let index = value.convert_to_number();
    if index.is_nan() || index < 0.0 {
        return None;
    }
    let index = index.floor();
    if index > f64::from(i32::MAX) {
        return None;
    }
    Some(index as usize)
}

fn evaluate_function(
    name: &str,
    args: &[Node],
    context: &dyn EvaluationContext,
) -> Result<Value, ExpressionError> {
    match name {
        "case" => evaluate_case(args, context),
        "contains" => evaluate_contains(args, context),
        "startsWith" => {
            let (left, right) = evaluate_affix_args(args, context)?;
            Ok(Value::Boolean(match (left, right) {
                // `Sdk/Functions/StartsWith.cs:14-27`
                (Some(left), Some(right)) => ordinal_ignore_case_starts_with(&left, &right),
                _ => false,
            }))
        }
        "endsWith" => {
            let (left, right) = evaluate_affix_args(args, context)?;
            Ok(Value::Boolean(match (left, right) {
                // `Sdk/Functions/EndsWith.cs:14-27`
                (Some(left), Some(right)) => ordinal_ignore_case_ends_with(&left, &right),
                _ => false,
            }))
        }
        "format" => evaluate_format(args, context),
        "join" => evaluate_join(args, context),
        "toJson" => {
            // `Sdk/Functions/ToJson.cs:11-156`.
            let value = evaluate_node(&args[0], context)?;
            Ok(Value::String(to_json(&value)))
        }
        "fromJson" => {
            // `Sdk/Functions/FromJson.cs:12-24`.
            let json = evaluate_node(&args[0], context)?.convert_to_string();
            from_json(&json)
        }
        other => {
            let mut values = Vec::with_capacity(args.len());
            for arg in args {
                values.push(evaluate_node(arg, context)?);
            }
            context.call_function(other, &values)
        }
    }
}

/// `Sdk/Functions/Case.cs:10-42`.
fn evaluate_case(args: &[Node], context: &dyn EvaluationContext) -> Result<Value, ExpressionError> {
    if args.len() % 2 == 0 {
        return Err(ExpressionError::evaluation(
            "case requires an odd number of arguments",
        ));
    }

    let mut index = 0;
    while index + 1 < args.len() {
        let predicate = evaluate_node(&args[index], context)?;
        let Value::Boolean(matched) = predicate else {
            return Err(ExpressionError::evaluation(
                "case predicate must evaluate to a boolean value",
            ));
        };
        if matched {
            return evaluate_node(&args[index + 1], context);
        }
        index += 2;
    }

    evaluate_node(&args[args.len() - 1], context)
}

/// `Sdk/Functions/Contains.cs:11-42`.
fn evaluate_contains(
    args: &[Node],
    context: &dyn EvaluationContext,
) -> Result<Value, ExpressionError> {
    let left = evaluate_node(&args[0], context)?;
    if left.is_primitive() {
        let left = left.convert_to_string();
        let right = evaluate_node(&args[1], context)?;
        if right.is_primitive() {
            return Ok(Value::Boolean(ordinal_ignore_case_contains(
                &left,
                &right.convert_to_string(),
            )));
        }
        return Ok(Value::Boolean(false));
    }

    if let Value::Array(array) = &left
        && array.len() > 0
    {
        let right = evaluate_node(&args[1], context)?;
        for item in array.items() {
            if right.abstract_equal(item) {
                return Ok(Value::Boolean(true));
            }
        }
    }

    Ok(Value::Boolean(false))
}

type AffixArgs = (Option<String>, Option<String>);

/// The shared `startsWith`/`endsWith` argument handling: both operands must be
/// primitives or the result is `false`.
fn evaluate_affix_args(
    args: &[Node],
    context: &dyn EvaluationContext,
) -> Result<AffixArgs, ExpressionError> {
    let left = evaluate_node(&args[0], context)?;
    if !left.is_primitive() {
        return Ok((None, None));
    }
    let right = evaluate_node(&args[1], context)?;
    if !right.is_primitive() {
        return Ok((Some(left.convert_to_string()), None));
    }
    Ok((
        Some(left.convert_to_string()),
        Some(right.convert_to_string()),
    ))
}

/// `Sdk/Functions/Format.cs:11-80` plus the lazy argument builder at
/// `Format.cs:215-268`. Arguments are evaluated at most once and only when a
/// placeholder actually references them.
fn evaluate_format(
    args: &[Node],
    context: &dyn EvaluationContext,
) -> Result<Value, ExpressionError> {
    let format = evaluate_node(&args[0], context)?.convert_to_string();
    let chars: Vec<char> = format.chars().collect();
    let mut cache: Vec<Option<String>> = vec![None; args.len().saturating_sub(1)];
    let mut result = String::new();
    let mut index = 0usize;

    let char_at = |position: usize| chars.get(position).copied().unwrap_or('\0');
    let invalid = || ExpressionError::evaluation(format!("Invalid format string: {format}"));

    while index < chars.len() {
        let lbrace = chars[index..]
            .iter()
            .position(|&c| c == '{')
            .map(|p| p + index);
        let rbrace = chars[index..]
            .iter()
            .position(|&c| c == '}')
            .map(|p| p + index);

        match (lbrace, rbrace) {
            (Some(lbrace), rbrace) if rbrace.is_none_or(|rbrace| rbrace > lbrace) => {
                if char_at(lbrace + 1) == '{' {
                    // Escaped left brace.
                    result.extend(&chars[index..=lbrace]);
                    index = lbrace + 2;
                    continue;
                }

                // Left brace, number, optional format specifiers, right brace
                // (`Format.cs:34-37`).
                if rbrace.is_none_or(|rbrace| rbrace <= lbrace + 1) {
                    return Err(invalid());
                }
                let Some((arg_index, end_arg_index)) = read_arg_index(&chars, lbrace + 1) else {
                    return Err(invalid());
                };
                let Some((specifiers, rbrace)) = read_format_specifiers(&chars, end_arg_index + 1)
                else {
                    return Err(invalid());
                };

                if arg_index + 1 >= args.len() {
                    return Err(ExpressionError::evaluation(format!(
                        "The following format string references more arguments than were supplied: {format}"
                    )));
                }
                if !specifiers.is_empty() {
                    return Err(ExpressionError::evaluation(format!(
                        "The format specifiers '{specifiers}' are not valid for objects of type '{}'",
                        evaluate_node(&args[arg_index + 1], context)?.kind().as_str()
                    )));
                }

                if lbrace > index {
                    result.extend(&chars[index..lbrace]);
                }

                if cache[arg_index].is_none() {
                    cache[arg_index] =
                        Some(evaluate_node(&args[arg_index + 1], context)?.convert_to_string());
                }
                result.push_str(cache[arg_index].as_deref().unwrap_or_default());
                index = rbrace + 1;
            }
            (_, Some(rbrace)) => {
                if char_at(rbrace + 1) == '}' {
                    // Escaped right brace.
                    result.extend(&chars[index..=rbrace]);
                    index = rbrace + 2;
                } else {
                    return Err(invalid());
                }
            }
            _ => {
                result.extend(&chars[index..]);
                break;
            }
        }
    }

    Ok(Value::String(result))
}

/// `Format.ReadArgIndex` (`Format.cs:82-105`) — the index is a `Byte`.
fn read_arg_index(chars: &[char], start: usize) -> Option<(usize, usize)> {
    let mut length = 0usize;
    while chars
        .get(start + length)
        .is_some_and(|c| c.is_ascii_digit())
    {
        length += 1;
    }
    if length == 0 {
        return None;
    }
    let digits: String = chars[start..start + length].iter().collect();
    let parsed = digits.parse::<u8>().ok()?;
    Some((usize::from(parsed), start + length - 1))
}

/// `Format.ReadFormatSpecifiers` (`Format.cs:107-165`).
fn read_format_specifiers(chars: &[char], start: usize) -> Option<(String, usize)> {
    let c = chars.get(start).copied().unwrap_or('\0');
    if c == '}' {
        return Some((String::new(), start));
    }
    if c != ':' {
        return None;
    }

    let mut specifiers = String::new();
    let mut index = start + 1;
    loop {
        let c = *chars.get(index)?;
        if c != '}' {
            specifiers.push(c);
            index += 1;
        } else if chars.get(index + 1).copied() == Some('}') {
            specifiers.push('}');
            index += 2;
        } else {
            return Some((specifiers, index));
        }
    }
}

/// `Sdk/Functions/Join.cs:11-73`.
fn evaluate_join(args: &[Node], context: &dyn EvaluationContext) -> Result<Value, ExpressionError> {
    let items = evaluate_node(&args[0], context)?;

    if let Value::Array(array) = &items
        && array.len() > 0
    {
        let separator = if args.len() > 1 {
            let separator = evaluate_node(&args[1], context)?;
            if separator.is_primitive() {
                separator.convert_to_string()
            } else {
                ",".to_string()
            }
        } else {
            ",".to_string()
        };
        let joined = array
            .items()
            .iter()
            .map(Value::convert_to_string)
            .collect::<Vec<_>>()
            .join(&separator);
        return Ok(Value::String(joined));
    }

    if items.is_primitive() {
        return Ok(Value::String(items.convert_to_string()));
    }

    Ok(Value::String(String::new()))
}

/// `Sdk/Functions/ToJson.cs` — reproduces upstream's exact layout: two-space
/// indentation, `": "` between a key and its value, and a leading newline
/// before every element.
pub fn to_json(value: &Value) -> String {
    let mut out = String::new();
    write_json(value, &mut out, 0, JsonPrefix::Root);
    out
}

#[derive(Clone, Copy)]
enum JsonPrefix {
    Root,
    /// The value slot of a mapping pair (`ToJson.PrefixValue`, `ToJson.cs:264-267`).
    MappingValue,
    /// An element of a collection; `true` when it is the first one.
    Element(bool),
}

fn write_json(value: &Value, out: &mut String, level: usize, prefix: JsonPrefix) {
    let indent = "  ".repeat(level);
    let prefix = match prefix {
        JsonPrefix::Root => String::new(),
        JsonPrefix::MappingValue => ": ".to_string(),
        JsonPrefix::Element(first) => {
            format!("{}\n{indent}", if first { "" } else { "," })
        }
    };

    match value {
        Value::Object(object) if object.len() > 0 => {
            out.push_str(&prefix);
            out.push('{');
            for (position, (key, entry)) in object.entries().iter().enumerate() {
                write_json(
                    &Value::String(key.clone()),
                    out,
                    level + 1,
                    JsonPrefix::Element(position == 0),
                );
                write_json(entry, out, level + 1, JsonPrefix::MappingValue);
            }
            out.push('\n');
            out.push_str(&"  ".repeat(level));
            out.push('}');
        }
        Value::Array(array) if array.len() > 0 => {
            out.push_str(&prefix);
            out.push('[');
            for (position, item) in array.items().iter().enumerate() {
                write_json(item, out, level + 1, JsonPrefix::Element(position == 0));
            }
            out.push('\n');
            out.push_str(&"  ".repeat(level));
            out.push(']');
        }
        // `ToJson.cs:229-256` — WriteValue / the empty-collection writers.
        other => {
            out.push_str(&prefix);
            out.push_str(&match other {
                Value::Null => "null".to_string(),
                Value::Boolean(value) => if *value { "true" } else { "false" }.to_string(),
                Value::Number(value) => format_number(*value),
                Value::String(value) => json_string(value),
                Value::Array(_) => "[]".to_string(),
                Value::Object(_) => "{}".to_string(),
            });
        }
    }
}

fn json_string(value: &str) -> String {
    serde_json::Value::String(value.to_string()).to_string()
}

/// `Sdk/Functions/FromJson.cs:12-24` — `JToken.ReadFrom(...).ToPipelineContextData()`.
fn from_json(json: &str) -> Result<Value, ExpressionError> {
    let parsed: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| ExpressionError::evaluation(format!("Unable to parse JSON: {error}")))?;
    Ok(from_serde_json(&parsed))
}

/// Convert a `serde_json` document to the expression value union. Objects
/// become case-insensitive mappings, matching `DictionaryContextData`
/// (`src/Sdk/DTPipelines/Pipelines/ContextData/DictionaryContextData.cs:77`).
pub fn from_serde_json(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Boolean(*value),
        serde_json::Value::Number(value) => Value::Number(value.as_f64().unwrap_or(f64::NAN)),
        serde_json::Value::String(value) => Value::String(value.clone()),
        serde_json::Value::Array(items) => {
            Value::Array(ArrayValue::new(items.iter().map(from_serde_json).collect()))
        }
        serde_json::Value::Object(entries) => Value::Object(ObjectValue::new(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), from_serde_json(value)))
                .collect(),
        )),
    }
}

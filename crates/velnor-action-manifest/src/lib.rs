//! Shared `actions/runner` v2.337.0 action-manifest parsing rules.

use serde_yaml::cst::{GreenChild, GreenNode, SyntaxKind};
use velnor_expression::{parse, ParseEnvironment};

/// Expression contexts used by the action-manifest schema in Runner v2.337.0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionExpressionContext {
    InputDefault,
    OutputValue,
    ContainerRun,
    CompositeString,
    CompositeBoolean,
    CompositeIf,
}

const DEFAULT_CONTEXTS: &[&str] = &["github", "strategy", "matrix", "job", "runner"];
const OUTPUT_CONTEXTS: &[&str] = &[
    "github", "strategy", "matrix", "steps", "inputs", "job", "runner", "env",
];
const STEP_CONTEXTS: &[&str] = &[
    "github", "inputs", "strategy", "matrix", "steps", "job", "runner", "env",
];
const INPUTS_CONTEXT: &[&str] = &["inputs"];
const HASH_FILES: &[(&str, usize, usize)] = &[("hashFiles", 1, 255)];
const STEP_IF_FUNCTIONS: &[(&str, usize, usize)] = &[
    ("always", 0, 0),
    ("failure", 0, 0),
    ("cancelled", 0, 0),
    ("success", 0, 0),
    ("hashFiles", 1, 255),
];
const RUNNER_NUMBER_TAG: &str = "!velnor-runner-number";

#[derive(Clone, Copy)]
enum RunnerNumericTag {
    Integer,
    Float,
}

#[derive(Clone, Copy)]
struct TaggedScalar {
    numeric_tag: Option<RunnerNumericTag>,
    start: usize,
    end: usize,
}

/// Read an internally tagged numeric scalar inserted by
/// [`normalize_runner_yaml_numbers`].
pub fn normalized_runner_number(value: &serde_yaml::Value) -> Option<&str> {
    let tagged = value.as_tagged()?;
    if tagged.tag().as_str() == RUNNER_NUMBER_TAG {
        tagged.value().as_str()
    } else {
        None
    }
}

struct Environment {
    contexts: &'static [&'static str],
    functions: &'static [(&'static str, usize, usize)],
}

impl ActionExpressionContext {
    const fn environment(self) -> Environment {
        match self {
            Self::InputDefault => Environment {
                contexts: DEFAULT_CONTEXTS,
                functions: HASH_FILES,
            },
            Self::OutputValue => Environment {
                contexts: OUTPUT_CONTEXTS,
                functions: &[],
            },
            Self::ContainerRun => Environment {
                contexts: INPUTS_CONTEXT,
                functions: &[],
            },
            Self::CompositeString | Self::CompositeBoolean => Environment {
                contexts: STEP_CONTEXTS,
                functions: HASH_FILES,
            },
            Self::CompositeIf => Environment {
                contexts: STEP_CONTEXTS,
                functions: STEP_IF_FUNCTIONS,
            },
        }
    }
}

impl ParseEnvironment for Environment {
    fn is_named_value(&self, name: &str) -> bool {
        self.contexts
            .iter()
            .any(|context| context.eq_ignore_ascii_case(name))
    }

    fn function_arity(&self, name: &str) -> Option<(usize, usize)> {
        self.functions
            .iter()
            .find(|(function, _, _)| function.eq_ignore_ascii_case(name))
            .map(|(_, min, max)| (*min, *max))
    }
}

/// Validate every `${{ ... }}` interpolation using the selected schema context.
pub fn validate_template_expressions(
    value: &str,
    context: ActionExpressionContext,
) -> Result<(), String> {
    let environment = context.environment();
    let mut cursor = 0;
    while let Some(relative_start) = value[cursor..].find("${{") {
        let expression_start = cursor + relative_start + 3;
        let expression_end = template_expression_end(value, expression_start)
            .ok_or_else(|| "unterminated template expression".to_owned())?;
        let expression = value[expression_start..expression_end].trim();
        if expression.is_empty() || !matches!(parse(expression, &environment), Ok(Some(_))) {
            return Err("invalid expression context or syntax".to_owned());
        }
        cursor = expression_end + 2;
    }
    Ok(())
}

/// Validate a complete boolean expression token, including its scalar form.
/// Callers handle YAML booleans directly and pass string tokens here.
pub fn validate_boolean_expression(
    value: &str,
    context: ActionExpressionContext,
) -> Result<(), String> {
    let value = value.trim();
    let expression = value
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map(str::trim)
        .filter(|expression| !expression.is_empty())
        .ok_or_else(|| "expected one complete template expression".to_owned())?;
    let environment = context.environment();
    if matches!(parse(expression, &environment), Ok(Some(_))) {
        Ok(())
    } else {
        Err("invalid expression context or syntax".to_owned())
    }
}

fn template_expression_end(value: &str, start: usize) -> Option<usize> {
    let mut in_string = false;
    let mut characters = value[start..].char_indices().peekable();
    while let Some((offset, character)) = characters.next() {
        if in_string {
            if character == '\'' {
                if characters.peek().is_some_and(|(_, next)| *next == '\'') {
                    let _ = characters.next();
                } else {
                    in_string = false;
                }
            }
        } else if character == '\'' {
            in_string = true;
        } else if character == '}' && value.get(start + offset..)?.starts_with("}}") {
            return Some(start + offset);
        }
    }
    None
}

/// Normalize YAML numeric scalars to the strings `TemplateReader` gets from
/// `NumberToken.ToString()`; preserve their source before Serde erases it.
pub fn normalize_runner_yaml_numbers(source: &str) -> Result<String, String> {
    let document = serde_yaml::cst::parse_document(source).map_err(|error| error.to_string())?;
    if has_reserved_number_tag(document.syntax(), source, 0) {
        return Err("reserved action-manifest scalar tag is not supported".to_owned());
    }
    let mut replacements = Vec::new();
    collect_numeric_replacements(document.syntax(), source, 0, false, &mut replacements)?;
    if replacements.is_empty() {
        return Ok(source.to_owned());
    }

    let mut normalized = String::with_capacity(source.len());
    let mut cursor = 0;
    for (start, end, replacement) in replacements {
        if start < cursor {
            return Err("overlapping YAML scalar tokens".to_owned());
        }
        normalized.push_str(
            source
                .get(cursor..start)
                .ok_or_else(|| "invalid YAML scalar token offset".to_owned())?,
        );
        normalized.push_str(&replacement);
        cursor = end;
    }
    normalized.push_str(
        source
            .get(cursor..)
            .ok_or_else(|| "invalid YAML scalar token offset".to_owned())?,
    );
    Ok(normalized)
}

fn collect_numeric_replacements(
    node: &GreenNode,
    source: &str,
    offset: usize,
    mapping_key: bool,
    replacements: &mut Vec<(usize, usize, String)>,
) -> Result<(), String> {
    if node.kind() == SyntaxKind::MappingEntry {
        let mut child_offset = offset;
        let mut in_key = true;
        let mut tagged_scalar = None;
        for child in node.children() {
            match child {
                GreenChild::Token {
                    kind: SyntaxKind::ColonIndicator,
                    ..
                } => in_key = false,
                GreenChild::Token {
                    kind: SyntaxKind::TagMark,
                    len,
                } => tagged_scalar = Some(read_tag_token(source, child_offset, *len)?),
                GreenChild::Token {
                    kind: SyntaxKind::PlainScalar,
                    len,
                } => {
                    let len = usize::try_from(*len)
                        .map_err(|_| "YAML scalar length does not fit usize")?;
                    if let Some(tagged) = tagged_scalar.take() {
                        if tagged.numeric_tag.is_some() {
                            collect_explicit_numeric_token(
                                source,
                                tagged,
                                child_offset,
                                len,
                                in_key,
                                replacements,
                            )?;
                        }
                    } else {
                        collect_numeric_token(source, child_offset, len, in_key, replacements)?;
                    }
                }
                GreenChild::Token {
                    kind:
                        SyntaxKind::SingleQuotedScalar
                        | SyntaxKind::DoubleQuotedScalar
                        | SyntaxKind::LiteralScalar
                        | SyntaxKind::FoldedScalar,
                    ..
                } => {
                    if tagged_scalar
                        .take()
                        .is_some_and(|tagged| tagged.numeric_tag.is_some())
                    {
                        return Err("numeric YAML tags require a plain scalar".to_owned());
                    }
                }
                GreenChild::Node(child_node) => {
                    if tagged_scalar
                        .take()
                        .is_some_and(|tagged| tagged.numeric_tag.is_some())
                    {
                        return Err("numeric YAML tags cannot tag collections".to_owned());
                    }
                    collect_numeric_replacements(
                        child_node,
                        source,
                        child_offset,
                        in_key,
                        replacements,
                    )?;
                }
                GreenChild::Token {
                    kind: SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::Comment,
                    ..
                } => {}
                GreenChild::Token { .. } => {
                    if tagged_scalar
                        .take()
                        .is_some_and(|tagged| tagged.numeric_tag.is_some())
                    {
                        return Err("numeric YAML tag must precede a scalar value".to_owned());
                    }
                }
            }
            child_offset += child.text_len();
        }
        if tagged_scalar.is_some_and(|tagged| tagged.numeric_tag.is_some()) {
            return Err("numeric YAML tag must precede a scalar value".to_owned());
        }
        return Ok(());
    }

    if node.kind() == SyntaxKind::FlowMapping {
        let mut child_offset = offset;
        let mut in_key = true;
        let mut tagged_scalar = None;
        for child in node.children() {
            match child {
                GreenChild::Token {
                    kind: SyntaxKind::OpenBrace | SyntaxKind::Comma,
                    ..
                } => in_key = true,
                GreenChild::Token {
                    kind: SyntaxKind::ColonIndicator,
                    ..
                } => in_key = false,
                GreenChild::Token {
                    kind: SyntaxKind::TagMark,
                    len,
                } => tagged_scalar = Some(read_tag_token(source, child_offset, *len)?),
                GreenChild::Token {
                    kind: SyntaxKind::PlainScalar,
                    len,
                } => {
                    let len = usize::try_from(*len)
                        .map_err(|_| "YAML scalar length does not fit usize")?;
                    if let Some(tagged) = tagged_scalar.take() {
                        if tagged.numeric_tag.is_some() {
                            collect_explicit_numeric_token(
                                source,
                                tagged,
                                child_offset,
                                len,
                                in_key,
                                replacements,
                            )?;
                        }
                    } else {
                        collect_numeric_token(source, child_offset, len, in_key, replacements)?;
                    }
                }
                GreenChild::Token {
                    kind:
                        SyntaxKind::SingleQuotedScalar
                        | SyntaxKind::DoubleQuotedScalar
                        | SyntaxKind::LiteralScalar
                        | SyntaxKind::FoldedScalar,
                    ..
                } => {
                    if tagged_scalar
                        .take()
                        .is_some_and(|tagged| tagged.numeric_tag.is_some())
                    {
                        return Err("numeric YAML tags require a plain scalar".to_owned());
                    }
                }
                GreenChild::Node(child_node) => {
                    if tagged_scalar
                        .take()
                        .is_some_and(|tagged| tagged.numeric_tag.is_some())
                    {
                        return Err("numeric YAML tags cannot tag collections".to_owned());
                    }
                    collect_numeric_replacements(
                        child_node,
                        source,
                        child_offset,
                        in_key,
                        replacements,
                    )?;
                }
                GreenChild::Token {
                    kind: SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::Comment,
                    ..
                } => {}
                GreenChild::Token { .. } => {
                    if tagged_scalar
                        .take()
                        .is_some_and(|tagged| tagged.numeric_tag.is_some())
                    {
                        return Err("numeric YAML tag must precede a scalar value".to_owned());
                    }
                }
            }
            child_offset += child.text_len();
        }
        if tagged_scalar.is_some_and(|tagged| tagged.numeric_tag.is_some()) {
            return Err("numeric YAML tag must precede a scalar value".to_owned());
        }
        return Ok(());
    }

    let mut child_offset = offset;
    let mut tagged_scalar = None;
    for child in node.children() {
        match child {
            GreenChild::Token {
                kind: SyntaxKind::TagMark,
                len,
            } => tagged_scalar = Some(read_tag_token(source, child_offset, *len)?),
            GreenChild::Token {
                kind: SyntaxKind::PlainScalar,
                len,
            } => {
                let len =
                    usize::try_from(*len).map_err(|_| "YAML scalar length does not fit usize")?;
                if let Some(tagged) = tagged_scalar.take() {
                    if tagged.numeric_tag.is_some() {
                        collect_explicit_numeric_token(
                            source,
                            tagged,
                            child_offset,
                            len,
                            mapping_key,
                            replacements,
                        )?;
                    }
                } else {
                    collect_numeric_token(source, child_offset, len, mapping_key, replacements)?;
                }
            }
            GreenChild::Token {
                kind:
                    SyntaxKind::SingleQuotedScalar
                    | SyntaxKind::DoubleQuotedScalar
                    | SyntaxKind::LiteralScalar
                    | SyntaxKind::FoldedScalar,
                ..
            } => {
                if tagged_scalar
                    .take()
                    .is_some_and(|tagged| tagged.numeric_tag.is_some())
                {
                    return Err("numeric YAML tags require a plain scalar".to_owned());
                }
            }
            GreenChild::Node(child_node) => {
                if tagged_scalar
                    .take()
                    .is_some_and(|tagged| tagged.numeric_tag.is_some())
                {
                    return Err("numeric YAML tags cannot tag collections".to_owned());
                }
                collect_numeric_replacements(
                    child_node,
                    source,
                    child_offset,
                    mapping_key,
                    replacements,
                )?;
            }
            GreenChild::Token {
                kind: SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::Comment,
                ..
            } => {}
            GreenChild::Token { .. } => {
                if tagged_scalar
                    .take()
                    .is_some_and(|tagged| tagged.numeric_tag.is_some())
                {
                    return Err("numeric YAML tag must precede a scalar value".to_owned());
                }
            }
        }
        child_offset += child.text_len();
    }
    if tagged_scalar.is_some_and(|tagged| tagged.numeric_tag.is_some()) {
        return Err("numeric YAML tag must precede a scalar value".to_owned());
    }
    Ok(())
}

fn read_tag_token(source: &str, start: usize, len: u32) -> Result<TaggedScalar, String> {
    let len = usize::try_from(len).map_err(|_| "YAML tag length does not fit usize")?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| "YAML tag offset overflow".to_owned())?;
    let tag = source
        .get(start..end)
        .ok_or_else(|| "invalid YAML tag token offset".to_owned())?;
    Ok(TaggedScalar {
        numeric_tag: runner_numeric_tag(tag),
        start,
        end,
    })
}

fn runner_numeric_tag(tag: &str) -> Option<RunnerNumericTag> {
    match tag {
        "!!int" | "!<tag:yaml.org,2002:int>" => Some(RunnerNumericTag::Integer),
        "!!float" | "!<tag:yaml.org,2002:float>" => Some(RunnerNumericTag::Float),
        _ => None,
    }
}

fn collect_explicit_numeric_token(
    source: &str,
    tagged: TaggedScalar,
    scalar_start: usize,
    len: usize,
    mapping_key: bool,
    replacements: &mut Vec<(usize, usize, String)>,
) -> Result<(), String> {
    let tag = tagged
        .numeric_tag
        .ok_or_else(|| "expected an explicit numeric YAML tag".to_owned())?;
    let end = scalar_start
        .checked_add(len)
        .ok_or_else(|| "YAML scalar offset overflow".to_owned())?;
    let raw = source
        .get(scalar_start..end)
        .ok_or_else(|| "invalid YAML scalar token offset".to_owned())?;
    let trimmed = raw.trim_matches([' ', '\t', '\r', '\n']);
    let leading = raw.len() - raw.trim_start_matches([' ', '\t', '\r', '\n']).len();
    let normalized = match tag {
        RunnerNumericTag::Integer => parse_runner_integer(trimmed)?,
        RunnerNumericTag::Float => parse_runner_float(trimmed)?,
    }
    .ok_or_else(|| match tag {
        RunnerNumericTag::Integer => {
            format!("invalid explicitly tagged YAML integer `{trimmed}` in actions/runner")
        }
        RunnerNumericTag::Float => {
            format!("invalid explicitly tagged YAML float `{trimmed}` in actions/runner")
        }
    })?;
    let normalized = velnor_expression::value::format_number(normalized);
    let scalar_replacement = serde_json::to_string(&normalized)
        .map_err(|error| format!("quote normalized YAML scalar: {error}"))?;
    replacements.push((
        tagged.start,
        tagged.end,
        if mapping_key {
            String::new()
        } else {
            RUNNER_NUMBER_TAG.to_owned()
        },
    ));
    let scalar_start = scalar_start + leading;
    replacements.push((
        scalar_start,
        scalar_start + trimmed.len(),
        scalar_replacement,
    ));
    Ok(())
}

fn has_reserved_number_tag(node: &GreenNode, source: &str, offset: usize) -> bool {
    let mut child_offset = offset;
    for child in node.children() {
        let reserved = match child {
            GreenChild::Token {
                kind: SyntaxKind::TagMark,
                len,
            } => usize::try_from(*len)
                .ok()
                .and_then(|len| child_offset.checked_add(len))
                .and_then(|end| source.get(child_offset..end))
                .is_some_and(|tag| tag == RUNNER_NUMBER_TAG),
            GreenChild::Node(child_node) => {
                has_reserved_number_tag(child_node, source, child_offset)
            }
            GreenChild::Token { .. } => false,
        };
        if reserved {
            return true;
        }
        child_offset += child.text_len();
    }
    false
}

fn collect_numeric_token(
    source: &str,
    start: usize,
    len: usize,
    mapping_key: bool,
    replacements: &mut Vec<(usize, usize, String)>,
) -> Result<(), String> {
    let end = start + len;
    let raw = source
        .get(start..end)
        .ok_or_else(|| "invalid YAML scalar token offset".to_owned())?;
    let trimmed = raw.trim_matches([' ', '\t', '\r', '\n']);
    let leading = raw.len() - raw.trim_start_matches([' ', '\t', '\r', '\n']).len();
    let scalar_start = start + leading;
    let scalar_end = scalar_start + trimmed.len();
    let Some((replacement, runner_number)) = normalize_numeric_lexeme(trimmed)? else {
        return Ok(());
    };
    let quoted = serde_json::to_string(&replacement)
        .map_err(|error| format!("quote normalized YAML scalar: {error}"))?;
    let replacement = if mapping_key {
        quoted
    } else if runner_number {
        format!("{RUNNER_NUMBER_TAG} {quoted}")
    } else {
        quoted
    };
    replacements.push((scalar_start, scalar_end, replacement));
    Ok(())
}

fn normalize_numeric_lexeme(raw: &str) -> Result<Option<(String, bool)>, String> {
    let Some(value) = parse_runner_number(raw)? else {
        return match serde_yaml::from_str::<serde_yaml::Value>(raw) {
            Ok(value) if value.is_number() => Ok(Some((raw.to_owned(), false))),
            _ => Ok(None),
        };
    };
    Ok(Some((velnor_expression::value::format_number(value), true)))
}

fn parse_runner_number(value: &str) -> Result<Option<f64>, String> {
    if let Some(value) = parse_runner_integer(value)? {
        return Ok(Some(value));
    }
    parse_runner_float(value)
}

fn parse_runner_integer(value: &str) -> Result<Option<f64>, String> {
    if is_ascii_digits(value) {
        return value
            .parse::<f64>()
            .map(Some)
            .map_err(|_| format!("invalid YAML integer `{value}` in actions/runner"));
    }
    if let Some(digits) = value.strip_prefix(['+', '-'])
        && !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return value
            .parse::<f64>()
            .map(Some)
            .map_err(|_| format!("invalid YAML integer `{value}` in actions/runner"));
    }
    if let Some(hex) = value.strip_prefix("0x")
        && !hex.is_empty()
        && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        let value = u32::from_str_radix(hex, 16)
            .map_err(|_| format!("invalid YAML hexadecimal integer `{value}` in actions/runner"))?;
        return Ok(Some((value as i32) as f64));
    }
    if let Some(octal) = value.strip_prefix("0o")
        && !octal.is_empty()
        && octal.bytes().all(|byte| (b'0'..=b'7').contains(&byte))
    {
        let value = u32::from_str_radix(octal, 8)
            .ok()
            .filter(|value| *value <= i32::MAX as u32)
            .ok_or_else(|| format!("invalid YAML octal integer `{value}` in actions/runner"))?;
        return Ok(Some(value as f64));
    }
    Ok(None)
}

fn parse_runner_float(value: &str) -> Result<Option<f64>, String> {
    match value {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return Ok(Some(f64::INFINITY));
        }
        "-.inf" | "-.Inf" | "-.INF" => return Ok(Some(f64::NEG_INFINITY)),
        ".nan" | ".NaN" | ".NAN" => return Ok(Some(f64::NAN)),
        _ => {}
    }

    if is_runner_float(value) {
        return value
            .parse::<f64>()
            .map(Some)
            .map_err(|_| format!("invalid YAML float `{value}` in actions/runner"));
    }
    Ok(None)
}

fn is_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_runner_float(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = usize::from(
        bytes
            .first()
            .is_some_and(|byte| matches!(byte, b'-' | b'+')),
    );
    let mut has_integer = false;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        has_integer = true;
        index += 1;
    }
    let mut has_dot = false;
    let mut has_decimal = false;
    if bytes.get(index) == Some(&b'.') {
        has_dot = true;
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            has_decimal = true;
            index += 1;
        }
    }
    if !(has_integer || has_dot && has_decimal) {
        return false;
    }
    if index == bytes.len() {
        return true;
    }
    if !matches!(bytes.get(index), Some(b'e' | b'E')) {
        return false;
    }
    index += 1;
    if matches!(bytes.get(index), Some(b'-' | b'+')) {
        index += 1;
    }
    let exponent_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    index == bytes.len() && index > exponent_start
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_runner_yaml_numbers, normalized_runner_number, validate_boolean_expression,
        validate_template_expressions, ActionExpressionContext,
    };

    #[test]
    fn expression_context_profiles_match_runner_action_schema() {
        assert!(validate_template_expressions(
            "${{ github.action_path }}",
            ActionExpressionContext::InputDefault
        )
        .is_ok());
        assert!(validate_template_expressions(
            "${{ inputs.value }}",
            ActionExpressionContext::ContainerRun
        )
        .is_ok());
        for (value, context) in [
            (
                "${{ secrets.token }}",
                ActionExpressionContext::InputDefault,
            ),
            ("${{ env.PATH }}", ActionExpressionContext::ContainerRun),
            (
                "${{ secrets.token }}",
                ActionExpressionContext::CompositeString,
            ),
            (
                "${{ success() }}",
                ActionExpressionContext::CompositeBoolean,
            ),
            ("${{ always() }}", ActionExpressionContext::OutputValue),
        ] {
            assert!(
                validate_template_expressions(value, context).is_err(),
                "{value} must be rejected in {context:?}"
            );
        }
        assert!(validate_boolean_expression(
            "${{ success() }}",
            ActionExpressionContext::CompositeIf
        )
        .is_ok());
        assert!(validate_boolean_expression(
            "${{ success() }}",
            ActionExpressionContext::CompositeBoolean
        )
        .is_err());
    }

    #[test]
    fn runner_numeric_scalars_use_double_g15_and_radix_rules() {
        let source = "value: 1.2345678901234567\na: 1e14\nb: 1e15\nc: 1e-4\nd: 1e-5\ne: -0\nf: -0.0\ng: 9007199254740993\nh: 0xFFFFFFFF\ni: 0o10\nj: 0b101\n9007199254740993: key\n";
        let value = normalize_runner_yaml_numbers(source).expect("normalization succeeds");
        assert!(value.contains("value: !velnor-runner-number \"1.23456789012346\""));
        assert!(value.contains("a: !velnor-runner-number \"100000000000000\""));
        assert!(value.contains("b: !velnor-runner-number \"1E+15\""));
        assert!(value.contains("c: !velnor-runner-number \"0.0001\""));
        assert!(value.contains("d: !velnor-runner-number \"1E-05\""));
        assert!(value.contains("e: !velnor-runner-number \"-0\""));
        assert!(value.contains("f: !velnor-runner-number \"-0\""));
        assert!(value.contains("g: !velnor-runner-number \"9.00719925474099E+15\""));
        assert!(value.contains("h: !velnor-runner-number \"-1\""));
        assert!(value.contains("i: !velnor-runner-number \"8\""));
        assert!(value.contains("j: 0b101") || value.contains("j: \"0b101\""));
        assert!(value.contains("\"9.00719925474099E+15\": key"));
        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&value).expect("normalized YAML parses");
        let map = parsed.as_mapping().expect("mapping");
        let number = map
            .get("value")
            .expect("value scalar is retained")
            .to_owned();
        assert_eq!(normalized_runner_number(&number), Some("1.23456789012346"));
    }

    #[test]
    fn runner_numeric_scalar_range_errors_match_runner() {
        assert!(normalize_runner_yaml_numbers("value: 0x100000000\n").is_err());
        assert!(normalize_runner_yaml_numbers("value: 0o40000000000\n").is_err());
    }

    #[test]
    fn explicitly_tagged_integers_follow_runner_parse_integer_rules() {
        let source = concat!(
            "hex_unsigned_bits: !!int 0xFFFFFFFF\n",
            "hex_signed_min: !<tag:yaml.org,2002:int> 0x80000000\n",
            "positive: !!int +42\n",
            "negative: !!int -42\n",
            "wide_decimal: !!int 4294967296\n",
        );
        let normalized =
            normalize_runner_yaml_numbers(source).expect("valid explicit integers normalize");
        assert!(normalized.contains("hex_unsigned_bits: !velnor-runner-number \"-1\""));
        assert!(normalized.contains("hex_signed_min: !velnor-runner-number \"-2147483648\""));
        assert!(normalized.contains("positive: !velnor-runner-number \"42\""));
        assert!(normalized.contains("negative: !velnor-runner-number \"-42\""));
        assert!(normalized.contains("wide_decimal: !velnor-runner-number \"4294967296\""));

        for source in [
            "value: !!int 0x100000000\n",
            "value: !!int +0x1\n",
            "value: !!int -0x1\n",
            "value: !!int 0o40000000000\n",
            "value: !!int 1.25\n",
            "value: !!int 1e3\n",
            "value: !!int \"7\"\n",
        ] {
            assert!(
                normalize_runner_yaml_numbers(source).is_err(),
                "Runner rejects explicitly tagged integer `{source}`"
            );
        }
    }

    #[test]
    fn explicitly_tagged_floats_follow_runner_parse_float_rules() {
        let source = concat!(
            "decimal: !!float 1.25\n",
            "signed_exponent: !!float +1.5e+2\n",
            "integer_form: !!float 1\n",
            "small_exponent: !!float .5e-3\n",
            "large_exponent: !!float 1e15\n",
        );
        let normalized =
            normalize_runner_yaml_numbers(source).expect("valid explicit floats normalize");
        assert!(normalized.contains("decimal: !velnor-runner-number \"1.25\""));
        assert!(normalized.contains("signed_exponent: !velnor-runner-number \"150\""));
        assert!(normalized.contains("integer_form: !velnor-runner-number \"1\""));
        assert!(normalized.contains("small_exponent: !velnor-runner-number \"0.0005\""));
        assert!(normalized.contains("large_exponent: !velnor-runner-number \"1E+15\""));

        for source in [
            "value: !!float 0x10\n",
            "value: !!float 1e\n",
            "value: !!float 1e+\n",
            "value: !!float \"1.5\"\n",
        ] {
            assert!(
                normalize_runner_yaml_numbers(source).is_err(),
                "Runner rejects explicitly tagged float `{source}`"
            );
        }
    }

    #[test]
    fn explicitly_tagged_numbers_normalize_in_flow_mappings_and_keys() {
        let normalized = normalize_runner_yaml_numbers(
            "values: { hex: !!int 0xFFFFFFFF, ratio: !!float 1.5e1 }\n",
        )
        .expect("flow-mapping numbers normalize");
        assert!(normalized.contains("hex: !velnor-runner-number \"-1\""));
        assert!(normalized.contains("ratio: !velnor-runner-number \"15\""));

        let normalized = normalize_runner_yaml_numbers("!!int 0xFFFFFFFF: value\n")
            .expect("tagged numeric mapping key normalizes");
        assert!(normalized.contains("\"-1\": value"));
    }
}

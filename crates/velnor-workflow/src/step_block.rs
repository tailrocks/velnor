//! Typed condition composition for generated workflow step blocks.
//!
//! Renderers build small YAML step blocks before a collapsed reusable job adds
//! a membership gate.  Conditions are step metadata, not shell text: this
//! module keeps the complete step body intact while composing one `if` field
//! for each top-level step.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepBlockError {
    message: String,
}

impl StepBlockError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for StepBlockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Add `guard` to every top-level step in `block`.
///
/// The parser only recognizes workflow-step metadata at its exact YAML
/// indentation.  Therefore `if:` text in a `run: |` body, `env:`, `with:`, or
/// another nested mapping remains byte-for-byte unchanged.  Existing step
/// metadata is retained in its original order and only its condition line is
/// replaced.  A step with no condition receives one immediately after its
/// list-item line.
pub(crate) fn prefix_step_block_with_if(
    block: &str,
    guard: Option<&str>,
) -> Result<String, StepBlockError> {
    if guard.is_some_and(|guard| guard.trim().is_empty()) {
        return Err(StepBlockError::new("step guard must not be empty"));
    }

    let starts = step_starts(block);
    if starts.is_empty() {
        return Ok(block.to_owned());
    }

    let mut output = String::with_capacity(block.len() + starts.len() * 32);
    output.push_str(&block[..starts[0]]);
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(block.len());
        output.push_str(&render_step(&block[start..end], guard)?);
    }
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConditionLocation {
    line_start: usize,
    line_end: usize,
    value_start: usize,
    value_end: usize,
}

fn render_step(step: &str, guard: Option<&str>) -> Result<String, StepBlockError> {
    let mut first_line_end = None;
    let mut condition = None;
    let mut line_start = 0;
    let mut line_number = 0;

    while line_start < step.len() {
        let line_end = next_line_end(step, line_start);
        let line = &step[line_start..line_end];
        if first_line_end.is_none() {
            first_line_end = Some(line_end);
        }
        if let Some(location) = condition_location(line_start, line_end, line, line_number == 0) {
            if condition.is_some() {
                return Err(StepBlockError::new(
                    "step block contains more than one top-level if condition",
                ));
            }
            condition = Some(location);
        }
        line_start = line_end;
        line_number += 1;
    }

    // A one-line step without a trailing newline still has a first line.
    let first_line_end = first_line_end.unwrap_or(step.len());
    let parsed_condition = condition
        .as_ref()
        .map(|location| parse_condition(&step[location.value_start..location.value_end]))
        .transpose()?;
    let Some(guard) = guard else {
        return Ok(step.to_owned());
    };

    let Some(condition) = condition else {
        let newline = line_ending(&step[..first_line_end]);
        let insertion_line_ending = if newline.is_empty() { "\n" } else { newline };
        let mut output = String::with_capacity(step.len() + guard.len() + 24);
        output.push_str(&step[..first_line_end]);
        if newline.is_empty() {
            output.push('\n');
        }
        output.push_str("        if: ");
        output.push_str(&wrapped_guard(guard));
        output.push_str(insertion_line_ending);
        output.push_str(&step[first_line_end..]);
        return Ok(output);
    };

    let line = &step[condition.line_start..condition.line_end];
    let Some((expression, suffix)) = parsed_condition else {
        return Err(StepBlockError::new(
            "top-level if condition could not be parsed",
        ));
    };
    let key_prefix = &line[..condition.value_start - condition.line_start];
    let composed = format!(
        "{}{}{}{}{}",
        key_prefix,
        " ",
        wrapped_composed_guard(guard, &expression),
        suffix,
        &line[condition.value_end - condition.line_start..],
    );

    let mut output = String::with_capacity(step.len() + composed.len());
    output.push_str(&step[..condition.line_start]);
    output.push_str(&composed);
    output.push_str(&step[condition.line_end..]);
    Ok(output)
}

fn step_starts(block: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut line_start = 0;
    while line_start < block.len() {
        let line_end = next_line_end(block, line_start);
        if is_step_start(&block[line_start..line_end]) {
            starts.push(line_start);
        }
        line_start = line_end;
    }
    starts
}

fn is_step_start(line: &str) -> bool {
    let content = without_line_ending(line);
    content == "      -" || content.starts_with("      - ")
}

fn condition_location(
    line_start: usize,
    line_end: usize,
    line: &str,
    first_line: bool,
) -> Option<ConditionLocation> {
    let content_end = line_end - line_ending(line).len();
    let content = &line[..content_end - line_start];
    let rest = if first_line && content.starts_with("      - ") {
        &content[8..]
    } else {
        content.strip_prefix("        ")?
    };
    if !rest.starts_with("if:") {
        return None;
    }
    let after_key = &rest[3..];
    if after_key
        .chars()
        .next()
        .is_some_and(|character| !character.is_whitespace())
    {
        return None;
    }
    Some(ConditionLocation {
        line_start,
        line_end,
        value_start: line_start + 8 + 3,
        value_end: line_start + content_end - line_start,
    })
}

fn parse_condition(raw: &str) -> Result<(String, String), StepBlockError> {
    // The renderer accepts only one-line scalar metadata.  Parsing happens even
    // when no new guard is requested: a malformed generated step must never be
    // allowed through merely because this composition pass is disabled.
    if raw.contains('\n') || raw.contains('\r') {
        return Err(StepBlockError::new(
            "top-level if condition must be a single-line scalar",
        ));
    }

    let value = raw.trim();
    if value.is_empty() {
        return Err(StepBlockError::new(
            "top-level if condition must not be empty",
        ));
    }

    let (scalar, suffix) = split_inline_comment(value)?;
    let scalar = decode_scalar(scalar.trim())?;
    let value = scalar.trim();
    if value.is_empty() {
        return Err(StepBlockError::new(
            "top-level if expression must not be empty",
        ));
    }

    if let Some(inner) = value.strip_prefix("${{") {
        let Some(close) = find_expression_close(inner)? else {
            return Err(StepBlockError::new(
                "top-level if expression has no closing `}}`",
            ));
        };
        let expression = inner[..close].trim();
        if expression.is_empty() {
            return Err(StepBlockError::new(
                "top-level if expression must not be empty",
            ));
        }
        if !inner[close + 2..].trim().is_empty() {
            return Err(StepBlockError::new(
                "top-level if expression has unsupported trailing text",
            ));
        }
        return Ok((expression.to_owned(), suffix.to_owned()));
    }

    if is_unsupported_scalar_form(value) {
        return Err(StepBlockError::new(
            "top-level if condition uses an unsupported YAML scalar form",
        ));
    }
    if value.trim().is_empty() {
        return Err(StepBlockError::new(
            "top-level if expression must not be empty",
        ));
    }
    Ok((value.to_owned(), suffix.to_owned()))
}

fn split_inline_comment(value: &str) -> Result<(&str, &str), StepBlockError> {
    let bytes = value.as_bytes();
    let mut quote = None;
    let mut index = 0;
    while index < bytes.len() {
        match quote {
            Some(b'\'') => {
                if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                    } else {
                        quote = None;
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            Some(b'"') => {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == b'"' {
                    quote = None;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            Some(_) => index += 1,
            None => {
                if bytes[index] == b'\'' || bytes[index] == b'"' {
                    quote = Some(bytes[index]);
                    index += 1;
                } else if bytes[index] == b'#'
                    && (index == 0 || bytes[index - 1].is_ascii_whitespace())
                {
                    let mut comment_start = index;
                    while comment_start > 0 && bytes[comment_start - 1].is_ascii_whitespace() {
                        comment_start -= 1;
                    }
                    return Ok((&value[..comment_start], &value[comment_start..]));
                } else {
                    index += 1;
                }
            }
        }
    }
    if quote.is_some() {
        return Err(StepBlockError::new(
            "top-level if condition has an unterminated quote",
        ));
    }
    Ok((value, ""))
}

fn decode_scalar(value: &str) -> Result<String, StepBlockError> {
    if value.is_empty() {
        return Ok(String::new());
    }
    if value
        .as_bytes()
        .first()
        .is_some_and(|character| matches!(character, b'|' | b'>'))
    {
        return Err(StepBlockError::new(
            "top-level if condition does not support block scalars",
        ));
    }
    if value
        .as_bytes()
        .first()
        .is_some_and(|character| matches!(character, b'[' | b'{' | b'&' | b'*'))
    {
        return Err(StepBlockError::new(
            "top-level if condition does not support collections or aliases",
        ));
    }
    if value.starts_with('\'') || value.starts_with('"') {
        let decoded = serde_yaml::from_str::<String>(value).map_err(|error| {
            StepBlockError::new(format!(
                "top-level if condition has invalid quoted scalar: {error}"
            ))
        })?;
        if decoded.contains('\n') || decoded.contains('\r') {
            return Err(StepBlockError::new(
                "top-level if condition must be a single-line scalar",
            ));
        }
        return Ok(decoded);
    }
    Ok(value.to_owned())
}

fn is_unsupported_scalar_form(value: &str) -> bool {
    let bytes = value.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    if matches!(
        first,
        b'[' | b'{' | b'|' | b'>' | b'&' | b'*' | b'!' | b'%' | b'@' | b'`'
    ) {
        return true;
    }
    matches!(first, b'-' | b'?' | b':') && bytes.get(1).is_some_and(u8::is_ascii_whitespace)
}

fn find_expression_close(inner: &str) -> Result<Option<usize>, StepBlockError> {
    let bytes = inner.as_bytes();
    let mut quote = None;
    let mut index = 0;
    while index < bytes.len() {
        match quote {
            Some(b'\'') => {
                if bytes[index] == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 2;
                    } else {
                        quote = None;
                        index += 1;
                    }
                } else {
                    index += 1;
                }
            }
            Some(b'"') => {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == b'"' {
                    quote = None;
                    index += 1;
                } else {
                    index += 1;
                }
            }
            Some(_) => index += 1,
            None => {
                if bytes[index] == b'\'' || bytes[index] == b'"' {
                    quote = Some(bytes[index]);
                    index += 1;
                } else if bytes[index] == b'}' && bytes.get(index + 1) == Some(&b'}') {
                    return Ok(Some(index));
                } else {
                    index += 1;
                }
            }
        }
    }
    if quote.is_some() {
        return Err(StepBlockError::new(
            "top-level if expression has an unterminated quote",
        ));
    }
    Ok(None)
}

fn wrapped_guard(guard: &str) -> String {
    format!("${{{{ ({guard}) }}}}")
}

fn wrapped_composed_guard(guard: &str, existing: &str) -> String {
    format!("${{{{ ({guard}) && ({existing}) }}}}")
}

fn next_line_end(text: &str, start: usize) -> usize {
    text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset + 1)
}

fn without_line_ending(line: &str) -> &str {
    line.strip_suffix('\n')
        .unwrap_or(line)
        .strip_suffix('\r')
        .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line))
}

fn line_ending(line: &str) -> &str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::prefix_step_block_with_if;

    #[expect(clippy::panic, reason = "fixture setup failure must name its cause")]
    fn must_render(result: Result<String, super::StepBlockError>, context: &str) -> String {
        match result {
            Ok(rendered) => rendered,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[test]
    fn composes_metadata_in_any_order_and_preserves_nested_if_text() {
        let block = "      - name: first\n        id: first\n        run: |\n          echo 'if: body'\n          if: body\n        if: ${{ foo || bar }} # keep\n      - uses: example/action@v1\n        with:\n          script: if: nested\n";
        let rendered = must_render(
            prefix_step_block_with_if(block, Some("inputs.member != ''")),
            "condition composition",
        );
        assert!(
            rendered.contains("        if: ${{ (inputs.member != '') && (foo || bar) }} # keep\n")
        );
        assert!(rendered.contains("          echo 'if: body'\n          if: body\n"));
        assert!(rendered.contains(
            "      - uses: example/action@v1\n        if: ${{ (inputs.member != '') }}\n"
        ));
    }

    #[test]
    fn rejects_duplicate_top_level_conditions_but_ignores_nested_text() {
        let duplicate =
            "      - name: duplicate\n        if: foo\n        id: step\n        if: bar\n";
        assert!(prefix_step_block_with_if(duplicate, Some("guard")).is_err());

        let nested = "      - name: nested\n        run: |\n          if: one\n          if: two\n";
        assert!(prefix_step_block_with_if(nested, Some("guard")).is_ok());
    }

    #[test]
    fn empty_and_no_guard_are_safe() {
        let block = "# preamble\n\n      - name: one\n        run: echo ok\n";
        assert_eq!(
            must_render(prefix_step_block_with_if(block, None), "no guard"),
            block
        );
        assert_eq!(
            must_render(
                prefix_step_block_with_if("# only comments\n", Some("guard")),
                "comment-only block",
            ),
            "# only comments\n"
        );
        assert!(prefix_step_block_with_if(block, Some(" ")).is_err());

        let no_trailing_newline = must_render(
            prefix_step_block_with_if("      - name: one", Some("guard")),
            "single-line step",
        );
        assert_eq!(
            no_trailing_newline,
            "      - name: one\n        if: ${{ (guard) }}\n"
        );
    }

    #[test]
    fn preserves_crlf_and_condition_comments() {
        let block = "      - name: one\r\n        if: ready # comment\r\n        run: echo ok\r\n";
        let rendered = must_render(
            prefix_step_block_with_if(block, Some("guard")),
            "CRLF condition",
        );
        assert!(rendered.contains("if: ${{ (guard) && (ready) }} # comment\r\n"));
        assert!(rendered.ends_with("run: echo ok\r\n"));
    }

    #[test]
    fn quoted_hash_is_not_an_inline_yaml_comment() {
        let hash = "      - name: quoted hash\n        if: contains(inputs.value, ' # marker')\n        run: true\n";
        let rendered = must_render(
            prefix_step_block_with_if(hash, Some("guard")),
            "quoted hash",
        );
        assert!(rendered.contains("(contains(inputs.value, ' # marker'))"));
    }

    #[test]
    fn quoted_expression_braces_are_not_expression_terminators() {
        let braces = "      - name: quoted braces\n        if: ${{ contains(inputs.value, '}}') }}\n        run: true\n";
        let rendered = must_render(
            prefix_step_block_with_if(braces, Some("guard")),
            "quoted braces",
        );
        assert!(rendered.contains("(contains(inputs.value, '}}'))"));
    }

    #[test]
    fn supports_quoted_yaml_scalars_and_rejects_unsupported_forms() {
        let double_quoted = "      - name: double quoted\n        if: \"contains(inputs.value, ' # marker')\"\n        run: true\n";
        let rendered = must_render(
            prefix_step_block_with_if(double_quoted, Some("guard")),
            "double quoted scalar",
        );
        assert!(rendered.contains("(contains(inputs.value, ' # marker'))"));

        let single_quoted = "      - name: single quoted\n        if: '${{ contains(inputs.value, ''marker'') }}'\n        run: true\n";
        let rendered = must_render(
            prefix_step_block_with_if(single_quoted, Some("guard")),
            "single quoted scalar",
        );
        assert!(rendered.contains("(contains(inputs.value, 'marker'))"));

        for scalar in ["|", ">", "[ready]", "{ready: true}", "!tag"] {
            let block =
                format!("      - name: unsupported\n        if: {scalar}\n        run: true\n");
            assert!(
                prefix_step_block_with_if(&block, Some("guard")).is_err(),
                "scalar form should be rejected: {scalar}"
            );
        }
    }

    #[test]
    fn malformed_condition_is_rejected_without_a_new_guard() {
        let malformed = "      - name: invalid\n        if: ${{\n        run: true\n";
        assert!(prefix_step_block_with_if(malformed, None).is_err());
    }

    #[test]
    fn unterminated_quoted_condition_is_rejected_without_a_new_guard() {
        let unterminated = "      - name: invalid\n        if: \"ready\n        run: true\n";
        assert!(prefix_step_block_with_if(unterminated, None).is_err());
    }
}

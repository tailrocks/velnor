#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum IncludeString {
    Relative(String),
    ManifestDir(String),
}

impl IncludeString {
    fn from_static(expression: StaticString) -> Self {
        if expression.manifest_dir {
            Self::ManifestDir(expression.value)
        } else {
            Self::Relative(expression.value)
        }
    }

    pub(crate) fn display(&self) -> &str {
        match self {
            Self::Relative(value) | Self::ManifestDir(value) => value,
        }
    }
}

#[derive(Default)]
struct StaticString {
    manifest_dir: bool,
    value: String,
}

/// Parse static path expressions accepted by Rust include macros. This
/// handles only literals, concat!, and `env!("CARGO_MANIFEST_DIR")`.
pub(crate) fn parse_include_paths(source: &str) -> Result<Vec<IncludeString>, String> {
    const MACROS: &[&str] = &["include_str!", "include_bytes!"];
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut included = Vec::new();
    while cursor < bytes.len() {
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'/') {
            cursor = skip_line_comment(bytes, cursor + 2);
            continue;
        }
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            cursor = skip_block_comment(bytes, cursor + 2)?;
            continue;
        }
        if let Some(end) = skip_raw_string(bytes, cursor)? {
            cursor = end;
            continue;
        }
        if bytes[cursor] == b'"' {
            cursor = skip_quoted_literal(bytes, cursor, b'"')
                .map_err(|error| format!("{error} at byte {cursor}"))?;
            continue;
        }
        if bytes[cursor] == b'\'' && is_char_literal_start(bytes, cursor) {
            cursor = skip_quoted_literal(bytes, cursor, b'\'')
                .map_err(|error| format!("{error} at byte {cursor}"))?;
            continue;
        }

        let Some(macro_name) = MACROS.iter().find(|name| {
            bytes[cursor..].starts_with(name.as_bytes())
                && (cursor == 0 || !is_rust_identifier_byte(bytes[cursor - 1]))
        }) else {
            cursor += 1;
            continue;
        };
        let mut argument = cursor + macro_name.len();
        argument = skip_expression_trivia(bytes, argument)?;
        if bytes.get(argument) != Some(&b'(') {
            cursor += macro_name.len();
            continue;
        }
        argument += 1;
        let (expression, mut close) =
            parse_static_string_expression(source, bytes, argument, macro_name)?;
        close = skip_expression_trivia(bytes, close)?;
        if bytes.get(close) != Some(&b')') {
            return Err(format!("unterminated {macro_name}"));
        }
        included.push(IncludeString::from_static(expression));
        cursor = close + 1;
    }
    Ok(included)
}

#[cfg(test)]
pub(crate) fn parse_include_str_literals(source: &str) -> Result<Vec<String>, String> {
    parse_include_paths(source)?
        .into_iter()
        .map(|path| match path {
            IncludeString::Relative(value) => Ok(value),
            IncludeString::ManifestDir(_) => Err(
                "include_str! CARGO_MANIFEST_DIR expression requires repository context".to_owned(),
            ),
        })
        .collect()
}

fn parse_static_string_expression(
    source: &str,
    bytes: &[u8],
    mut cursor: usize,
    macro_name: &str,
) -> Result<(StaticString, usize), String> {
    cursor = skip_expression_trivia(bytes, cursor)?;
    match bytes.get(cursor) {
        Some(b'"') => {
            let end = skip_quoted_literal(bytes, cursor, b'"')
                .map_err(|error| format!("{error} at byte {cursor}"))?;
            let value = decode_rust_string_literal(source, cursor, end).map_err(|error| {
                format!("{macro_name} must use a static string expression: {error}")
            })?;
            Ok((
                StaticString {
                    value,
                    ..Default::default()
                },
                end,
            ))
        }
        Some(b'r') => parse_raw_string_expression(source, bytes, cursor)?
            .ok_or_else(|| format!("{macro_name} must use a static string expression")),
        Some(byte) if is_rust_identifier_byte(*byte) && !byte.is_ascii_digit() => {
            let name_start = cursor;
            cursor += 1;
            while bytes
                .get(cursor)
                .is_some_and(|byte| is_rust_identifier_byte(*byte))
            {
                cursor += 1;
            }
            let name = &source[name_start..cursor];
            cursor = skip_expression_trivia(bytes, cursor)?;
            if bytes.get(cursor) != Some(&b'!') {
                return Err(format!("{macro_name} must use a static string expression"));
            }
            cursor = skip_expression_trivia(bytes, cursor + 1)?;
            if bytes.get(cursor) != Some(&b'(') {
                return Err(format!("{name}! must have a parenthesized argument"));
            }
            cursor += 1;
            match name {
                "concat" => parse_concat_expression(source, bytes, cursor, macro_name),
                "env" => parse_env_expression(source, bytes, cursor, macro_name),
                _ => Err(format!(
                    "{macro_name} macro {name}! is not a supported static expression"
                )),
            }
        }
        _ => Err(format!("{macro_name} must use a static string expression")),
    }
}

fn parse_raw_string_expression(
    source: &str,
    bytes: &[u8],
    cursor: usize,
) -> Result<Option<(StaticString, usize)>, String> {
    let Some((hash_start, content_start)) = raw_string_opening(bytes, cursor) else {
        return Ok(None);
    };
    let hashes = content_start - hash_start;
    let Some(end) = find_raw_string_end(bytes, content_start, hashes) else {
        return Err("unterminated raw string literal".to_owned());
    };
    let value = source[content_start + 1..end - hashes - 1].to_owned();
    Ok(Some((
        StaticString {
            value,
            ..Default::default()
        },
        end,
    )))
}

fn parse_concat_expression(
    source: &str,
    bytes: &[u8],
    mut cursor: usize,
    macro_name: &str,
) -> Result<(StaticString, usize), String> {
    let mut combined = StaticString::default();
    cursor = skip_expression_trivia(bytes, cursor)?;
    if bytes.get(cursor) == Some(&b')') {
        return Ok((combined, cursor + 1));
    }
    loop {
        let (part, next) = parse_static_string_expression(source, bytes, cursor, macro_name)?;
        combine_static_strings(&mut combined, &part, macro_name)?;
        cursor = skip_expression_trivia(bytes, next)?;
        match bytes.get(cursor) {
            Some(b',') => {
                cursor = skip_expression_trivia(bytes, cursor + 1)?;
                if bytes.get(cursor) == Some(&b')') {
                    return Ok((combined, cursor + 1));
                }
            }
            Some(b')') => return Ok((combined, cursor + 1)),
            _ => return Err(format!("unterminated concat! expression in {macro_name}")),
        }
    }
}

fn parse_env_expression(
    source: &str,
    bytes: &[u8],
    mut cursor: usize,
    macro_name: &str,
) -> Result<(StaticString, usize), String> {
    let (name, next) = parse_static_string_expression(source, bytes, cursor, macro_name)?;
    cursor = skip_expression_trivia(bytes, next)?;
    if bytes.get(cursor) != Some(&b')') {
        return Err("env! must use exactly one string literal argument".to_owned());
    }
    if name.manifest_dir || name.value != "CARGO_MANIFEST_DIR" {
        return Err(format!(
            "{macro_name} only resolves env!(\"CARGO_MANIFEST_DIR\")"
        ));
    }
    Ok((
        StaticString {
            manifest_dir: true,
            value: String::new(),
        },
        cursor + 1,
    ))
}

fn combine_static_strings(
    left: &mut StaticString,
    right: &StaticString,
    macro_name: &str,
) -> Result<(), String> {
    if right.manifest_dir {
        if left.manifest_dir || !left.value.is_empty() {
            return Err(format!(
                "{macro_name} CARGO_MANIFEST_DIR must be the first concat! expression"
            ));
        }
        left.manifest_dir = true;
    }
    left.value.push_str(&right.value);
    Ok(())
}

fn decode_rust_string_literal(source: &str, start: usize, end: usize) -> Result<String, String> {
    let bytes = source.as_bytes();
    let content_end = end
        .checked_sub(1)
        .ok_or_else(|| "invalid string literal bounds".to_owned())?;
    let mut cursor = start + 1;
    let mut value = String::new();
    while cursor < content_end {
        if bytes[cursor] != b'\\' {
            let character = source[cursor..content_end]
                .chars()
                .next()
                .ok_or_else(|| "invalid UTF-8 string literal".to_owned())?;
            value.push(character);
            cursor += character.len_utf8();
            continue;
        }
        cursor += 1;
        let escape = *bytes
            .get(cursor)
            .ok_or_else(|| "unterminated string escape".to_owned())?;
        cursor += 1;
        match escape {
            b'\\' => value.push('\\'),
            b'"' => value.push('"'),
            b'\'' => value.push('\''),
            b'n' => value.push('\n'),
            b'r' => value.push('\r'),
            b't' => value.push('\t'),
            b'0' => value.push('\0'),
            b'x' => {
                let first =
                    hex_value(*bytes.get(cursor).ok_or_else(|| {
                        "\\x escape must contain two hexadecimal digits".to_owned()
                    })?)?;
                let second =
                    hex_value(*bytes.get(cursor + 1).ok_or_else(|| {
                        "\\x escape must contain two hexadecimal digits".to_owned()
                    })?)?;
                let byte = first * 16 + second;
                if byte > 0x7f {
                    return Err("\\x escapes must be ASCII".to_owned());
                }
                value.push(char::from(byte));
                cursor += 2;
            }
            b'u' => {
                let (character, next) = decode_unicode_escape(bytes, cursor)?;
                value.push(character);
                cursor = next;
            }
            b'\n' => {
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
            }
            b'\r' => {
                if bytes.get(cursor) == Some(&b'\n') {
                    cursor += 1;
                }
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
            }
            _ => return Err(format!("unsupported Rust string escape \\{escape}")),
        }
    }
    Ok(value)
}

fn decode_unicode_escape(bytes: &[u8], mut cursor: usize) -> Result<(char, usize), String> {
    if bytes.get(cursor) != Some(&b'{') {
        return Err("\\u escape must use braces".to_owned());
    }
    cursor += 1;
    let start = cursor;
    let mut value = 0_u32;
    while let Some(byte) = bytes.get(cursor) {
        if *byte == b'}' {
            let digits = cursor - start;
            if !(1..=6).contains(&digits) {
                return Err("\\u escape must contain one to six hexadecimal digits".to_owned());
            }
            let character = char::from_u32(value)
                .ok_or_else(|| "\\u escape is not a Unicode scalar value".to_owned())?;
            return Ok((character, cursor + 1));
        }
        value = value
            .checked_mul(16)
            .and_then(|value| hex_value(*byte).ok().map(|digit| value + u32::from(digit)))
            .ok_or_else(|| "\\u escape must contain hexadecimal digits".to_owned())?;
        cursor += 1;
    }
    Err("unterminated \\u escape".to_owned())
}

fn hex_value(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("expected hexadecimal digit".to_owned()),
    }
}

fn skip_expression_trivia(bytes: &[u8], mut cursor: usize) -> Result<usize, String> {
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) == Some(&b'/') && bytes.get(cursor + 1) == Some(&b'/') {
            cursor = skip_line_comment(bytes, cursor + 2);
        } else if bytes.get(cursor) == Some(&b'/') && bytes.get(cursor + 1) == Some(&b'*') {
            cursor = skip_block_comment(bytes, cursor + 2)?;
        } else {
            return Ok(cursor);
        }
    }
}

fn is_rust_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_line_comment(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() && bytes[cursor] != b'\n' {
        cursor += 1;
    }
    cursor
}

fn skip_block_comment(bytes: &[u8], mut cursor: usize) -> Result<usize, String> {
    let mut depth = 1;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'/' && bytes[cursor + 1] == b'*' {
            depth += 1;
            cursor += 2;
        } else if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return Ok(cursor);
            }
        } else {
            cursor += 1;
        }
    }
    Err("unterminated block comment".to_owned())
}

fn raw_string_opening(bytes: &[u8], cursor: usize) -> Option<(usize, usize)> {
    let (hash_start, content_start) = match bytes.get(cursor..) {
        Some([b'r', rest @ ..]) => (
            cursor + 1,
            cursor + 1 + rest.iter().take_while(|byte| **byte == b'#').count(),
        ),
        Some([b'b', b'r', rest @ ..]) => (
            cursor + 2,
            cursor + 2 + rest.iter().take_while(|byte| **byte == b'#').count(),
        ),
        _ => return None,
    };
    (bytes.get(content_start) == Some(&b'"')).then_some((hash_start, content_start))
}

fn find_raw_string_end(bytes: &[u8], content_start: usize, hashes: usize) -> Option<usize> {
    let mut end = content_start + 1;
    while end < bytes.len() {
        if bytes[end] == b'"'
            && bytes
                .get(end + 1..end + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return Some(end + 1 + hashes);
        }
        end += 1;
    }
    None
}

fn skip_raw_string(bytes: &[u8], cursor: usize) -> Result<Option<usize>, String> {
    let Some((hash_start, content_start)) = raw_string_opening(bytes, cursor) else {
        return Ok(None);
    };
    let hashes = content_start - hash_start;
    find_raw_string_end(bytes, content_start, hashes)
        .map(Some)
        .ok_or_else(|| "unterminated raw string literal".to_owned())
}

fn is_char_literal_start(bytes: &[u8], cursor: usize) -> bool {
    match bytes.get(cursor + 1) {
        Some(b'\\') => true,
        Some(byte) if *byte != b'\'' && *byte != b'\n' => bytes.get(cursor + 2) == Some(&b'\''),
        _ => false,
    }
}

fn skip_quoted_literal(bytes: &[u8], mut cursor: usize, delimiter: u8) -> Result<usize, String> {
    cursor += 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = cursor.saturating_add(2),
            character if character == delimiter => return Ok(cursor + 1),
            b'\n' if delimiter == b'\'' => {
                return Err(format!("unterminated quoted literal at byte {cursor}"));
            }
            _ => cursor += 1,
        }
    }
    Err(format!("unterminated quoted literal at byte {cursor}"))
}

#[cfg(test)]
mod tests {
    use super::{parse_include_paths, IncludeString};

    #[test]
    fn parses_manifest_dir_concat_with_suffix() {
        assert_eq!(
            parse_include_paths(
                "include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/lib.rs\"));"
            )
            .ok(),
            Some(vec![IncludeString::ManifestDir("/src/lib.rs".to_owned())])
        );
    }

    #[test]
    fn parses_include_bytes_and_rust_escapes() {
        assert_eq!(
            parse_include_paths("include_bytes!(concat!(\"assets\\\\\", \"font\\x2Ettf\"));").ok(),
            Some(vec![IncludeString::Relative("assets\\font.ttf".to_owned())])
        );
    }

    #[test]
    fn rejects_dynamic_and_unterminated_input() {
        for source in [
            "include_str!(PATH);",
            "/* include_str!(\"x\")",
            "include_str!(r#\"x);",
        ] {
            let result = parse_include_paths(source);
            assert!(result.is_err(), "invalid source was accepted: {source}");
        }
    }
}

//! Ordered JSON reader for the Actions runner's Newtonsoft wire boundaries.
//!
//! `serde_json::Value` loses duplicate object members and cannot represent
//! Newtonsoft's BigInteger, nonfinite Float, hex, or octal reader tokens.
//! Keep source order and numeric spelling until the owning converter applies
//! its CLR-specific projection.

use regex::Regex;
use serde_json::{Number, Value};
use std::sync::OnceLock;
use url::Url;
use velnor_model::NonFinite;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonReaderOrigin {
    /// A token read directly by `JsonTextReader` from the response body.
    TextReader,
    /// A token replayed through a reader created from an already-loaded JObject.
    JObjectReader,
}

#[derive(Debug, Clone)]
pub(crate) struct JsonNumber {
    pub(crate) kind: JsonNumberKind,
    /// Exact spelling from the source reader, including octal/hex prefixes.
    pub(crate) lexeme: String,
    pub(crate) origin: JsonReaderOrigin,
}

#[derive(Debug, Clone)]
pub(crate) enum JsonNumberKind {
    Int64(i64),
    /// Canonical decimal spelling, outside signed Int64.
    BigInteger(String),
    Float(f64),
}

impl JsonNumber {
    pub(crate) fn as_i64(&self) -> Option<i64> {
        match &self.kind {
            JsonNumberKind::Int64(value) => Some(*value),
            JsonNumberKind::BigInteger(_) | JsonNumberKind::Float(_) => None,
        }
    }

    pub(crate) fn is_float(&self) -> bool {
        matches!(&self.kind, JsonNumberKind::Float(_))
    }

    pub(crate) fn big_integer_decimal(&self) -> Option<&str> {
        match &self.kind {
            JsonNumberKind::BigInteger(value) => Some(value),
            JsonNumberKind::Int64(_) | JsonNumberKind::Float(_) => None,
        }
    }

    pub(crate) fn to_serde_number(&self) -> Option<Number> {
        match &self.kind {
            JsonNumberKind::Int64(value) => Some((*value).into()),
            JsonNumberKind::BigInteger(_) => None,
            JsonNumberKind::Float(value) => Number::from_f64(*value),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum OrderedJsonValue {
    Null,
    Bool(bool),
    Number(JsonNumber),
    NonFinite {
        value: NonFinite,
        origin: JsonReaderOrigin,
        /// Original numeric token text. TextReader string coercion returns
        /// this spelling; JObjectReader formats the parsed value instead.
        lexeme: String,
    },
    String(String),
    Undefined,
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
    Constructor {
        name: String,
        arguments: Vec<Self>,
    },
}

impl OrderedJsonValue {
    /// Mark numeric children as replayed through a reader over a JObject.
    /// JObject.Load preserves JToken kinds while changing how downstream
    /// serializer conversions observe them.
    pub(crate) fn set_number_origin(&mut self, origin: JsonReaderOrigin) {
        match self {
            Self::Number(number) => number.origin = origin,
            Self::NonFinite {
                origin: number_origin,
                ..
            } => *number_origin = origin,
            Self::Array(values) => {
                for value in values {
                    value.set_number_origin(origin);
                }
            }
            Self::Object(values) => {
                for (_, value) in values {
                    value.set_number_origin(origin);
                }
            }
            Self::Constructor { arguments, .. } => {
                for argument in arguments {
                    argument.set_number_origin(origin);
                }
            }
            Self::Null | Self::Undefined | Self::Bool(_) | Self::String(_) => {}
        }
    }
}

/// Parse Newtonsoft's response-body number grammar without converting
/// nonfinite values through ordinary strings or losing duplicate members.
pub(crate) fn parse_json_text(body: &str) -> Result<OrderedJsonValue, serde_json::Error> {
    ClrJsonParser::new(body).parse()
}

/// Convert a canonical BigInteger decimal string using .NET's explicit
/// `BigInteger`→`Double` cast. The CLR discards low magnitude bits instead of
/// rounding the decimal spelling to nearest, so parsing the string as f64 is
/// observably different.
pub(crate) fn clr_big_integer_to_f64(decimal: &str) -> Option<f64> {
    let (negative, digits) = match decimal.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, decimal),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let mut limbs = Vec::<u32>::new();
    for byte in digits.bytes() {
        let mut carry = u64::from(byte - b'0');
        for limb in &mut limbs {
            let value = u64::from(*limb) * 10 + carry;
            *limb = value as u32;
            carry = value >> 32;
        }
        if carry != 0 {
            limbs.push(carry as u32);
        }
        if limbs.is_empty() {
            limbs.push(u32::from(byte - b'0'));
        }
    }

    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    if limbs.is_empty() {
        return Some(if negative { -0.0 } else { 0.0 });
    }

    let high = *limbs.last()?;
    let bit_length = (limbs.len() - 1) * 32 + (32 - high.leading_zeros() as usize);
    if bit_length > 1024 {
        return Some(if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }

    let discarded_bits = bit_length.saturating_sub(53);
    let mut significand = 0_u64;
    for bit in (discarded_bits..bit_length).rev() {
        let word = bit / 32;
        let offset = bit % 32;
        significand = (significand << 1) | u64::from((limbs[word] >> offset) & 1);
    }
    if bit_length < 53 {
        significand <<= 53 - bit_length;
    }

    let exponent = bit_length - 1;
    let exponent_bits = u64::try_from(exponent + 1023).ok()?;
    let fraction = significand & ((1_u64 << 52) - 1);
    let sign = u64::from(negative) << 63;
    Some(f64::from_bits(sign | (exponent_bits << 52) | fraction))
}

/// Parse Json.NET's invariant `ReadAsDouble` string values, including its
/// accepted special names and permissive grouping separators.
pub(crate) fn parse_clr_double_text(value: &str) -> Option<f64> {
    if let Some(value) = parse_special_double(value.trim()) {
        return Some(value);
    }
    parse_clr_float_text(trim_ascii_whitespace(value), true, true)
}

fn parse_special_double(value: &str) -> Option<f64> {
    let (negative, unsigned) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    if unsigned.eq_ignore_ascii_case("nan") {
        Some(f64::NAN)
    } else if unsigned.eq_ignore_ascii_case("infinity") {
        Some(if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        })
    } else {
        None
    }
}

fn trim_ascii_whitespace(value: &str) -> &str {
    value.trim_matches(|character: char| character.is_ascii_whitespace())
}

pub(crate) fn non_finite_text(value: NonFinite) -> &'static str {
    match value {
        NonFinite::NaN => "NaN",
        NonFinite::PositiveInfinity => "Infinity",
        NonFinite::NegativeInfinity => "-Infinity",
    }
}

/// Match invariant `Convert.ToString(double)` / Newtonsoft JValue formatting:
/// shortest round-trip digits, fixed notation for exponents -4 through 16,
/// and uppercase scientific notation with a signed, two-digit exponent
/// outside that range.
pub(crate) fn dotnet_double_general(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0".to_owned()
        } else {
            "0".to_owned()
        };
    }

    let mut shortest = format!("{:?}", value.abs());
    let explicit_exponent =
        if let Some(index) = shortest.find(|character| matches!(character, 'e' | 'E')) {
            let exponent = shortest[index + 1..]
                .parse::<i32>()
                .expect("Rust emitted a valid exponent");
            shortest.truncate(index);
            exponent
        } else {
            0
        };
    let decimal_index = shortest.find('.').unwrap_or(shortest.len());
    let mut digits: String = shortest
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let leading_zeroes = digits.bytes().take_while(|digit| *digit == b'0').count();
    digits.drain(..leading_zeroes);
    let exponent = decimal_index as i32 - leading_zeroes as i32 - 1 + explicit_exponent;
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }

    let sign = if value.is_sign_negative() { "-" } else { "" };
    if exponent < -4 || exponent >= 17 {
        let fraction = &digits[1..];
        let mantissa = if fraction.is_empty() {
            digits[..1].to_owned()
        } else {
            format!("{}.{}", &digits[..1], fraction)
        };
        let exponent_sign = if exponent < 0 { "-" } else { "+" };
        let exponent_magnitude = exponent.unsigned_abs();
        return format!("{sign}{mantissa}E{exponent_sign}{exponent_magnitude:02}");
    }

    let decimal_index = exponent + 1;
    let fixed = if decimal_index <= 0 {
        format!("0.{}{}", "0".repeat((-decimal_index) as usize), digits)
    } else if decimal_index as usize >= digits.len() {
        format!(
            "{}{}",
            digits,
            "0".repeat(decimal_index as usize - digits.len())
        )
    } else {
        let decimal_index = decimal_index as usize;
        format!("{}.{}", &digits[..decimal_index], &digits[decimal_index..])
    };
    format!("{sign}{fixed}")
}

/// Validate the runner's nullable `System.Uri(string, RelativeOrAbsolute)`
/// wire fields. Empty strings are accepted here; their nullable DTO projection
/// converts them to null before retaining the local `Option<String>` value.
pub(crate) fn is_clr_uri(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }

    // System.Uri rejects inputs at or above Uri.MaxUriBufferSize (65,520
    // UTF-16 code units). Rust strings count Unicode scalar values, so count
    // the UTF-16 representation used by the CLR.
    if value.encode_utf16().count() >= 65_520 {
        return false;
    }

    // WHATWG URL parsing treats backslashes as separators for HTTP(S), but
    // System.Uri rejects the runner-probed backslash authority form.
    if let Some((scheme, remainder)) = value.split_once(':') {
        if (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
            && remainder.starts_with("\\\\")
        {
            return false;
        }
    }

    Url::parse(value).is_ok()
        || Url::parse("http://velnor.invalid/")
            .and_then(|base| base.join(value))
            .is_ok()
}

struct ClrJsonParser<'a> {
    body: &'a str,
    offset: usize,
    depth: usize,
}

impl<'a> ClrJsonParser<'a> {
    fn new(body: &'a str) -> Self {
        Self {
            body,
            offset: 0,
            depth: 0,
        }
    }

    fn parse(mut self) -> Result<OrderedJsonValue, serde_json::Error> {
        let value = self.parse_value()?;
        self.skip_ignored()?;
        if self.offset != self.body.len() {
            return Err(self.syntax_error());
        }
        Ok(value)
    }

    fn parse_value(&mut self) -> Result<OrderedJsonValue, serde_json::Error> {
        if self.depth >= 128 {
            return Err(self.syntax_error());
        }
        self.depth += 1;
        let value = self.parse_value_inner();
        self.depth -= 1;
        value
    }

    fn parse_value_inner(&mut self) -> Result<OrderedJsonValue, serde_json::Error> {
        self.skip_ignored()?;
        match self.peek_byte() {
            Some(b'n') if self.body[self.offset..].starts_with("new") => self.parse_constructor(),
            Some(b'n') => self.parse_keyword(b"null", OrderedJsonValue::Null),
            Some(b't') => self.parse_keyword(b"true", OrderedJsonValue::Bool(true)),
            Some(b'f') => self.parse_keyword(b"false", OrderedJsonValue::Bool(false)),
            Some(b'u') => self.parse_keyword(b"undefined", OrderedJsonValue::Undefined),
            Some(b'"' | b'\'') => self.parse_string().map(OrderedJsonValue::String),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'N') => self.parse_nonfinite("NaN", NonFinite::NaN),
            Some(b'I') => self.parse_nonfinite("Infinity", NonFinite::PositiveInfinity),
            Some(b'-') if self.body[self.offset..].starts_with("-Infinity") => {
                self.parse_nonfinite("-Infinity", NonFinite::NegativeInfinity)
            }
            Some(b'-' | b'0'..=b'9' | b'.') => self.parse_number(),
            _ => Err(self.syntax_error()),
        }
    }

    fn parse_object(&mut self) -> Result<OrderedJsonValue, serde_json::Error> {
        self.expect_byte(b'{')?;
        self.skip_ignored()?;
        if self.consume_byte(b'}') {
            return Ok(OrderedJsonValue::Object(Vec::new()));
        }

        let mut entries = Vec::new();
        loop {
            self.skip_ignored()?;
            let name = if matches!(self.peek_byte(), Some(b'"' | b'\'')) {
                self.parse_string()?
            } else {
                self.parse_unquoted_property_name()?
            };
            // JsonTextReader does not treat comments as ignorable between a
            // property name and its colon, though whitespace is accepted.
            self.skip_clr_whitespace();
            self.expect_byte(b':')?;
            let value = self.parse_value()?;
            entries.push((name, value));
            self.skip_ignored()?;
            if self.consume_byte(b'}') {
                return Ok(OrderedJsonValue::Object(entries));
            }
            self.expect_byte(b',')?;
            self.skip_ignored()?;
            if self.consume_byte(b'}') {
                return Ok(OrderedJsonValue::Object(entries));
            }
        }
    }

    fn parse_array(&mut self) -> Result<OrderedJsonValue, serde_json::Error> {
        self.expect_byte(b'[')?;
        self.skip_ignored()?;
        if self.consume_byte(b']') {
            return Ok(OrderedJsonValue::Array(Vec::new()));
        }

        let mut values = Vec::new();
        loop {
            self.skip_ignored()?;
            if self.consume_byte(b',') {
                values.push(OrderedJsonValue::Undefined);
                self.skip_ignored()?;
                if self.consume_byte(b']') {
                    return Ok(OrderedJsonValue::Array(values));
                }
                continue;
            }
            values.push(self.parse_value()?);
            self.skip_ignored()?;
            if self.consume_byte(b']') {
                return Ok(OrderedJsonValue::Array(values));
            }
            self.expect_byte(b',')?;
            self.skip_ignored()?;
            if self.consume_byte(b']') {
                return Ok(OrderedJsonValue::Array(values));
            }
        }
    }

    fn parse_constructor(&mut self) -> Result<OrderedJsonValue, serde_json::Error> {
        self.offset += "new".len();
        self.skip_clr_whitespace();
        let name = self.parse_unquoted_property_name()?;
        self.skip_ignored()?;
        self.expect_byte(b'(')?;
        self.skip_ignored()?;
        let mut arguments = Vec::new();
        if self.consume_byte(b')') {
            return Ok(OrderedJsonValue::Constructor { name, arguments });
        }

        loop {
            self.skip_ignored()?;
            if self.consume_byte(b',') {
                arguments.push(OrderedJsonValue::Undefined);
                self.skip_ignored()?;
                if self.consume_byte(b')') {
                    return Ok(OrderedJsonValue::Constructor { name, arguments });
                }
                continue;
            }
            arguments.push(self.parse_value()?);
            self.skip_ignored()?;
            if self.consume_byte(b')') {
                return Ok(OrderedJsonValue::Constructor { name, arguments });
            }
            self.expect_byte(b',')?;
            self.skip_ignored()?;
            if self.consume_byte(b')') {
                return Ok(OrderedJsonValue::Constructor { name, arguments });
            }
        }
    }

    fn parse_number(&mut self) -> Result<OrderedJsonValue, serde_json::Error> {
        let start = self.offset;
        while let Some(byte) = self.peek_byte() {
            if matches!(byte, b',' | b']' | b'}' | b')') {
                break;
            }
            let character = self.body[self.offset..]
                .chars()
                .next()
                .expect("offset is at a UTF-8 boundary");
            if character.is_whitespace() || character == '\0' {
                break;
            }
            if character == '/' && self.is_comment_start() {
                break;
            }
            self.offset += character.len_utf8();
        }
        let raw = &self.body[start..self.offset];

        if let Some(digits) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            let Some(value) = parse_radix_i64(digits, 16) else {
                return Err(self.syntax_error());
            };
            return Ok(self.integer(value, raw));
        }

        if raw.starts_with('0') && raw.len() > 1 {
            match raw.as_bytes()[1] {
                b'.' | b'e' | b'E' => return self.parse_float(raw),
                b'0'..=b'9' => {
                    let Some(value) = parse_radix_i64(&raw[1..], 8) else {
                        return Err(self.syntax_error());
                    };
                    return Ok(self.integer(value, raw));
                }
                _ => return Err(self.syntax_error()),
            }
        }

        if raw.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E')) {
            return self.parse_float(raw);
        }

        let canonical = canonical_decimal_integer(raw).ok_or_else(|| self.syntax_error())?;
        if let Ok(value) = canonical.parse::<i64>() {
            return Ok(self.integer(value, raw));
        }
        // JsonTextReader first attempts Int64, then applies its BigInteger
        // source-text bound. Thus arbitrarily many leading zeros that parse
        // as Int64 remain valid, while a 381-character overflow token fails.
        if raw.len() > 380 {
            return Err(self.syntax_error());
        }
        Ok(OrderedJsonValue::Number(JsonNumber {
            kind: JsonNumberKind::BigInteger(canonical),
            lexeme: raw.to_owned(),
            origin: JsonReaderOrigin::TextReader,
        }))
    }

    fn parse_float(&self, raw: &str) -> Result<OrderedJsonValue, serde_json::Error> {
        if raw.starts_with('+') {
            return Err(self.syntax_error());
        }
        let Some(value) = parse_clr_float_text(raw, false, false) else {
            return Err(self.syntax_error());
        };
        if !value.is_finite() {
            return Ok(OrderedJsonValue::NonFinite {
                value: if value.is_nan() {
                    NonFinite::NaN
                } else if value.is_sign_negative() {
                    NonFinite::NegativeInfinity
                } else {
                    NonFinite::PositiveInfinity
                },
                origin: JsonReaderOrigin::TextReader,
                lexeme: raw.to_owned(),
            });
        }
        Ok(OrderedJsonValue::Number(JsonNumber {
            kind: JsonNumberKind::Float(value),
            lexeme: raw.to_owned(),
            origin: JsonReaderOrigin::TextReader,
        }))
    }

    fn integer(&self, value: i64, lexeme: &str) -> OrderedJsonValue {
        OrderedJsonValue::Number(JsonNumber {
            kind: JsonNumberKind::Int64(value),
            lexeme: lexeme.to_owned(),
            origin: JsonReaderOrigin::TextReader,
        })
    }

    fn parse_keyword(
        &mut self,
        keyword: &[u8],
        value: OrderedJsonValue,
    ) -> Result<OrderedJsonValue, serde_json::Error> {
        if self.body.as_bytes()[self.offset..].starts_with(keyword) {
            self.offset += keyword.len();
            Ok(value)
        } else {
            Err(self.syntax_error())
        }
    }

    fn parse_nonfinite(
        &mut self,
        literal: &str,
        value: NonFinite,
    ) -> Result<OrderedJsonValue, serde_json::Error> {
        if self.body[self.offset..].starts_with(literal) {
            self.offset += literal.len();
            Ok(OrderedJsonValue::NonFinite {
                value,
                origin: JsonReaderOrigin::TextReader,
                lexeme: literal.to_owned(),
            })
        } else {
            Err(self.syntax_error())
        }
    }

    fn parse_unquoted_property_name(&mut self) -> Result<String, serde_json::Error> {
        let start = self.offset;
        while self.offset < self.body.len() {
            let character = self.body[self.offset..]
                .chars()
                .next()
                .expect("offset is at a UTF-8 boundary");
            if !is_dotnet_property_character(character) {
                break;
            }
            self.offset += character.len_utf8();
        }
        if self.offset == start {
            Err(self.syntax_error())
        } else {
            Ok(self.body[start..self.offset].to_owned())
        }
    }

    fn parse_string(&mut self) -> Result<String, serde_json::Error> {
        let quote = self.peek_byte().ok_or_else(|| self.syntax_error())?;
        if !matches!(quote, b'"' | b'\'') {
            return Err(self.syntax_error());
        }
        self.offset += 1;
        let mut value = String::new();
        while self.offset < self.body.len() {
            let character = self.body[self.offset..]
                .chars()
                .next()
                .expect("offset is at a UTF-8 boundary");
            self.offset += character.len_utf8();
            if character as u32 == u32::from(quote) {
                return Ok(value);
            }
            if character != '\\' {
                // JsonTextReader permits literal line breaks and control
                // characters inside quoted strings.
                value.push(character);
                continue;
            }

            let escaped = self.peek_byte().ok_or_else(|| self.syntax_error())?;
            self.offset += 1;
            match escaped {
                b'"' => value.push('"'),
                b'\'' => value.push('\''),
                b'/' => value.push('/'),
                b'\\' => value.push('\\'),
                b'b' => value.push('\u{0008}'),
                b'f' => value.push('\u{000c}'),
                b'n' => value.push('\n'),
                b'r' => value.push('\r'),
                b't' => value.push('\t'),
                b'u' => {
                    let first = self.parse_hex_code_unit()?;
                    match first {
                        0xd800..=0xdbff => {
                            let next_escape = self.offset;
                            if self.body[self.offset..].starts_with("\\u") {
                                self.offset += 2;
                                match self.parse_hex_code_unit() {
                                    Ok(second @ 0xdc00..=0xdfff) => {
                                        let scalar = 0x10000
                                            + ((u32::from(first) - 0xd800) << 10)
                                            + (u32::from(second) - 0xdc00);
                                        value.push(
                                            char::from_u32(scalar)
                                                .expect("paired surrogate is valid"),
                                        );
                                    }
                                    Ok(_) => {
                                        value.push('\u{fffd}');
                                        self.offset = next_escape;
                                    }
                                    Err(error) => {
                                        value.push('\u{fffd}');
                                        self.offset = next_escape;
                                        let _ = error;
                                    }
                                }
                            } else {
                                value.push('\u{fffd}');
                            }
                        }
                        0xdc00..=0xdfff => value.push('\u{fffd}'),
                        code_unit => value.push(
                            char::from_u32(u32::from(code_unit))
                                .expect("non-surrogate UTF-16 unit is a Unicode scalar"),
                        ),
                    }
                }
                _ => return Err(self.syntax_error()),
            }
        }
        Err(self.syntax_error())
    }

    fn parse_hex_code_unit(&mut self) -> Result<u16, serde_json::Error> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.peek_byte().ok_or_else(|| self.syntax_error())?;
            self.offset += 1;
            let digit = match digit {
                b'0'..=b'9' => u16::from(digit - b'0'),
                b'a'..=b'f' => u16::from(digit - b'a') + 10,
                b'A'..=b'F' => u16::from(digit - b'A') + 10,
                _ => return Err(self.syntax_error()),
            };
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), serde_json::Error> {
        if self.consume_byte(expected) {
            Ok(())
        } else {
            Err(self.syntax_error())
        }
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.peek_byte() == Some(expected) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn peek_byte(&self) -> Option<u8> {
        self.body.as_bytes().get(self.offset).copied()
    }

    fn skip_clr_whitespace(&mut self) {
        while self.offset < self.body.len() {
            let character = self.body[self.offset..]
                .chars()
                .next()
                .expect("offset is at a UTF-8 boundary");
            if !character.is_whitespace() && character != '\0' {
                break;
            }
            self.offset += character.len_utf8();
        }
    }

    fn skip_ignored(&mut self) -> Result<(), serde_json::Error> {
        loop {
            self.skip_clr_whitespace();
            if !self.is_comment_start() {
                return Ok(());
            }
            if self.body[self.offset..].starts_with("//") {
                self.offset += 2;
                while self.offset < self.body.len() {
                    let character = self.body[self.offset..]
                        .chars()
                        .next()
                        .expect("offset is at a UTF-8 boundary");
                    if matches!(character, '\r' | '\n') {
                        break;
                    }
                    self.offset += character.len_utf8();
                }
                continue;
            }

            self.offset += 2;
            let mut closed = false;
            while self.offset < self.body.len() {
                if self.body[self.offset..].starts_with("*/") {
                    self.offset += 2;
                    closed = true;
                    break;
                }
                let character = self.body[self.offset..]
                    .chars()
                    .next()
                    .expect("offset is at a UTF-8 boundary");
                self.offset += character.len_utf8();
            }
            if !closed {
                return Err(self.syntax_error());
            }
        }
    }

    fn is_comment_start(&self) -> bool {
        self.body[self.offset..].starts_with("//") || self.body[self.offset..].starts_with("/*")
    }

    fn syntax_error(&self) -> serde_json::Error {
        serde_json::from_str::<Value>(self.body)
            .err()
            .unwrap_or_else(|| {
                serde_json::from_str::<Value>("{").expect_err("unterminated object is invalid JSON")
            })
    }
}

fn parse_radix_i64(digits: &str, radix: u32) -> Option<i64> {
    if digits.is_empty() {
        return None;
    }
    let mut value = 0_u64;
    for digit in digits.bytes() {
        let digit = match digit {
            b'0'..=b'9' => u32::from(digit - b'0'),
            b'a'..=b'f' => u32::from(digit - b'a') + 10,
            b'A'..=b'F' => u32::from(digit - b'A') + 10,
            _ => return None,
        };
        if digit >= radix {
            return None;
        }
        value = value
            .checked_mul(u64::from(radix))?
            .checked_add(u64::from(digit))?;
    }
    Some(value as i64)
}

fn canonical_decimal_integer(raw: &str) -> Option<String> {
    let (negative, digits) = match raw.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, raw),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        Some("0".to_owned())
    } else if negative {
        Some(format!("-{digits}"))
    } else {
        Some(digits.to_owned())
    }
}

fn parse_clr_float_text(value: &str, allow_plus: bool, allow_grouping: bool) -> Option<f64> {
    if value.is_empty() {
        return None;
    }
    let (sign, unsigned) = match value.as_bytes()[0] {
        b'-' => ("-", &value[1..]),
        b'+' if allow_plus => ("+", &value[1..]),
        b'+' => return None,
        _ => ("", value),
    };
    if unsigned.eq_ignore_ascii_case("nan") {
        return Some(f64::NAN);
    }
    if unsigned.eq_ignore_ascii_case("infinity") {
        return Some(if sign == "-" {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }

    let mut body = String::with_capacity(unsigned.len() + 2);
    let mut saw_integer_digit = false;
    let mut saw_mantissa_digit = false;
    let mut saw_exponent = false;
    let mut saw_exponent_digit = false;
    let mut exponent_sign_allowed = false;
    let mut after_decimal = false;
    for character in unsigned.chars() {
        match character {
            ',' if allow_grouping && !after_decimal && !saw_exponent && saw_integer_digit => {}
            ',' => return None,
            '.' if !after_decimal && !saw_exponent => {
                after_decimal = true;
                body.push('.');
            }
            'e' | 'E' if !saw_exponent && saw_mantissa_digit => {
                saw_exponent = true;
                exponent_sign_allowed = true;
                body.push(character);
            }
            '+' | '-' if exponent_sign_allowed => {
                exponent_sign_allowed = false;
                body.push(character);
            }
            digit @ '0'..='9' => {
                if saw_exponent {
                    saw_exponent_digit = true;
                    exponent_sign_allowed = false;
                } else {
                    saw_mantissa_digit = true;
                    if !after_decimal {
                        saw_integer_digit = true;
                    }
                }
                body.push(digit);
            }
            _ => return None,
        }
    }
    if !saw_mantissa_digit || (saw_exponent && !saw_exponent_digit) {
        return None;
    }

    let exponent_index = body.find(['e', 'E']).unwrap_or(body.len());
    let mut mantissa = body[..exponent_index].to_owned();
    let exponent = body[exponent_index..].to_owned();
    if mantissa.starts_with('.') {
        mantissa.insert(0, '0');
    }
    if mantissa.ends_with('.') {
        mantissa.push('0');
    }
    let normalized = format!("{sign}{mantissa}{exponent}");
    normalized
        .parse::<f64>()
        .ok()
        .or_else(|| float_extreme_from_decimal(&normalized))
}

fn is_dotnet_property_character(character: char) -> bool {
    if character.len_utf16() != 1 {
        return false;
    }
    static CATEGORY_REGEX: OnceLock<Result<Regex, regex::Error>> = OnceLock::new();
    let mut encoded = [0_u8; 4];
    let character = character.encode_utf8(&mut encoded);
    CATEGORY_REGEX
        .get_or_init(|| Regex::new(r"[\p{L}\p{Nd}_$]"))
        .as_ref()
        .is_ok_and(|regex| regex.is_match(character))
}

fn float_extreme_from_decimal(value: &str) -> Option<f64> {
    let (negative, unsigned) = match value.strip_prefix('-') {
        Some(unsigned) => (true, unsigned),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let exponent_index = unsigned.find(['e', 'E']).unwrap_or(unsigned.len());
    let (mantissa, exponent) = unsigned.split_at(exponent_index);
    let explicit_exponent = if exponent.is_empty() {
        0_i128
    } else {
        let digits = exponent[1..].trim_start_matches(['+', '-']);
        let magnitude = digits.parse::<i128>().unwrap_or(i128::MAX / 4);
        if exponent.as_bytes().get(1) == Some(&b'-') {
            -magnitude
        } else {
            magnitude
        }
    };
    let integer_digits = mantissa.find('.').unwrap_or(mantissa.len()) as i128;
    let first_significant = mantissa
        .bytes()
        .filter(|byte| *byte != b'.')
        .position(|byte| byte != b'0');
    let Some(first_significant) = first_significant else {
        return Some(if negative { -0.0 } else { 0.0 });
    };
    let decimal_exponent = integer_digits - first_significant as i128 - 1 + explicit_exponent;
    if decimal_exponent > 308 {
        Some(if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        })
    } else if decimal_exponent < -324 {
        Some(if negative { -0.0 } else { 0.0 })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bigint_cast_discards_low_bits_like_clr() {
        assert_eq!(
            clr_big_integer_to_f64("9007199254740995"),
            Some(9007199254740994.0)
        );
        assert_eq!(
            clr_big_integer_to_f64("9223372036854776833"),
            Some(9.223372036854776e18)
        );
        assert_eq!(
            clr_big_integer_to_f64("18446744073709553665"),
            Some(1.8446744073709552e19)
        );
        assert_eq!(
            clr_big_integer_to_f64("179769313486231570814527423731704356798070567525844996598917476803157260780028538760589558632766878171540458953514382464234321326889464182768467546703537516986049910576551282076245490090389328944075868508455133942304583236903222948165808559332123348274797826204144723168738177180919299881250404026184124858368"),
            Some(f64::MAX)
        );
        assert_eq!(
            clr_big_integer_to_f64("179769313486231590772930519078902473361797697894230657273430081157732675805500963132708477322407536021120113879871393357658789768814416622492847430639474124377767893424865485276302219601246094119453082952085005768838150682342462881473913110540827237163350510684586298239947245938479716304835356329624224137215"),
            Some(f64::MAX)
        );
        assert_eq!(clr_big_integer_to_f64("1".to_owned().as_str()), Some(1.0));
    }

    #[test]
    fn dotnet_double_general_uses_clr_notation_thresholds() {
        for (value, expected) in [
            (1.0, "1"),
            (100.0, "100"),
            (1e-4, "0.0001"),
            (1e-5, "1E-05"),
            (1e-7, "1E-07"),
            (1e16, "10000000000000000"),
            (1e17, "1E+17"),
            (-0.0, "-0"),
            (0.0, "0"),
        ] {
            assert_eq!(dotnet_double_general(value), expected, "{value:?}");
        }
    }

    #[test]
    fn typed_double_text_keeps_newtonsoft_whitespace_and_special_rules() {
        assert_eq!(parse_clr_double_text(" \t1.5\r\n"), Some(1.5));
        assert_eq!(parse_clr_double_text("1,,2"), Some(12.0));
        assert_eq!(parse_clr_double_text("1,"), Some(1.0));
        assert_eq!(parse_clr_double_text("1,.2"), Some(1.2));
        assert_eq!(
            parse_clr_double_text("\u{00a0}+iNfInItY\u{00a0}"),
            Some(f64::INFINITY)
        );
        assert_eq!(
            parse_clr_double_text("\u{202f}-NaN\u{202f}")
                .unwrap()
                .is_nan(),
            true
        );
        assert_eq!(parse_clr_double_text("+Infinity"), Some(f64::INFINITY));
        assert_eq!(parse_clr_double_text("-infinity"), Some(f64::NEG_INFINITY));

        for text in [
            "\u{00a0}1.5\u{00a0}",
            "1.5\u{202f}",
            "inf",
            "1.2,3",
            "1e1,2",
            ",1",
            "1.5-",
            "−Infinity",
        ] {
            assert_eq!(parse_clr_double_text(text), None, "{text:?}");
        }
    }

    #[test]
    fn number_reader_keeps_newtonsoft_integer_and_float_classes() {
        for (text, expected) in [
            ("0x10", 16),
            ("0XFF", 255),
            ("010", 8),
            ("01", 1),
            ("00", 0),
            ("-010", -10),
            ("-00", 0),
            ("0x8000000000000000", i64::MIN),
            ("0xffffffffffffffff", -1),
        ] {
            let OrderedJsonValue::Number(number) = parse_json_text(text).unwrap() else {
                panic!("expected Int64 token for {text}");
            };
            assert!(matches!(number.kind, JsonNumberKind::Int64(value) if value == expected));
            assert_eq!(number.lexeme, text);
            assert_eq!(number.origin, JsonReaderOrigin::TextReader);
        }

        for (text, expected) in [
            (format!("0{:o}", i64::MAX as u64), i64::MAX),
            (format!("0{:o}", 1_u64 << 63), i64::MIN),
            (format!("0{:o}", u64::MAX), -1),
        ] {
            let OrderedJsonValue::Number(number) = parse_json_text(&text).unwrap() else {
                panic!("expected octal Int64 token for {text}");
            };
            assert!(matches!(number.kind, JsonNumberKind::Int64(value) if value == expected));
            assert_eq!(number.lexeme, text);
        }

        for text in [
            "08",
            "09",
            "-0x10",
            "0x10000000000000000",
            "+1",
            "1\u{00a0}2",
            "{NaN\u{00a0}one:1}",
        ] {
            assert!(parse_json_text(text).is_err(), "{text}");
        }
        let octal_overflow = format!("0{:o}", 1_u128 << 64);
        assert!(parse_json_text(&octal_overflow).is_err());
        assert!(matches!(
            parse_json_text(&"9".repeat(380)).unwrap(),
            OrderedJsonValue::Number(JsonNumber {
                kind: JsonNumberKind::BigInteger(_),
                ..
            })
        ));
        assert!(parse_json_text(&"9".repeat(381)).is_err());
        assert!(matches!(
            parse_json_text(&"0".repeat(381)).unwrap(),
            OrderedJsonValue::Number(JsonNumber {
                kind: JsonNumberKind::Int64(0),
                ..
            })
        ));
        for (text, expected) in [(".5", 0.5), ("1.", 1.0), ("1e309", f64::INFINITY)] {
            match parse_json_text(text).unwrap() {
                OrderedJsonValue::Number(JsonNumber {
                    kind: JsonNumberKind::Float(value),
                    ..
                }) => assert_eq!(value, expected),
                OrderedJsonValue::NonFinite {
                    value: NonFinite::PositiveInfinity,
                    lexeme,
                    ..
                } if expected.is_infinite() => assert_eq!(lexeme, text),
                _ => panic!("expected Float token for {text}"),
            }
        }
        match parse_json_text("-1e-5000").unwrap() {
            OrderedJsonValue::Number(JsonNumber {
                kind: JsonNumberKind::Float(value),
                lexeme,
                origin,
            }) => {
                assert_eq!(value.to_bits(), (-0.0_f64).to_bits());
                assert_eq!(lexeme, "-1e-5000");
                assert_eq!(origin, JsonReaderOrigin::TextReader);
            }
            value => panic!("expected underflowed Float token: {value:?}"),
        }
    }

    #[test]
    fn text_reader_nonfinite_float_keeps_raw_lexeme_and_jobject_origin_is_canonical() {
        let OrderedJsonValue::NonFinite {
            value: NonFinite::PositiveInfinity,
            origin,
            lexeme,
        } = parse_json_text("1e309").unwrap()
        else {
            panic!("expected nonfinite Float token");
        };
        assert_eq!(origin, JsonReaderOrigin::TextReader);
        assert_eq!(lexeme, "1e309");

        let mut value = parse_json_text("1e309").unwrap();
        value.set_number_origin(JsonReaderOrigin::JObjectReader);
        let OrderedJsonValue::NonFinite {
            value: NonFinite::PositiveInfinity,
            origin,
            lexeme,
        } = value
        else {
            panic!("expected nonfinite Float token");
        };
        assert_eq!(origin, JsonReaderOrigin::JObjectReader);
        assert_eq!(lexeme, "1e309");
        assert_eq!(non_finite_text(NonFinite::PositiveInfinity), "Infinity");
    }

    #[test]
    fn json_reader_accepts_newtonsoft_quotes_comments_controls_and_trailing_commas() {
        let text = "/* head */ {'single':'value', double:\"a\\'b\", // line\n unquoted_é:1, $key:2, _key:3, ٣:4, list:[1,,2,undefined,], } // tail";
        let OrderedJsonValue::Object(entries) = parse_json_text(text).unwrap() else {
            panic!("expected object");
        };
        assert!(matches!(&entries[0].1, OrderedJsonValue::String(value) if value == "value"));
        assert!(matches!(&entries[1].1, OrderedJsonValue::String(value) if value == "a'b"));
        assert_eq!(entries[2].0, "unquoted_é");
        assert_eq!(entries[3].0, "$key");
        assert_eq!(entries[4].0, "_key");
        assert_eq!(entries[5].0, "٣");
        assert!(matches!(
            &entries[6].1,
            OrderedJsonValue::Array(values)
                if matches!(values.as_slice(), [
                    OrderedJsonValue::Number(_),
                    OrderedJsonValue::Undefined,
                    OrderedJsonValue::Number(_),
                    OrderedJsonValue::Undefined,
                ])
        ));

        let raw = "'line one\nline two\t\0'";
        assert!(matches!(
            parse_json_text(raw).unwrap(),
            OrderedJsonValue::String(value) if value == "line one\nline two\t\0"
        ));
        assert!(matches!(
            parse_json_text("\"\\uD800\\uDC00\\uD800\\u0041\\uDC00\"").unwrap(),
            OrderedJsonValue::String(value) if value == "𐀀�A�"
        ));
        assert!(matches!(
            parse_json_text("new Date(0, 'x',)").unwrap(),
            OrderedJsonValue::Constructor { name, arguments }
                if name == "Date" && arguments.len() == 2
        ));
        assert!(matches!(
            parse_json_text("\u{00a0}\0 1\u{202f}/* tail */").unwrap(),
            OrderedJsonValue::Number(JsonNumber {
                kind: JsonNumberKind::Int64(1),
                ..
            })
        ));
        assert!(matches!(
            parse_json_text("{NaN\u{00a0}:1}").unwrap(),
            OrderedJsonValue::Object(entries)
                if matches!(entries.as_slice(), [(key, OrderedJsonValue::Number(_))] if key == "NaN")
        ));
        assert!(matches!(
            parse_json_text("\"2025-03-04T05:06:07Z\"").unwrap(),
            OrderedJsonValue::String(value) if value == "2025-03-04T05:06:07Z"
        ));
    }

    #[test]
    fn json_reader_rejects_invalid_unquoted_names_and_comment_key_gap() {
        for text in [
            "{a\u{0301}:1}",
            "{Ⅳ:1}",
            "{𐐀:1}",
            "{a-b:1}",
            "{\\u0061:1}",
            "{key/* gap */:1}",
            "{\"key\"/* gap */:1}",
            "1 2",
        ] {
            assert!(parse_json_text(text).is_err(), "{text:?}");
        }

        assert!(matches!(
            parse_json_text("{key :1, NaN:Infinity}").unwrap(),
            OrderedJsonValue::Object(_)
        ));
        assert!(parse_json_text("1 /* trailing */ 2").is_err());
    }
}

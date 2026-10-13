//! The one secret-masking implementation.
//!
//! There were five, with three different sentinels (`***`, `[REDACTED]`,
//! `[redacted]`), and they disagreed on what a secret even is. The store's
//! event validator accepted only `[REDACTED]`, so a string the runner had
//! already masked with `***` was rejected as if it still carried a
//! credential, and the copy facing attacker-controlled step output was the
//! weakest of the five. Divergent redactors are worse than one imperfect
//! redactor: whichever copy is weakest defines the actual exposure, and no
//! reviewer can tell which copy a given string went through.
//!
//! This module is that single implementation. The sentinel is `***`, matching
//! `SecretMasker` in actions/runner (`src/Runner.Sdk/Util/SecretMasker.cs`),
//! so a masked string means the same thing in a log line, a telemetry event
//! and a durable store row.
//!
//! Masking rules follow upstream:
//!
//! * every registered value is masked wherever it occurs, longest match first;
//! * a multi-line secret is also registered line by line, because the runner
//!   emits step output one line at a time and would otherwise never match the
//!   whole value (`ExecutionContext.AddMask` splits before registering);
//! * each value is registered in its encoded forms as well — upstream's
//!   `ValueEncoders`: JSON string escape, URI data escape, XML escape,
//!   backslash escape, surrounding-quote trim, and base64 — because a secret
//!   that reaches a log through a JSON body or a URL query is the same secret.

use std::collections::BTreeSet;

use aho_corasick::{AhoCorasick, MatchKind};

/// The sentinel every masked value is replaced with.
///
/// `SecretMasker.Mask` in actions/runner uses the same three asterisks.
pub const REDACTION: &str = "***";

/// Values shorter than this are never registered: masking one or two
/// characters would redact ordinary text everywhere it occurs and destroy the
/// logs without protecting anything.
pub const MIN_MASK_LENGTH: usize = 3;

/// Whether `text` is exactly the redaction sentinel, ignoring surrounding
/// whitespace. Validators use this to tell "already masked" from "still
/// carries a credential".
#[must_use]
pub fn is_redaction(text: &str) -> bool {
    text.trim() == REDACTION
}

/// A compiled set of secret values and their encoded forms.
#[derive(Debug, Clone, Default)]
pub struct SecretMasker {
    patterns: Vec<String>,
    automaton: Option<AhoCorasick>,
    literal_fallback: bool,
}

impl SecretMasker {
    /// Register every value, its per-line parts, and its encoded forms.
    #[must_use]
    pub fn new<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut patterns: BTreeSet<String> = BTreeSet::new();
        for value in values {
            register(value.as_ref(), &mut patterns);
        }
        // Longest first so a value that contains another is masked whole.
        let mut patterns: Vec<String> = patterns.into_iter().collect();
        patterns.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        let (automaton, literal_fallback) = if patterns.is_empty() {
            (None, false)
        } else {
            match AhoCorasick::builder()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&patterns)
            {
                Ok(automaton) => (Some(automaton), false),
                // Keep the registered values usable if the optimized matcher
                // cannot be built. Returning the original value here would
                // make redaction fail open while contains_secret says false.
                Err(_) => (None, true),
            }
        };
        Self {
            patterns,
            automaton,
            literal_fallback,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Replace every registered value with [`REDACTION`].
    #[must_use]
    pub fn mask(&self, value: &str) -> String {
        if let Some(automaton) = &self.automaton {
            let mut masked = String::with_capacity(value.len());
            automaton.replace_all_with(value, &mut masked, |_match, _text, destination| {
                destination.push_str(REDACTION);
                true
            });
            return masked;
        }

        if self.literal_fallback {
            return self.mask_literal_fallback(value);
        }

        value.to_owned()
    }

    /// Whether `value` still contains a registered secret.
    ///
    /// Callers that must fail closed (durable stores) use this after masking
    /// to prove the transformation actually removed everything.
    #[must_use]
    pub fn contains_secret(&self, value: &str) -> bool {
        if let Some(automaton) = &self.automaton {
            return automaton.find(value).is_some();
        }

        self.literal_fallback && self.patterns.iter().any(|pattern| value.contains(pattern))
    }

    fn mask_literal_fallback(&self, value: &str) -> String {
        let mut masked = String::with_capacity(value.len());
        let mut cursor = 0;

        while cursor < value.len() {
            let mut selected: Option<(usize, &str)> = None;
            for pattern in &self.patterns {
                let Some(offset) = value[cursor..].find(pattern) else {
                    continue;
                };
                let start = cursor + offset;
                let should_select = selected.is_none_or(|(selected_start, selected_pattern)| {
                    start < selected_start
                        || (start == selected_start && pattern.len() > selected_pattern.len())
                });
                if should_select {
                    selected = Some((start, pattern));
                }
            }

            let Some((start, pattern)) = selected else {
                break;
            };
            masked.push_str(&value[cursor..start]);
            masked.push_str(REDACTION);
            cursor = start + pattern.len();
        }

        masked.push_str(&value[cursor..]);
        masked
    }

    /// The registered patterns, longest first. Exposed for callers that need
    /// their own matcher (for example a streaming line masker).
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }
}

fn register(value: &str, patterns: &mut BTreeSet<String>) {
    add_encodings(value, patterns);
    // A multi-line secret never arrives whole in line-oriented output.
    if value.contains('\n') || value.contains('\r') {
        for line in value.lines() {
            add_encodings(line, patterns);
        }
    }
}

fn add_encodings(value: &str, patterns: &mut BTreeSet<String>) {
    let value = value.trim();
    // A too-short source value stays unregistered in every encoding: its
    // base64 form is longer than the minimum but masking it would still
    // redact ordinary text.
    if value.chars().count() < MIN_MASK_LENGTH {
        return;
    }
    for candidate in [
        value.to_owned(),
        trim_double_quotes(value),
        json_string_escape(value),
        uri_data_escape(value),
        xml_escape(value),
        backslash_escape(value),
        base64_encode(value),
    ] {
        if candidate.chars().count() >= MIN_MASK_LENGTH && !candidate.trim().is_empty() {
            patterns.insert(candidate);
        }
    }
}

fn trim_double_quotes(value: &str) -> String {
    value.trim_matches('"').to_owned()
}

fn json_string_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            character if (character as u32) < 0x20 => {
                escaped.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn uri_data_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            escaped.push(*byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn backslash_escape(value: &str) -> String {
    value.replace('\\', "\\\\")
}

fn base64_encode(value: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = value.as_bytes();
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let buffer = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple =
            (u32::from(buffer[0]) << 16) | (u32::from(buffer[1]) << 8) | u32::from(buffer[2]);
        encoded.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        encoded.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn force_literal_fallback(masker: SecretMasker) -> SecretMasker {
        SecretMasker {
            automaton: None,
            literal_fallback: true,
            ..masker
        }
    }

    #[test]
    fn masks_the_literal_value_with_the_shared_sentinel() {
        let masker = SecretMasker::new(["hunter2secret"]);
        assert_eq!(masker.mask("token=hunter2secret;"), "token=***;");
        assert!(is_redaction(REDACTION));
        assert!(!is_redaction("[REDACTED]"));
    }

    /// A PEM key never arrives on one line, so the whole-value pattern alone
    /// would never match anything the runner actually prints.
    #[test]
    fn masks_each_line_of_a_multi_line_secret() {
        let key = "-----BEGIN KEY-----\nabcdefghij\nklmnopqrst\n-----END KEY-----";
        let masker = SecretMasker::new([key]);
        assert_eq!(masker.mask("leaked abcdefghij here"), "leaked *** here");
        assert!(masker.contains_secret("klmnopqrst"));
    }

    #[test]
    fn masks_encoded_forms_of_the_same_secret() {
        let masker = SecretMasker::new([r#"a b"c\d"#]);
        assert!(masker.mask(&format!("json {}", json_string_escape(r#"a b"c\d"#))) == "json ***");
        assert!(masker.mask(&format!("uri {}", uri_data_escape(r#"a b"c\d"#))) == "uri ***");
        assert!(masker.mask(&format!("b64 {}", base64_encode(r#"a b"c\d"#))) == "b64 ***");
    }

    #[test]
    fn literal_fallback_masks_and_reports_registered_values_truthfully() {
        let masker = force_literal_fallback(SecretMasker::new(["hunter2secret"]));
        let input = "before hunter2secret after";
        let masked = masker.mask(input);

        assert_eq!(masked, "before *** after");
        assert!(masker.contains_secret(input));
        assert!(!masker.contains_secret(&masked));
    }

    #[test]
    fn literal_fallback_preserves_longest_match_and_encoded_registration() {
        let secret = r#"a b"c\d"#;
        let masker = force_literal_fallback(SecretMasker::new(["abc", "abcdef", secret]));
        assert_eq!(
            masker.mask("prefix abcdef suffix abc"),
            "prefix *** suffix ***"
        );

        let encoded = [
            json_string_escape(secret),
            uri_data_escape(secret),
            base64_encode(secret),
        ];
        for value in encoded {
            assert!(masker.contains_secret(&value));
            assert_eq!(masker.mask(&value), "***");
        }
    }

    #[test]
    fn refuses_to_register_values_too_short_to_be_secrets() {
        let masker = SecretMasker::new(["a", "ab", ""]);
        assert!(masker.is_empty());
        assert_eq!(masker.mask("a ab"), "a ab");
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(base64_encode("f"), "Zg==");
        assert_eq!(base64_encode("fo"), "Zm8=");
        assert_eq!(base64_encode("foo"), "Zm9v");
        assert_eq!(base64_encode("foobar"), "Zm9vYmFy");
    }
}

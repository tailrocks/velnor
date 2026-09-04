//! The one secret-masking implementation.
//!
//! There were several, with three different sentinels (`***`, `[REDACTED]`,
//! `[redacted]`), and they disagreed on what a secret even is. The store's
//! event validator accepted only `[REDACTED]`, so a string the runner had
//! already masked with `***` was rejected as if it still carried a
//! credential. Divergent redactors are worse than one imperfect redactor:
//! whichever copy is weakest defines the actual exposure, and no reviewer can
//! tell which copy a given string went through.
//!
//! A first consolidation moved every caller onto this module while this module
//! held only seven encoders — a weaker set than the runner's — so the
//! divergence it removed was replaced by a quieter one. This module now holds
//! the strong implementation: the encoder set is exactly the one
//! `HostContext` registers on its `SecretMasker`, and the runner's masker is a
//! thin wrapper over this type rather than a second copy.
//!
//! The sentinel is `***`, matching `SecretMasker.Mask` in actions/runner
//! (`src/Sdk/DTLogging/Logging/SecretMasker.cs`), so a masked string means the
//! same thing in a log line, a telemetry event and a durable store row.
//!
//! Masking rules follow upstream at commit
//! `397b032cbf865e9c3ddfab89d533ec19325e1273` (v2.337.0):
//!
//! * every registered value is masked wherever it occurs, longest match first;
//! * there is **no minimum length**. `SecretMasker.AddValue`
//!   (`src/Sdk/DTLogging/Logging/SecretMasker.cs:78`) rejects only the empty
//!   string. An earlier three-character floor here was a local invention that
//!   silently left short secrets in cleartext;
//! * a multi-line value is also registered line by line, because output
//!   arrives one line at a time and the whole value would otherwise never
//!   match. `AddMaskCommandExtension.ProcessCommand`
//!   (`src/Runner.Worker/ActionCommandManager.cs`) splits on `\r` and `\n`
//!   with `RemoveEmptyEntries | TrimEntries` and registers each part;
//! * each value is registered in its encoded forms as well — the eleven
//!   encoders `HostContext` installs at
//!   `src/Runner.Common/HostContext.cs:103-113`, whose bodies live in
//!   `src/Sdk/DTLogging/Logging/ValueEncoders.cs`. A secret that reaches a log
//!   through a JSON body, a URL query, a base64 credential or a PowerShell
//!   error is the same secret. `AddValue` drops any encoder that returns the
//!   empty string, and so does this module.

use std::collections::BTreeSet;

use aho_corasick::{AhoCorasick, MatchKind};
use base64::{engine::general_purpose, Engine as _};

/// The sentinel every masked value is replaced with.
///
/// `SecretMasker.Mask` in actions/runner uses the same three asterisks.
pub const REDACTION: &str = "***";

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
        let automaton = if patterns.is_empty() {
            None
        } else {
            AhoCorasick::builder()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&patterns)
                .ok()
        };
        Self {
            patterns,
            automaton,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Replace every registered value with [`REDACTION`].
    #[must_use]
    pub fn mask(&self, value: &str) -> String {
        let Some(automaton) = &self.automaton else {
            // No automaton means no patterns: the builder only fails to
            // compile an empty set. Fall back to literal replacement so a
            // future build failure degrades to masking rather than to leaking.
            return self
                .patterns
                .iter()
                .fold(value.to_owned(), |text, pattern| {
                    text.replace(pattern.as_str(), REDACTION)
                });
        };
        let mut masked = String::with_capacity(value.len());
        automaton.replace_all_with(value, &mut masked, |_match, _text, destination| {
            destination.push_str(REDACTION);
            true
        });
        masked
    }

    /// Whether `value` still contains a registered secret.
    ///
    /// Callers that must fail closed (durable stores) use this after masking
    /// to prove the transformation actually removed everything.
    #[must_use]
    pub fn contains_secret(&self, value: &str) -> bool {
        self.automaton
            .as_ref()
            .is_some_and(|automaton| automaton.find(value).is_some())
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
    // Upstream splits with `RemoveEmptyEntries | TrimEntries`.
    if value.contains('\n') || value.contains('\r') {
        for line in value.split(['\n', '\r']) {
            let line = line.trim();
            if !line.is_empty() {
                add_encodings(line, patterns);
            }
        }
    }
}

fn add_encodings(value: &str, patterns: &mut BTreeSet<String>) {
    // `SecretMasker.AddValue` returns early only for null or empty. There is
    // no length floor upstream and there must not be one here.
    if value.is_empty() {
        return;
    }
    patterns.insert(value.to_owned());
    for candidate in [
        base64_string_escape(value),
        base64_string_escape_shift(value, 1),
        base64_string_escape_shift(value, 2),
        command_line_argument_escape(value),
        expression_string_escape(value),
        json_string_escape(value),
        uri_data_escape(value),
        xml_data_escape(value),
        trim_double_quotes(value),
        power_shell_pre_ampersand_escape(value),
        power_shell_post_ampersand_escape(value),
    ] {
        // Matches `AddValue`'s `!String.IsNullOrEmpty(encodedValue)` guard.
        if !candidate.is_empty() {
            patterns.insert(candidate);
        }
    }
}

/// `ValueEncoders.Base64StringEscape`.
fn base64_string_escape(value: &str) -> String {
    general_purpose::STANDARD.encode(value.as_bytes())
}

/// `ValueEncoders.Base64StringEscapeShift1` / `Shift2`, via
/// `Base64StringEscapeShift`. Base64 packs six bits per character, so a secret
/// that follows a prefix of unknown length (`base64(user:password)`) starts at
/// one of three byte offsets and encodes differently at each; upstream
/// registers all three.
fn base64_string_escape_shift(value: &str, shift: usize) -> String {
    let bytes = value.as_bytes();
    if bytes.len() > shift {
        general_purpose::STANDARD.encode(&bytes[shift..])
    } else {
        general_purpose::STANDARD.encode(bytes)
    }
}

/// `ValueEncoders.CommandLineArgumentEscape`: how environment variables are
/// escaped on their way to `docker`.
fn command_line_argument_escape(value: &str) -> String {
    value.replace('"', "\\\"")
}

/// `ValueEncoders.ExpressionStringEscape`, i.e.
/// `Expressions2.Sdk.ExpressionUtility.StringEscape`, which is
/// `value.Replace("'", "''")`.
fn expression_string_escape(value: &str) -> String {
    value.replace('\'', "''")
}

/// `ValueEncoders.JsonStringEscape`: `JsonConvert.ToString` with the wrapping
/// double quotes removed. Mirrors Newtonsoft's default escape table.
fn json_string_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{8}' => escaped.push_str("\\b"),
            '\u{c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            // Newtonsoft escapes the C0 controls plus NEL and the Unicode
            // line/paragraph separators as \uXXXX with lower-case hex.
            other
                if (other as u32) < 0x20
                    || other == '\u{85}'
                    || other == '\u{2028}'
                    || other == '\u{2029}' =>
            {
                escaped.push_str(&format!("\\u{:04x}", other as u32));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// `ValueEncoders.UriDataEscape`, i.e. `Uri.EscapeDataString`: everything
/// outside RFC 3986 unreserved is percent-encoded from its UTF-8 bytes in
/// upper-case hex. Upstream's segment chunking only works around a .NET size
/// limit and does not change the output.
fn uri_data_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                escaped.push(*byte as char);
            }
            other => escaped.push_str(&format!("%{other:02X}")),
        }
    }
    escaped
}

/// `ValueEncoders.XmlDataEscape`, i.e. `SecurityElement.Escape`.
fn xml_data_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '&' => escaped.push_str("&amp;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// `ValueEncoders.TrimDoubleQuotes`. Not a general trim: it yields the inner
/// text only for a value longer than eight characters that both starts and
/// ends with a double quote, and the empty string otherwise. Upstream's
/// `value.Length` counts UTF-16 units, so the gate is measured the same way.
fn trim_double_quotes(value: &str) -> String {
    if utf16_len(value) > 8 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_owned()
    } else {
        String::new()
    }
}

/// `ValueEncoders.PowerShellPreAmpersandEscape`. PowerShell can split a secret
/// containing `&` across colour-code boundaries and print the pieces
/// separately; this covers the leading section, and upstream refuses to
/// register a section shorter than six characters.
fn power_shell_pre_ampersand_escape(value: &str) -> String {
    if value.is_empty() || !value.contains('&') {
        return String::new();
    }
    let section = match value.find("&+") {
        Some(index) => &value[..index + "&+".len()],
        None => &value[..value.rfind('&').expect("value contains '&'") + '&'.len_utf8()],
    };
    if utf16_len(section) >= 6 {
        section.to_owned()
    } else {
        String::new()
    }
}

/// `ValueEncoders.PowerShellPostAmpersandEscape`, the trailing counterpart.
/// After `&+` upstream also skips the one character PowerShell colours, and
/// yields nothing when no character follows it.
fn power_shell_post_ampersand_escape(value: &str) -> String {
    if value.is_empty() || !value.contains('&') {
        return String::new();
    }
    let section = match value.find("&+") {
        Some(index) => {
            let after = &value[index + "&+".len()..];
            // "+1 to skip the letter that got colored".
            match after.chars().next() {
                Some(coloured) => &after[coloured.len_utf8()..],
                None => "",
            }
        }
        None => &value[value.rfind('&').expect("value contains '&'") + '&'.len_utf8()..],
    };
    if utf16_len(section) >= 6 {
        section.to_owned()
    } else {
        String::new()
    }
}

/// Upstream measures every length gate in UTF-16 code units, because that is
/// what `String.Length` counts in .NET.
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `SecretMasker.AddValue` rejects only the empty string. A local
    /// three-character floor used to leave short secrets in cleartext.
    #[test]
    fn registers_short_values_because_upstream_has_no_length_floor() {
        let masker = SecretMasker::new(["ab", "x", ""]);
        assert!(!masker.is_empty());
        assert_eq!(masker.mask("ab x"), "*** ***");
        assert!(SecretMasker::new([""]).is_empty());
    }

    #[test]
    fn empty_masker_leaves_text_untouched() {
        let masker = SecretMasker::default();
        assert!(masker.is_empty());
        assert_eq!(masker.mask("nothing registered"), "nothing registered");
        assert!(!masker.contains_secret("nothing registered"));
    }

    /// One fixed vector per encoder, taken from the upstream body rather than
    /// from the encoder's name.
    mod encoders {
        use super::*;

        /// `Convert.ToBase64String(Encoding.UTF8.GetBytes(value))`.
        #[test]
        fn base64_string_escape_encodes_the_utf8_bytes() {
            assert_eq!(base64_string_escape("foobar"), "Zm9vYmFy");
            assert_eq!(base64_string_escape("f"), "Zg==");
        }

        /// The shifts drop leading bytes so a secret embedded at a non-zero
        /// offset in a base64 stream still matches. `base64("user:secretpw")`
        /// contains the shift-2 encoding of `secretpw`, because `"user:"` is
        /// five bytes and 5 % 3 == 2.
        #[test]
        fn base64_shifts_cover_the_three_byte_offsets() {
            assert_eq!(base64_string_escape_shift("foobar", 1), "b29iYXI=");
            assert_eq!(base64_string_escape_shift("foobar", 2), "b2Jhcg==");
            let embedded = base64_string_escape("user:secretpw");
            let shifted = base64_string_escape_shift("secretpw", 2);
            let overlap = &shifted[..shifted.len() - 4];
            assert!(
                embedded.contains(overlap),
                "shift-2 form {shifted} should align inside {embedded}"
            );
        }

        /// `bytes.Length > shift` is false, so the unshifted encoding is used.
        #[test]
        fn base64_shift_falls_back_when_the_value_is_shorter_than_the_shift() {
            assert_eq!(
                base64_string_escape_shift("a", 1),
                base64_string_escape("a")
            );
            assert_eq!(
                base64_string_escape_shift("ab", 2),
                base64_string_escape("ab")
            );
        }

        /// `value.Replace("\"", "\\\"")`.
        #[test]
        fn command_line_argument_escape_escapes_double_quotes() {
            assert_eq!(command_line_argument_escape(r#"a"b"#), r#"a\"b"#);
        }

        /// `ExpressionUtility.StringEscape`: `value.Replace("'", "''")`.
        #[test]
        fn expression_string_escape_doubles_single_quotes() {
            assert_eq!(expression_string_escape("it's"), "it''s");
        }

        /// `JsonConvert.ToString` minus the wrapping quotes, including the
        /// lower-case `\uXXXX` form for controls.
        #[test]
        fn json_string_escape_matches_the_newtonsoft_table() {
            assert_eq!(json_string_escape("a\"b\\c"), r#"a\"b\\c"#);
            assert_eq!(json_string_escape("a\nb\tc\rd"), r"a\nb\tc\rd");
            assert_eq!(json_string_escape("a\u{8}b\u{c}c"), r"a\bb\fc");
            assert_eq!(json_string_escape("\u{1}"), "\\u0001");
            assert_eq!(json_string_escape("\u{2028}"), "\\u2028");
        }

        /// `Uri.EscapeDataString`: RFC 3986 unreserved survives, everything
        /// else becomes upper-case percent-escaped UTF-8 bytes.
        #[test]
        fn uri_data_escape_percent_encodes_all_but_unreserved() {
            assert_eq!(uri_data_escape("a b"), "a%20b");
            assert_eq!(uri_data_escape("-._~"), "-._~");
            assert_eq!(uri_data_escape("/?&="), "%2F%3F%26%3D");
            assert_eq!(uri_data_escape("é"), "%C3%A9");
        }

        /// `SecurityElement.Escape`.
        #[test]
        fn xml_data_escape_escapes_the_five_markup_characters() {
            assert_eq!(
                xml_data_escape(r#"<a href="x">&'"#),
                "&lt;a href=&quot;x&quot;&gt;&amp;&apos;"
            );
        }

        /// Longer than eight characters *and* quoted at both ends, else the
        /// empty string. The length gate is what makes this not a trim.
        #[test]
        fn trim_double_quotes_requires_quotes_and_more_than_eight_characters() {
            assert_eq!(trim_double_quotes(r#""abcdefghij""#), "abcdefghij");
            // Exactly eight characters: the gate is `> 8`, so nothing.
            assert_eq!(trim_double_quotes(r#""abcdef""#), "");
            // Long enough, but only quoted on one side.
            assert_eq!(trim_double_quotes(r#""abcdefghij"#), "");
            assert_eq!(trim_double_quotes("abcdefghij"), "");
        }

        /// Everything up to and including the last `&`, or up to and including
        /// `&+` when present; nothing under six characters.
        #[test]
        fn power_shell_pre_ampersand_takes_the_leading_section() {
            assert_eq!(
                power_shell_pre_ampersand_escape("secretpart1&secretpart2&secretpart3"),
                "secretpart1&secretpart2&"
            );
            assert_eq!(
                power_shell_pre_ampersand_escape("secretpart1&+secretpart2&secretpart3"),
                "secretpart1&+"
            );
            // Section shorter than six characters is refused.
            assert_eq!(power_shell_pre_ampersand_escape("ab&cdefghij"), "");
            // No ampersand at all: no section.
            assert_eq!(power_shell_pre_ampersand_escape("noampersandhere"), "");
        }

        /// The trailing counterpart, which additionally drops the one
        /// character PowerShell colours after `&+`.
        #[test]
        fn power_shell_post_ampersand_takes_the_trailing_section() {
            assert_eq!(
                power_shell_post_ampersand_escape("secretpart1&secretpart2&secretpart3"),
                "secretpart3"
            );
            assert_eq!(
                power_shell_post_ampersand_escape("secretpart1&+secretpart2&secretpart3"),
                "ecretpart2&secretpart3"
            );
            // Trailing section shorter than six characters is refused.
            assert_eq!(power_shell_post_ampersand_escape("secretpart1&abc"), "");
            assert_eq!(power_shell_post_ampersand_escape("noampersandhere"), "");
        }
    }

    /// Every encoder registered at `HostContext.cs:103-113` must actually be
    /// reachable through the masker, not merely defined.
    #[test]
    fn masker_registers_every_upstream_encoding_of_a_value() {
        let secret = "\"secret&+value'with\"quotes\"";
        let masker = SecretMasker::new([secret]);
        for encoded in [
            base64_string_escape(secret),
            base64_string_escape_shift(secret, 1),
            base64_string_escape_shift(secret, 2),
            command_line_argument_escape(secret),
            expression_string_escape(secret),
            json_string_escape(secret),
            uri_data_escape(secret),
            xml_data_escape(secret),
            trim_double_quotes(secret),
            power_shell_pre_ampersand_escape(secret),
            power_shell_post_ampersand_escape(secret),
        ] {
            assert!(!encoded.is_empty(), "test vector must exercise every encoder");
            assert_eq!(
                masker.mask(&format!("log line: {encoded} end")),
                "log line: *** end",
                "unmasked encoding: {encoded}"
            );
        }
    }

    /// An encoder that yields the empty string is dropped, exactly as
    /// `AddValue` drops it; otherwise the automaton would match everywhere.
    #[test]
    fn empty_encodings_are_never_registered() {
        let masker = SecretMasker::new(["plainsecret"]);
        assert!(trim_double_quotes("plainsecret").is_empty());
        assert!(masker.patterns().iter().all(|pattern| !pattern.is_empty()));
        assert_eq!(masker.mask("untouched text"), "untouched text");
    }

    /// Longest-first ordering: a secret that contains another is masked whole
    /// rather than leaving a tail of the outer value in the log.
    #[test]
    fn longest_match_wins() {
        let masker = SecretMasker::new(["abcdef", "abcdefghij"]);
        assert_eq!(masker.mask("x abcdefghij y"), "x *** y");
    }
}

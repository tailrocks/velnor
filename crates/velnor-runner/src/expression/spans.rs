//! Verbatim source spans for the arguments of a function call.
//!
//! Upstream never needs this: `ActionRunner.RunAsync`
//! (`src/Runner.Worker/ActionRunner.cs:174-185`) evaluates a step's inputs at
//! step-run time, when every context is already populated, so an argument is
//! either evaluated or the whole template evaluation fails.
//!
//! Velnor renders a script step's `run`/`working-directory` template once at
//! job setup, before `env`, `steps`, `job` and `runner` exist. An argument that
//! reads one of those must be handed on to the step-time pass *verbatim*, and
//! the parse tree is not a faithful source. Splitting the argument list with
//! the real [`LexicalAnalyzer`] — rather than scanning for commas — keeps
//! quoting, nesting and escapes exactly as upstream lexes them.

use super::lexer::{LexicalAnalyzer, TokenKind};

/// The function name and the verbatim source of each argument, when
/// `expression` is exactly one function call and nothing else.
///
/// Returns `None` for anything else, including a trailing operator or an
/// unexpected symbol; callers then treat the expression as unsplittable.
pub fn function_call_argument_spans(expression: &str) -> Option<(String, Vec<String>)> {
    let chars: Vec<char> = expression.chars().collect();
    let mut lexer = LexicalAnalyzer::new(expression);

    let name = match lexer.next_token() {
        Some(token) if token.kind == TokenKind::Function => token.raw_value,
        _ => return None,
    };
    match lexer.next_token() {
        Some(token) if token.kind == TokenKind::StartParameters => {}
        _ => return None,
    }

    let mut spans: Vec<String> = Vec::new();
    let mut depth = 1usize;
    let mut start: Option<usize> = None;
    let mut closed = false;

    while let Some(token) = lexer.next_token() {
        match token.kind {
            TokenKind::Unexpected => return None,
            TokenKind::StartGroup | TokenKind::StartIndex | TokenKind::StartParameters => {
                start.get_or_insert(token.index);
                depth += 1;
            }
            TokenKind::EndGroup | TokenKind::EndIndex | TokenKind::EndParameters => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    if let Some(begin) = start.take() {
                        spans.push(span_text(&chars, begin, token.index)?);
                    }
                    closed = true;
                    break;
                }
            }
            TokenKind::Separator if depth == 1 => {
                let begin = start.take()?;
                spans.push(span_text(&chars, begin, token.index)?);
            }
            _ => {
                start.get_or_insert(token.index);
            }
        }
    }

    // The call must close, and must be the entire expression.
    if !closed || lexer.next_token().is_some() {
        return None;
    }
    Some((name, spans))
}

fn span_text(chars: &[char], start: usize, end: usize) -> Option<String> {
    let text: String = chars.get(start..end)?.iter().collect();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

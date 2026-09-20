//! A real lexer and parser for GitHub Actions expressions.
//!
//! This crate contains the shared Runner-compatible parsing core used by the
//! worker and workflow scanner:
//!
//! ```text
//! text -> LexicalAnalyzer -> ExpressionParser (shunting-yard) -> Node
//! ```
//!
//! Every semantic decision is transcribed from `actions/runner` v2.337.0
//! (commit 397b032cbf865e9c3ddfab89d533ec19325e1273) under
//! `src/Sdk/DTExpressions2/Expressions2/`, cited at the point of use.

mod lexer;
mod parser;
mod spans;
pub mod value;

use std::fmt;

pub use lexer::is_legal_keyword;
pub use parser::{parse, ParseEnvironment, MAX_DEPTH, MAX_LENGTH, WELL_KNOWN_FUNCTIONS};
pub use spans::function_call_argument_spans;
pub use value::{ObjectValue, Value};

use lexer::Token;

/// The parsed form of an expression.
#[derive(Debug, Clone)]
pub enum Node {
    Literal(Value),
    /// `Sdk/Wildcard.cs` — `*` inside an index or after a dereference.
    Wildcard,
    /// A root context such as `github`, `env` or `steps`.
    NamedValue(String),
    /// `Sdk/Operators/Index.cs` — both `a.b` and `a[b]` parse to this.
    Index(Box<Node>, Box<Node>),
    /// `Sdk/Operators/Not.cs`.
    Not(Box<Node>),
    /// `Sdk/Operators/And.cs` — n-ary after upstream's flattening.
    And(Vec<Node>),
    /// `Sdk/Operators/Or.cs` — n-ary after upstream's flattening.
    Or(Vec<Node>),
    Binary(BinaryOperator, Box<Node>, Box<Node>),
    Function {
        name: String,
        args: Vec<Node>,
    },
}

impl Node {
    pub fn children(&self) -> Vec<&Node> {
        match self {
            Node::Literal(_) | Node::Wildcard | Node::NamedValue(_) => Vec::new(),
            Node::Not(inner) => vec![inner.as_ref()],
            Node::Index(left, right) | Node::Binary(_, left, right) => {
                vec![left.as_ref(), right.as_ref()]
            }
            Node::And(nodes) | Node::Or(nodes) => nodes.iter().collect(),
            Node::Function { args, .. } => args.iter().collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    Equal,
    NotEqual,
    GreaterThan,
    GreaterThanOrEqual,
    LessThan,
    LessThanOrEqual,
}

/// `ParseExceptionKind` (`ExpressionParser`'s companion,
/// `src/Sdk/DTExpressions2/Expressions2/ParseExceptionKind.cs:3-14`), plus an
/// `Internal` variant for the states upstream raises
/// `NotSupportedException`/`InvalidOperationException` for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorKind {
    ExceededMaxDepth,
    ExceededMaxLength,
    TooFewParameters,
    TooManyParameters,
    EvenParameters,
    UnexpectedEndOfExpression,
    UnexpectedSymbol,
    UnrecognizedFunction,
    UnrecognizedNamedValue,
    Internal,
}

impl ParseErrorKind {
    /// The exact descriptions upstream builds in
    /// `ParseException.cs:18-49`.
    fn description(self) -> String {
        match self {
            ParseErrorKind::ExceededMaxDepth => {
                format!("Exceeded max expression depth {MAX_DEPTH}")
            }
            ParseErrorKind::ExceededMaxLength => {
                format!("Exceeded max expression length {MAX_LENGTH}")
            }
            ParseErrorKind::TooFewParameters => "Too few parameters supplied".to_string(),
            ParseErrorKind::TooManyParameters => "Too many parameters supplied".to_string(),
            ParseErrorKind::EvenParameters => {
                "Even number of parameters supplied, requires an odd number of parameters"
                    .to_string()
            }
            ParseErrorKind::UnexpectedEndOfExpression => "Unexpected end of expression".to_string(),
            ParseErrorKind::UnexpectedSymbol => "Unexpected symbol".to_string(),
            ParseErrorKind::UnrecognizedFunction => "Unrecognized function".to_string(),
            ParseErrorKind::UnrecognizedNamedValue => "Unrecognized named-value".to_string(),
            ParseErrorKind::Internal => "Invalid expression".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    message: String,
}

impl ParseError {
    /// `ParseException.cs:51-58` — the message shape with a token.
    pub(crate) fn with_token(kind: ParseErrorKind, token: &Token, expression: &str) -> Self {
        Self {
            kind,
            message: format!(
                "{}: '{}'. Located at position {} within expression: {expression}",
                kind.description(),
                token.raw_value,
                token.index + 1
            ),
        }
    }

    /// `ParseException.cs:51-53` — the message shape without a token.
    pub(crate) fn without_token(kind: ParseErrorKind, _expression: &str) -> Self {
        Self {
            kind,
            message: kind.description(),
        }
    }

    pub(crate) fn internal(expression: &str, detail: &str) -> Self {
        Self {
            kind: ParseErrorKind::Internal,
            message: format!("Invalid expression: {detail}. Expression: {expression}"),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

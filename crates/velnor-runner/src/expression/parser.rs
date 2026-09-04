//! Parser for GitHub Actions expressions.
//!
//! Transcribed from `actions/runner` v2.337.0
//! (commit 397b032cbf865e9c3ddfab89d533ec19325e1273):
//!
//! * `src/Sdk/DTExpressions2/Expressions2/ExpressionParser.cs`
//! * `src/Sdk/DTExpressions2/Expressions2/ParseException.cs`
//!
//! The algorithm is upstream's shunting-yard, kept structurally identical so
//! that precedence, associativity and the `&&`/`||` flattening match exactly.

use super::lexer::{Associativity, LexicalAnalyzer, Token, TokenKind};
use super::value::Value;
use super::{BinaryOperator, Node, ParseError, ParseErrorKind};

/// `ExpressionConstants.MaxDepth` (`ExpressionConstants.cs:30`).
pub const MAX_DEPTH: usize = 50;
/// `ExpressionConstants.MaxLength` (`ExpressionConstants.cs:31`).
pub const MAX_LENGTH: usize = 21000;

/// Minimum and maximum parameter counts of the well-known functions
/// (`ExpressionConstants.cs:10-20`). `Byte.MaxValue` is upstream's "unbounded".
pub const WELL_KNOWN_FUNCTIONS: &[(&str, usize, usize)] = &[
    ("case", 3, 255),
    ("contains", 2, 2),
    ("endsWith", 2, 2),
    ("format", 1, 255),
    ("join", 1, 2),
    ("startsWith", 2, 2),
    ("toJson", 1, 1),
    ("fromJson", 1, 1),
];

/// The names and arities a parse needs to know about. Upstream carries these
/// as `INamedValueInfo` / `IFunctionInfo` collections handed to
/// `ExpressionParser.CreateTree` (`ExpressionParser.cs:16-25`).
pub trait ParseEnvironment {
    /// Whether `name` is a recognized root context. Unknown roots are a parse
    /// error upstream (`ExpressionParser.cs:144-147`), not a silent null.
    fn is_named_value(&self, name: &str) -> bool;

    /// Extension function arity, e.g. `success` → `(0, 0)`, `hashFiles` →
    /// `(1, 255)` (`src/Runner.Worker/StepsRunner.cs:92-97`).
    fn function_arity(&self, name: &str) -> Option<(usize, usize)>;
}

fn lookup_function(env: &dyn ParseEnvironment, name: &str) -> Option<(&'static str, usize, usize)> {
    if let Some((canonical, min, max)) = WELL_KNOWN_FUNCTIONS
        .iter()
        .find(|(known, _, _)| known.eq_ignore_ascii_case(name))
    {
        return Some((canonical, *min, *max));
    }
    env.function_arity(name).map(|(min, max)| ("", min, max))
}

struct ParseContext<'a> {
    expression: &'a str,
    lexer: LexicalAnalyzer,
    operands: Vec<Node>,
    operators: Vec<Token>,
    last_token: Option<Token>,
    env: &'a dyn ParseEnvironment,
}

/// `ExpressionParser.CreateTree` (`ExpressionParser.cs:36-103`).
pub fn parse(expression: &str, env: &dyn ParseEnvironment) -> Result<Option<Node>, ParseError> {
    if expression.chars().count() > MAX_LENGTH {
        return Err(ParseError::without_token(
            ParseErrorKind::ExceededMaxLength,
            expression,
        ));
    }

    let mut context = ParseContext {
        expression,
        lexer: LexicalAnalyzer::new(expression),
        operands: Vec::new(),
        operators: Vec::new(),
        last_token: None,
        env,
    };

    while let Some(token) = context.lexer.next_token() {
        if token.kind == TokenKind::Unexpected {
            return Err(ParseError::with_token(
                ParseErrorKind::UnexpectedSymbol,
                &token,
                expression,
            ));
        } else if token.is_operator() {
            push_operator(&mut context, token.clone())?;
        } else {
            push_operand(&mut context, &token)?;
        }
        context.last_token = Some(token);
    }

    let Some(last_token) = context.last_token.clone() else {
        return Ok(None);
    };

    if !context.operators.is_empty() {
        let unexpected_last_token = match last_token.kind {
            TokenKind::EndGroup | TokenKind::EndIndex | TokenKind::EndParameters => false,
            TokenKind::Function => true,
            _ => last_token.is_operator(),
        };

        if unexpected_last_token || context.lexer.has_unclosed_tokens() {
            return Err(ParseError::with_token(
                ParseErrorKind::UnexpectedEndOfExpression,
                &last_token,
                expression,
            ));
        }
    }

    while !context.operators.is_empty() {
        flush_top_operator(&mut context)?;
    }

    if context.operands.len() != 1 {
        return Err(ParseError::with_token(
            ParseErrorKind::UnexpectedEndOfExpression,
            &last_token,
            expression,
        ));
    }

    let result = context.operands.pop().expect("exactly one operand remains");
    check_max_depth(expression, &result, 1)?;
    Ok(Some(result))
}

/// `ExpressionParser.PushOperand` (`ExpressionParser.cs:105-158`).
fn push_operand(context: &mut ParseContext<'_>, token: &Token) -> Result<(), ParseError> {
    let node = match token.kind {
        TokenKind::Function => {
            let Some((canonical, _, _)) = lookup_function(context.env, &token.raw_value) else {
                return Err(ParseError::with_token(
                    ParseErrorKind::UnrecognizedFunction,
                    token,
                    context.expression,
                ));
            };
            let name = if canonical.is_empty() {
                token.raw_value.clone()
            } else {
                canonical.to_string()
            };
            Node::Function {
                name,
                args: Vec::new(),
            }
        }
        TokenKind::NamedValue => {
            if !context.env.is_named_value(&token.raw_value) {
                return Err(ParseError::with_token(
                    ParseErrorKind::UnrecognizedNamedValue,
                    token,
                    context.expression,
                ));
            }
            Node::NamedValue(token.raw_value.clone())
        }
        TokenKind::Wildcard => Node::Wildcard,
        TokenKind::PropertyName => Node::Literal(Value::String(token.raw_value.clone())),
        TokenKind::Null | TokenKind::Boolean | TokenKind::Number | TokenKind::String => {
            Node::Literal(token.parsed_value.clone().unwrap_or(Value::Null))
        }
        other => unreachable!("token kind {other:?} is not an operand"),
    };

    context.operands.push(node);
    Ok(())
}

/// `ExpressionParser.PushOperator` (`ExpressionParser.cs:160-196`).
fn push_operator(context: &mut ParseContext<'_>, token: Token) -> Result<(), ParseError> {
    if token.associativity() == Associativity::LeftToRight {
        let precedence = token.precedence();
        while let Some(top) = context.operators.last() {
            if precedence <= top.precedence()
                && !matches!(
                    top.kind,
                    TokenKind::StartGroup
                        | TokenKind::StartIndex
                        | TokenKind::StartParameters
                        | TokenKind::Separator
                )
            {
                flush_top_operator(context)?;
                continue;
            }
            break;
        }
    }

    let kind = token.kind;
    context.operators.push(token);

    // Closing operators are processed here because `last_token` is required to
    // process `EndParameters` accurately.
    if matches!(
        kind,
        TokenKind::EndGroup | TokenKind::EndIndex | TokenKind::EndParameters
    ) {
        flush_top_operator(context)?;
    }

    Ok(())
}

/// `ExpressionParser.FlushTopOperator` (`ExpressionParser.cs:198-258`).
fn flush_top_operator(context: &mut ParseContext<'_>) -> Result<(), ParseError> {
    match context.operators.last().map(|token| token.kind) {
        Some(TokenKind::EndIndex) => return flush_top_end_index(context),
        Some(TokenKind::EndGroup) => {
            pop_operator(context, TokenKind::EndGroup)?;
            pop_operator(context, TokenKind::StartGroup)?;
            return Ok(());
        }
        Some(TokenKind::EndParameters) => return flush_top_end_parameters(context),
        _ => {}
    }

    let operator = context
        .operators
        .pop()
        .ok_or_else(|| ParseError::internal(context.expression, "operator stack underflow"))?;
    let operands = pop_operands(context, operator.operand_count())?;

    let node = match operator.kind {
        TokenKind::LogicalOperator => match operator.raw_value.as_str() {
            "!" => Node::Not(Box::new(operands.into_iter().next().expect("one operand"))),
            "&&" | "||" => {
                // Upstream flattens nested `And`/`Or` into a single n-ary node
                // (`ExpressionParser.cs:226-251`).
                let is_and = operator.raw_value == "&&";
                let mut parameters = Vec::new();
                for operand in operands {
                    match (&operand, is_and) {
                        (Node::And(nested), true) => parameters.extend(nested.iter().cloned()),
                        (Node::Or(nested), false) => parameters.extend(nested.iter().cloned()),
                        _ => parameters.push(operand),
                    }
                }
                if is_and {
                    Node::And(parameters)
                } else {
                    Node::Or(parameters)
                }
            }
            raw => {
                let operator = match raw {
                    "==" => BinaryOperator::Equal,
                    "!=" => BinaryOperator::NotEqual,
                    ">" => BinaryOperator::GreaterThan,
                    ">=" => BinaryOperator::GreaterThanOrEqual,
                    "<" => BinaryOperator::LessThan,
                    "<=" => BinaryOperator::LessThanOrEqual,
                    other => {
                        return Err(ParseError::internal(
                            context.expression,
                            &format!("unexpected logical operator '{other}'"),
                        ));
                    }
                };
                let mut operands = operands.into_iter();
                let left = operands.next().expect("binary operator has a left operand");
                let right = operands
                    .next()
                    .expect("binary operator has a right operand");
                Node::Binary(operator, Box::new(left), Box::new(right))
            }
        },
        TokenKind::Dereference | TokenKind::StartIndex => {
            let mut operands = operands.into_iter();
            let left = operands.next().expect("index has a left operand");
            let right = operands.next().expect("index has a right operand");
            Node::Index(Box::new(left), Box::new(right))
        }
        other => {
            return Err(ParseError::internal(
                context.expression,
                &format!("unexpected operator kind {other:?}"),
            ));
        }
    };

    context.operands.push(node);
    Ok(())
}

/// `ExpressionParser.FlushTopEndIndex` (`ExpressionParser.cs:270-291`).
fn flush_top_end_index(context: &mut ParseContext<'_>) -> Result<(), ParseError> {
    pop_operator(context, TokenKind::EndIndex)?;
    let operator = pop_operator(context, TokenKind::StartIndex)?;
    let operands = pop_operands(context, operator.operand_count())?;
    let mut operands = operands.into_iter();
    let left = operands.next().expect("index has a left operand");
    let right = operands.next().expect("index has a right operand");
    context
        .operands
        .push(Node::Index(Box::new(left), Box::new(right)));
    Ok(())
}

/// `ExpressionParser.FlushTopEndParameters` (`ExpressionParser.cs:293-356`).
fn flush_top_end_parameters(context: &mut ParseContext<'_>) -> Result<(), ParseError> {
    let operator = pop_operator(context, TokenKind::EndParameters)?;

    let parameters = if context.last_token.as_ref().map(|token| token.kind)
        == Some(TokenKind::StartParameters)
    {
        Vec::new()
    } else {
        let mut parameter_count = 1;
        while context.operators.last().map(|token| token.kind) == Some(TokenKind::Separator) {
            parameter_count += 1;
            context.operators.pop();
        }
        pop_operands(context, parameter_count)?
    };

    let Some(Node::Function { name, args }) = context.operands.last_mut() else {
        return Err(ParseError::internal(
            context.expression,
            "expected a function node on the operand stack",
        ));
    };
    args.extend(parameters);
    let name = name.clone();
    let arg_count = args.len();

    let start = pop_operator(context, TokenKind::StartParameters)?;
    let _ = operator;

    let Some((_, min, max)) = lookup_function(context.env, &name) else {
        return Err(ParseError::with_token(
            ParseErrorKind::UnrecognizedFunction,
            &start,
            context.expression,
        ));
    };

    if arg_count < min {
        return Err(ParseError::with_token(
            ParseErrorKind::TooFewParameters,
            &start,
            context.expression,
        ));
    }
    if arg_count > max {
        return Err(ParseError::with_token(
            ParseErrorKind::TooManyParameters,
            &start,
            context.expression,
        ));
    }
    if name.eq_ignore_ascii_case("case") && arg_count % 2 == 0 {
        return Err(ParseError::with_token(
            ParseErrorKind::EvenParameters,
            &start,
            context.expression,
        ));
    }

    Ok(())
}

/// `ExpressionParser.PopOperands` (`ExpressionParser.cs:358-374`) — natural
/// listed order, not last-in-first-out.
fn pop_operands(context: &mut ParseContext<'_>, count: usize) -> Result<Vec<Node>, ParseError> {
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        let operand = context
            .operands
            .pop()
            .ok_or_else(|| ParseError::internal(context.expression, "operand stack underflow"))?;
        result.push(operand);
    }
    result.reverse();
    Ok(result)
}

/// `ExpressionParser.PopOperator` (`ExpressionParser.cs:376-389`).
fn pop_operator(context: &mut ParseContext<'_>, expected: TokenKind) -> Result<Token, ParseError> {
    let token = context
        .operators
        .pop()
        .ok_or_else(|| ParseError::internal(context.expression, "operator stack underflow"))?;
    if token.kind != expected {
        return Err(ParseError::internal(
            context.expression,
            &format!(
                "expected operator {expected:?} to be popped, found {:?}",
                token.kind
            ),
        ));
    }
    Ok(token)
}

/// `ExpressionParser.CheckMaxDepth` (`ExpressionParser.cs:391-411`).
fn check_max_depth(expression: &str, node: &Node, depth: usize) -> Result<(), ParseError> {
    if depth > MAX_DEPTH {
        return Err(ParseError::without_token(
            ParseErrorKind::ExceededMaxDepth,
            expression,
        ));
    }
    for child in node.children() {
        check_max_depth(expression, child, depth + 1)?;
    }
    Ok(())
}

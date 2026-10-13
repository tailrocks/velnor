//! Lexical analyzer for GitHub Actions expressions.
//!
//! Transcribed from `actions/runner` v2.337.0
//! (commit 397b032cbf865e9c3ddfab89d533ec19325e1273):
//!
//! * `src/Sdk/DTExpressions2/Expressions2/Tokens/LexicalAnalyzer.cs`
//! * `src/Sdk/DTExpressions2/Expressions2/Tokens/Token.cs`
//! * `src/Sdk/DTExpressions2/Expressions2/Tokens/TokenKind.cs`
//! * `src/Sdk/DTExpressions2/Expressions2/ExpressionConstants.cs`

use super::value::{parse_number, Value};

/// `Tokens/TokenKind.cs:3-27`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    StartGroup,      // "(" logical grouping
    StartIndex,      // "["
    StartParameters, // "(" function call
    EndGroup,        // ")" logical grouping
    EndIndex,        // "]"
    EndParameters,   // ")" function call
    Separator,       // ","
    Dereference,     // "."
    Wildcard,        // "*"
    LogicalOperator, // "!", "==", etc

    Null,
    Boolean,
    Number,
    String,
    PropertyName,
    Function,
    NamedValue,

    Unexpected,
}

/// `Tokens/Associativity.cs:3-8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Associativity {
    None,
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: TokenKind,
    pub raw_value: String,
    /// Character index (not byte index) of the token start, matching the
    /// positions upstream reports in `ParseException`.
    pub index: usize,
    pub parsed_value: Option<Value>,
}

impl Token {
    fn new(kind: TokenKind, raw_value: String, index: usize, parsed_value: Option<Value>) -> Self {
        Self {
            kind,
            raw_value,
            index,
            parsed_value,
        }
    }

    /// `Tokens/Token.cs:29-48`.
    pub fn associativity(&self) -> Associativity {
        match self.kind {
            TokenKind::StartGroup => Associativity::None,
            TokenKind::LogicalOperator if self.raw_value == "!" => Associativity::RightToLeft,
            _ => {
                if self.is_operator() {
                    Associativity::LeftToRight
                } else {
                    Associativity::None
                }
            }
        }
    }

    /// `Tokens/Token.cs:50-70`.
    pub fn is_operator(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::StartGroup
                | TokenKind::StartIndex
                | TokenKind::StartParameters
                | TokenKind::EndGroup
                | TokenKind::EndIndex
                | TokenKind::EndParameters
                | TokenKind::Separator
                | TokenKind::Dereference
                | TokenKind::LogicalOperator
        )
    }

    /// `Tokens/Token.cs:72-115`.
    pub fn precedence(&self) -> i32 {
        match self.kind {
            TokenKind::StartGroup => 20,
            TokenKind::StartIndex | TokenKind::StartParameters | TokenKind::Dereference => 19,
            TokenKind::LogicalOperator => match self.raw_value.as_str() {
                "!" => 16,
                ">" | ">=" | "<" | "<=" => 11,
                "==" | "!=" => 10,
                "&&" => 6,
                "||" => 5,
                _ => 0,
            },
            TokenKind::EndGroup
            | TokenKind::EndIndex
            | TokenKind::EndParameters
            | TokenKind::Separator => 1,
            _ => 0,
        }
    }

    /// `Tokens/Token.cs:117-149`.
    pub fn operand_count(&self) -> usize {
        match self.kind {
            TokenKind::StartIndex | TokenKind::Dereference => 2,
            TokenKind::LogicalOperator => match self.raw_value.as_str() {
                "!" => 1,
                ">" | ">=" | "<" | "<=" | "==" | "!=" | "&&" | "||" => 2,
                _ => 0,
            },
            _ => 0,
        }
    }
}

/// `ExpressionUtility.IsLegalKeyword` (`Sdk/ExpressionUtility.cs:133-165`).
pub fn is_legal_keyword(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// `LexicalAnalyzer.TestTokenBoundary` (`Tokens/LexicalAnalyzer.cs:295-315`).
fn is_token_boundary(c: char) -> bool {
    matches!(
        c,
        '(' | '[' | ')' | ']' | ',' | '.' | '!' | '>' | '<' | '=' | '&' | '|'
    ) || c.is_whitespace()
}

pub struct LexicalAnalyzer {
    expression: Vec<char>,
    index: usize,
    last_token: Option<Token>,
    unclosed_tokens: Vec<Token>,
}

impl LexicalAnalyzer {
    pub fn new(expression: &str) -> Self {
        Self {
            expression: expression.chars().collect(),
            index: 0,
            last_token: None,
            unclosed_tokens: Vec::new(),
        }
    }

    pub fn has_unclosed_tokens(&self) -> bool {
        !self.unclosed_tokens.is_empty()
    }

    fn peek_unclosed(&self) -> Option<TokenKind> {
        self.unclosed_tokens.last().map(|token| token.kind)
    }

    fn char_at(&self, index: usize) -> Option<char> {
        self.expression.get(index).copied()
    }

    fn substring(&self, start: usize, end: usize) -> String {
        self.expression[start..end].iter().collect()
    }

    /// `LexicalAnalyzer.TryGetNextToken` (`Tokens/LexicalAnalyzer.cs:19-118`).
    pub fn next_token(&mut self) -> Option<Token> {
        while self.char_at(self.index).is_some_and(|c| c.is_whitespace()) {
            self.index += 1;
        }

        let c = self.char_at(self.index)?;

        let token = match c {
            '(' => {
                let kind = if self.last_token.as_ref().map(|token| token.kind)
                    == Some(TokenKind::Function)
                {
                    TokenKind::StartParameters
                } else {
                    TokenKind::StartGroup
                };
                let index = self.index;
                self.index += 1;
                self.create_token(kind, c.to_string(), index, None)
            }
            '[' => {
                let index = self.index;
                self.index += 1;
                self.create_token(TokenKind::StartIndex, c.to_string(), index, None)
            }
            ')' => {
                let kind = if self.peek_unclosed() == Some(TokenKind::StartParameters) {
                    TokenKind::EndParameters
                } else {
                    TokenKind::EndGroup
                };
                let index = self.index;
                self.index += 1;
                self.create_token(kind, c.to_string(), index, None)
            }
            ']' => {
                let index = self.index;
                self.index += 1;
                self.create_token(TokenKind::EndIndex, c.to_string(), index, None)
            }
            ',' => {
                let index = self.index;
                self.index += 1;
                self.create_token(TokenKind::Separator, c.to_string(), index, None)
            }
            '*' => {
                let index = self.index;
                self.index += 1;
                self.create_token(TokenKind::Wildcard, c.to_string(), index, None)
            }
            '\'' => self.read_string_token(),
            '!' | '>' | '<' | '=' | '&' | '|' => self.read_operator(),
            '.' => {
                // A leading "." starts a number when it cannot be a property
                // dereference (`LexicalAnalyzer.cs:86-103`).
                let starts_number = match self.last_token.as_ref().map(|token| token.kind) {
                    None
                    | Some(
                        TokenKind::Separator
                        | TokenKind::StartGroup
                        | TokenKind::StartIndex
                        | TokenKind::StartParameters
                        | TokenKind::LogicalOperator,
                    ) => true,
                    Some(_) => false,
                };
                if starts_number {
                    self.read_number_token()
                } else {
                    let index = self.index;
                    self.index += 1;
                    self.create_token(TokenKind::Dereference, c.to_string(), index, None)
                }
            }
            '-' | '+' | '0'..='9' => self.read_number_token(),
            _ => self.read_keyword_token(),
        };

        self.last_token = Some(token.clone());
        Some(token)
    }

    /// `LexicalAnalyzer.ReadNumberToken` (`Tokens/LexicalAnalyzer.cs:120-139`).
    fn read_number_token(&mut self) -> Token {
        let start_index = self.index;
        loop {
            self.index += 1;
            match self.char_at(self.index) {
                Some(c) if !is_token_boundary(c) || c == '.' => continue,
                _ => break,
            }
        }

        let raw = self.substring(start_index, self.index);
        let parsed = parse_number(&raw);
        if parsed.is_nan() {
            return self.create_token(TokenKind::Unexpected, raw, start_index, None);
        }
        self.create_token(
            TokenKind::Number,
            raw,
            start_index,
            Some(Value::Number(parsed)),
        )
    }

    /// `LexicalAnalyzer.ReadKeywordToken` (`Tokens/LexicalAnalyzer.cs:141-210`).
    fn read_keyword_token(&mut self) -> Token {
        let start_index = self.index;
        self.index += 1;
        while self
            .char_at(self.index)
            .is_some_and(|c| !is_token_boundary(c))
        {
            self.index += 1;
        }

        let raw = self.substring(start_index, self.index);
        if !is_legal_keyword(&raw) {
            return self.create_token(TokenKind::Unexpected, raw, start_index, None);
        }

        if self.last_token.as_ref().map(|token| token.kind) == Some(TokenKind::Dereference) {
            return self.create_token(TokenKind::PropertyName, raw, start_index, None);
        }

        match raw.as_str() {
            "null" => {
                return self.create_token(TokenKind::Null, raw, start_index, Some(Value::Null))
            }
            "true" => {
                return self.create_token(
                    TokenKind::Boolean,
                    raw,
                    start_index,
                    Some(Value::Boolean(true)),
                );
            }
            "false" => {
                return self.create_token(
                    TokenKind::Boolean,
                    raw,
                    start_index,
                    Some(Value::Boolean(false)),
                );
            }
            "NaN" => {
                return self.create_token(
                    TokenKind::Number,
                    raw,
                    start_index,
                    Some(Value::Number(f64::NAN)),
                );
            }
            "Infinity" => {
                return self.create_token(
                    TokenKind::Number,
                    raw,
                    start_index,
                    Some(Value::Number(f64::INFINITY)),
                );
            }
            _ => {}
        }

        // Lookahead: a keyword directly followed by "(" is a function.
        let mut lookahead = self.index;
        while self.char_at(lookahead).is_some_and(|c| c.is_whitespace()) {
            lookahead += 1;
        }
        let kind = if self.char_at(lookahead) == Some('(') {
            TokenKind::Function
        } else {
            TokenKind::NamedValue
        };
        self.create_token(kind, raw, start_index, None)
    }

    /// `LexicalAnalyzer.ReadStringToken` (`Tokens/LexicalAnalyzer.cs:212-246`).
    fn read_string_token(&mut self) -> Token {
        let start_index = self.index;
        let mut value = String::new();
        let mut closed = false;
        self.index += 1; // Skip the leading single-quote.
        while let Some(c) = self.char_at(self.index) {
            self.index += 1;
            if c == '\'' {
                if self.char_at(self.index) != Some('\'') {
                    closed = true;
                    break;
                }
                // Escaped single quote.
                self.index += 1;
            }
            value.push(c);
        }

        let raw = self.substring(start_index, self.index);
        if closed {
            self.create_token(
                TokenKind::String,
                raw,
                start_index,
                Some(Value::String(value)),
            )
        } else {
            self.create_token(TokenKind::Unexpected, raw, start_index, None)
        }
    }

    /// `LexicalAnalyzer.ReadOperator` (`Tokens/LexicalAnalyzer.cs:248-293`).
    fn read_operator(&mut self) -> Token {
        let start_index = self.index;
        self.index += 1;

        if self.index < self.expression.len() {
            self.index += 1;
            let raw = self.substring(start_index, start_index + 2);
            if matches!(raw.as_str(), "!=" | ">=" | "<=" | "==" | "&&" | "||") {
                return self.create_token(TokenKind::LogicalOperator, raw, start_index, None);
            }
            self.index -= 1;
        }

        let raw = self.substring(start_index, start_index + 1);
        if matches!(raw.as_str(), "!" | ">" | "<") {
            return self.create_token(TokenKind::LogicalOperator, raw, start_index, None);
        }

        while self
            .char_at(self.index)
            .is_some_and(|c| !is_token_boundary(c))
        {
            self.index += 1;
        }
        let raw = self.substring(start_index, self.index);
        self.create_token(TokenKind::Unexpected, raw, start_index, None)
    }

    fn check_last_token(&self, allowed: &[Option<TokenKind>]) -> bool {
        let last = self.last_token.as_ref().map(|token| token.kind);
        allowed.contains(&last)
    }

    /// `LexicalAnalyzer.CreateToken` (`Tokens/LexicalAnalyzer.cs:326-467`) —
    /// the grammar's "what may follow what" table, which is what makes
    /// malformed expressions a parse error instead of a silent no-op.
    fn create_token(
        &mut self,
        kind: TokenKind,
        raw_value: String,
        index: usize,
        parsed_value: Option<Value>,
    ) -> Token {
        const AFTER_OPERAND: &[Option<TokenKind>] = &[
            Some(TokenKind::EndGroup),
            Some(TokenKind::EndParameters),
            Some(TokenKind::EndIndex),
            Some(TokenKind::Wildcard),
            Some(TokenKind::Null),
            Some(TokenKind::Boolean),
            Some(TokenKind::Number),
            Some(TokenKind::String),
            Some(TokenKind::PropertyName),
            Some(TokenKind::NamedValue),
        ];
        const BEFORE_OPERAND: &[Option<TokenKind>] = &[
            None,
            Some(TokenKind::Separator),
            Some(TokenKind::StartGroup),
            Some(TokenKind::StartParameters),
            Some(TokenKind::StartIndex),
            Some(TokenKind::LogicalOperator),
        ];

        let legal = match kind {
            TokenKind::StartGroup => self.check_last_token(BEFORE_OPERAND),
            TokenKind::StartIndex => self.check_last_token(&[
                Some(TokenKind::EndGroup),
                Some(TokenKind::EndParameters),
                Some(TokenKind::EndIndex),
                Some(TokenKind::Wildcard),
                Some(TokenKind::PropertyName),
                Some(TokenKind::NamedValue),
            ]),
            TokenKind::StartParameters => self.check_last_token(&[Some(TokenKind::Function)]),
            TokenKind::EndGroup | TokenKind::EndIndex => self.check_last_token(AFTER_OPERAND),
            TokenKind::EndParameters => {
                self.check_last_token(&[Some(TokenKind::StartParameters)])
                    || self.check_last_token(AFTER_OPERAND)
            }
            TokenKind::Separator => self.check_last_token(AFTER_OPERAND),
            TokenKind::Dereference => self.check_last_token(&[
                Some(TokenKind::EndGroup),
                Some(TokenKind::EndParameters),
                Some(TokenKind::EndIndex),
                Some(TokenKind::Wildcard),
                Some(TokenKind::PropertyName),
                Some(TokenKind::NamedValue),
            ]),
            TokenKind::Wildcard => {
                self.check_last_token(&[Some(TokenKind::StartIndex), Some(TokenKind::Dereference)])
            }
            TokenKind::LogicalOperator => {
                if raw_value == "!" {
                    self.check_last_token(BEFORE_OPERAND)
                } else {
                    self.check_last_token(AFTER_OPERAND)
                }
            }
            TokenKind::Null
            | TokenKind::Boolean
            | TokenKind::Number
            | TokenKind::String
            | TokenKind::Function
            | TokenKind::NamedValue => self.check_last_token(&[
                None,
                Some(TokenKind::Separator),
                Some(TokenKind::StartIndex),
                Some(TokenKind::StartGroup),
                Some(TokenKind::StartParameters),
                Some(TokenKind::LogicalOperator),
            ]),
            TokenKind::PropertyName => self.check_last_token(&[Some(TokenKind::Dereference)]),
            TokenKind::Unexpected => false,
        };

        if !legal {
            return Token::new(TokenKind::Unexpected, raw_value, index, None);
        }

        let token = Token::new(kind, raw_value.clone(), index, parsed_value);

        match kind {
            TokenKind::StartGroup | TokenKind::StartIndex | TokenKind::StartParameters => {
                self.unclosed_tokens.push(token.clone());
            }
            TokenKind::EndGroup => {
                if self.peek_unclosed() != Some(TokenKind::StartGroup) {
                    return Token::new(TokenKind::Unexpected, raw_value, index, None);
                }
                self.unclosed_tokens.pop();
            }
            TokenKind::EndIndex => {
                if self.peek_unclosed() != Some(TokenKind::StartIndex) {
                    return Token::new(TokenKind::Unexpected, raw_value, index, None);
                }
                self.unclosed_tokens.pop();
            }
            TokenKind::EndParameters => {
                if self.peek_unclosed() != Some(TokenKind::StartParameters) {
                    return Token::new(TokenKind::Unexpected, raw_value, index, None);
                }
                self.unclosed_tokens.pop();
            }
            TokenKind::Separator => {
                if self.peek_unclosed() != Some(TokenKind::StartParameters) {
                    return Token::new(TokenKind::Unexpected, raw_value, index, None);
                }
            }
            _ => {}
        }

        token
    }
}

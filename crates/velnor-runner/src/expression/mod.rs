//! Runner-specific evaluation around the shared Runner expression parser.

pub mod eval;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests;

use std::fmt;

pub use eval::{evaluate, evaluate_node, EvaluationContext};
use velnor_expression::{Node, ParseError};

/// The root contexts GitHub always defines for a step. Referencing anything
/// outside this set is a parse error upstream
/// (`ExpressionParser.cs:144-147`), never a silent null.
pub const ROOT_CONTEXTS: &[&str] = &[
    "github", "env", "job", "jobs", "runner", "steps", "secrets", "strategy", "matrix", "needs",
    "inputs", "vars",
];

/// The extension functions the worker registers
/// (`src/Runner.Worker/StepsRunner.cs:92-97`), with upstream's arities.
pub const RUNNER_FUNCTIONS: &[(&str, usize, usize)] = &[
    ("success", 0, 0),
    ("failure", 0, 0),
    ("always", 0, 0),
    ("cancelled", 0, 0),
    ("hashFiles", 1, 255),
];

/// Whether a tree reads anything that only exists once the job is running.
///
/// `env`, `steps`, `job`, `jobs` and `runner` are populated by the executor as
/// steps run, and every runner extension function answers from live step
/// state. A tree touching one of them cannot be evaluated at job setup; it has
/// to be deferred verbatim to the step-time pass.
pub fn reads_runtime_context(node: &Node) -> bool {
    match node {
        Node::NamedValue(name) => matches!(
            name.to_ascii_lowercase().as_str(),
            "env" | "steps" | "job" | "jobs" | "runner"
        ),
        Node::Function { name, .. } => {
            RUNNER_FUNCTIONS
                .iter()
                .any(|(known, _, _)| known.eq_ignore_ascii_case(name))
                || node
                    .children()
                    .iter()
                    .any(|child| reads_runtime_context(child))
        }
        node => node
            .children()
            .iter()
            .any(|child| reads_runtime_context(child)),
    }
}

/// A typed expression failure. A condition that produces one of these fails
/// the step, matching `src/Runner.Worker/StepsRunner.cs:231-242`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionError {
    Parse(ParseError),
    /// Upstream raises these from function bodies, e.g. `FormatException` in
    /// `Sdk/Functions/Format.cs:41` or the `case` predicate check in
    /// `Sdk/Functions/Case.cs:19-30`.
    Evaluation(String),
}

impl ExpressionError {
    pub fn evaluation(message: impl Into<String>) -> Self {
        ExpressionError::Evaluation(message.into())
    }
}

impl fmt::Display for ExpressionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpressionError::Parse(error) => error.fmt(f),
            ExpressionError::Evaluation(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ExpressionError {}

impl From<ParseError> for ExpressionError {
    fn from(error: ParseError) -> Self {
        ExpressionError::Parse(error)
    }
}

//! Step-condition evaluation: `if:` parsing, status functions, and cancelled re-evaluation.
//!
//! Moved out of `executor.rs` (decomposition slice 3, GOAL 49/58). The
//! step-gating surface — the default `success()` condition, the implicit
//! `success() && (...)` prefix, the `success`/`failure`/`cancelled`/`always`
//! status functions, the `job.status` / `github.action_status` derivations,
//! the live-token cancelled read that replaces upstream's re-evaluation pass,
//! and the immutable-`github` static-false preflight — lives here behind a
//! narrow `pub(crate)` API so the gating truth tables hold in one place.
//! Behavior is byte-identical to the pre-move code: bodies moved verbatim,
//! visibility widened only where a cross-module caller needs it.

use crate::{
    executor::{JobExecutionState, JobExpressionContext},
    expression,
};
use serde_json::Value;

impl JobExecutionState {
    /// Whether this job has been cancelled.
    ///
    /// Upstream sets `JobContext.Status` to `cancelled` on the cancellation
    /// callback and re-evaluates every remaining step's condition against it
    /// (`src/Runner.Worker/StepsRunner.cs:146-187`). Velnor evaluates each
    /// step's condition immediately before dispatching it, so reading the live
    /// token here gives the same result without a separate re-evaluation pass.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Install the running job's cancellation.
    pub(crate) fn set_cancellation(
        &mut self,
        cancellation: crate::execution::cancel::JobCancellation,
    ) {
        self.cancellation = cancellation;
    }

    /// `success()` — `src/Runner.Worker/Expressions/SuccessFunction.cs:28-39`
    /// reads the status of the enclosing scope, which cancellation sets to
    /// `cancelled`. A cancelled job is therefore not successful, which is what
    /// stops its remaining ordinary steps: their implicit condition is
    /// `success()`.
    pub(crate) fn success_status(&self) -> bool {
        !self.is_cancelled() && !self.status_scope_has_failure()
    }

    /// `failure()` — `src/Runner.Worker/Expressions/FailureFunction.cs:28-39`.
    /// Also false under cancellation: the status is `cancelled`, not `failure`,
    /// so `if: failure()` cleanup does not run on a cancelled job.
    pub(crate) fn failure_status(&self) -> bool {
        !self.is_cancelled() && self.status_scope_has_failure()
    }

    /// `job.status` — `cancelled` once the job is cancelled, otherwise
    /// `success` unless a step has already concluded failed. Converted
    /// inner ids (see `convert_conclusions`) do not count: upstream
    /// derives the status from top-level step results only.
    pub(crate) fn job_status(&self) -> &'static str {
        if self.is_cancelled() {
            "cancelled"
        } else if self
            .composite_scopes
            .top_level_has_failure(&self.conclusions)
        {
            "failure"
        } else {
            "success"
        }
    }

    /// `github.action_status` — the composite-scoped equivalent of
    /// `job.status`, matching how `SuccessFunction` picks its source
    /// (`src/Runner.Worker/Expressions/SuccessFunction.cs:28-39`).
    pub(crate) fn action_status(&self) -> &'static str {
        if self.is_cancelled() {
            "cancelled"
        } else if self.status_scope_has_failure() {
            "failure"
        } else {
            "success"
        }
    }

    fn status_scope_has_failure(&self) -> bool {
        self.composite_scopes.scope_has_failure(&self.conclusions)
    }

    /// Evaluate a step condition.
    ///
    /// The default condition is `success()`, and a condition that does not
    /// itself reference a status function is implicitly `success() && (...)`
    /// — how the service composes `if:` before handing the runner a
    /// `Condition` string.
    ///
    /// A condition that fails to evaluate returns `Err`, which callers must
    /// turn into a failed step, matching
    /// `src/Runner.Worker/StepsRunner.cs:231-242`.
    pub(crate) fn evaluate_condition(
        &self,
        condition: Option<&str>,
    ) -> Result<bool, expression::ExpressionError> {
        let Some(condition) = condition
            .map(str::trim)
            .filter(|condition| !condition.is_empty())
        else {
            return Ok(self.success_status());
        };
        self.evaluate_condition_expression(strip_expression(condition))
    }

    /// Pre/post step conditions. An absent condition runs unconditionally
    /// (`ActionRunner` only registers a pre/post step when its condition is
    /// satisfied), otherwise the `success()` default applies as above.
    ///
    /// A condition that fails to evaluate returns `Err`, which callers turn
    /// into a failed pre/post record — upstream fails the owning step
    /// (`src/Runner.Worker/StepsRunner.cs:231-242`), never silently skips it.
    pub(crate) fn evaluate_post_condition(
        &self,
        condition: Option<&str>,
    ) -> Result<bool, expression::ExpressionError> {
        let Some(condition) = condition
            .map(str::trim)
            .filter(|condition| !condition.is_empty())
        else {
            return Ok(true);
        };
        self.evaluate_condition_expression(strip_expression(condition))
    }

    fn evaluate_condition_expression(
        &self,
        expression: &str,
    ) -> Result<bool, expression::ExpressionError> {
        let context = self.expression_context();
        let Some(node) = expression::parse(expression, &context)? else {
            return Ok(self.success_status());
        };
        // `success() && (...)` short-circuits, so a job that has already failed
        // — or been cancelled — never evaluates, and therefore never errors on,
        // the rest.
        if !node_references_status_function(&node) && !self.success_status() {
            return Ok(false);
        }
        Ok(expression::evaluate_node(&node, &context)?.is_truthy())
    }
}

/// Return true only when a step condition is provably false from immutable
/// GitHub job context before execution begins. Local composite actions are
/// prepared mid-job by actions/runner, after their parent condition has been
/// evaluated; this narrow preflight proof lets the planner preserve that
/// behavior without treating runtime `steps`, `needs`, or `env` state as
/// known early.
pub(crate) fn condition_is_statically_false(
    condition: Option<&str>,
    base_env: &[(String, String)],
    context_data: &[(String, Value)],
) -> bool {
    let Some(condition) = condition else {
        return false;
    };
    let Ok(state) = JobExecutionState::try_new_with_context(base_env, context_data) else {
        // A malformed immutable expression is not proof that a condition is
        // false; leave the step for normal fail-closed lifecycle handling.
        return false;
    };
    let context = state.expression_context();
    let Ok(Some(node)) = expression::parse(strip_expression(condition), &context) else {
        // An expression that does not even parse is not provably false.
        return false;
    };
    immutable_expression_is_false(&node, &context)
}

/// Whether the tree calls one of the runner's status functions, which is what
/// suppresses the implicit `success() &&` prefix.
fn node_references_status_function(node: &expression::Node) -> bool {
    if let expression::Node::Function { name, .. } = node
        && matches!(
            name.to_ascii_lowercase().as_str(),
            "success" | "failure" | "always" | "cancelled"
        )
    {
        return true;
    }
    node.children()
        .iter()
        .any(|child| node_references_status_function(child))
}

/// Walk the parsed condition rather than its text: a conjunction is provably
/// false when any operand is, a disjunction only when every operand is, and a
/// leaf counts only when it reads exclusively immutable `github` context.
fn immutable_expression_is_false(
    node: &expression::Node,
    context: &JobExpressionContext<'_>,
) -> bool {
    match node {
        expression::Node::And(parameters) => parameters
            .iter()
            .any(|parameter| immutable_expression_is_false(parameter, context)),
        expression::Node::Or(parameters) => parameters
            .iter()
            .all(|parameter| immutable_expression_is_false(parameter, context)),
        node => {
            if !reads_only_immutable_github(node) {
                return false;
            }
            matches!(
                expression::evaluate_node(node, context),
                Ok(value) if value.is_falsy()
            )
        }
    }
}

/// True when the subtree reads the `github` context and nothing that only
/// exists once the job is running: other root contexts, the status functions,
/// or `hashFiles`.
fn reads_only_immutable_github(node: &expression::Node) -> bool {
    fn walk(node: &expression::Node, saw_github: &mut bool, immutable: &mut bool) {
        match node {
            expression::Node::NamedValue(name) => {
                if name.eq_ignore_ascii_case("github") {
                    *saw_github = true;
                } else {
                    *immutable = false;
                }
            }
            expression::Node::Function { .. } => *immutable = false,
            _ => {}
        }
        for child in node.children() {
            walk(child, saw_github, immutable);
        }
    }

    let mut saw_github = false;
    let mut immutable = true;
    walk(node, &mut saw_github, &mut immutable);
    saw_github && immutable
}

fn strip_expression(condition: &str) -> &str {
    condition
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map(str::trim)
        .unwrap_or(condition)
}

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
mod tests {
    use super::*;
    use crate::{executor::StepExecutionResult, script_step::StepCommandState};

    /// D-7: a condition that cannot be evaluated was fail-open and ran the
    /// step. Upstream fails the step (src/Runner.Worker/StepsRunner.cs:231-242),
    /// which requires a typed error out of the evaluator.
    #[test]
    fn condition_evaluation_failure_is_reported() {
        let state = JobExecutionState::default();

        for condition in [
            "unknownContext.value",
            "noSuchFunction('a')",
            "contains('a')",
            "github.ref ==",
            // The live dual-lane pin evaluates `fromJSON` over a runtime
            // output holding invalid JSON, behind an `always() &&` guard so
            // the implicit `success() &&` prefix cannot short-circuit it to
            // a skip after the designed failure; upstream throws out of
            // `JToken.ReadFrom` (`FromJson.cs`) exactly like the serde
            // failure below, so both runners fail the step.
            "fromJSON('not json')",
        ] {
            assert!(
                state.evaluate_condition(Some(condition)).is_err(),
                "{condition} must fail the step rather than run it"
            );
        }
        // Failed-state trap the live pin must avoid: after the designed
        // failure the implicit `success() &&` prefix short-circuits a bare
        // `fromJSON(...)` condition to a skip on both runners (GitHub docs;
        // `evaluate_condition_expression`), so the pin would assert nothing.
        // The `always() &&` guard forces evaluation of the RHS instead.
        let mut failed = JobExecutionState::default();
        failed.apply(
            "real-failure",
            &StepExecutionResult {
                exit_code: 1,
                state: StepCommandState::default(),
                skipped: false,
                failure_ignored: false,
                stdout: String::new(),
                stderr: String::new(),
            },
        );
        assert!(
            !failed
                .evaluate_condition(Some("fromJSON('not json')"))
                .unwrap(),
            "bare fromJSON skips after a failure — the live pin must not use this shape"
        );
        assert!(
            failed
                .evaluate_condition(Some("always() && fromJSON('not json') == ''"))
                .is_err(),
            "the live-pin shape must fail the step, not skip it"
        );
        // A failed condition is not "provably false" either: the planner must
        // not prune the step on it.
        assert!(!condition_is_statically_false(
            Some("noSuchFunction('a')"),
            &[],
            &[]
        ));
    }

    #[test]
    fn immutable_github_condition_can_prove_local_action_is_skipped() {
        let context = vec![(
            "github".to_string(),
            serde_json::json!({
                "ref": "refs/heads/perf/subminute-ci",
                "event_name": "workflow_dispatch"
            }),
        )];
        let condition = "github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'workflow_dispatch')";

        assert!(condition_is_statically_false(
            Some(condition),
            &[],
            &context
        ));
        assert!(condition_is_statically_false(
            Some(&format!("success() && ({condition})")),
            &[],
            &context
        ));
        assert!(!condition_is_statically_false(
            Some("steps.changes.outputs.docs == 'true'"),
            &[],
            &context
        ));
        assert!(!condition_is_statically_false(
            Some("success() && github.ref != ''"),
            &[],
            &context
        ));
    }
}

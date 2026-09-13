//! Step outcome/conclusion application: recording results, `continue-on-error` merging, and `steps` context exposure.
//!
//! Moved out of `executor.rs` (decomposition slice 5, GOAL 49/58). The
//! application surface — the outcome-vs-conclusion split (`apply` derives
//! `outcome` from `skipped`/`exit_code` and `conclusion` by converting
//! `failure` to `success` only when `failure_ignored`, i.e. upstream's
//! `ApplyContinueOnError`), the cancelled override (`apply_cancelled`
//! records `cancelled`/`cancelled`, never converted), and the
//! `steps.<id>.{outputs,outcome,conclusion}` context built from the
//! runtime step state — lives here behind a narrow `pub(crate)` API.
//! Behavior is byte-identical to the pre-move code: bodies moved verbatim,
//! visibility widened only where a cross-module caller needs it.

use crate::{
    execution::StepOutcome,
    executor::{JobExecutionState, JobExpressionContext, StepExecutionResult},
    expression,
};

impl JobExecutionState {
    pub(crate) fn apply(&mut self, step_id: &str, result: &StepExecutionResult) {
        let outcome = if result.skipped {
            StepOutcome::Skipped
        } else if result.exit_code == 0 {
            StepOutcome::Success
        } else {
            StepOutcome::Failure
        };
        let conclusion = if result.failure_ignored && outcome == StepOutcome::Failure {
            StepOutcome::Success
        } else {
            outcome
        };
        self.outcomes.insert(step_id.to_string(), outcome);
        self.conclusions.insert(step_id.to_string(), conclusion);
        self.composite_scopes.record(step_id, conclusion);

        if !result.state.outputs.is_empty() {
            self.outputs
                .insert(step_id.to_string(), result.state.outputs.clone());
        }
        if !result.state.state.is_empty() {
            self.action_states
                .entry(step_id.to_string())
                .or_default()
                .extend(result.state.state.clone());
        }
        for (name, value) in &result.state.env {
            self.env.insert(name.clone(), value.clone());
            if let Some(existing) = self
                .dynamic_env
                .iter_mut()
                .find(|(existing_name, _)| existing_name == name)
            {
                existing.1 = value.clone();
            } else {
                self.dynamic_env.push((name.clone(), value.clone()));
            }
        }
        for path in result.state.path.iter().rev() {
            self.path.insert(0, path.clone());
        }
        self.masks.extend(result.state.masks.iter().cloned());
    }

    /// Record a step killed by cancellation. Upstream completes it
    /// `TaskResult.Canceled` (`src/Runner.Worker/StepsRunner.cs:331-337`),
    /// so `steps.<id>.outcome` and `steps.<id>.conclusion` read `cancelled`
    /// — never `failure`, and never converted by `continue-on-error`
    /// (`ApplyContinueOnError` only converts `Failed`).
    pub(crate) fn apply_cancelled(&mut self, step_id: &str, result: &StepExecutionResult) {
        self.apply(step_id, result);
        self.outcomes
            .insert(step_id.to_string(), StepOutcome::Cancelled);
        self.conclusions
            .insert(step_id.to_string(), StepOutcome::Cancelled);
        self.composite_scopes
            .record(step_id, StepOutcome::Cancelled);
    }
}

impl JobExpressionContext<'_> {
    /// `steps.<id>.{outputs,outcome,conclusion}`, built from the runtime
    /// step state rather than parsed out of the expression text.
    pub(crate) fn steps_context(&self) -> expression::Value {
        let mut ids: Vec<&String> = Vec::new();
        for id in self
            .state
            .outputs
            .keys()
            .chain(self.state.outcomes.keys())
            .chain(self.state.conclusions.keys())
        {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }

        let entries = ids
            .into_iter()
            .map(|id| {
                let mut step: Vec<(String, expression::Value)> = Vec::new();
                let outputs = self
                    .state
                    .outputs
                    .get(id)
                    .map(|outputs| {
                        outputs
                            .iter()
                            .map(|(name, value)| (name.clone(), expression::Value::string(value)))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                step.push((
                    "outputs".to_string(),
                    expression::Value::Object(expression::ObjectValue::new(outputs)),
                ));
                if let Some(outcome) = self.state.outcomes.get(id) {
                    step.push((
                        "outcome".to_string(),
                        expression::Value::string(outcome.as_str()),
                    ));
                }
                if let Some(conclusion) = self.state.conclusions.get(id) {
                    step.push((
                        "conclusion".to_string(),
                        expression::Value::string(conclusion.as_str()),
                    ));
                }
                (
                    id.clone(),
                    expression::Value::Object(expression::ObjectValue::new(step)),
                )
            })
            .collect();
        expression::Value::Object(expression::ObjectValue::new(entries))
    }
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
    use crate::script_step::StepCommandState;

    #[test]
    fn job_state_flows_env_and_path_to_later_steps() {
        let mut state = JobExecutionState::default();
        state.apply(
            "producer",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState {
                    outputs: [("answer".to_string(), "42".to_string())].into(),
                    env: [("NAME".to_string(), "value".to_string())].into(),
                    path: vec!["/opt/tool".to_string()],
                    masks: vec!["secret".to_string()],
                    ..Default::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let env = state.step_env(&[("GITHUB_OUTPUT".into(), "/__t/out".into())]);

        assert!(env.contains(&("NAME".into(), "value".into())));
        assert!(env.contains(&("GITHUB_OUTPUT".into(), "/__t/out".into())));
        assert!(!env.iter().any(|(name, _)| name == "PATH"));
        assert_eq!(state.path, vec!["/opt/tool"]);
        assert_eq!(state.masks, vec!["secret"]);
        assert_eq!(
            state
                .resolve_expressions("value=${{ steps.producer.outputs.answer }}")
                .unwrap(),
            "value=42"
        );
        assert_eq!(
            state
                .resolve_expressions("value=${{ steps.producer.outputs['answer'] }}")
                .unwrap(),
            "value=42"
        );
        // A context value that is not set is null, and null renders as the
        // empty string (EvaluationResult.cs:140-141). The deleted evaluator
        // rendered the source text instead, which is divergence D-4.
        assert_eq!(
            state.resolve_expressions("keep=${{ github.ref }}").unwrap(),
            "keep="
        );
    }

    #[test]
    fn resolves_step_outputs_in_later_action_env() {
        let mut state = JobExecutionState::default();
        state.apply(
            "meta",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: StepCommandState {
                    outputs: [("tags".to_string(), "image:latest".to_string())].into(),
                    ..Default::default()
                },
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        let env = state
            .resolve_env(&[("INPUT_TAGS".into(), "${{ steps.meta.outputs.tags }}".into())])
            .unwrap();

        assert_eq!(env, vec![("INPUT_TAGS".into(), "image:latest".into())]);
    }
}

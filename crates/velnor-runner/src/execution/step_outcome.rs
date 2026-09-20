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
};

impl JobExecutionState {
    pub(crate) fn apply(&mut self, step_id: &str, result: &StepExecutionResult) {
        self.apply_command_state(step_id, result);
        self.apply_step_outcome(step_id, result);
    }

    /// Apply command-file side effects before `continue-on-error` is
    /// evaluated. Runner processes action files before applying COE, so the
    /// expression can read this step's outputs and the next step can observe
    /// env/path changes even when the action later fails.
    pub(crate) fn apply_command_state(&mut self, step_id: &str, result: &StepExecutionResult) {
        self.apply_command_effects(step_id, result, true, true);
    }

    /// Record outcome/conclusion after all command-file state and any
    /// continue-on-error expression have been applied.
    pub(crate) fn apply_step_outcome(&mut self, step_id: &str, result: &StepExecutionResult) {
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
        self.expose_root_step_context(step_id);
        self.outcomes.insert(step_id.to_string(), outcome);
        self.conclusions.insert(step_id.to_string(), conclusion);
        self.composite_scopes.record(step_id, conclusion);
    }

    /// Post children have no context name in actions/runner. Their results
    /// update root job status, but their random execution ids must not appear
    /// in `steps.*` or output lookups.
    pub(crate) fn apply_post(&mut self, step_id: &str, result: &StepExecutionResult) {
        self.apply_command_effects(step_id, result, false, false);
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
        self.composite_scopes.record_job(step_id, conclusion);
    }

    fn apply_command_effects(
        &mut self,
        step_id: &str,
        result: &StepExecutionResult,
        expose_outputs: bool,
        expose_action_state: bool,
    ) {
        if expose_outputs && !result.state.outputs.is_empty() {
            self.outputs
                .insert(step_id.to_string(), result.state.outputs.clone());
        }
        if expose_action_state && !result.state.state.is_empty() {
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
        self.apply_command_state(step_id, result);
        self.apply_cancelled_outcome(step_id);
    }

    /// Record the canceled conclusion after callers already applied the
    /// command-file effects from this step.
    pub(crate) fn apply_cancelled_outcome(&mut self, step_id: &str) {
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
    pub(crate) fn steps_context(&self) -> velnor_expression::Value {
        let ids: Vec<(String, String)> =
            if let Some(visible) = self.state.active_composite_step_ids() {
                visible
                    .iter()
                    .map(|(visible_id, runtime_id)| (visible_id.clone(), runtime_id.clone()))
                    .collect()
            } else {
                self.state
                    .root_step_context_ids
                    .iter()
                    .map(|id| (id.clone(), id.clone()))
                    .collect()
            };

        let entries = ids
            .into_iter()
            .map(|(visible_id, runtime_id)| {
                let mut step: Vec<(String, velnor_expression::Value)> = Vec::new();
                let outputs = self
                    .state
                    .outputs
                    .get(&runtime_id)
                    .map(|outputs| {
                        outputs
                            .iter()
                            .map(|(name, value)| {
                                (name.clone(), velnor_expression::Value::string(value))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                step.push((
                    "outputs".to_string(),
                    velnor_expression::Value::Object(velnor_expression::ObjectValue::new(outputs)),
                ));
                if let Some(outcome) = self.state.outcomes.get(&runtime_id) {
                    step.push((
                        "outcome".to_string(),
                        velnor_expression::Value::string(outcome.as_str()),
                    ));
                }
                if let Some(conclusion) = self.state.conclusions.get(&runtime_id) {
                    step.push((
                        "conclusion".to_string(),
                        velnor_expression::Value::string(conclusion.as_str()),
                    ));
                }
                (
                    visible_id,
                    velnor_expression::Value::Object(velnor_expression::ObjectValue::new(step)),
                )
            })
            .collect();
        velnor_expression::Value::Object(velnor_expression::ObjectValue::new(entries))
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

    #[test]
    fn step_output_json_keeps_first_key_spelling_and_latest_value() {
        let mut command_state = StepCommandState::default();
        command_state.outputs.insert("z".into(), "old".into());
        command_state.outputs.insert("a".into(), "first".into());
        command_state.outputs.insert("Z".into(), "new".into());
        let mut state = JobExecutionState::default();
        state.apply(
            "x",
            &StepExecutionResult {
                exit_code: 0,
                skipped: false,
                failure_ignored: false,
                state: command_state,
                stdout: String::new(),
                stderr: String::new(),
            },
        );

        assert_eq!(
            state
                .resolve_expressions("outputs=${{ toJSON(steps.x.outputs) }}")
                .unwrap(),
            "outputs={\n  \"z\": \"new\",\n  \"a\": \"first\"\n}"
        );
    }
}

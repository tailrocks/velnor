//! Composite conclusion scopes: the nested-umbrella bookkeeping behind
//! step outcome/conclusion tracking.
//!
//! Moved out of `executor.rs` (decomposition pilot, GOAL 49/58). The frame
//! stack, the converted-id set, and the converted-aware status scans live
//! here behind a narrow API so the push/pop/record/scan invariants hold in
//! one place. Behavior is byte-identical to the pre-move code: bodies moved
//! verbatim, field renames only.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepOutcome {
    Success,
    Failure,
    Cancelled,
    Skipped,
}

impl StepOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StepOutcome::Success => "success",
            StepOutcome::Failure => "failure",
            StepOutcome::Cancelled => "cancelled",
            StepOutcome::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct CompositeConclusionFrame {
    step_id: String,
    conclusions: BTreeMap<String, StepOutcome>,
    /// Transitive inner ids popped from nested scopes. Kept separate from
    /// `conclusions` so the scope status scan never sees them; an ignored
    /// outer umbrella converts them with its own direct ids.
    descendants: BTreeSet<String>,
}

/// Nested composite conclusion scopes for one job: the open-umbrella stack
/// plus the ignored-umbrella conversion set.
#[derive(Debug, Clone, Default)]
pub(crate) struct CompositeConclusionScopes {
    /// Open scope ids, outermost first. Pushed and popped together with
    /// `frames`; the pop only fires when the top matches the closing id.
    stack: Vec<String>,
    /// Inner step ids converted by an ignored umbrella conclusion. Inner
    /// `Failure` entries stay in `conclusions` (raw `steps.<id>` reads),
    /// but the job-scope status scans skip these ids: upstream
    /// `job.status` derives from top-level step results only, so after an
    /// ignored umbrella a later `failure()` step must not run.
    converted: BTreeSet<String>,
    /// One frame per open composite, outermost first.
    frames: Vec<CompositeConclusionFrame>,
}

impl CompositeConclusionScopes {
    pub(crate) fn push(&mut self, step_id: &str) {
        self.stack.push(step_id.to_string());
        self.frames.push(CompositeConclusionFrame {
            step_id: step_id.to_string(),
            conclusions: BTreeMap::new(),
            descendants: BTreeSet::new(),
        });
    }

    /// Pop a composite scope, returning the transitive inner step ids
    /// (direct plus nested descendants). The transitive set always
    /// propagates to the parent frame's descendants — even when this
    /// level stands — so an ignored outer umbrella converts nested
    /// failures with its own. Descendants never enter `conclusions`,
    /// so the scope status scan is unchanged.
    pub(crate) fn pop(&mut self, step_id: &str) -> Vec<String> {
        if self.stack.last().is_some_and(|scope| scope == step_id) {
            self.stack.pop();
        }
        if self
            .frames
            .last()
            .is_some_and(|frame| frame.step_id == step_id)
        {
            match self.frames.pop() {
                Some(frame) => {
                    let mut transitive: BTreeSet<String> = frame.conclusions.into_keys().collect();
                    transitive.extend(frame.descendants);
                    if let Some(parent) = self.frames.last_mut() {
                        parent.descendants.extend(transitive.iter().cloned());
                    }
                    transitive.into_iter().collect()
                }
                None => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    /// Mark inner step ids as converted by their umbrella's ignored
    /// conclusion. Their `Failure` entries stay in `conclusions` (raw
    /// `steps.<id>` reads), but the job-scope status scans skip them —
    /// upstream `job.status` derives from top-level step results only.
    pub(crate) fn convert(&mut self, step_ids: Vec<String>) {
        self.converted.extend(step_ids);
    }

    /// Defensive-flush twin of the End path's `pop` harvest: a
    /// plan whose `CompositeEnd` never arrived leaves its scopes open, so
    /// an ignored flush umbrella converts every still-open scope's ids.
    /// The stacks stay untouched — only the status scans change.
    pub(crate) fn convert_open_scopes(&mut self) {
        let ids: Vec<String> = self
            .frames
            .iter()
            .flat_map(|frame| {
                frame
                    .conclusions
                    .keys()
                    .cloned()
                    .chain(frame.descendants.iter().cloned())
            })
            .collect();
        self.convert(ids);
    }

    /// Record a completed step's conclusion in the innermost open scope,
    /// if any. This is the scope half of umbrella result application: the
    /// job state records its own outcome/conclusion maps and delegates
    /// the scope write here (both the normal and the cancelled paths).
    pub(crate) fn record(&mut self, step_id: &str, conclusion: StepOutcome) {
        if let Some(frame) = self.frames.last_mut() {
            frame.conclusions.insert(step_id.to_string(), conclusion);
        }
    }

    /// Job-scope status scan: an unconverted `Failure` anywhere in the
    /// top-level conclusions. Converted inner ids (see [`Self::convert`])
    /// do not count: upstream derives the status from top-level step
    /// results only.
    pub(crate) fn top_level_has_failure(
        &self,
        conclusions: &BTreeMap<String, StepOutcome>,
    ) -> bool {
        conclusions
            .iter()
            .any(|(id, outcome)| *outcome == StepOutcome::Failure && !self.converted.contains(id))
    }

    /// Scope status scan: the innermost open scope's conclusions when
    /// inside a composite, else the top-level conclusions. Same conversion
    /// rule as [`Self::top_level_has_failure`].
    pub(crate) fn scope_has_failure(&self, conclusions: &BTreeMap<String, StepOutcome>) -> bool {
        if let Some(frame) = self.frames.last() {
            frame.conclusions.iter().any(|(id, outcome)| {
                *outcome == StepOutcome::Failure && !self.converted.contains(id)
            })
        } else {
            self.top_level_has_failure(conclusions)
        }
    }
}

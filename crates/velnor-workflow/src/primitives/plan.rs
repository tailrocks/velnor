//! `affected-plan`: the affected-selection plan job every aggregate starts from.

use super::{Args, GraphNode, Primitive, RenderCtx, Rendered, AFFECTED_PLAN};
use crate::{GeneratorError, RunnerMode};

/// Render the `plan:` job.
///
/// Planning executes checked-in shell files, so it always runs on a hosted
/// runner and is never trusted-event gated: it must not expose untrusted pull
/// request code to a self-hosted runner, and it must run on every event.
pub(crate) struct AffectedPlan;

impl Primitive for AffectedPlan {
    fn id(&self) -> &'static str {
        AFFECTED_PLAN
    }

    fn schema(&self) -> &'static [&'static str] {
        &[]
    }

    fn render(&self, ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let mut job = String::new();
        ctx.lanes
            .ir()
            .render_plan(&mut job, RunnerMode::Github, false);
        Ok(Rendered {
            nodes: vec![GraphNode::Plan { job }],
            ..Rendered::default()
        })
    }
}

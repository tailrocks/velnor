//! `affected-plan`: the affected-selection plan job every aggregate starts from.

use super::{Args, GraphNode, Primitive, RenderCtx, Rendered, AFFECTED_PLAN};
use crate::{GeneratorError, RunnerMode};

/// Render the `plan:` job.
///
/// Planning executes checked-in shell files on the selected control-plane
/// runner. Self-hosted planning is restricted to the default branch and
/// trusted event types by the generated expression.
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
        let ir = ctx.lanes.ir();
        ir.render_plan(&mut job, ir.runners, ir.runners == RunnerMode::Velnor);
        Ok(Rendered {
            nodes: vec![GraphNode::Plan { job }],
            ..Rendered::default()
        })
    }
}

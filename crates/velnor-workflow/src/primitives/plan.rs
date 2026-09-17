//! `affected-plan`: the affected-selection plan job every aggregate starts from.

use super::{Args, GraphNode, Primitive, RenderCtx, Rendered, AFFECTED_PLAN};
use crate::GeneratorError;

/// Render the `plan:` job.
///
/// Planning is control plane: always hosted. GitHub planning pins `uses:` to
/// `SOURCE_REV`. `rev:` uses a context-gated `${{ github.sha }}` with a
/// static fallback when this repository owns the setup action.
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
        let ir = ctx.providers.ir();
        ir.render_plan(&mut job);
        Ok(Rendered {
            nodes: vec![GraphNode::Plan { job }],
            ..Rendered::default()
        })
    }
}

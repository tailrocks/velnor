//! `unit-aggregation`: the aggregate workflow that composes the CI graph.
//!
//! One declaration per aggregate file: the pull request surface, the main
//! surface, and the nightly dispatcher. PR and main aggregates own composition —
//! the plan job a primitive contributed, the advisory policy job, one
//! reusable-workflow caller per contributed unit node, and the required check
//! that validates every one of them. Nightly dispatches `ci-main.yml` on the
//! default branch so trusted mbx saves fire via `workflow_dispatch`.

use super::WorkflowKind;
use super::{Args, GraphNode, Primitive, RenderCtx, Rendered, UNIT_AGGREGATION};
use crate::GeneratorError;

/// The aggregate files the surface can own, mapped to their workflow kind.
fn kind_for_file(file: &str) -> Option<WorkflowKind> {
    match file {
        "ci-pull-request.yml" | "ci-pr.yml" => Some(WorkflowKind::PullRequest),
        "ci-main.yml" => Some(WorkflowKind::Main),
        "nightly.yml" => Some(WorkflowKind::Nightly),
        _ => None,
    }
}

/// Render one aggregate workflow from the contributed graph nodes.
pub(crate) struct UnitAggregation;

impl Primitive for UnitAggregation {
    fn id(&self) -> &'static str {
        UNIT_AGGREGATION
    }

    fn schema(&self) -> &'static [&'static str] {
        &[]
    }

    fn render(&self, ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let file = ctx.file.unwrap_or_default();
        let kind = kind_for_file(file).ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{UNIT_AGGREGATION}` renders `ci-pull-request.yml`, `ci-pr.yml`, `ci-main.yml`, or `nightly.yml`, not `{file}`"
            ))
        })?;
        // The plan job is a contributed node, not something the aggregate
        // renders for itself: an aggregate without a plan is a broken graph.
        let plan = ctx.nodes.iter().find_map(GraphNode::plan_job);
        if plan.is_none() {
            return Err(GeneratorError::usage(format!(
                "`{UNIT_AGGREGATION}` needs a declared `{}` row before it can compose the graph",
                super::AFFECTED_PLAN
            )));
        }
        let content = if kind == WorkflowKind::Nightly {
            ctx.lanes.ir().render_nightly_dispatcher()
        } else {
            ctx.lanes
                .ir()
                .render_nested(kind, ctx.nodes, Some(ctx.contracts))
        };
        Ok(Rendered {
            files: std::iter::once((
                std::path::PathBuf::from(".github/workflows").join(file),
                content,
            ))
            .collect(),
            ..Rendered::default()
        })
    }
}

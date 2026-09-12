//! `lane-matrix`: the hosted and self-hosted lane fan-out with its trust gate.
//!
//! The lane matrix is the contract every unit pipeline renders against: which
//! lane jobs a unit emits, which runner each lane selects, and which lane is
//! allowed to save a cache entry. The self-hosted lane is always gated to
//! trusted events on the default branch — that gate is the boundary that keeps
//! untrusted pull request code off a self-hosted runner, so it is a law of the
//! primitive and not a declared argument.

use super::{Args, LaneJob, Primitive, RenderCtx, Rendered, LANE_MATRIX};
use crate::{GeneratorError, ProjectConfig, RunnerMode};

/// Render nothing: the lane matrix is a contract, not a workflow family.
pub(crate) struct LaneMatrix;

impl Primitive for LaneMatrix {
    fn id(&self) -> &'static str {
        LANE_MATRIX
    }

    fn schema(&self) -> &'static [&'static str] {
        &["jobs"]
    }

    fn render(&self, _ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        Ok(Rendered::default())
    }
}

/// The resolved lane matrix.
pub(crate) struct ResolvedLanes {
    ir: super::WorkflowIr,
    jobs: Vec<LaneJob>,
}

impl ResolvedLanes {
    pub(crate) fn ir(&self) -> &super::WorkflowIr {
        &self.ir
    }

    pub(crate) fn jobs(&self) -> &[LaneJob] {
        &self.jobs
    }
}

/// Resolve the lane matrix from the declared rows over the runner mode.
///
/// # Errors
/// Returns a usage error for a declared lane the runner mode does not support.
pub(crate) fn resolve(
    config: &ProjectConfig,
    rows: &[super::ResolvedRow],
) -> Result<ResolvedLanes, GeneratorError> {
    let ir = super::WorkflowIr::from_config(config);
    let default = ir.default_lane_jobs(true);
    let mut jobs = default.clone();
    for row in rows.iter().filter(|row| row.primitive == LANE_MATRIX) {
        if let Some(names) = Args(&row.args).strings("jobs")? {
            jobs = names
                .iter()
                .map(|name| {
                    default
                        .iter()
                        .find(|job| job.lane.as_str() == name.as_str())
                        .copied()
                        .ok_or_else(|| {
                            GeneratorError::usage(format!(
                                "`{LANE_MATRIX}` lane `{name}` is not supported by the `{}` runner mode; supported lanes: {}",
                                config.runners.as_str(),
                                default
                                    .iter()
                                    .map(|job| job.lane.as_str().to_owned())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                        })
                })
                .collect::<Result<Vec<_>, GeneratorError>>()?;
        }
    }
    if jobs.is_empty() {
        return Err(GeneratorError::usage(
            "the lane matrix resolved to no lane job; at least one verification lane is required",
        ));
    }
    // The self-hosted lane is emitted after the hosted lane, the order the
    // aggregate and every unit surface has always used.
    jobs.sort_by_key(|job| match job.lane {
        RunnerMode::Github => 0,
        RunnerMode::Velnor => 1,
        RunnerMode::Both => 2,
    });
    Ok(ResolvedLanes { ir, jobs })
}
